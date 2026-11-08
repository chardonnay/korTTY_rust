use windows_sys::Win32::Foundation::{POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_RBUTTON;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GetAncestor, GetClientRect, GetCursorPos, GetSystemMetrics, SM_SWAPBUTTON,
    WindowFromPoint,
};

use super::{Hit, NativeWindow, Sample, WindowId};

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
