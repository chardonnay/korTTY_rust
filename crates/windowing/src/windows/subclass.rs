use std::mem::transmute;
use std::ptr::null_mut;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{FreeLibrary, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows_sys::Win32::UI::Shell::SUBCLASSPROC;
use windows_sys::core::BOOL;

type Install = unsafe extern "system" fn(HWND, SUBCLASSPROC, usize, usize) -> BOOL;
type Remove = unsafe extern "system" fn(HWND, SUBCLASSPROC, usize) -> BOOL;
type Next = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

pub(super) struct Api {
    pub(super) install: Install,
    pub(super) remove: Remove,
    pub(super) next: Next,
}

impl Api {
    pub(super) fn get() -> Option<&'static Self> {
        static API: OnceLock<Option<Api>> = OnceLock::new();
        API.get_or_init(Self::load).as_ref()
    }

    fn load() -> Option<Self> {
        // SAFETY: Load only the system DLL, and resolve the documented exports
        // by name. Common Controls v5 does not name every subclass helper.
        unsafe {
            let module = LoadLibraryExW(
                windows_sys::w!("comctl32.dll"),
                null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            );
            if module.is_null() {
                return None;
            }
            let (Some(install), Some(remove), Some(next)) = (
                GetProcAddress(module, c"SetWindowSubclass".as_ptr().cast()),
                GetProcAddress(module, c"RemoveWindowSubclass".as_ptr().cast()),
                GetProcAddress(module, c"DefSubclassProc".as_ptr().cast()),
            ) else {
                FreeLibrary(module);
                return None;
            };
            // Keep the successful reference for the life of the callbacks.
            Some(Self {
                install: transmute::<unsafe extern "system" fn() -> isize, Install>(install),
                remove: transmute::<unsafe extern "system" fn() -> isize, Remove>(remove),
                next: transmute::<unsafe extern "system" fn() -> isize, Next>(next),
            })
        }
    }
}
