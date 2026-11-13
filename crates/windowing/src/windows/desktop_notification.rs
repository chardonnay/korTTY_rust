//! System-tray balloons for portable Windows desktop builds.

use std::cell::RefCell;
use std::mem::{ManuallyDrop, size_of};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{null, null_mut};
use std::sync::OnceLock;

use raw_window_handle::{RawWindowHandle, WindowHandle};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_RESPECT_QUIET_TIME,
    NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_BALLOONUSERCLICK, NIN_SELECT, NINF_KEY,
    NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CopyIcon, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, HICON, ICON_SMALL,
    IDI_APPLICATION, LoadIconW, RegisterClassW, SendMessageW, WM_APP, WM_GETICON, WM_LBUTTONDBLCLK,
    WM_NCDESTROY, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows_sys::w;

use super::on_current_thread;

const CLASS: *const u16 = w!("kortty.DesktopNotifications.1");
const CALLBACK: u32 = WM_APP + 1;
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;

type Activate = Box<dyn FnOnce() + Send + 'static>;

thread_local! {
    // Release explicitly on the UI thread, away from Windows TLS destruction.
    static TRAY: RefCell<ManuallyDrop<Option<Tray>>> = const {
        RefCell::new(ManuallyDrop::new(None))
    };
}

struct Tray {
    window: HWND,
    next_id: u16,
    registration: Option<Registration>,
    activation: Option<(u16, Activate)>,
}

impl Tray {
    fn new() -> Option<Self> {
        static CLASS_REGISTERED: OnceLock<bool> = OnceLock::new();
        // SAFETY: Null requests the current executable's module handle.
        let instance = unsafe { GetModuleHandleW(null()) };
        if instance.is_null()
            || !*CLASS_REGISTERED.get_or_init(|| {
                let class = WNDCLASSW {
                    lpfnWndProc: Some(tray_event),
                    hInstance: instance,
                    lpszClassName: CLASS,
                    ..WNDCLASSW::default()
                };
                // SAFETY: The class name and procedure remain valid for the application.
                unsafe { RegisterClassW(&class) != 0 }
            })
        {
            return None;
        }
        // SAFETY: This is an unshown, unowned tool window on the caller's UI thread.
        let window = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                CLASS,
                w!("kortty"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };
        if window.is_null() {
            return None;
        }
        Some(Self {
            window,
            next_id: 0,
            registration: None,
            activation: None,
        })
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        drop(self.registration.take());
        if !self.window.is_null() {
            // SAFETY: The tray stays on the UI thread that created this HWND.
            unsafe { DestroyWindow(self.window) };
        }
    }
}

struct TrayIcon(HICON);

impl TrayIcon {
    fn copy(window: HWND) -> Option<Self> {
        // SAFETY: The owner HWND belongs to this thread. Only the copied icon
        // is owned by the tray, so it remains valid when the owner closes.
        let icon = unsafe {
            let small = SendMessageW(window, WM_GETICON, ICON_SMALL as WPARAM, 0) as HICON;
            let source = if small.is_null() {
                LoadIconW(null_mut(), IDI_APPLICATION)
            } else {
                small
            };
            CopyIcon(source)
        };
        if icon.is_null() {
            return None;
        }
        Some(Self(icon))
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        // SAFETY: This is our private CopyIcon handle.
        unsafe { DestroyIcon(self.0) };
    }
}

struct Registration {
    window: HWND,
    id: u16,
    _icon: TrayIcon,
}

impl Registration {
    fn add(window: HWND, id: u16, icon: TrayIcon) -> Option<Self> {
        let mut data = data_for(window, id);
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = CALLBACK;
        data.hIcon = icon.0;
        data.szTip = utf16("kortty");
        // SAFETY: The structure and its private icon are live for the shell calls.
        if unsafe { Shell_NotifyIconW(NIM_ADD, &data) } == 0 {
            return None;
        }
        let registration = Self {
            window,
            id,
            _icon: icon,
        };
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        // SAFETY: This applies version 4 to the icon we just added.
        if unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) } == 0 {
            return None;
        }
        Some(registration)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let data = data_for(self.window, self.id);
        // SAFETY: The icon ID belongs to our window and has not been reused.
        unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
    }
}

/// Posts a native notification using the tray from the window's UI thread.
///
/// Returns whether the shell accepted the submission. The shell owns presentation
/// and quiet-time behavior. The most recent balloon can activate once. Replacing
/// it invalidates callbacks from its previous icon ID.
pub fn send_desktop_notification(
    window: WindowHandle<'_>,
    title: String,
    body: String,
    on_activate: impl FnOnce() + Send + 'static,
) -> bool {
    let RawWindowHandle::Win32(owner) = window.as_raw() else {
        return false;
    };
    let owner = owner.hwnd.get() as HWND;
    // SAFETY: We only access the borrowed HWND on its owner thread.
    if !unsafe { on_current_thread(owner) } {
        return false;
    }
    let Some((title, info)) = balloon_text(&title, &body) else {
        return false;
    };
    let Some(icon) = TrayIcon::copy(owner) else {
        return false;
    };
    if TRAY.with(|tray| {
        tray.borrow()
            .as_ref()
            .is_some_and(|tray| tray.next_id == u16::MAX)
    }) {
        // A new HWND prevents delayed callbacks from matching a reused icon ID.
        shutdown_desktop_notifications();
    }
    if TRAY.with(|tray| tray.borrow().is_none()) {
        let Some(tray) = Tray::new() else {
            return false;
        };
        TRAY.with(|slot| **slot.borrow_mut() = Some(tray));
    }
    let Some((window, id, previous, activation)) = TRAY.with(|slot| {
        let mut slot = slot.borrow_mut();
        let tray = slot.as_mut()?;
        tray.next_id += 1;
        Some((
            tray.window,
            tray.next_id,
            tray.registration.take(),
            tray.activation.take(),
        ))
    }) else {
        return false;
    };
    // Shell calls can reenter the window procedure, so drop outside the TLS borrow.
    drop(activation);
    drop(previous);
    let Some(registration) = Registration::add(window, id, icon) else {
        return false;
    };
    let mut registration = Some(registration);
    let installed = TRAY.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(tray) = slot
            .as_mut()
            .filter(|tray| tray.window == window && tray.next_id == id)
        else {
            return false;
        };
        tray.registration = registration.take();
        tray.activation = Some((id, Box::new(on_activate)));
        true
    });
    if !installed {
        return false;
    }
    let mut data = data_for(window, id);
    data.uFlags = NIF_INFO;
    data.szInfo = info;
    data.szInfoTitle = title;
    data.dwInfoFlags = NIIF_INFO | NIIF_RESPECT_QUIET_TIME;
    // SAFETY: The current registration and the complete notification data are live.
    if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } != 0 {
        return true;
    }
    let failed = TRAY.with(|slot| {
        let mut slot = slot.borrow_mut();
        let tray = slot
            .as_mut()
            .filter(|tray| tray.window == window && tray.next_id == id)?;
        Some((tray.registration.take(), tray.activation.take()))
    });
    drop(failed);
    false
}

/// Removes this UI thread's tray icon and disarms any pending click.
pub fn shutdown_desktop_notifications() {
    let tray = TRAY.with(|slot| slot.borrow_mut().take());
    drop(tray);
}

fn data_for(window: HWND, id: u16) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: u32::from(id),
        ..NOTIFYICONDATAW::default()
    }
}

fn balloon_text(title: &str, body: &str) -> Option<([u16; 64], [u16; 256])> {
    let title = utf16::<64>(title);
    let mut info = utf16::<256>(body);
    if info[0] == 0 {
        if title[0] == 0 {
            return None;
        }
        // An empty szInfo removes the balloon, even when szInfoTitle has text.
        info[0] = u16::from(b' ');
    }
    Some((title, info))
}

fn utf16<const N: usize>(value: &str) -> [u16; N] {
    let mut buffer = [0; N];
    let mut offset = 0;
    for character in value.chars().filter(|&character| character != '\0') {
        let end = offset + character.len_utf16();
        if end >= N {
            break;
        }
        character.encode_utf16(&mut buffer[offset..end]);
        offset = end;
    }
    buffer
}

unsafe extern "system" fn tray_event(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == CALLBACK {
        // Version 4 encodes the event and icon ID in the low/high words.
        let event = (lparam & 0xffff) as u32;
        let id = ((lparam >> 16) & 0xffff) as u16;
        if matches!(
            event,
            NIN_BALLOONUSERCLICK | NIN_KEYSELECT | WM_LBUTTONDBLCLK
        ) {
            let activation = TRAY
                .try_with(|slot| {
                    let mut slot = slot.borrow_mut();
                    let tray = slot.as_mut().filter(|tray| tray.window == window)?;
                    tray.activation
                        .take_if(|(current_id, _)| *current_id == id)
                        .map(|(_, activate)| activate)
                })
                .ok()
                .flatten();
            if let Some(activate) = activation {
                let _ = catch_unwind(AssertUnwindSafe(activate));
            }
        }
        return 0;
    }
    if message == WM_NCDESTROY
        && let Ok(Some(mut tray)) =
            TRAY.try_with(|slot| slot.borrow_mut().take_if(|tray| tray.window == window))
    {
        tray.window = null_mut();
        drop(tray);
    }
    // SAFETY: The system supplied these arguments to this window procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    #[test]
    fn windows_desktop_notification_text_fits_native_fields() {
        let text = utf16::<5>("a\0🙂b");
        assert_eq!(text, [0x61, 0xd83d, 0xde42, 0x62, 0]);
        assert_eq!(utf16::<4>("ab🙂"), [0x61, 0x62, 0, 0]);
        let (title, info) = balloon_text(
            &format!("{}🙂", "t".repeat(62)),
            &format!("{}🙂c", "b".repeat(253)),
        )
        .unwrap();
        assert_eq!(&title[..62], &[u16::from(b't'); 62]);
        assert_eq!(&title[62..], &[0, 0]);
        assert_eq!(&info[..253], &[u16::from(b'b'); 253]);
        assert_eq!(&info[253..], &[0xd83d, 0xde42, 0]);
    }

    #[test]
    fn windows_title_only_notification_has_blank_shell_body() {
        let (title, info) = balloon_text("Build complete", "").unwrap();
        assert_eq!(title, utf16::<64>("Build complete"));
        assert_eq!(info, utf16::<256>(" "));
        assert_eq!(balloon_text("Build complete", "\0"), Some((title, info)));
        assert!(balloon_text("", "").is_none());
        assert_eq!(
            balloon_text("", "Details").unwrap().1,
            utf16::<256>("Details")
        );
    }

    #[test]
    fn windows_desktop_notification_click_is_bound_to_hidden_window_and_id() {
        struct Shutdown;
        impl Drop for Shutdown {
            fn drop(&mut self) {
                shutdown_desktop_notifications();
            }
        }
        let _shutdown = Shutdown;
        let mut tray = Tray::new().expect("hidden notification window");
        let window = tray.window;
        let counter = Arc::new(AtomicUsize::new(0));
        let clicked = Arc::clone(&counter);
        tray.activation = Some((
            7,
            Box::new(move || {
                clicked.fetch_add(1, Ordering::Relaxed);
            }),
        ));
        TRAY.with(|slot| **slot.borrow_mut() = Some(tray));
        let send = |id: u16, event: u32| {
            // SAFETY: The owned hidden HWND is only sent test callback messages.
            unsafe {
                SendMessageW(
                    window,
                    CALLBACK,
                    0,
                    ((u32::from(id) << 16) | event) as LPARAM,
                )
            };
        };
        send(6, NIN_BALLOONUSERCLICK);
        send(7, WM_APP);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
        send(7, NIN_BALLOONUSERCLICK);
        send(7, NIN_BALLOONUSERCLICK);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        shutdown_desktop_notifications();
        assert!(TRAY.with(|slot| slot.borrow().is_none()));
    }
}
