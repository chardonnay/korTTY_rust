use std::fmt;
use std::io::{self, Cursor};
use std::mem::{offset_of, size_of};
use std::ptr;
use std::sync::Arc;

use image::{ImageFormat, ImageReader, Limits, RgbaImage};
use raw_window_handle::Win32WindowHandle;
use windows_sys::Win32::{
    Foundation::{ERROR_SUCCESS, GetLastError, GlobalFree, HGLOBAL, HWND, SetLastError},
    Graphics::Gdi::{BI_BITFIELDS, BITMAPV5HEADER, LCS_GM_IMAGES},
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW,
            SetClipboardData,
        },
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        Ole::CF_DIBV5,
    },
    UI::ColorSystem::LCS_sRGB,
};

const MAX_PNG_BYTES: usize = 16 * 1024 * 1024;
const MAX_PNG_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_PNG_ALLOCATION: u64 = 160 * 1024 * 1024;
const HEADER_BYTES: usize = size_of::<BITMAPV5HEADER>();
const PNG_FORMAT: [u16; 4] = [b'P' as u16, b'N' as u16, b'G' as u16, 0];

#[derive(Clone)]
pub(super) struct Png {
    original: Arc<[u8]>,
    dib: Arc<[u8]>,
}

impl fmt::Debug for Png {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedClipboardPng")
            .finish_non_exhaustive()
    }
}

impl Png {
    /// Prepares the interoperable bitmap before the UI opens the system clipboard.
    pub(super) fn prepare(png: Arc<[u8]>) -> io::Result<Self> {
        let [width, height] = dimensions(&png)?;
        let mut limits = Limits::default();
        limits.max_image_width = Some(width);
        limits.max_image_height = Some(height);
        limits.max_alloc = Some(MAX_PNG_ALLOCATION);
        let mut reader = ImageReader::with_format(Cursor::new(png.as_ref()), ImageFormat::Png);
        reader.limits(limits);
        let rgba = reader
            .decode()
            .map_err(|_| invalid("Invalid or oversized clipboard PNG image"))?
            .into_rgba8();
        let dib = dib_bytes(&rgba)?;
        drop(rgba);
        Ok(Self {
            original: png,
            dib: dib.into(),
        })
    }
}

/// Writes a prepared image while the caller holds the owning HWND on its UI thread.
pub(super) fn write(window: Win32WindowHandle, prepared: Png) -> io::Result<()> {
    // SAFETY: The format name is a static, terminated UTF-16 string.
    let png_format = unsafe { RegisterClipboardFormatW(PNG_FORMAT.as_ptr()) };
    if png_format == 0 {
        return Err(io::Error::last_os_error());
    }
    // Finish both allocations before replacing the current clipboard data.
    let original = GlobalMemory::new(&prepared.original)?;
    let bitmap = GlobalMemory::new(&prepared.dib)?;
    let clipboard = Clipboard::open(window.hwnd.get() as HWND)?;
    clipboard.empty()?;
    clipboard.put(png_format, original)?;
    clipboard.put(u32::from(CF_DIBV5), bitmap)?;
    clipboard.close()
}

fn dimensions(png: &[u8]) -> io::Result<[u32; 2]> {
    if png.len() > MAX_PNG_BYTES {
        return Err(invalid("Clipboard PNG exceeds the file size limit"));
    }
    if png.len() < 33 || !png.starts_with(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR") {
        return Err(invalid("Invalid clipboard PNG header"));
    }
    let width = u32::from_be_bytes(png[16..20].try_into().expect("PNG width header"));
    let height = u32::from_be_bytes(png[20..24].try_into().expect("PNG height header"));
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PNG_PIXELS {
        return Err(invalid("Clipboard PNG exceeds the image size limit"));
    }
    Ok([width, height])
}

fn dib_bytes(rgba: &RgbaImage) -> io::Result<Vec<u8>> {
    let (width, height) = rgba.dimensions();
    let pixels = rgba.as_raw();
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(HEADER_BYTES + pixels.len())
        .map_err(io::Error::other)?;
    let mut header = [0; HEADER_BYTES];
    macro_rules! field {
        ($name:ident, $value:expr) => {{
            let encoded = $value.to_le_bytes();
            let offset = offset_of!(BITMAPV5HEADER, $name);
            header[offset..offset + encoded.len()].copy_from_slice(&encoded);
        }};
    }
    field!(bV5Size, (HEADER_BYTES as u32));
    field!(bV5Width, (width as i32));
    field!(bV5Height, (height as i32));
    field!(bV5Planes, 1u16);
    field!(bV5BitCount, 32u16);
    field!(bV5Compression, BI_BITFIELDS);
    field!(bV5SizeImage, (pixels.len() as u32));
    field!(bV5RedMask, 0x00ff_0000u32);
    field!(bV5GreenMask, 0x0000_ff00u32);
    field!(bV5BlueMask, 0x0000_00ffu32);
    field!(bV5AlphaMask, 0xff00_0000u32);
    field!(bV5CSType, (LCS_sRGB as u32));
    field!(bV5Intent, (LCS_GM_IMAGES as u32));
    bytes.extend_from_slice(&header);

    // A positive height stores rows bottom-up, including in readers that do
    // not support a top-down DIB. 32-bit BGRA needs no row padding.
    for row in pixels.chunks_exact(width as usize * 4).rev() {
        for &[red, green, blue, alpha] in row.as_chunks::<4>().0 {
            bytes.extend_from_slice(&[blue, green, red, alpha]);
        }
    }
    Ok(bytes)
}

struct GlobalMemory(HGLOBAL);

impl GlobalMemory {
    fn new(bytes: &[u8]) -> io::Result<Self> {
        // SAFETY: GMEM_MOVEABLE is required for clipboard ownership transfer.
        // This handle is uniquely owned, and the locked allocation is at least bytes.len().
        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let memory = Self(handle);
            let data = GlobalLock(handle).cast::<u8>();
            if data.is_null() {
                return Err(io::Error::last_os_error());
            }
            ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
            SetLastError(ERROR_SUCCESS);
            if GlobalUnlock(handle) != 0 {
                return Err(io::Error::other("Clipboard memory is still locked"));
            }
            if GetLastError() != ERROR_SUCCESS {
                return Err(io::Error::last_os_error());
            }
            Ok(memory)
        }
    }
}

impl Drop for GlobalMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: This handle has not been transferred to the clipboard.
            unsafe { GlobalFree(self.0) };
        }
    }
}

struct Clipboard(bool);

impl Clipboard {
    fn open(owner: HWND) -> io::Result<Self> {
        // SAFETY: The caller supplied the live native HWND on its UI thread.
        win_bool(unsafe { OpenClipboard(owner) })?;
        Ok(Self(true))
    }

    fn empty(&self) -> io::Result<()> {
        // SAFETY: The guard opened the clipboard on this thread.
        win_bool(unsafe { EmptyClipboard() })
    }

    fn put(&self, format: u32, mut memory: GlobalMemory) -> io::Result<()> {
        // SAFETY: The clipboard is open and the unique movable memory block is unlocked.
        if unsafe { SetClipboardData(format, memory.0) }.is_null() {
            return Err(io::Error::last_os_error());
        }
        // Windows owns the handle after success.
        memory.0 = ptr::null_mut();
        Ok(())
    }

    fn close(mut self) -> io::Result<()> {
        // SAFETY: The guard opened the clipboard on this thread and owns the close.
        win_bool(unsafe { CloseClipboard() })?;
        self.0 = false;
        Ok(())
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: This is the same thread and a successful close was not recorded.
            unsafe { CloseClipboard() };
        }
    }
}

fn win_bool(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use image::{ExtendedColorType, ImageEncoder as _, codecs::png::PngEncoder};

    use super::*;

    fn png_fixture() -> Arc<[u8]> {
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(
                &[230, 60, 110, 128, 0, 255, 40, 255],
                1,
                2,
                ExtendedColorType::Rgba8,
            )
            .unwrap();
        png.into()
    }

    #[test]
    fn png_clipboard_preserves_png_and_prepares_a_bottom_up_alpha_dib() {
        let png = png_fixture();
        let image = Png::prepare(Arc::clone(&png)).unwrap();
        let cloned = image.clone();
        assert!(Arc::ptr_eq(&image.original, &png));
        assert!(Arc::ptr_eq(&image.dib, &cloned.dib));
        let header = &image.dib[..HEADER_BYTES];
        let field = |offset| u32::from_le_bytes(header[offset..offset + 4].try_into().unwrap());
        assert_eq!(HEADER_BYTES, 124);
        assert_eq!(field(offset_of!(BITMAPV5HEADER, bV5Size)), 124);
        assert_eq!(field(offset_of!(BITMAPV5HEADER, bV5Width)), 1);
        assert_eq!(field(offset_of!(BITMAPV5HEADER, bV5Height)), 2);
        assert_eq!(
            field(offset_of!(BITMAPV5HEADER, bV5Compression)),
            BI_BITFIELDS
        );
        assert_eq!(field(offset_of!(BITMAPV5HEADER, bV5AlphaMask)), 0xff00_0000);
        assert_eq!(field(offset_of!(BITMAPV5HEADER, bV5SizeImage)), 8);
        assert_eq!(
            &image.dib[HEADER_BYTES..],
            &[40, 255, 0, 255, 110, 60, 230, 128]
        );
    }

    #[test]
    fn png_clipboard_rejects_truncated_or_oversized_images_before_allocating_a_dib() {
        let png = png_fixture();
        assert!(Png::prepare(Arc::from(&png[..33])).is_err());
        let mut oversized = png.to_vec();
        oversized[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(Png::prepare(oversized.into()).is_err());
    }

    #[test]
    fn png_clipboard_global_memory_is_unlocked_and_freed_if_unpublished() {
        use windows_sys::Win32::System::Memory::{GlobalFlags, GlobalSize};

        let memory = GlobalMemory::new(b"png").unwrap();
        let handle = memory.0;
        // SAFETY: The first queries read the live allocation. GlobalSize also
        // accepts an invalid handle and returns zero without dereferencing it.
        unsafe {
            assert_eq!(GlobalFlags(handle) & 0xff, 0);
            assert!(GlobalSize(handle) >= 3);
            drop(memory);
            assert_eq!(GlobalSize(handle), 0);
        }
    }
}
