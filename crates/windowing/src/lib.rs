//! Native window placement and hit testing. Window identifiers are opaque to the app.
#![deny(unsafe_op_in_unsafe_fn)]

use raw_window_handle::{DisplayHandle, RawDisplayHandle, RawWindowHandle, WindowHandle};

#[cfg(target_os = "linux")]
mod bell;
mod clipboard;
#[cfg(any(target_os = "windows", target_os = "linux", test))]
mod placement;
mod power;
#[cfg(target_os = "linux")]
mod wayland_clipboard;
#[cfg(target_os = "linux")]
pub mod wayland_drag;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(all(target_os = "windows", target_endian = "little"))]
mod windows_alpha;
#[cfg(target_os = "windows")]
mod windows_clipboard;
#[cfg(target_os = "windows")]
mod windows_placement;
#[cfg(target_os = "linux")]
mod x11;
#[cfg(target_os = "linux")]
mod x11_clipboard;
#[cfg(target_os = "linux")]
mod x11_mode;
#[cfg(target_os = "linux")]
mod x11_placement;

mod uri;

pub use clipboard::{ClipboardPng, shutdown_clipboard};
#[cfg(target_os = "macos")]
pub use kortty_macos::TabPress;
#[cfg(any(target_os = "windows", target_os = "linux", test))]
pub use placement::{DesktopPoint, DialogWorkArea};
pub use power::{SystemSleepRequest, supports_system_sleep_prevention};
pub use uri::{open_desktop_directory, open_desktop_uri};

/// Requests the native alert for the window's display, respecting system settings.
pub fn request_system_bell(display: DisplayHandle<'_>) {
    match display.as_raw() {
        #[cfg(target_os = "macos")]
        RawDisplayHandle::AppKit(_) => kortty_macos::system_bell(),
        #[cfg(target_os = "windows")]
        RawDisplayHandle::Windows(_) => {
            use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
            use windows_sys::Win32::UI::WindowsAndMessaging::MB_OK;

            // SAFETY: MB_OK selects the user's default sound. The OS queues it asynchronously.
            unsafe { MessageBeep(MB_OK) };
        }
        #[cfg(target_os = "linux")]
        RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_) => bell::request(bell::Display::X11),
        #[cfg(target_os = "linux")]
        RawDisplayHandle::Wayland(_) => bell::request(bell::Display::Wayland),
        _ => {}
    }
}

/// Identity of a client window on a backend that allows exact hit testing.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NativeWindow(WindowId);

/// The available path for a premultiplied ARGB client buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaSurface {
    Native,
    #[cfg(target_os = "linux")]
    X11(NativeWindow),
}

/// Prepares a native client surface for premultiplied alpha, or identifies the
/// X11 surface so its visual and compositor can be checked separately.
pub fn prepare_alpha_surface(
    window: WindowHandle<'_>,
    display: DisplayHandle<'_>,
) -> Option<AlphaSurface> {
    match (window.as_raw(), display.as_raw()) {
        #[cfg(all(target_os = "macos", target_endian = "little"))]
        (RawWindowHandle::AppKit(_), RawDisplayHandle::AppKit(_)) => Some(AlphaSurface::Native),
        #[cfg(all(target_os = "windows", target_endian = "little"))]
        (RawWindowHandle::Win32(window), RawDisplayHandle::Windows(_)) => {
            windows_alpha::enable(window).then_some(AlphaSurface::Native)
        }
        #[cfg(all(target_os = "linux", target_endian = "little"))]
        (RawWindowHandle::Wayland(_), RawDisplayHandle::Wayland(_)) => Some(AlphaSurface::Native),
        #[cfg(all(target_os = "linux", target_endian = "little"))]
        (RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_), _) => {
            identify(window, display).map(AlphaSurface::X11)
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum WindowId {
    #[cfg(target_os = "macos")]
    Mac(isize),
    #[cfg(target_os = "windows")]
    Windows(usize),
    #[cfg(target_os = "linux")]
    X11 { window: u32, screen: usize },
}

/// Client-relative position. Pixels may be physical or logical, but use the
/// same units for the horizontal position and client width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub window: NativeWindow,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub logical_coordinates: bool,
}

/// Current primary mouse-button state and an app-owned destination, if any.
#[cfg(not(target_os = "macos"))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub held: bool,
    pub hit: Option<Hit>,
}

/// Obtains a backend identifier while a native window is borrowed.
pub fn identify(window: WindowHandle<'_>, display: DisplayHandle<'_>) -> Option<NativeWindow> {
    match (window.as_raw(), display.as_raw()) {
        #[cfg(target_os = "macos")]
        (RawWindowHandle::AppKit(_), RawDisplayHandle::AppKit(_)) => {
            kortty_macos::tab_window_number(window).map(|id| NativeWindow(WindowId::Mac(id)))
        }
        #[cfg(target_os = "windows")]
        (RawWindowHandle::Win32(window), RawDisplayHandle::Windows(_)) => {
            Some(NativeWindow(WindowId::Windows(window.hwnd.get() as usize)))
        }
        #[cfg(target_os = "linux")]
        (RawWindowHandle::Xlib(window), RawDisplayHandle::Xlib(display)) => {
            let screen = display.screen.try_into().ok()?;
            let window = window.window.try_into().ok()?;
            Some(NativeWindow(WindowId::X11 { window, screen }))
        }
        #[cfg(target_os = "linux")]
        (RawWindowHandle::Xcb(window), RawDisplayHandle::Xcb(display)) => {
            let screen = display.screen.try_into().ok()?;
            let window = window.window.get();
            Some(NativeWindow(WindowId::X11 { window, screen }))
        }
        _ => None,
    }
}

/// Samples the primary pointer. On X11 this performs an X server request and
/// belongs off the UI thread.
#[cfg(not(target_os = "macos"))]
pub fn sample(source: NativeWindow, windows: &[NativeWindow]) -> Option<Sample> {
    match source.0 {
        #[cfg(target_os = "windows")]
        WindowId::Windows(_) => windows::sample(windows),
        #[cfg(target_os = "linux")]
        WindowId::X11 { screen, .. } => x11::sample(screen, windows),
    }
}

/// Progress from an AppKit primary-button sequence. A release is an actual
/// mouse-up event with the destination resolved when AppKit dispatched it.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MacTabSample {
    pub held: bool,
    pub moved: bool,
    pub hit: Option<Hit>,
}

#[cfg(target_os = "macos")]
impl NativeWindow {
    /// Registers a main window with the main-thread AppKit local event monitor.
    pub fn track_tab_pointer(self) -> bool {
        let WindowId::Mac(number) = self.0;
        kortty_macos::track_tab_window(number)
    }

    /// Unregisters a closed main window from the main-thread AppKit monitor.
    pub fn untrack_tab_pointer(self) {
        let WindowId::Mac(number) = self.0;
        kortty_macos::untrack_tab_window(number);
    }

    /// Reads the delivered mouse-down for native tab hit testing.
    pub fn pending_tab_press(self) -> Option<TabPress> {
        let WindowId::Mac(number) = self.0;
        kortty_macos::pending_tab_press(number)
    }

    /// Claims the same mouse-down used for native tab hit testing.
    pub fn claim_tab_press(self, press: TabPress) -> bool {
        let WindowId::Mac(number) = self.0;
        kortty_macos::claim_tab_press(number, press)
    }

    /// Reads AppKit's stamped gesture state from the main thread.
    pub fn sample_tab_press(self, press: TabPress, threshold: f64) -> Option<MacTabSample> {
        let WindowId::Mac(number) = self.0;
        let sample = kortty_macos::sample_tab_pointer(number, press, threshold)?;
        Some(MacTabSample {
            held: sample.held,
            moved: sample.moved,
            hit: sample.hit.map(|hit| Hit {
                window: NativeWindow(WindowId::Mac(hit.window)),
                x: hit.x,
                y: hit.y,
                width: hit.width,
                logical_coordinates: true,
            }),
        })
    }
}

/// Fits a saved virtual desktop position to a current native work area.
/// X11 performs server I/O and must be called off the application thread.
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub fn constrain_position(window: NativeWindow, saved: DesktopPoint) -> Option<DesktopPoint> {
    match window.0 {
        #[cfg(target_os = "windows")]
        WindowId::Windows(window) => windows_placement::constrain(window, saved),
        #[cfg(target_os = "linux")]
        WindowId::X11 { window, screen } => x11_placement::constrain(window, screen, saved),
    }
}

/// Returns the maximum decorated-dialog client size on the chosen monitor in physical pixels.
/// A saved position selects its monitor, falling back to the primary monitor if it is gone.
/// With no saved position, the owner window selects the monitor. Call on the UI thread on
/// Windows. On X11 this performs server I/O and belongs off the application thread.
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub fn dialog_work_area(
    window: NativeWindow,
    saved: Option<DesktopPoint>,
) -> Option<DialogWorkArea> {
    match window.0 {
        #[cfg(target_os = "windows")]
        WindowId::Windows(window) => windows_placement::dialog_work_area(window, saved),
        #[cfg(target_os = "linux")]
        WindowId::X11 { window, screen } => x11_placement::dialog_work_area(window, screen, saved),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowMode {
    pub normal_geometry: bool,
    pub windowed: bool,
    pub fullscreen: bool,
    pub maximized: Option<bool>,
    pub titlebar_inset: u16,
}

/// Reads the X11 window manager's presentation state.
/// This performs X server I/O and belongs off the application thread.
#[cfg(target_os = "linux")]
pub fn x11_window_mode(window: NativeWindow) -> WindowMode {
    let WindowId::X11 { window, screen } = window.0;
    x11_mode::window_mode(window, screen)
}

/// Checks that the live X11 window has a matching ARGB visual and a compositor.
/// This performs X server I/O and belongs off the application thread.
#[cfg(target_os = "linux")]
pub fn x11_has_alpha_surface(window: NativeWindow) -> bool {
    let WindowId::X11 { window, screen } = window.0;
    x11_mode::has_alpha_surface(window, screen)
}

/// Asks the X11 window manager to toggle fullscreen for a live client window.
/// This performs X server I/O and belongs off the application thread.
#[cfg(target_os = "linux")]
pub fn request_x11_fullscreen_toggle(window: NativeWindow) {
    let WindowId::X11 { window, screen } = window.0;
    x11_mode::request_fullscreen_toggle(window, screen);
}

/// Asks the X11 window manager to enter or leave fullscreen explicitly.
/// This performs X server I/O and belongs off the application thread.
#[cfg(target_os = "linux")]
pub fn request_x11_fullscreen(window: NativeWindow, fullscreen: bool) {
    let WindowId::X11 { window, screen } = window.0;
    x11_mode::request_fullscreen(window, screen, fullscreen);
}
