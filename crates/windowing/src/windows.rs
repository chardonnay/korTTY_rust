use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_RBUTTON;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetAsyncKeyState, IsWindowEnabled, VK_LBUTTON,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, GA_ROOT, GW_OWNER, GWLP_HWNDPARENT, GetAncestor, GetClientRect, GetCursorPos,
    GetSystemMetrics, GetWindow, GetWindowThreadProcessId, SM_SWAPBUTTON, SetWindowLongPtrW,
    WM_NCDESTROY, WindowFromPoint,
};

use super::{Hit, NativeWindow, Sample, WindowId};

pub(super) mod app_icon;
pub(super) mod desktop_notification;
pub(super) mod password_prompt;
mod subclass;

#[derive(Clone, Copy)]
enum ModalInput {
    Owner { dialogs: usize, disabling: bool },
    Dialog { owner: Option<HWND> },
}

thread_local! {
    // Each entry has a same-thread subclass that removes it on WM_NCDESTROY.
    static MODAL_WINDOWS: RefCell<HashMap<HWND, ModalInput>> = RefCell::default();
}

fn bind_dialog_owner(dialog: HWND, owner: HWND) -> bool {
    // SAFETY: These queries validate both handles on the current UI thread. Assigning
    // GWLP_HWNDPARENT to a top-level window sets its owner without reparenting it.
    unsafe {
        let current = GetCurrentThreadId();
        if dialog == owner
            || GetWindowThreadProcessId(dialog, null_mut()) != current
            || GetWindowThreadProcessId(owner, null_mut()) != current
            || GetAncestor(dialog, GA_ROOT) != dialog
            || GetAncestor(owner, GA_ROOT) != owner
            || !GetWindow(owner, GW_OWNER).is_null()
        {
            return false;
        }
        let previous = GetWindow(dialog, GW_OWNER);
        if previous.is_null() {
            SetWindowLongPtrW(dialog, GWLP_HWNDPARENT, owner as isize);
        }
        GetWindow(dialog, GW_OWNER) == owner
    }
}

pub(super) fn bind_modeless_dialog_owner(dialog: HWND, owner: HWND) -> bool {
    // A released child must not acquire a native owner after close was queued.
    modal_input_state(dialog).is_none() && bind_dialog_owner(dialog, owner)
}

pub(super) fn bind_modal_dialog_owner(dialog: HWND, owner: HWND) -> bool {
    // SAFETY: Only the thread that owns each HWND may change its subclasses.
    unsafe {
        if !on_current_thread(dialog) {
            return false;
        }
        if let Some(input) = modal_input_state(dialog) {
            return matches!(input, ModalInput::Dialog { owner: Some(bound) } if bound == owner);
        }
        if !bind_dialog_owner(dialog, owner) {
            return false;
        }
        let Some(api) = subclass::Api::get() else {
            return false;
        };
        let (dialogs, disabling, first) = match modal_input_state(owner) {
            Some(ModalInput::Owner { dialogs, disabling }) => (dialogs, disabling, false),
            None if IsWindowEnabled(owner) != 0 => (0, true, true),
            _ => return true,
        };
        if (api.install)(dialog, Some(modal_input), 0, 0) == 0 {
            return false;
        }
        if first && (api.install)(owner, Some(modal_input), 0, 0) == 0 {
            (api.remove)(dialog, Some(modal_input), 0);
            return false;
        }
        remember_modal_input(dialog, ModalInput::Dialog { owner: Some(owner) });
        remember_modal_input(
            owner,
            ModalInput::Owner {
                dialogs: dialogs + 1,
                disabling,
            },
        );
        if first {
            EnableWindow(owner, 0);
            // A callback can release the last dialog during WM_CANCELMODE.
            if on_current_thread(owner)
                && let Some(ModalInput::Owner {
                    dialogs,
                    disabling: true,
                }) = modal_input_state(owner)
            {
                update_owner_input(api, owner, dialogs, false);
            }
        }
        true
    }
}

pub(super) fn release_modal_dialog_owner(dialog: HWND) {
    // SAFETY: This runs with a borrowed dialog HWND on its owning thread.
    unsafe {
        if !on_current_thread(dialog) {
            return;
        }
        let Some(api) = subclass::Api::get() else {
            return;
        };
        match modal_input_state(dialog) {
            Some(ModalInput::Dialog { owner: Some(owner) }) => {
                remember_modal_input(dialog, ModalInput::Dialog { owner: None });
                release_owner_input(api, owner);
            }
            // A delayed bind must not acquire a lock after close was queued.
            None if (api.install)(dialog, Some(modal_input), 0, 0) != 0 => {
                remember_modal_input(dialog, ModalInput::Dialog { owner: None });
            }
            _ => {}
        }
    }
}

unsafe fn on_current_thread(window: HWND) -> bool {
    // SAFETY: This query also rejects HWNDs that have been destroyed.
    unsafe { GetWindowThreadProcessId(window, null_mut()) == GetCurrentThreadId() }
}

fn modal_input_state(window: HWND) -> Option<ModalInput> {
    MODAL_WINDOWS
        .try_with(|windows| windows.borrow().get(&window).copied())
        .ok()
        .flatten()
}

fn remember_modal_input(window: HWND, input: ModalInput) {
    let _ = MODAL_WINDOWS.try_with(|windows| windows.borrow_mut().insert(window, input));
}

fn take_modal_input(window: HWND) -> Option<ModalInput> {
    MODAL_WINDOWS
        .try_with(|windows| windows.borrow_mut().remove(&window))
        .ok()
        .flatten()
}

unsafe fn release_owner_input(api: &subclass::Api, owner: HWND) {
    // SAFETY: The stored owner belongs to this dialog. Win32 destroys the
    // dialog before its owner and the owner's subclass anchors the input state.
    unsafe {
        if on_current_thread(owner)
            && let Some(ModalInput::Owner { dialogs, disabling }) = modal_input_state(owner)
            && let Some(remaining) = dialogs.checked_sub(1)
        {
            update_owner_input(api, owner, remaining, disabling);
        }
    }
}

unsafe fn update_owner_input(api: &subclass::Api, owner: HWND, dialogs: usize, disabling: bool) {
    // SAFETY: This is the owning thread, and the subclass belongs to this lock.
    unsafe {
        if dialogs == 0 && !disabling {
            take_modal_input(owner);
            (api.remove)(owner, Some(modal_input), 0);
            EnableWindow(owner, 1);
        } else {
            remember_modal_input(owner, ModalInput::Owner { dialogs, disabling });
        }
    }
}

unsafe extern "system" fn modal_input(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    // SAFETY: Comctl32 calls this on the subclassed window's thread. Remove
    // our handler before releasing input so reentrant destruction counts once.
    unsafe {
        let Some(api) = subclass::Api::get() else {
            return DefWindowProcW(window, message, wparam, lparam);
        };
        if message == WM_NCDESTROY {
            let input = take_modal_input(window);
            (api.remove)(window, Some(modal_input), id);
            if let Some(ModalInput::Dialog { owner: Some(owner) }) = input {
                release_owner_input(api, owner);
            }
        }
        (api.next)(window, message, wparam, lparam)
    }
}

pub(super) fn sample(windows: &[NativeWindow]) -> Option<Sample> {
    let mut point = POINT::default();
    // SAFETY: Win32 only writes to the supplied point and reads system state.
    if unsafe { GetCursorPos(&mut point) } == 0 {
        return None;
    }
    // App mouse events use logical left, including Windows' swapped buttons.
    // SAFETY: GetSystemMetrics only reads the requested system setting.
    let button = if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } == 0 {
        VK_LBUTTON
    } else {
        VK_RBUTTON
    };
    // SAFETY: These calls read global input state and query window identities.
    let held = unsafe { GetAsyncKeyState(i32::from(button)) } < 0;
    let child = unsafe { WindowFromPoint(point) };
    let target = if child.is_null() {
        child
    } else {
        // SAFETY: child was just supplied by WindowFromPoint.
        unsafe { GetAncestor(child, GA_ROOT) }
    };
    let window = NativeWindow(WindowId::Windows(target as usize));
    if !windows.contains(&window) {
        return Some(Sample { held, hit: None });
    }
    let mut rect = RECT::default();
    // SAFETY: Target is a registered HWND and the outputs are initialized locals.
    let valid_client =
        unsafe { ScreenToClient(target, &mut point) != 0 && GetClientRect(target, &mut rect) != 0 };
    let hit = (valid_client
        && (rect.left..rect.right).contains(&point.x)
        && (rect.top..rect.bottom).contains(&point.y))
    .then_some(Hit {
        window,
        x: f64::from(point.x),
        y: f64::from(point.y),
        width: f64::from(rect.right - rect.left),
        logical_coordinates: false,
    });
    Some(Sample { held, hit })
}

pub(super) fn file_drop_position(target: HWND) -> Option<(f64, f64)> {
    let mut point = POINT::default();
    let mut rect = RECT::default();
    // SAFETY: Win32 only queries the supplied HWND and fills initialized locals.
    let valid = unsafe {
        GetCursorPos(&mut point) != 0
            && ScreenToClient(target, &mut point) != 0
            && GetClientRect(target, &mut rect) != 0
    };
    (valid
        && (rect.left..rect.right).contains(&point.x)
        && (rect.top..rect.bottom).contains(&point.y))
    .then_some((f64::from(point.x), f64::from(point.y)))
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, IsWindow, WM_CANCELMODE, WS_POPUP,
    };

    use super::*;

    struct TestWindow(HWND);

    impl TestWindow {
        fn new() -> Self {
            let class: Vec<_> = "STATIC\0".encode_utf16().collect();
            // SAFETY: STATIC is a system window class. This creates a hidden,
            // top-level window on the test thread without retaining any arguments.
            let handle = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    null_mut(),
                    WS_POPUP,
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
            assert!(!handle.is_null());
            Self(handle)
        }
    }

    impl Drop for TestWindow {
        fn drop(&mut self) {
            // SAFETY: The test creates and releases the window on the same thread.
            unsafe { DestroyWindow(self.0) };
        }
    }

    #[test]
    fn modeless_dialog_keeps_its_owner_enabled_and_refuses_reassignment() {
        let owner = TestWindow::new();
        let other = TestWindow::new();
        let dialog = TestWindow::new();

        assert!(bind_modeless_dialog_owner(dialog.0, owner.0));
        assert!(bind_modeless_dialog_owner(dialog.0, owner.0));
        assert!(!bind_modeless_dialog_owner(dialog.0, other.0));
        assert!(!bind_modeless_dialog_owner(owner.0, dialog.0));
        // SAFETY: All three test windows are live and on this thread.
        unsafe {
            assert_eq!(GetAncestor(dialog.0, GA_ROOT), dialog.0);
            assert_eq!(GetWindow(dialog.0, GW_OWNER), owner.0);
            assert!(GetWindow(other.0, GW_OWNER).is_null());
            assert_ne!(IsWindowEnabled(owner.0), 0);
        }
        release_modal_dialog_owner(dialog.0);
        assert!(!bind_modeless_dialog_owner(dialog.0, owner.0));
    }

    #[test]
    fn native_modal_input_blocks_only_the_owner_until_the_last_dialog_closes() {
        let owner = TestWindow::new();
        let other = TestWindow::new();
        let first = TestWindow::new();
        let second = TestWindow::new();

        assert!(bind_modal_dialog_owner(first.0, owner.0));
        assert!(bind_modal_dialog_owner(first.0, owner.0));
        assert!(bind_modal_dialog_owner(second.0, owner.0));
        // SAFETY: These windows remain live on the test thread.
        unsafe {
            assert_eq!(IsWindowEnabled(owner.0), 0);
            assert_ne!(IsWindowEnabled(other.0), 0);
            assert_ne!(IsWindowEnabled(second.0), 0);
        }

        release_modal_dialog_owner(first.0);
        assert!(!bind_modal_dialog_owner(first.0, owner.0));
        drop(first);
        // SAFETY: The owner remains live on the test thread.
        assert_eq!(unsafe { IsWindowEnabled(owner.0) }, 0);

        drop(second);
        // SAFETY: The owner remains live on the test thread.
        assert_ne!(unsafe { IsWindowEnabled(owner.0) }, 0);
    }

    #[test]
    fn native_modal_close_preserves_a_preexisting_owner_disable() {
        let owner = TestWindow::new();
        let dialog = TestWindow::new();
        // SAFETY: The owner is a live window on the test thread.
        unsafe { EnableWindow(owner.0, 0) };
        assert!(bind_modal_dialog_owner(dialog.0, owner.0));
        release_modal_dialog_owner(dialog.0);
        drop(dialog);
        // SAFETY: The owner remains live on the test thread.
        assert_eq!(unsafe { IsWindowEnabled(owner.0) }, 0);
    }

    #[test]
    fn reentrant_dialog_destruction_during_owner_disable_restores_input() {
        let owner = TestWindow::new();
        let dialog = std::mem::ManuallyDrop::new(TestWindow::new());
        let api = subclass::Api::get().expect("the Win32 subclass APIs must be available");
        // SAFETY: The test callback and its target are live on this thread.
        unsafe {
            assert_ne!(
                (api.install)(owner.0, Some(close_during_disable), 0, dialog.0 as usize),
                0
            );
            bind_modal_dialog_owner(dialog.0, owner.0);
            assert_eq!(IsWindow(dialog.0), 0);
            assert_ne!(IsWindowEnabled(owner.0), 0);
        }
    }

    unsafe extern "system" fn close_during_disable(
        owner: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        id: usize,
        dialog: usize,
    ) -> LRESULT {
        // SAFETY: Comctl32 calls this on the test thread with the test HWND.
        unsafe {
            let Some(api) = subclass::Api::get() else {
                return DefWindowProcW(owner, message, wparam, lparam);
            };
            if message == WM_CANCELMODE || message == WM_NCDESTROY {
                (api.remove)(owner, Some(close_during_disable), id);
                if message == WM_CANCELMODE {
                    DestroyWindow(dialog as HWND);
                }
            }
            (api.next)(owner, message, wparam, lparam)
        }
    }
}
