//! OLE Copy drags of existing file-system paths.

use std::collections::HashSet;
use std::ffi::c_void;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt as _;
use std::path::{Component, PathBuf, Prefix};
use std::ptr::{NonNull, copy_nonoverlapping, null, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use raw_window_handle::{RawWindowHandle, WindowHandle};
use windows_sys::Win32::Foundation::{
    DATA_S_SAMEFORMATETC, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS,
    DV_E_DVASPECT, DV_E_DVTARGETDEVICE, DV_E_FORMATETC, DV_E_LINDEX, DV_E_TYMED, E_NOINTERFACE,
    E_NOTIMPL, E_OUTOFMEMORY, E_POINTER, GlobalFree, HWND, OLE_E_ADVISENOTSUPPORTED, S_OK,
};
use windows_sys::Win32::System::Com::{
    DATADIR_GET, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT_COPY, DROPEFFECT_NONE, DoDragDrop, OleInitialize, OleUninitialize,
};
use windows_sys::Win32::System::SystemServices::MK_LBUTTON;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetCapture, IsWindowEnabled, VK_LBUTTON, VK_RBUTTON,
};
use windows_sys::Win32::UI::Shell::{DROPFILES, SHCreateStdEnumFmtEtc};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetSystemMetrics, GetWindowThreadProcessId, SM_SWAPBUTTON,
};
use windows_sys::core::{BOOL, GUID, HRESULT, IID_IUnknown, IUnknown_Vtbl};

const MAX_FILES: usize = 256;
const MAX_PATH_UNITS: usize = 32_767;
const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
const IID_DATA_OBJECT: GUID = GUID::from_u128(0x0000010e_0000_0000_c000_000000000046);
const IID_DROP_SOURCE: GUID = GUID::from_u128(0x00000121_0000_0000_c000_000000000046);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Copied,
    Cancelled,
    Unavailable,
}

/// The application can revoke a queued or live offer even while OLE owns the pointer.
#[derive(Clone)]
pub struct Permit(Arc<Authority>);

struct Authority {
    live: AtomicBool,
    check: Box<dyn Fn() -> bool + Send + Sync>,
}

impl Permit {
    /// The caller's check runs on the OLE thread and must return promptly.
    pub fn new(check: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(Authority {
            live: AtomicBool::new(true),
            check: Box::new(check),
        }))
    }

    pub fn revoke(&self) {
        self.0.live.store(false, Ordering::Release);
    }

    fn allows(&self) -> bool {
        self.0.live.load(Ordering::Acquire)
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(&self.0.check))
                .unwrap_or(false)
            && self.0.live.load(Ordering::Acquire)
    }
}

/// Starts a native Copy drag on the source window's GUI thread with its primary button held.
pub fn start(window: WindowHandle<'_>, paths: &[PathBuf], permit: Permit) -> Outcome {
    let RawWindowHandle::Win32(window) = window.as_raw() else {
        return Outcome::Unavailable;
    };
    let hwnd = window.hwnd.get() as HWND;
    if !owns_press(hwnd) || !permit.allows() {
        return Outcome::Cancelled;
    }
    let Some(paths) = file_list(paths) else {
        return Outcome::Unavailable;
    };
    let Some(_ole) = Ole::initialize() else {
        return Outcome::Unavailable;
    };
    let data = ComReference::new(FileData {
        vtable: &DATA_VTABLE,
        refs: AtomicU32::new(1),
        paths,
        permit: permit.clone(),
    });
    let source = ComReference::new(DropSource {
        vtable: &SOURCE_VTABLE,
        refs: AtomicU32::new(1),
        hwnd,
        permit: permit.clone(),
    });
    if !owns_press(hwnd) || !permit.allows() {
        return Outcome::Cancelled;
    }
    let mut effect = DROPEFFECT_NONE;
    // SAFETY: Both live COM interfaces belong to this STA. OLE writes only the output effect.
    let result =
        unsafe { DoDragDrop(data.as_ptr(), source.as_ptr(), DROPEFFECT_COPY, &mut effect) };
    permit.revoke();
    match result {
        DRAGDROP_S_DROP if effect == DROPEFFECT_COPY => Outcome::Copied,
        DRAGDROP_S_DROP | DRAGDROP_S_CANCEL => Outcome::Cancelled,
        _ => Outcome::Unavailable,
    }
}

fn owns_press(hwnd: HWND) -> bool {
    // SAFETY: The caller borrowed this native window on its owning thread.
    unsafe {
        let primary = if GetSystemMetrics(SM_SWAPBUTTON) == 0 {
            VK_LBUTTON
        } else {
            VK_RBUTTON
        };
        GetWindowThreadProcessId(hwnd, null_mut()) == GetCurrentThreadId()
            && IsWindowEnabled(hwnd) != 0
            && GetForegroundWindow() == hwnd
            && GetCapture() == hwnd
            && GetAsyncKeyState(i32::from(primary)) < 0
    }
}

struct Ole;

impl Ole {
    fn initialize() -> Option<Self> {
        // SAFETY: OLE is initialized on this thread with its reserved pointer null.
        (unsafe { OleInitialize(null()) } >= 0).then_some(Self)
    }
}

impl Drop for Ole {
    fn drop(&mut self) {
        // SAFETY: Both successful OleInitialize outcomes own a matching call on this thread.
        unsafe { OleUninitialize() };
    }
}

fn file_list(paths: &[PathBuf]) -> Option<Vec<u16>> {
    if paths.is_empty() || paths.len() > MAX_FILES {
        return None;
    }
    let mut seen = HashSet::new();
    let mut wide = Vec::new();
    for path in paths {
        let Some(Component::Prefix(prefix)) = path.components().next() else {
            return None;
        };
        if !matches!(
            prefix.kind(),
            Prefix::Disk(_) | Prefix::VerbatimDisk(_) | Prefix::UNC(..) | Prefix::VerbatimUNC(..)
        ) || !path.is_absolute()
            || path.file_name().is_none()
            || path.components().any(|part| part == Component::ParentDir)
        {
            return None;
        }
        if !seen.insert(path) {
            continue;
        }
        let start = wide.len();
        for unit in path.as_os_str().encode_wide() {
            if unit == 0
                || wide.len() - start == MAX_PATH_UNITS
                || size_of::<DROPFILES>() + (wide.len() + 3) * size_of::<u16>() > MAX_PAYLOAD_BYTES
            {
                return None;
            }
            wide.push(unit);
        }
        wide.push(0);
    }
    wide.push(0);
    Some(wide)
}

struct ComReference<T: ComObject>(NonNull<T>);

trait ComObject {
    const RELEASE: unsafe extern "system" fn(*mut c_void) -> u32;
}

impl ComObject for FileData {
    const RELEASE: unsafe extern "system" fn(*mut c_void) -> u32 = data_release;
}

impl ComObject for DropSource {
    const RELEASE: unsafe extern "system" fn(*mut c_void) -> u32 = source_release;
}

impl<T: ComObject> ComReference<T> {
    fn new(object: T) -> Self {
        Self(NonNull::from(Box::leak(Box::new(object))))
    }

    fn as_ptr(&self) -> *mut c_void {
        self.0.cast().as_ptr()
    }
}

impl<T: ComObject> Drop for ComReference<T> {
    fn drop(&mut self) {
        // SAFETY: The constructor binds this one owned reference to its matching release method.
        unsafe { T::RELEASE(self.as_ptr()) };
    }
}

#[repr(C)]
struct DropSource {
    vtable: &'static DropSourceVtable,
    refs: AtomicU32,
    hwnd: HWND,
    permit: Permit,
}

#[repr(C)]
struct DropSourceVtable {
    base: IUnknown_Vtbl,
    query_continue_drag: unsafe extern "system" fn(*mut c_void, BOOL, u32) -> HRESULT,
    give_feedback: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
}

static SOURCE_VTABLE: DropSourceVtable = DropSourceVtable {
    base: IUnknown_Vtbl {
        QueryInterface: source_query,
        AddRef: source_add,
        Release: source_release,
    },
    query_continue_drag: source_continue,
    give_feedback: source_feedback,
};

unsafe extern "system" fn source_query(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: OLE supplies this interface and the input/output pointers for QueryInterface.
    unsafe { query_interface(this, iid, out, &IID_DROP_SOURCE, source_add) }
}

unsafe extern "system" fn source_add(this: *mut c_void) -> u32 {
    // SAFETY: COM holds a live reference to this DropSource.
    unsafe { &*this.cast::<DropSource>() }
        .refs
        .fetch_add(1, Ordering::Relaxed)
        + 1
}

unsafe extern "system" fn source_release(this: *mut c_void) -> u32 {
    let this = this.cast::<DropSource>();
    // SAFETY: The caller owns this reference and cannot use it after Release.
    let remaining = unsafe { &*this }.refs.fetch_sub(1, Ordering::AcqRel) - 1;
    if remaining == 0 {
        // SAFETY: Only the last Release reclaims the original boxed allocation.
        drop(unsafe { Box::from_raw(this) });
    }
    remaining
}

fn continue_drag(escape: bool, primary: bool, allowed: bool) -> HRESULT {
    if escape || !allowed {
        DRAGDROP_S_CANCEL
    } else if primary {
        S_OK
    } else {
        DRAGDROP_S_DROP
    }
}

unsafe extern "system" fn source_continue(this: *mut c_void, escape: BOOL, keys: u32) -> HRESULT {
    // SAFETY: COM holds a live reference to this DropSource.
    let source = unsafe { &*this.cast::<DropSource>() };
    // OLE may transfer capture during the drag. The source still has to be live and enabled.
    let allowed = escape == 0
        // SAFETY: The saved HWND is used only for a system query that handles invalid windows.
        && unsafe { IsWindowEnabled(source.hwnd) } != 0
        && source.permit.allows();
    continue_drag(escape != 0, keys & MK_LBUTTON != 0, allowed)
}

unsafe extern "system" fn source_feedback(_: *mut c_void, _: u32) -> HRESULT {
    DRAGDROP_S_USEDEFAULTCURSORS
}

#[repr(C)]
struct FileData {
    vtable: &'static DataVtable,
    refs: AtomicU32,
    paths: Vec<u16>,
    permit: Permit,
}

#[repr(C)]
struct DataVtable {
    base: IUnknown_Vtbl,
    get_data: unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut STGMEDIUM) -> HRESULT,
    get_data_here:
        unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut STGMEDIUM) -> HRESULT,
    query_get_data: unsafe extern "system" fn(*mut c_void, *const FORMATETC) -> HRESULT,
    canonical_format:
        unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut FORMATETC) -> HRESULT,
    set_data:
        unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut STGMEDIUM, BOOL) -> HRESULT,
    enum_format: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
    advise: unsafe extern "system" fn(
        *mut c_void,
        *const FORMATETC,
        u32,
        *mut c_void,
        *mut u32,
    ) -> HRESULT,
    unadvise: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
    enum_advise: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

static DATA_VTABLE: DataVtable = DataVtable {
    base: IUnknown_Vtbl {
        QueryInterface: data_query,
        AddRef: data_add,
        Release: data_release,
    },
    get_data: data_get,
    get_data_here: data_here,
    query_get_data: data_query_format,
    canonical_format: data_canonical_format,
    set_data: data_set,
    enum_format: data_enum_format,
    advise: data_advise,
    unadvise: data_unadvise,
    enum_advise: data_enum_advise,
};

unsafe fn query_interface(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
    supported: &GUID,
    add: unsafe extern "system" fn(*mut c_void) -> u32,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    // SAFETY: QueryInterface requires a writable output pointer.
    unsafe { out.write(null_mut()) };
    // SAFETY: COM supplies a valid interface ID when the pointer is non-null.
    let Some(iid) = (unsafe { iid.as_ref() }) else {
        return E_POINTER;
    };
    if !same_guid(iid, &IID_IUnknown) && !same_guid(iid, supported) {
        return E_NOINTERFACE;
    }
    // SAFETY: The returned pointer carries a new reference owned by the caller.
    unsafe {
        add(this);
        out.write(this);
    }
    S_OK
}

fn same_guid(left: &GUID, right: &GUID) -> bool {
    left.data1 == right.data1
        && left.data2 == right.data2
        && left.data3 == right.data3
        && left.data4 == right.data4
}

unsafe extern "system" fn data_query(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: OLE supplies this interface and the input/output pointers for QueryInterface.
    unsafe { query_interface(this, iid, out, &IID_DATA_OBJECT, data_add) }
}

unsafe extern "system" fn data_add(this: *mut c_void) -> u32 {
    // SAFETY: COM holds a live reference to this FileData.
    unsafe { &*this.cast::<FileData>() }
        .refs
        .fetch_add(1, Ordering::Relaxed)
        + 1
}

unsafe extern "system" fn data_release(this: *mut c_void) -> u32 {
    let this = this.cast::<FileData>();
    // SAFETY: The caller owns this reference and cannot use it after Release.
    let remaining = unsafe { &*this }.refs.fetch_sub(1, Ordering::AcqRel) - 1;
    if remaining == 0 {
        // SAFETY: Only the last Release reclaims the original boxed allocation.
        drop(unsafe { Box::from_raw(this) });
    }
    remaining
}

const FILE_FORMAT: FORMATETC = FORMATETC {
    cfFormat: CF_HDROP,
    ptd: null_mut(),
    dwAspect: DVASPECT_CONTENT,
    lindex: -1,
    tymed: TYMED_HGLOBAL as u32,
};

fn file_format(format: &FORMATETC) -> HRESULT {
    if format.cfFormat != CF_HDROP {
        DV_E_FORMATETC
    } else if !format.ptd.is_null() {
        DV_E_DVTARGETDEVICE
    } else if format.dwAspect != DVASPECT_CONTENT {
        DV_E_DVASPECT
    } else if format.lindex != -1 {
        DV_E_LINDEX
    } else if format.tymed & TYMED_HGLOBAL as u32 == 0 {
        DV_E_TYMED
    } else {
        S_OK
    }
}

unsafe extern "system" fn data_query_format(
    this: *mut c_void,
    format: *const FORMATETC,
) -> HRESULT {
    // SAFETY: COM supplies the requested format or a null pointer.
    let Some(format) = (unsafe { format.as_ref() }) else {
        return E_POINTER;
    };
    let result = file_format(format);
    if result != S_OK {
        return result;
    }
    // SAFETY: COM holds a live reference to this FileData.
    if unsafe { &*this.cast::<FileData>() }.permit.allows() {
        S_OK
    } else {
        DV_E_FORMATETC
    }
}

unsafe extern "system" fn data_get(
    this: *mut c_void,
    format: *const FORMATETC,
    out: *mut STGMEDIUM,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    // SAFETY: GetData owns a writable output slot and queries the same COM interface.
    unsafe { out.write(STGMEDIUM::default()) };
    let result = unsafe { data_query_format(this, format) };
    if result != S_OK {
        return result;
    }
    // SAFETY: COM holds a live reference to this FileData.
    let data = unsafe { &*this.cast::<FileData>() };
    let bytes = size_of::<DROPFILES>() + data.paths.len() * size_of::<u16>();
    // SAFETY: GlobalAlloc returns a movable HGLOBAL, whose lock covers the bounded allocation.
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
    if memory.is_null() {
        return E_OUTOFMEMORY;
    }
    // SAFETY: The preceding allocation returned a live HGLOBAL.
    let buffer = unsafe { GlobalLock(memory) }.cast::<u8>();
    if buffer.is_null() {
        // SAFETY: Allocation ownership has not been transferred.
        unsafe { GlobalFree(memory) };
        return E_OUTOFMEMORY;
    }
    let header = DROPFILES {
        pFiles: size_of::<DROPFILES>() as u32,
        fWide: 1,
        ..DROPFILES::default()
    };
    // SAFETY: The locked buffer has room for the packed header and every UTF-16 unit.
    unsafe {
        buffer.cast::<DROPFILES>().write_unaligned(header);
        copy_nonoverlapping(
            data.paths.as_ptr(),
            buffer.add(size_of::<DROPFILES>()).cast::<u16>(),
            data.paths.len(),
        );
        GlobalUnlock(memory);
    }
    if !data.permit.0.live.load(Ordering::Acquire) {
        // SAFETY: Allocation ownership has not been transferred.
        unsafe { GlobalFree(memory) };
        return DV_E_FORMATETC;
    }
    // SAFETY: GetData transfers the unlocked HGLOBAL to the caller through its output slot.
    unsafe {
        out.write(STGMEDIUM {
            tymed: TYMED_HGLOBAL as u32,
            u: STGMEDIUM_0 { hGlobal: memory },
            pUnkForRelease: null_mut(),
        });
    }
    S_OK
}

unsafe extern "system" fn data_here(
    _: *mut c_void,
    _: *const FORMATETC,
    _: *mut STGMEDIUM,
) -> HRESULT {
    DV_E_TYMED
}

unsafe extern "system" fn data_canonical_format(
    _: *mut c_void,
    _: *const FORMATETC,
    out: *mut FORMATETC,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    // SAFETY: OLE supplies a writable FORMATETC output.
    unsafe { out.write(FORMATETC::default()) };
    DATA_S_SAMEFORMATETC
}

unsafe extern "system" fn data_set(
    _: *mut c_void,
    _: *const FORMATETC,
    _: *mut STGMEDIUM,
    _: BOOL,
) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn data_enum_format(
    _: *mut c_void,
    direction: u32,
    out: *mut *mut c_void,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    // SAFETY: OLE supplies a writable output pointer.
    unsafe { out.write(null_mut()) };
    if direction != DATADIR_GET as u32 {
        return E_NOTIMPL;
    }
    // SAFETY: FILE_FORMAT contains the complete format and no subsidiary pointer.
    unsafe { SHCreateStdEnumFmtEtc(1, &FILE_FORMAT, out) }
}

unsafe extern "system" fn data_advise(
    _: *mut c_void,
    _: *const FORMATETC,
    _: u32,
    _: *mut c_void,
    _: *mut u32,
) -> HRESULT {
    OLE_E_ADVISENOTSUPPORTED
}

unsafe extern "system" fn data_unadvise(_: *mut c_void, _: u32) -> HRESULT {
    OLE_E_ADVISENOTSUPPORTED
}

unsafe extern "system" fn data_enum_advise(_: *mut c_void, _: *mut *mut c_void) -> HRESULT {
    OLE_E_ADVISENOTSUPPORTED
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt as _;

    use windows_sys::Win32::System::Ole::ReleaseStgMedium;
    use windows_sys::Win32::UI::Shell::DragQueryFileW;

    use super::*;

    #[test]
    fn ole_file_data_round_trips_unicode_paths_through_shell() {
        let _ole = Ole::initialize().expect("test thread can initialize OLE");
        let first = PathBuf::from(r"C:\scratch\Grüße ＃ 例.txt");
        let second = PathBuf::from(r"C:\scratch\other");
        let paths = file_list(&[first.clone(), second.clone(), first.clone()]).unwrap();
        let permit = Permit::new(|| true);
        let data = ComReference::new(FileData {
            vtable: &DATA_VTABLE,
            refs: AtomicU32::new(1),
            paths,
            permit: permit.clone(),
        });
        let mut first_medium = STGMEDIUM::default();
        let mut second_medium = STGMEDIUM::default();
        let mut queried = null_mut();
        // SAFETY: The test owns the interface and supplies valid format and medium pointers.
        unsafe {
            assert_eq!(
                data_query(data.as_ptr(), &IID_DATA_OBJECT, &mut queried),
                S_OK,
            );
            assert_eq!(queried, data.as_ptr());
            assert_eq!(data_release(queried), 1);
            assert_eq!(
                data_query(data.as_ptr(), &IID_DROP_SOURCE, &mut queried),
                E_NOINTERFACE,
            );
            assert!(queried.is_null());
            assert_eq!(
                data_get(data.as_ptr(), &FILE_FORMAT, &mut first_medium),
                S_OK
            );
            assert_eq!(
                data_get(data.as_ptr(), &FILE_FORMAT, &mut second_medium),
                S_OK
            );
            assert_ne!(first_medium.u.hGlobal, second_medium.u.hGlobal);
            ReleaseStgMedium(&mut second_medium);
            let files = first_medium.u.hGlobal;
            assert_eq!(DragQueryFileW(files, u32::MAX, null_mut(), 0), 2);
            for (index, expected) in [first, second].into_iter().enumerate() {
                let index = index as u32;
                let size = DragQueryFileW(files, index, null_mut(), 0);
                let mut path = vec![0; size as usize + 1];
                assert_eq!(
                    DragQueryFileW(files, index, path.as_mut_ptr(), size + 1),
                    size
                );
                assert_eq!(
                    PathBuf::from(OsString::from_wide(&path[..size as usize])),
                    expected
                );
            }
            ReleaseStgMedium(&mut first_medium);
            let wrong_medium = FORMATETC {
                tymed: 0,
                ..FILE_FORMAT
            };
            assert_eq!(data_query_format(data.as_ptr(), &wrong_medium), DV_E_TYMED);
            permit.revoke();
            assert_eq!(
                data_get(data.as_ptr(), &FILE_FORMAT, &mut first_medium),
                DV_E_FORMATETC
            );
        }
    }

    #[test]
    fn ole_file_data_rejects_non_paths_and_bounds_file_count() {
        for invalid in [
            r"C:relative.txt",
            r"relative.txt",
            r"C:\",
            r"C:\root\..\x",
            r"\\.\pipe\example",
            "C:\\nul\0.txt",
        ] {
            assert!(
                file_list(&[PathBuf::from(invalid)]).is_none(),
                "{invalid:?}"
            );
        }
        assert!(file_list(&vec![PathBuf::from(r"C:\x"); MAX_FILES + 1]).is_none());
        assert!(file_list(&[]).is_none());
    }

    #[test]
    fn ole_source_cancels_escape_and_revocation_and_drops_on_primary_release() {
        assert_eq!(continue_drag(false, true, true), S_OK);
        assert_eq!(continue_drag(false, false, true), DRAGDROP_S_DROP);
        assert_eq!(continue_drag(true, false, true), DRAGDROP_S_CANCEL);
        assert_eq!(continue_drag(false, false, false), DRAGDROP_S_CANCEL);
    }
}
