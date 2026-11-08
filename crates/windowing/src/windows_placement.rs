use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow,
};
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows_sys::Win32::UI::Shell::GetScaleFactorForMonitor;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, MONITORINFOF_PRIMARY, USER_DEFAULT_SCREEN_DPI, WS_OVERLAPPEDWINDOW,
};
use windows_sys::core::BOOL;

use super::placement::{Area, DesktopPoint, DialogWorkArea, fit_position};

struct Monitor {
    id: HMONITOR,
    primary: bool,
    bounds: Area,
    work: Area,
}

pub(super) fn constrain(window: usize, saved: DesktopPoint) -> Option<DesktopPoint> {
    let mut bounds = RECT::default();
    // SAFETY: the HWND came from the native window. GetWindowRect reports
    // failure if that window has already closed and writes to a live local.
    if unsafe { GetWindowRect(window as HWND, &mut bounds) } == 0 {
        return None;
    }
    let width = u32::try_from(bounds.right.checked_sub(bounds.left)?).ok()?;
    let height = u32::try_from(bounds.bottom.checked_sub(bounds.top)?).ok()?;
    let areas: Vec<_> = monitors()?
        .into_iter()
        .map(|monitor| monitor.work)
        .collect();
    fit_position(saved, (width, height), &areas)
}

pub(super) fn dialog_work_area(
    window: usize,
    saved: Option<DesktopPoint>,
) -> Option<DialogWorkArea> {
    let window = window as HWND;
    // SAFETY: the HWND comes from the borrowed native window on this thread.
    let current = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    let monitors = monitors()?;
    let selected = match saved {
        Some(point) => monitors
            .iter()
            .find(|monitor| monitor.bounds.contains(point)),
        None => monitors.iter().find(|monitor| monitor.id == current),
    };
    let monitor = selected.or_else(|| monitors.first())?;

    // SAFETY: GetDpiForWindow returns zero if the HWND is no longer valid.
    let current_dpi = unsafe { GetDpiForWindow(window) };
    let dpi = if monitor.id == current && current_dpi != 0 {
        Some(current_dpi)
    } else {
        monitor_dpi(monitor.id)
    };
    let frame_dpi = dpi.unwrap_or(current_dpi.max(USER_DEFAULT_SCREEN_DPI));
    let scale_factor = dpi.map(|dpi| dpi as f32 / USER_DEFAULT_SCREEN_DPI as f32);
    monitor
        .work
        .dialog_work_area(dialog_frame(frame_dpi)?, scale_factor)
}

fn monitor_dpi(monitor: HMONITOR) -> Option<u32> {
    let mut percent = 0;
    // SAFETY: the monitor came from this query's synchronous enumeration and
    // the function writes one scale value into the local output parameter.
    if unsafe { GetScaleFactorForMonitor(monitor, &mut percent) } != 0 || percent <= 0 {
        return None;
    }
    let percent = u32::try_from(percent).ok()?;
    Some(
        USER_DEFAULT_SCREEN_DPI
            .checked_mul(percent)?
            .checked_add(50)?
            / 100,
    )
}

fn dialog_frame(dpi: u32) -> Option<(u32, u32)> {
    // The source window can be maximized, fullscreen or have custom chrome.
    // Compute the default decorated frame at the selected monitor's DPI.
    let mut frame = RECT {
        right: 1,
        bottom: 1,
        ..RECT::default()
    };
    // SAFETY: the API mutates only the supplied client rectangle.
    if unsafe { AdjustWindowRectExForDpi(&mut frame, WS_OVERLAPPEDWINDOW, 0, 0, dpi) } == 0 {
        return None;
    }
    Some((
        u32::try_from(frame.right.checked_sub(frame.left)?.checked_sub(1)?).ok()?,
        u32::try_from(frame.bottom.checked_sub(frame.top)?.checked_sub(1)?).ok()?,
    ))
}

fn monitors() -> Option<Vec<Monitor>> {
    let mut monitors = Vec::<Monitor>::new();
    // SAFETY: The enumeration and callbacks are synchronous. The pointer
    // stays valid and exclusive for the duration of the enumeration.
    if unsafe {
        EnumDisplayMonitors(
            null_mut(),
            std::ptr::null(),
            Some(collect),
            (&mut monitors as *mut Vec<Monitor>) as LPARAM,
        )
    } == 0
    {
        return None;
    }
    monitors.sort_by_key(|monitor| !monitor.primary);
    Some(monitors)
}

unsafe extern "system" fn collect(
    monitor: HMONITOR,
    _context: HDC,
    _bounds: *mut RECT,
    payload: LPARAM,
) -> BOOL {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..MONITORINFO::default()
    };
    // SAFETY: Windows supplies the monitor for this callback. The output is
    // initialized and has its documented size.
    if unsafe { GetMonitorInfoW(monitor, &mut info) } != 0 {
        let work = info.rcWork;
        if let Some(work) = Area::new(work.left, work.top, work.right, work.bottom) {
            let bounds = info.rcMonitor;
            let bounds =
                Area::new(bounds.left, bounds.top, bounds.right, bounds.bottom).unwrap_or(work);
            // SAFETY: monitors owns this vector for the synchronous callback.
            let monitors = unsafe { &mut *(payload as *mut Vec<Monitor>) };
            monitors.push(Monitor {
                id: monitor,
                primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
                bounds,
                work,
            });
        }
    }
    1
}
