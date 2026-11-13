//! App-local tab drag using the compositor and the existing Wayland data device.

use raw_window_handle::{DisplayHandle, RawDisplayHandle, RawWindowHandle, WindowHandle};
use smithay_clipboard::drag::{self, DragOperation, DragProgress, DragTarget};

/// Opaque identities only. The clipboard worker owns the corresponding Wayland
/// proxies and obtains the live surface and implicit grab from its pointer event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Surface {
    pub(super) display: usize,
    pub(super) surface: usize,
}

pub fn identify(window: WindowHandle<'_>, display: DisplayHandle<'_>) -> Option<Surface> {
    let (RawWindowHandle::Wayland(window), RawDisplayHandle::Wayland(display)) =
        (window.as_raw(), display.as_raw())
    else {
        return None;
    };
    Some(Surface {
        display: display.display.as_ptr() as usize,
        surface: window.surface.as_ptr() as usize,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    pub surface: Surface,
    pub x: f64,
    pub y: f64,
}

pub enum Progress {
    Pending,
    Starting,
    Hover(Option<Target>),
    Finished(Option<Target>),
    Unavailable,
}

pub struct Operation {
    display: usize,
    operation: DragOperation,
}

impl Operation {
    pub fn start(
        origin: Target,
        destinations: impl Iterator<Item = Surface>,
        token: [u8; 16],
    ) -> Option<Self> {
        let display = origin.surface.display;
        let destinations = destinations
            .filter(|destination| destination.display == display)
            .map(|destination| destination.surface)
            .collect();
        let operation = drag::start(
            display,
            DragTarget {
                surface: origin.surface.surface,
                x: origin.x,
                y: origin.y,
            },
            destinations,
            token,
        )?;
        Some(Self { display, operation })
    }

    pub fn progress(&self) -> Progress {
        let target = |target: DragTarget| Target {
            surface: Surface {
                display: self.display,
                surface: target.surface,
            },
            x: target.x,
            y: target.y,
        };
        match self.operation.progress() {
            DragProgress::Pending => Progress::Pending,
            DragProgress::Starting => Progress::Starting,
            DragProgress::Hover(hit) => Progress::Hover(hit.map(target)),
            DragProgress::Finished(hit) => Progress::Finished(hit.map(target)),
            DragProgress::Unavailable => Progress::Unavailable,
        }
    }
}
