use std::path::Path;

/// Hands a previously validated network URI to the desktop's registered application.
pub fn open_desktop_uri(uri: &str) -> bool {
    let Some((scheme, _)) = uri.split_once(':') else {
        return false;
    };
    if !matches!(scheme, "http" | "https" | "ftp" | "ftps" | "mailto")
        || !uri.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return false;
    }
    open(uri)
}

/// Reveals a previously validated absolute directory using the desktop's registered handler.
/// Call on the AppKit main thread on macOS and on a background worker elsewhere.
pub fn open_desktop_directory(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt as _;

        let path: Vec<_> = path.as_os_str().encode_wide().collect();
        if path.contains(&0) {
            return false;
        }
        let mut path = path;
        path.push(0);
        shell_open(&path, true)
    }
    #[cfg(not(target_os = "windows"))]
    {
        url::Url::from_directory_path(path).is_ok_and(|uri| open(uri.as_str()))
    }
}

#[cfg(target_os = "macos")]
fn open(uri: &str) -> bool {
    kortty_macos::open_desktop_uri(uri)
}

#[cfg(target_os = "windows")]
fn open(uri: &str) -> bool {
    let uri: Vec<_> = uri.encode_utf16().chain([0]).collect();
    shell_open(&uri, false)
}

#[cfg(target_os = "windows")]
fn shell_open(target: &[u16], directory: bool) -> bool {
    use std::ptr::null;
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    // SAFETY: The caller runs this on a background worker. The target is NUL-terminated,
    // all optional pointers are null, and a successful COM initialization is balanced.
    unsafe {
        if CoInitializeEx(
            null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        ) < 0
        {
            return false;
        }
        let result = ShellExecuteW(
            std::ptr::null_mut(),
            if directory {
                windows_sys::w!("explore")
            } else {
                windows_sys::w!("open")
            },
            target.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        ) as isize;
        CoUninitialize();
        result > 32
    }
}

#[cfg(target_os = "linux")]
fn open(uri: &str) -> bool {
    use std::ffi::{CString, c_char, c_int, c_void};
    use std::ptr::null_mut;
    use std::sync::OnceLock;

    static GIO: OnceLock<Option<libloading::Library>> = OnceLock::new();
    let Some(gio) = GIO.get_or_init(|| {
        // SAFETY: Load the system GIO ABI and keep the library alive for the process.
        unsafe { libloading::Library::new("libgio-2.0.so.0").ok() }
    }) else {
        return false;
    };
    let Ok(uri) = CString::new(uri) else {
        return false;
    };
    type Launch = unsafe extern "C" fn(*const c_char, *mut c_void, *mut *mut c_void) -> c_int;
    // SAFETY: The symbol has the GIO 2.0 signature. Both optional pointer arguments
    // are null and the URI is a valid NUL-terminated string throughout the call.
    unsafe {
        gio.get::<Launch>(b"g_app_info_launch_default_for_uri\0")
            .is_ok_and(|launch| launch(uri.as_ptr(), null_mut(), null_mut()) != 0)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn open(_uri: &str) -> bool {
    false
}
