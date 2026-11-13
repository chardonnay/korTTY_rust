//! A short-lived, owner-modal Windows credential prompt.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::mem::size_of;
use std::ptr::{null, null_mut};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use raw_window_handle::{RawWindowHandle, WindowHandle};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_BTNFACE, DEFAULT_GUI_FONT, GetMonitorInfoW, GetStockObject, GetSysColorBrush,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows_sys::Win32::UI::Input::Ime::ImmAssociateContextEx;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetKeyState, IsWindowEnabled, SetActiveWindow, SetFocus, VK_ESCAPE, VK_RETURN,
    VK_SHIFT, VK_TAB,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, CreateWindowExW, DefWindowProcW, DestroyWindow, ES_AUTOHSCROLL, ES_PASSWORD,
    GetForegroundWindow, GetWindowRect, IDC_ARROW, KillTimer, LoadCursorW, RegisterClassW, SW_SHOW,
    SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetTimer, SetWindowPos, SetWindowTextW, ShowWindow,
    WA_INACTIVE, WM_ACTIVATE, WM_CHAR, WM_CLEAR, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU, WM_COPY,
    WM_CUT, WM_DPICHANGED, WM_IME_CHAR, WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION,
    WM_IME_SETCONTEXT, WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_NCDESTROY, WM_PASTE, WM_SETFONT,
    WM_SETTEXT, WM_SYSCHAR, WM_TIMER, WM_UNDO, WM_UNICHAR, WNDCLASSW, WS_CAPTION, WS_CHILD,
    WS_EX_CLIENTEDGE, WS_EX_DLGMODALFRAME, WS_EX_TOOLWINDOW, WS_POPUP, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE,
};
use windows_sys::w;
use zeroize::{Zeroize, Zeroizing};

use super::{bind_modal_dialog_owner, on_current_thread, release_modal_dialog_owner, subclass};

const CLASS: *const u16 = w!("kortty.TransientPassword.1");
const WIDTH: i32 = 510;
const HEIGHT: i32 = 184;
const MAX_BYTES: usize = 1024;
const TIMER: usize = 1;
const SUBMIT: usize = 1;
const CANCEL: usize = 2;
const EDIT_SUBCLASS: usize = 1;
const EDIT_SELECT: u32 = 0x00b1;
const EDIT_REPLACE: u32 = 0x00c2;
const UNICODE_NOCHAR: usize = 0xffff;

type Permission = Box<dyn Fn() -> bool + Send>;
type Finished = Box<dyn FnOnce(Option<NativePassword>) + Send>;

/// A bounded password from one native prompt. Dropping it wipes its storage.
pub struct NativePassword {
    bytes: Box<Zeroizing<[u8; MAX_BYTES]>>,
    len: usize,
}

impl NativePassword {
    fn new() -> Self {
        Self {
            bytes: Box::new(Zeroizing::new([0; MAX_BYTES])),
            len: 0,
        }
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).unwrap_or_default()
    }

    fn push(&mut self, character: char) -> bool {
        let end = self.len + character.len_utf8();
        if character.is_control() || end > MAX_BYTES {
            return false;
        }
        character.encode_utf8(&mut self.bytes[self.len..end]);
        self.len = end;
        true
    }

    fn backspace(&mut self) {
        if let Some((offset, _)) = self.as_str().char_indices().next_back() {
            self.bytes[offset..self.len].zeroize();
            self.len = offset;
        }
    }
}

impl fmt::Debug for NativePassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Native password")
    }
}

struct Input {
    password: NativePassword,
    high: Zeroizing<u16>,
}

impl Input {
    fn new() -> Self {
        Self {
            password: NativePassword::new(),
            high: Zeroizing::new(0),
        }
    }

    fn unit(&mut self, unit: u16) {
        match unit {
            8 => {
                if *self.high == 0 {
                    self.password.backspace();
                }
                self.high.zeroize();
            }
            0xd800..=0xdbff => *self.high = unit,
            0xdc00..=0xdfff if *self.high != 0 => {
                let scalar =
                    0x1_0000 + ((u32::from(*self.high) - 0xd800) << 10) + u32::from(unit) - 0xdc00;
                self.scalar(scalar);
            }
            _ => self.scalar(u32::from(unit)),
        }
    }

    fn scalar(&mut self, scalar: u32) {
        self.high.zeroize();
        if let Some(character) = char::from_u32(scalar) {
            self.password.push(character);
        }
    }
}

#[derive(Clone, Copy)]
struct Controls {
    details: HWND,
    label: HWND,
    edit: HWND,
    submit: HWND,
    cancel: HWND,
}

struct Prompt {
    owner: HWND,
    controls: Controls,
    input: Input,
    permitted: Permission,
    finished: Finished,
    deadline: Instant,
    activated: bool,
}

thread_local! {
    static PROMPTS: RefCell<HashMap<HWND, Prompt>> = RefCell::default();
    static MASK_UPDATE: Cell<HWND> = const { Cell::new(null_mut()) };
}

/// Opens a native modal prompt on the window's UI thread without a nested event loop.
/// The callback receives a password only after an explicit submit in the foreground.
pub fn open_password_prompt(
    owner: WindowHandle<'_>,
    destination: &str,
    permitted: impl Fn() -> bool + Send + 'static,
    finished: impl FnOnce(Option<NativePassword>) + Send + 'static,
) -> bool {
    let RawWindowHandle::Win32(owner) = owner.as_raw() else {
        return false;
    };
    let owner = owner.hwnd.get() as HWND;
    // SAFETY: The borrowed HWND is valid for this call. All control creation and
    // destruction happens on the same thread as its native owner.
    unsafe {
        if !on_current_thread(owner)
            || IsWindowEnabled(owner) == 0
            || GetForegroundWindow() != owner
            || !permitted()
        {
            return false;
        }
        let instance = GetModuleHandleW(null());
        if instance.is_null() || !register(instance) {
            return false;
        }
        let dpi = GetDpiForWindow(owner).max(96);
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let ex = WS_EX_DLGMODALFRAME | WS_EX_TOOLWINDOW;
        let Some(bounds) = bounds(owner, dpi, style, ex) else {
            return false;
        };
        let window = CreateWindowExW(
            ex,
            CLASS,
            w!("Authorize SSH sudo password"),
            style,
            bounds.left,
            bounds.top,
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            owner,
            null_mut(),
            instance,
            null(),
        );
        if window.is_null() {
            return false;
        }
        let Some(controls) = create_controls(window, destination) else {
            DestroyWindow(window);
            return false;
        };
        if ImmAssociateContextEx(controls.edit, null_mut(), 0) == 0
            || !permitted()
            || IsWindowEnabled(owner) == 0
            || !bind_modal_dialog_owner(window, owner)
            || !on_current_thread(window)
            || SetTimer(window, TIMER, 40, None) == 0
        {
            DestroyWindow(window);
            return false;
        }
        PROMPTS.with(|prompts| {
            prompts.borrow_mut().insert(
                window,
                Prompt {
                    owner,
                    controls,
                    input: Input::new(),
                    permitted: Box::new(permitted),
                    finished: Box::new(finished),
                    deadline: Instant::now() + Duration::from_secs(60),
                    activated: false,
                },
            )
        });
        layout(controls, dpi);
        EnableWindow(controls.submit, 0);
        ShowWindow(window, SW_SHOW);
        SetActiveWindow(window);
        SetFocus(controls.edit);
        if GetForegroundWindow() != window {
            finish(window, false);
        }
        true
    }
}

unsafe fn register(instance: windows_sys::Win32::Foundation::HINSTANCE) -> bool {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    // SAFETY: The instance is the current application module. The procedure,
    // cursor and class name remain valid for the lifetime of the application.
    *REGISTERED.get_or_init(|| unsafe {
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(dialog_event),
            hInstance: instance,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            lpszClassName: CLASS,
            ..WNDCLASSW::default()
        }) != 0
    })
}

unsafe fn bounds(owner: HWND, dpi: u32, style: u32, ex: u32) -> Option<RECT> {
    // SAFETY: The owner is borrowed on its UI thread and output pointers refer
    // to local structures for each synchronous call.
    unsafe {
        let mut outer = RECT {
            left: 0,
            top: 0,
            right: scale(WIDTH, dpi),
            bottom: scale(HEIGHT, dpi),
        };
        let mut owner_bounds = RECT::default();
        let mut monitor = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..MONITORINFO::default()
        };
        if AdjustWindowRectExForDpi(&mut outer, style, 0, ex, dpi) == 0
            || GetWindowRect(owner, &mut owner_bounds) == 0
            || GetMonitorInfoW(
                MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            ) == 0
        {
            return None;
        }
        let width = outer.right - outer.left;
        let height = outer.bottom - outer.top;
        let work = monitor.rcWork;
        if width > work.right - work.left || height > work.bottom - work.top {
            return None;
        }
        let left = (owner_bounds.left + (owner_bounds.right - owner_bounds.left - width) / 2)
            .clamp(work.left, work.right - width);
        let top = (owner_bounds.top + (owner_bounds.bottom - owner_bounds.top - height) / 2)
            .clamp(work.top, work.bottom - height);
        Some(RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        })
    }
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi) + 48) / 96) as i32
}

unsafe fn create_controls(window: HWND, destination: &str) -> Option<Controls> {
    let details = format!(
        "Enter the sudo password for {}",
        destination.chars().take(220).collect::<String>()
    );
    let details = details.encode_utf16().chain([0]).collect::<Vec<_>>();
    let api = subclass::Api::get()?;
    // SAFETY: Each child is created on the prompt's thread. The system copies
    // the public labels synchronously. Password bytes are never control text.
    unsafe {
        let make = |class, label, style, ex, id| {
            CreateWindowExW(
                ex,
                class,
                label,
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                1,
                1,
                window,
                id as _,
                null_mut(),
                null(),
            )
        };
        let controls = Controls {
            details: make(w!("STATIC"), details.as_ptr(), 0, 0, 0usize),
            label: make(w!("STATIC"), w!("Password"), 0, 0, 0),
            edit: make(
                w!("EDIT"),
                w!(""),
                WS_TABSTOP | ES_AUTOHSCROLL as u32 | ES_PASSWORD as u32,
                WS_EX_CLIENTEDGE,
                0,
            ),
            submit: make(
                w!("BUTTON"),
                w!("Use password once"),
                WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
                0,
                SUBMIT,
            ),
            cancel: make(w!("BUTTON"), w!("Cancel"), WS_TABSTOP, 0, CANCEL),
        };
        let font = GetStockObject(DEFAULT_GUI_FONT) as WPARAM;
        for child in [
            controls.details,
            controls.label,
            controls.edit,
            controls.submit,
            controls.cancel,
        ] {
            if child.is_null() {
                return None;
            }
            SendMessageW(child, WM_SETFONT, font, 1);
        }
        for child in [controls.edit, controls.submit, controls.cancel] {
            let id = if child == controls.edit {
                EDIT_SUBCLASS
            } else {
                0
            };
            if (api.install)(child, Some(control_event), id, window as usize) == 0 {
                return None;
            }
        }
        Some(controls)
    }
}

unsafe fn layout(controls: Controls, dpi: u32) {
    // SAFETY: All controls belong to the live prompt on this UI thread.
    unsafe {
        for (control, x, y, width, height) in [
            (controls.details, 14, 14, WIDTH - 28, 61),
            (controls.label, 14, 80, WIDTH - 28, 20),
            (controls.edit, 14, 102, WIDTH - 28, 26),
            (controls.submit, WIDTH - 252, 143, 138, 28),
            (controls.cancel, WIDTH - 104, 143, 90, 28),
        ] {
            SetWindowPos(
                control,
                null_mut(),
                scale(x, dpi),
                scale(y, dpi),
                scale(width, dpi),
                scale(height, dpi),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

fn finish(window: HWND, submitted: bool) {
    if submitted
        && PROMPTS.with(|prompts| {
            prompts
                .borrow()
                .get(&window)
                .is_some_and(|prompt| prompt.input.password.len == 0 || *prompt.input.high != 0)
        })
    {
        return;
    }
    let Some(prompt) = PROMPTS.with(|prompts| prompts.borrow_mut().remove(&window)) else {
        return;
    };
    // SAFETY: Called by this prompt's UI thread after extracting state, so
    // reentrant close/activation messages cannot submit a second result.
    let active = unsafe { GetForegroundWindow() == window };
    let password = (submitted
        && active
        && (prompt.permitted)()
        && Instant::now() < prompt.deadline
        && prompt.input.password.len != 0
        && *prompt.input.high == 0)
        .then_some(prompt.input.password);
    unsafe {
        KillTimer(window, TIMER);
        release_modal_dialog_owner(window);
        DestroyWindow(window);
        if active && on_current_thread(prompt.owner) && IsWindowEnabled(prompt.owner) != 0 {
            SetActiveWindow(prompt.owner);
            SetFocus(prompt.owner);
        }
    }
    (prompt.finished)(password);
}

unsafe extern "system" fn dialog_event(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLOSE => {
            finish(window, false);
            return 0;
        }
        WM_ACTIVATE => {
            let inactive = (wparam & 0xffff) as u32 == WA_INACTIVE;
            let lost = PROMPTS.with(|prompts| {
                let mut prompts = prompts.borrow_mut();
                prompts.get_mut(&window).is_some_and(|prompt| {
                    if inactive {
                        prompt.activated
                    } else {
                        prompt.activated = true;
                        false
                    }
                })
            });
            if lost {
                finish(window, false);
                return 0;
            }
        }
        WM_TIMER if wparam == TIMER => {
            let expired = PROMPTS.with(|prompts| {
                prompts.borrow().get(&window).is_some_and(|prompt| {
                    Instant::now() >= prompt.deadline || !(prompt.permitted)()
                })
            });
            if expired {
                finish(window, false);
            }
            return 0;
        }
        WM_COMMAND if wparam >> 16 == 0 => {
            let controls =
                PROMPTS.with(|prompts| prompts.borrow().get(&window).map(|prompt| prompt.controls));
            if let Some(controls) = controls {
                match (wparam & 0xffff, lparam as HWND) {
                    (SUBMIT, child) if child == controls.submit => finish(window, true),
                    (CANCEL, child) if child == controls.cancel => finish(window, false),
                    _ => {}
                }
            }
            return 0;
        }
        WM_DPICHANGED if lparam != 0 => {
            let controls =
                PROMPTS.with(|prompts| prompts.borrow().get(&window).map(|prompt| prompt.controls));
            if let Some(controls) = controls {
                // SAFETY: WM_DPICHANGED supplies a RECT valid for this callback.
                unsafe {
                    let rect = &*(lparam as *const RECT);
                    SetWindowPos(
                        window,
                        null_mut(),
                        rect.left,
                        rect.top,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                    layout(controls, (wparam & 0xffff) as u32);
                }
            }
            return 0;
        }
        WM_NCDESTROY => {
            let prompt = PROMPTS.with(|prompts| prompts.borrow_mut().remove(&window));
            if let Some(prompt) = prompt {
                (prompt.finished)(None);
            }
        }
        _ => {}
    }
    // SAFETY: Unhandled messages go to the live class's default procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

unsafe extern "system" fn control_event(
    child: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    window: usize,
) -> LRESULT {
    let window = window as HWND;
    let controls =
        PROMPTS.with(|prompts| prompts.borrow().get(&window).map(|prompt| prompt.controls));
    if let Some(controls) = controls
        && message == WM_KEYDOWN
    {
        match wparam as u16 {
            VK_ESCAPE => {
                finish(window, false);
                return 0;
            }
            VK_RETURN => {
                finish(window, child != controls.cancel);
                return 0;
            }
            VK_TAB => {
                // SAFETY: Focus remains inside the currently active dialog.
                unsafe {
                    let backward = GetKeyState(i32::from(VK_SHIFT)) < 0;
                    let submit = IsWindowEnabled(controls.submit) != 0;
                    let next = if child == controls.edit {
                        if backward || !submit {
                            controls.cancel
                        } else {
                            controls.submit
                        }
                    } else if child == controls.submit {
                        if backward {
                            controls.edit
                        } else {
                            controls.cancel
                        }
                    } else if backward && submit {
                        controls.submit
                    } else {
                        controls.edit
                    };
                    SetFocus(next);
                }
                return 0;
            }
            _ => {}
        }
    }
    if id == EDIT_SUBCLASS {
        // Reject plaintext edits even during creation or reentrant destruction,
        // when the prompt may have no entry in the thread-local state.
        match message {
            WM_CHAR | WM_UNICHAR => {
                if message == WM_UNICHAR && wparam == UNICODE_NOCHAR {
                    return 1;
                }
                if let Some(controls) = controls {
                    update_mask(window, controls, message, wparam);
                }
                return 0;
            }
            WM_SETTEXT if MASK_UPDATE.with(|active| active.get() != child) => return 0,
            WM_PASTE
            | WM_COPY
            | WM_CUT
            | WM_CLEAR
            | WM_UNDO
            | WM_CONTEXTMENU
            | WM_IME_STARTCOMPOSITION
            | WM_IME_COMPOSITION
            | WM_IME_ENDCOMPOSITION
            | WM_IME_CHAR
            | WM_IME_SETCONTEXT
            | WM_SYSCHAR
            | EDIT_REPLACE => return 0,
            _ => {}
        }
    }
    // SAFETY: The subclass is installed on this control's thread. Remove it
    // before passing WM_NCDESTROY to the next procedure.
    unsafe {
        let Some(api) = subclass::Api::get() else {
            return DefWindowProcW(child, message, wparam, lparam);
        };
        if message == WM_NCDESTROY {
            (api.remove)(child, Some(control_event), id);
        }
        (api.next)(child, message, wparam, lparam)
    }
}

fn update_mask(window: HWND, controls: Controls, message: u32, wparam: WPARAM) {
    let count = PROMPTS.with(|prompts| {
        let mut prompts = prompts.borrow_mut();
        let prompt = prompts.get_mut(&window)?;
        if !(prompt.permitted)() || Instant::now() >= prompt.deadline {
            return None;
        }
        if message == WM_CHAR {
            if let Ok(unit) = u16::try_from(wparam) {
                prompt.input.unit(unit);
            }
        } else if let Ok(scalar) = u32::try_from(wparam) {
            prompt.input.scalar(scalar);
        }
        Some((
            prompt.input.password.as_str().chars().count(),
            *prompt.input.high == 0,
        ))
    });
    let Some((count, complete)) = count else {
        finish(window, false);
        return;
    };
    let mut mask = [u16::from(b'*'); MAX_BYTES + 1];
    mask[count] = 0;
    // SAFETY: All children belong to this prompt. Only the mask is sent to EDIT.
    unsafe {
        MASK_UPDATE.with(|active| {
            let previous = active.replace(controls.edit);
            SetWindowTextW(controls.edit, mask.as_ptr());
            active.set(previous);
        });
        SendMessageW(controls.edit, EDIT_SELECT, count, count as LPARAM);
        EnableWindow(controls.submit, i32::from(count != 0 && complete));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowTextW;

    use super::*;

    #[test]
    fn native_password_buffer_handles_surrogates_limits_and_wiping_backspace() {
        let mut input = Input::new();
        input.unit(0xd83d);
        assert!(input.password.as_str().is_empty());
        input.unit(0xde42);
        assert_eq!(input.password.as_str(), "🙂");
        input.unit(8);
        assert_eq!(&input.password.bytes[..4], &[0; 4]);
        input.unit(0xdc00);
        input.unit(0);
        input.unit(13);
        assert!(input.password.as_str().is_empty());
        for _ in 0..MAX_BYTES - 1 {
            input.unit(u16::from(b'a'));
        }
        input.scalar('ä' as u32);
        assert_eq!(input.password.len, MAX_BYTES - 1);
        input.unit(u16::from(b'b'));
        input.unit(u16::from(b'c'));
        assert_eq!(input.password.len, MAX_BYTES);
        assert_eq!(format!("{:?}", input.password), "Native password");
    }

    #[test]
    fn native_password_edit_stores_only_mask_and_restores_its_owner() {
        struct Window(HWND);
        impl Drop for Window {
            fn drop(&mut self) {
                // SAFETY: The test owns each hidden window on this thread.
                unsafe { DestroyWindow(self.0) };
            }
        }

        // SAFETY: All native test windows and child controls stay on this
        // thread. Only synthetic characters are sent, with no clipboard access.
        unsafe {
            let instance = GetModuleHandleW(null());
            assert!(register(instance));
            let owner_window = || {
                Window(CreateWindowExW(
                    0,
                    w!("STATIC"),
                    w!("fixture"),
                    WS_POPUP,
                    0,
                    0,
                    600,
                    240,
                    null_mut(),
                    null_mut(),
                    instance,
                    null(),
                ))
            };
            let owner = owner_window();
            let other = owner_window();
            let prompt = Window(CreateWindowExW(
                WS_EX_TOOLWINDOW,
                CLASS,
                w!("fixture"),
                WS_POPUP,
                0,
                0,
                510,
                184,
                owner.0,
                null_mut(),
                instance,
                null(),
            ));
            assert!(!owner.0.is_null() && !other.0.is_null() && !prompt.0.is_null());
            let controls = create_controls(prompt.0, "fixture account").unwrap();
            assert!(bind_modal_dialog_owner(prompt.0, owner.0));
            assert_eq!(IsWindowEnabled(owner.0), 0);
            assert_ne!(IsWindowEnabled(other.0), 0);
            let (send, received) = mpsc::channel();
            PROMPTS.with(|prompts| {
                prompts.borrow_mut().insert(
                    prompt.0,
                    Prompt {
                        owner: owner.0,
                        controls,
                        input: Input::new(),
                        permitted: Box::new(|| true),
                        finished: Box::new(move |secret| {
                            let _ = send.send(secret);
                        }),
                        deadline: Instant::now() + Duration::from_secs(10),
                        activated: false,
                    },
                )
            });
            SendMessageW(controls.edit, WM_CHAR, usize::from(b'x'), 0);
            SetWindowTextW(controls.edit, w!("forbidden fixture"));
            let mut mask = [0u16; 16];
            assert_eq!(
                GetWindowTextW(controls.edit, mask.as_mut_ptr(), mask.len() as i32),
                1
            );
            assert_eq!(mask[0], u16::from(b'*'));
            assert!(
                PROMPTS.with(|prompts| prompts.borrow()[&prompt.0].input.password.as_str() == "x")
            );
            drop(prompt);
            assert!(received.try_recv().unwrap().is_none());
            assert_ne!(IsWindowEnabled(owner.0), 0);
        }
    }
}
