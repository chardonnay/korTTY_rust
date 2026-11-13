use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::null_mut;
use std::rc::Rc;

use raw_window_handle::{RawWindowHandle, WindowHandle};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIcon, DefWindowProcW, DestroyIcon, HICON, ICON_BIG, ICON_SMALL, SendMessageW, WM_GETICON,
    WM_NCDESTROY, WM_SETICON,
};

use super::{on_current_thread, subclass};

struct OwnedIcon(HICON);

impl Drop for OwnedIcon {
    fn drop(&mut self) {
        // SAFETY: This is a private CreateIcon handle, retained while a window uses it.
        unsafe { DestroyIcon(self.0) };
    }
}

#[derive(Default)]
struct WindowIcons {
    big: Option<Rc<OwnedIcon>>,
    small: Option<Rc<OwnedIcon>>,
}

thread_local! {
    static ICONS: RefCell<HashMap<HWND, WindowIcons>> = RefCell::default();
}

/// Applies a private RGBA icon to the taskbar and small window-icon slots.
/// Call on the thread owning the Win32 window.
pub fn set_app_window_icon(
    window: WindowHandle<'_>,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
) -> bool {
    let RawWindowHandle::Win32(handle) = window.as_raw() else {
        return false;
    };
    let window = handle.hwnd.get() as HWND;
    // SAFETY: The borrowed window handle stays live throughout this call.
    if !unsafe { on_current_thread(window) } {
        return false;
    }
    let Some(icon) = create_icon(rgba, width, height).map(Rc::new) else {
        return false;
    };
    if !ICONS.with(|icons| icons.borrow().contains_key(&window)) {
        let Some(api) = subclass::Api::get() else {
            return false;
        };
        // SAFETY: The window belongs to this thread and the callback has static lifetime.
        if unsafe { (api.install)(window, Some(release_icons), 0, 0) } == 0 {
            return false;
        }
        ICONS.with(|icons| icons.borrow_mut().insert(window, WindowIcons::default()));
    }
    let big = apply(window, ICON_BIG, &icon);
    let small = apply(window, ICON_SMALL, &icon);
    big && small
}

fn create_icon(mut rgba: Vec<u8>, width: u32, height: u32) -> Option<OwnedIcon> {
    if !(1..=512).contains(&width)
        || !(1..=512).contains(&height)
        || rgba.len() != (width * height * 4) as usize
    {
        return None;
    }
    let width_usize = width as usize;
    let stride = width_usize.div_ceil(16) * 2;
    let mut mask = vec![0; stride * height as usize];
    for (index, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        if pixel[3] == 0 {
            let x = index % width_usize;
            let y = index / width_usize;
            mask[y * stride + x / 8] |= 0x80 >> (x % 8);
        }
        pixel.swap(0, 2);
    }
    // SAFETY: The AND mask is one bit per pixel with WORD-aligned rows, and
    // the color mask is a checked buffer of BGRA pixels. CreateIcon copies both.
    let handle = unsafe {
        CreateIcon(
            null_mut(),
            width as i32,
            height as i32,
            1,
            32,
            mask.as_ptr(),
            rgba.as_ptr(),
        )
    };
    if handle.is_null() {
        None
    } else {
        Some(OwnedIcon(handle))
    }
}

fn apply(window: HWND, slot: u32, icon: &Rc<OwnedIcon>) -> bool {
    let handle = icon.0 as LPARAM;
    // SAFETY: The HWND is used only on its owning thread and the Rc retains
    // the HICON across both messages, even if a callback destroys the window.
    unsafe {
        if !on_current_thread(window) {
            return false;
        }
        SendMessageW(window, WM_SETICON, slot as WPARAM, handle);
        if SendMessageW(window, WM_GETICON, slot as WPARAM, 0) != handle {
            return false;
        }
    }
    ICONS.with(|icons| {
        let mut icons = icons.borrow_mut();
        let Some(applied) = icons.get_mut(&window) else {
            return false;
        };
        if slot == ICON_BIG {
            applied.big = Some(Rc::clone(icon));
        } else {
            applied.small = Some(Rc::clone(icon));
        }
        true
    })
}

unsafe extern "system" fn release_icons(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    // SAFETY: Comctl32 invokes this on the subclassed window's thread.
    // Keep the icon handles valid until default destruction finishes.
    unsafe {
        let Some(api) = subclass::Api::get() else {
            return DefWindowProcW(window, message, wparam, lparam);
        };
        if message != WM_NCDESTROY {
            return (api.next)(window, message, wparam, lparam);
        }
        (api.remove)(window, Some(release_icons), id);
        let result = (api.next)(window, message, wparam, lparam);
        let _ = ICONS.try_with(|icons| icons.borrow_mut().remove(&window));
        result
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroIsize;

    use raw_window_handle::Win32WindowHandle;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WS_OVERLAPPED,
    };
    use windows_sys::w;

    use super::*;

    struct HiddenWindow(HWND);

    impl Drop for HiddenWindow {
        fn drop(&mut self) {
            // SAFETY: The test owns this window on the same thread that created it.
            unsafe { DestroyWindow(self.0) };
        }
    }

    #[test]
    fn windows_app_icon_replaces_both_slots_and_releases_icons_on_destroy() {
        // SAFETY: STATIC is a built-in window class. The window is private and never shown.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                w!("STATIC"),
                w!("kortty icon test"),
                WS_OVERLAPPED,
                0,
                0,
                32,
                32,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        assert!(!hwnd.is_null());
        let window = HiddenWindow(hwnd);
        let raw = RawWindowHandle::Win32(Win32WindowHandle::new(
            NonZeroIsize::new(hwnd as isize).unwrap(),
        ));
        let apply_color = |color| {
            // SAFETY: The HWND is owned by `window` for both calls.
            let handle = unsafe { WindowHandle::borrow_raw(raw) };
            assert!(set_app_window_icon(
                handle,
                [color, 0, 0, 255].repeat(256),
                16,
                16
            ));
            // SAFETY: The test still owns the hidden window and only queries its icon slots.
            let (small, big) = unsafe {
                (
                    SendMessageW(hwnd, WM_GETICON, ICON_SMALL as WPARAM, 0),
                    SendMessageW(hwnd, WM_GETICON, ICON_BIG as WPARAM, 0),
                )
            };
            assert_ne!(small, 0);
            assert_eq!(small, big);
            let tracked = ICONS.with(|icons| {
                let icons = icons.borrow();
                let slots = &icons[&hwnd];
                let retained = slots.big.as_ref().unwrap();
                assert_eq!(retained.0 as LPARAM, big);
                assert!(Rc::ptr_eq(retained, slots.small.as_ref().unwrap()));
                Rc::downgrade(retained)
            });
            (small, tracked)
        };
        let (first_handle, first) = apply_color(120);
        let (second_handle, second) = apply_color(220);
        assert_ne!(first_handle, second_handle);
        assert!(first.upgrade().is_none());
        drop(window);
        assert!(second.upgrade().is_none());
        assert!(!ICONS.with(|icons| icons.borrow().contains_key(&hwnd)));
    }
}
