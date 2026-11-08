use raw_window_handle::Win32WindowHandle;
use windows_sys::Win32::Graphics::{
    Dwm::{DWM_BB_BLURREGION, DWM_BB_ENABLE, DWM_BLURBEHIND, DwmEnableBlurBehindWindow},
    Gdi::{CreateRectRgn, DeleteObject},
};

pub(super) fn enable(window: Win32WindowHandle) -> bool {
    // SAFETY: The caller holds the borrowed native window handle. DWM
    // borrows the initialized region for the duration of this call.
    unsafe {
        let region = CreateRectRgn(-1, -1, 0, 0);
        if region.is_null() {
            return false;
        }
        // The one-pixel region lies outside the client area. DWM composites
        // the redirected client pixels with their alpha and no client blur.
        let blur = DWM_BLURBEHIND {
            dwFlags: DWM_BB_ENABLE | DWM_BB_BLURREGION,
            fEnable: 1,
            hRgnBlur: region,
            fTransitionOnMaximized: 0,
        };
        let result = DwmEnableBlurBehindWindow(window.hwnd.get() as _, &blur);
        DeleteObject(region);
        result >= 0
    }
}
