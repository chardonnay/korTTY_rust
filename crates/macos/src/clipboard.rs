use std::io;

use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypePNG, NSPasteboardWriting};
use objc2_foundation::{MainThreadMarker, NSArray, NSData};

const MAX_PNG_BYTES: usize = 16 * 1024 * 1024;
const MAX_PNG_PIXELS: u64 = 16 * 1024 * 1024;

/// Publishes the exact PNG representation on AppKit's main thread.
pub fn write_clipboard_png(png: &[u8]) -> io::Result<()> {
    MainThreadMarker::new()
        .ok_or_else(|| io::Error::other("AppKit clipboard requires the main thread"))?;
    validate_header(png)?;
    let data = NSData::with_bytes(png);
    // SAFETY: This is AppKit's main thread. The retained item and NSData are live,
    // and the static UTI identifies the exact PNG bytes held by NSData.
    let item = unsafe {
        let item = NSPasteboardItem::new();
        if !item.setData_forType(&data, NSPasteboardTypePNG) {
            return Err(io::Error::other("AppKit rejected the PNG pasteboard item"));
        }
        item
    };
    let item = ProtocolObject::<dyn NSPasteboardWriting>::from_retained(item);
    let objects = NSArray::from_vec(vec![item]);
    // SAFETY: The array and its protocol-conforming item are retained through the
    // synchronous write to the main-thread AppKit pasteboard.
    unsafe {
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        if !pasteboard.writeObjects(&objects) {
            return Err(io::Error::other(
                "AppKit could not write the PNG pasteboard item",
            ));
        }
    }
    Ok(())
}

fn validate_header(png: &[u8]) -> io::Result<()> {
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
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_clipboard_rejects_bad_and_oversized_headers_before_touching_appkit() {
        assert!(validate_header(b"not a PNG").is_err());
        let mut header =
            b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\0\0\0\0".to_owned();
        assert!(validate_header(&header).is_ok());
        header[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(validate_header(&header).is_err());
        header[16..20].fill(0);
        assert!(validate_header(&header).is_err());
    }
}
