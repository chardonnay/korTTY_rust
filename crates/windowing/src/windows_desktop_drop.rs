//! Incoming OLE Copy drops for native Windows windows.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{OsString, c_void};
use std::os::windows::ffi::OsStringExt as _;
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Weak};

use raw_window_handle::{RawWindowHandle, WindowHandle};
use windows_sys::Win32::Foundation::{E_POINTER, HWND, POINT, POINTL, S_OK};
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL};
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows_sys::Win32::System::Ole::{
    CF_HDROP, CF_UNICODETEXT, DROPEFFECT_COPY, DROPEFFECT_NONE, RegisterDragDrop, ReleaseStgMedium,
    RevokeDragDrop,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows_sys::Win32::UI::Shell::{DROPFILES, DragQueryFileW};
use windows_sys::core::{GUID, HRESULT, IUnknown_Vtbl};

use crate::windows_file_drag::{ComObject, ComReference, DataVtable, Ole, query_interface};

const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_FILES: u32 = 256;
const MAX_PATH_UNITS: u32 = 32_767;
const MAX_FILE_LIST_BYTES: usize = 1024 * 1024;
const IID_DROP_TARGET: GUID = GUID::from_u128(0x00000122_0000_0000_c000_000000000046);

pub enum Event {
    Files {
        point: (f64, f64),
        paths: Vec<PathBuf>,
        dropped: bool,
    },
    Left,
}

struct TextRegistration {
    enabled: AtomicBool,
    notify: Box<dyn Fn((f64, f64), String) -> bool + Send + Sync>,
}

#[derive(Clone)]
pub struct Watch(Arc<TextRegistration>);

impl Watch {
    pub fn set_enabled(&self, enabled: bool) {
        self.0.enabled.store(enabled, Ordering::Release);
    }
}

thread_local! {
    static TEXT_TARGETS: RefCell<HashMap<usize, Weak<TextRegistration>>> = RefCell::new(HashMap::new());
}

pub fn watch_text(
    window: WindowHandle<'_>,
    notify: impl Fn((f64, f64), String) -> bool + Send + Sync + 'static,
) -> Option<Watch> {
    let hwnd = hwnd(window)?;
    let registration = Arc::new(TextRegistration {
        enabled: AtomicBool::new(false),
        notify: Box::new(notify),
    });
    TEXT_TARGETS.with_borrow_mut(|targets| {
        targets.retain(|_, registration| registration.strong_count() != 0);
        targets.insert(hwnd as usize, Arc::downgrade(&registration));
    });
    Some(Watch(registration))
}

pub struct Target {
    hwnd: HWND,
    _interface: ComReference<DropTarget>,
    _ole: Ole,
}

/// Installs the window's sole OLE target on its owning GUI thread.
pub fn register(
    window: WindowHandle<'_>,
    notify: impl Fn(Event) -> bool + 'static,
) -> Option<Target> {
    let hwnd = hwnd(window)?;
    let ole = Ole::initialize()?;
    let interface = ComReference::new(DropTarget {
        vtable: &VTABLE,
        refs: AtomicU32::new(1),
        hwnd,
        hover: Cell::new(None),
        notify: Box::new(notify),
    });
    // SAFETY: The window belongs to this STA and the COM interface remains alive.
    if unsafe { RegisterDragDrop(hwnd, interface.as_ptr()) } < 0 {
        return None;
    }
    Some(Target {
        hwnd,
        _interface: interface,
        _ole: ole,
    })
}

impl Drop for Target {
    fn drop(&mut self) {
        // SAFETY: The registration and OLE apartment remain live on this GUI thread.
        unsafe { RevokeDragDrop(self.hwnd) };
    }
}

fn hwnd(window: WindowHandle<'_>) -> Option<HWND> {
    let RawWindowHandle::Win32(window) = window.as_raw() else {
        return None;
    };
    Some(window.hwnd.get() as HWND)
}

#[derive(Clone, Copy)]
enum Format {
    Files,
    Text,
}

#[repr(C)]
struct DropTarget {
    vtable: &'static DropTargetVtable,
    refs: AtomicU32,
    hwnd: HWND,
    hover: Cell<Option<Format>>,
    notify: Box<dyn Fn(Event) -> bool>,
}

#[repr(C)]
struct DropTargetVtable {
    base: IUnknown_Vtbl,
    enter: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, POINTL, *mut u32) -> HRESULT,
    over: unsafe extern "system" fn(*mut c_void, u32, POINTL, *mut u32) -> HRESULT,
    leave: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    drop: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, POINTL, *mut u32) -> HRESULT,
}

static VTABLE: DropTargetVtable = DropTargetVtable {
    base: IUnknown_Vtbl {
        QueryInterface: query,
        AddRef: add,
        Release: release,
    },
    enter,
    over,
    leave,
    drop: drop_data,
};

impl ComObject for DropTarget {
    const RELEASE: unsafe extern "system" fn(*mut c_void) -> u32 = release;
}

unsafe extern "system" fn query(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: COM supplies the live interface and QueryInterface arguments.
    unsafe { query_interface(this, iid, out, &IID_DROP_TARGET, add) }
}

unsafe extern "system" fn add(this: *mut c_void) -> u32 {
    // SAFETY: COM holds a reference to the live target.
    unsafe { &*this.cast::<DropTarget>() }
        .refs
        .fetch_add(1, Ordering::Relaxed)
        + 1
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let this = this.cast::<DropTarget>();
    // SAFETY: This call consumes a live COM reference.
    let remaining = unsafe { &*this }.refs.fetch_sub(1, Ordering::AcqRel) - 1;
    if remaining == 0 {
        // SAFETY: The last reference frees the original allocation.
        drop(unsafe { Box::from_raw(this) });
    }
    remaining
}

unsafe extern "system" fn enter(
    this: *mut c_void,
    data: *mut c_void,
    _: u32,
    pt: POINTL,
    effect: *mut u32,
) -> HRESULT {
    // SAFETY: OLE lends the target, data and effect for this callback.
    let (target, effect) = (unsafe { &*this.cast::<DropTarget>() }, unsafe {
        effect.as_mut()
    });
    let Some(effect) = effect else {
        return E_POINTER;
    };
    let allowed = *effect & DROPEFFECT_COPY != 0;
    *effect = DROPEFFECT_NONE;
    target.hover.set(None);
    if allowed
        && let Some(point) = target.client_point(pt)
        && let Some(format) = unsafe { target.format(data) }
    {
        if matches!(format, Format::Files)
            && let Some(paths) = unsafe { files(data) }
        {
            (target.notify)(Event::Files {
                point,
                paths,
                dropped: false,
            });
        }
        target.hover.set(Some(format));
        *effect = DROPEFFECT_COPY;
    }
    S_OK
}

unsafe extern "system" fn over(this: *mut c_void, _: u32, pt: POINTL, effect: *mut u32) -> HRESULT {
    // SAFETY: OLE lends the target and output effect for this callback.
    let (target, effect) = (unsafe { &*this.cast::<DropTarget>() }, unsafe {
        effect.as_mut()
    });
    let Some(effect) = effect else {
        return E_POINTER;
    };
    let accepts = match target.hover.get() {
        Some(Format::Files) => true,
        Some(Format::Text) => target.text().is_some(),
        None => false,
    };
    *effect = if accepts && *effect & DROPEFFECT_COPY != 0 && target.client_point(pt).is_some() {
        DROPEFFECT_COPY
    } else {
        DROPEFFECT_NONE
    };
    S_OK
}

unsafe extern "system" fn leave(this: *mut c_void) -> HRESULT {
    // SAFETY: OLE holds a live reference to the target.
    let target = unsafe { &*this.cast::<DropTarget>() };
    if matches!(target.hover.take(), Some(Format::Files)) {
        (target.notify)(Event::Left);
    }
    S_OK
}

unsafe extern "system" fn drop_data(
    this: *mut c_void,
    data: *mut c_void,
    _: u32,
    pt: POINTL,
    effect: *mut u32,
) -> HRESULT {
    // SAFETY: OLE lends the target, data and effect for this callback.
    let (target, effect) = (unsafe { &*this.cast::<DropTarget>() }, unsafe {
        effect.as_mut()
    });
    let Some(effect) = effect else {
        return E_POINTER;
    };
    let allowed = *effect & DROPEFFECT_COPY != 0;
    *effect = DROPEFFECT_NONE;
    let hovered = target.hover.take();
    if allowed && let Some(point) = target.client_point(pt) {
        let accepted = match hovered {
            Some(Format::Files) => unsafe { files(data) }.is_some_and(|paths| {
                (target.notify)(Event::Files {
                    point,
                    paths,
                    dropped: true,
                })
            }),
            Some(Format::Text) => target.text().is_some_and(|registration| {
                // SAFETY: Query DPI for the live window on its owning GUI thread.
                let dpi = unsafe { GetDpiForWindow(target.hwnd) };
                if dpi == 0 {
                    return false;
                }
                let factor = 96.0 / f64::from(dpi);
                unsafe { medium(data, CF_UNICODETEXT) }
                    .and_then(|medium| medium.text())
                    .is_some_and(|text| {
                        (registration.notify)((point.0 * factor, point.1 * factor), text)
                    })
            }),
            None => false,
        };
        if accepted {
            *effect = DROPEFFECT_COPY;
        }
    }
    if *effect == DROPEFFECT_NONE && matches!(hovered, Some(Format::Files)) {
        (target.notify)(Event::Left);
    }
    S_OK
}

impl DropTarget {
    fn client_point(&self, point: POINTL) -> Option<(f64, f64)> {
        let mut point = POINT {
            x: point.x,
            y: point.y,
        };
        // SAFETY: This callback is on the owning GUI thread for a registered HWND.
        (unsafe { IsWindowEnabled(self.hwnd) != 0 && ScreenToClient(self.hwnd, &mut point) != 0 })
            .then_some((f64::from(point.x), f64::from(point.y)))
    }

    fn text(&self) -> Option<Arc<TextRegistration>> {
        TEXT_TARGETS
            .with_borrow(|targets| targets.get(&(self.hwnd as usize)).and_then(Weak::upgrade))
            .filter(|watch| watch.enabled.load(Ordering::Acquire))
    }

    unsafe fn format(&self, data: *mut c_void) -> Option<Format> {
        // SAFETY: The current OLE callback lends a live IDataObject.
        let vtable = unsafe { data_vtable(data) }?;
        if unsafe { (vtable.query_get_data)(data, &format(CF_HDROP)) } == S_OK {
            Some(Format::Files)
        } else if self.text().is_some()
            && unsafe { (vtable.query_get_data)(data, &format(CF_UNICODETEXT)) } == S_OK
        {
            Some(Format::Text)
        } else {
            None
        }
    }
}

fn format(kind: u16) -> FORMATETC {
    FORMATETC {
        cfFormat: kind,
        ptd: null_mut(),
        dwAspect: DVASPECT_CONTENT,
        lindex: -1,
        tymed: TYMED_HGLOBAL as u32,
    }
}

unsafe fn data_vtable(data: *mut c_void) -> Option<&'static DataVtable> {
    let interface = data.cast::<*const DataVtable>();
    // SAFETY: IDataObject begins with the vtable pointer, valid for the callback.
    unsafe { interface.as_ref()?.as_ref() }
}

struct Medium(STGMEDIUM);

unsafe fn medium(data: *mut c_void, kind: u16) -> Option<Medium> {
    // SAFETY: The callback lends a live IDataObject and GetData initializes its output on success.
    let vtable = unsafe { data_vtable(data) }?;
    let mut result = std::mem::MaybeUninit::<STGMEDIUM>::zeroed();
    if unsafe { (vtable.get_data)(data, &format(kind), result.as_mut_ptr()) } != S_OK {
        return None;
    }
    Some(Medium(unsafe { result.assume_init() }))
}

impl Medium {
    fn global(&self) -> Option<*mut c_void> {
        if self.0.tymed != TYMED_HGLOBAL as u32 {
            return None;
        }
        // SAFETY: This union arm is active only for TYMED_HGLOBAL.
        let handle = unsafe { self.0.u.hGlobal };
        (!handle.is_null()).then_some(handle)
    }

    fn text(&self) -> Option<String> {
        let global = self.global()?;
        // SAFETY: The medium owns the source's global allocation until ReleaseStgMedium.
        let units = (unsafe { GlobalSize(global) } / size_of::<u16>()).min(MAX_TEXT_BYTES + 1);
        if units == 0 {
            return None;
        }
        let buffer = unsafe { GlobalLock(global) }.cast::<u16>();
        if buffer.is_null() {
            return None;
        }
        // SAFETY: GlobalSize bounds this aligned allocation while it is locked.
        let text = decode_text(unsafe { std::slice::from_raw_parts(buffer, units) });
        unsafe { GlobalUnlock(global) };
        text
    }
}

impl Drop for Medium {
    fn drop(&mut self) {
        // SAFETY: This medium was initialized by a successful GetData call.
        unsafe { ReleaseStgMedium(&mut self.0) };
    }
}

fn decode_text(units: &[u16]) -> Option<String> {
    let end = units.iter().position(|unit| *unit == 0)?;
    if end == 0 || end > MAX_TEXT_BYTES {
        return None;
    }
    let text = String::from_utf16(&units[..end]).ok()?;
    (text.len() <= MAX_TEXT_BYTES).then_some(text)
}

pub(super) unsafe fn files(data: *mut c_void) -> Option<Vec<PathBuf>> {
    // SAFETY: OLE lends a live data object and DragQueryFileW reads the locked format.
    let medium = unsafe { medium(data, CF_HDROP) }?;
    let handle = medium.global()?;
    if !valid_file_medium(handle) {
        return None;
    }
    let count = unsafe { DragQueryFileW(handle, u32::MAX, null_mut(), 0) };
    if count == 0 || count > MAX_FILES {
        return None;
    }
    (0..count)
        .map(|index| {
            let len = unsafe { DragQueryFileW(handle, index, null_mut(), 0) };
            if len == 0 || len > MAX_PATH_UNITS {
                return None;
            }
            let mut path = vec![0; len as usize + 1];
            let read = unsafe { DragQueryFileW(handle, index, path.as_mut_ptr(), len + 1) };
            (read == len).then(|| PathBuf::from(OsString::from_wide(&path[..len as usize])))
        })
        .collect()
}

fn valid_file_medium(handle: *mut c_void) -> bool {
    // SAFETY: The caller owns the HGLOBAL in a live STGMEDIUM.
    let bytes = unsafe { GlobalSize(handle) };
    if !(size_of::<DROPFILES>()..=MAX_FILE_LIST_BYTES).contains(&bytes) {
        return false;
    }
    let buffer = unsafe { GlobalLock(handle) }.cast::<u8>();
    if buffer.is_null() {
        return false;
    }
    // SAFETY: The size check above bounds the header and full locked buffer.
    let header = unsafe { buffer.cast::<DROPFILES>().read_unaligned() };
    let content = unsafe { std::slice::from_raw_parts(buffer, bytes) };
    let offset = header.pFiles as usize;
    let valid = offset >= size_of::<DROPFILES>()
        && content.get(offset..).is_some_and(|names| {
            if header.fWide != 0 {
                offset.is_multiple_of(2)
                    && names
                        .windows(4)
                        .step_by(2)
                        .any(|units| units == [0, 0, 0, 0])
            } else {
                names.windows(2).any(|units| units == [0, 0])
            }
        });
    unsafe { GlobalUnlock(handle) };
    valid
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_target_registers_and_revokes_on_the_gui_thread() {
        use super::*;
        use raw_window_handle::Win32WindowHandle;
        use std::num::NonZeroIsize;
        use std::ptr::null;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, WS_POPUP,
        };

        struct TestWindow(HWND);
        impl Drop for TestWindow {
            fn drop(&mut self) {
                // SAFETY: This hidden window is owned by the current test thread.
                unsafe { DestroyWindow(self.0) };
            }
        }

        let class = [83, 84, 65, 84, 73, 67, 0];
        // SAFETY: STATIC is a system class. No visible style or user data is supplied.
        let window = TestWindow(unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                16,
                16,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        });
        let hwnd = NonZeroIsize::new(window.0 as isize).expect("hidden Win32 test window");
        // SAFETY: The handle stays valid on this thread until TestWindow is dropped.
        let borrowed = unsafe {
            WindowHandle::borrow_raw(RawWindowHandle::Win32(Win32WindowHandle::new(hwnd)))
        };
        let registered = register(borrowed, |_| true).expect("registers a native target");
        assert!(register(borrowed, |_| true).is_none());
        drop(registered);
        assert!(register(borrowed, |_| true).is_some());
    }

    #[test]
    fn unicode_text_requires_a_terminator_and_rejects_invalid_or_oversized_text() {
        let text = "Grüße • ターミナル";
        let units = text.encode_utf16().chain([0]).collect::<Vec<_>>();
        assert_eq!(super::decode_text(&units).as_deref(), Some(text));
        assert!(super::decode_text(&[0]).is_none());
        assert!(super::decode_text(&[0xd800, 0]).is_none());
        assert!(super::decode_text(&[65]).is_none());
        let mut oversized = vec![0x0800; super::MAX_TEXT_BYTES / 3 + 1];
        oversized.push(0);
        assert!(super::decode_text(&oversized).is_none());
    }
}
