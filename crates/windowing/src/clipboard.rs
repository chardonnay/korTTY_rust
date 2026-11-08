use std::io;
use std::sync::Arc;

use raw_window_handle::{DisplayHandle, RawDisplayHandle, RawWindowHandle, WindowHandle};

/// A validated journal PNG and the native formats prepared for its publication.
#[derive(Clone)]
pub struct ClipboardPng {
    #[cfg(target_os = "windows")]
    prepared: super::windows_clipboard::Png,
    #[cfg(not(target_os = "windows"))]
    png: Arc<[u8]>,
}

impl ClipboardPng {
    /// Prepares formats off the event thread before attempting to own the clipboard.
    pub fn prepare(png: Arc<[u8]>) -> io::Result<Self> {
        if !png.starts_with(b"\x89PNG\r\n\x1a\n") || png.len() > 16 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid clipboard PNG",
            ));
        }
        Ok(Self {
            #[cfg(target_os = "windows")]
            prepared: super::windows_clipboard::Png::prepare(png)?,
            #[cfg(not(target_os = "windows"))]
            png,
        })
    }

    /// Publishes from the native window event thread. Linux may acknowledge on its selection worker.
    pub fn write(
        self,
        window: WindowHandle<'_>,
        display: DisplayHandle<'_>,
        complete: impl FnOnce(io::Result<()>) + Send + 'static,
    ) {
        match (window.as_raw(), display.as_raw()) {
            #[cfg(target_os = "macos")]
            (RawWindowHandle::AppKit(_), RawDisplayHandle::AppKit(_)) => {
                complete(kortty_macos::write_clipboard_png(&self.png));
            }
            #[cfg(target_os = "windows")]
            (RawWindowHandle::Win32(window), RawDisplayHandle::Windows(_)) => {
                complete(super::windows_clipboard::write(window, self.prepared));
            }
            #[cfg(target_os = "linux")]
            (RawWindowHandle::Wayland(window), RawDisplayHandle::Wayland(display)) => {
                super::wayland_clipboard::write(window, display, self.png, Box::new(complete));
            }
            #[cfg(target_os = "linux")]
            (
                RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_),
                RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_),
            ) => {
                super::x11_clipboard::write(display, self.png, Box::new(complete));
            }
            _ => complete(Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "No native image clipboard for this window",
            ))),
        }
    }
}

/// Stops the optional in-process X11 owner after the desktop event loop has exited.
pub fn shutdown_clipboard() {
    #[cfg(target_os = "linux")]
    super::x11_clipboard::shutdown();
}
