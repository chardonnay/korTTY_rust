use std::io;
use std::sync::Arc;

use raw_window_handle::{WaylandDisplayHandle, WaylandWindowHandle};

pub(super) fn write(
    window: WaylandWindowHandle,
    display: WaylandDisplayHandle,
    png: Arc<[u8]>,
    complete: Box<dyn FnOnce(io::Result<()>) + Send>,
) {
    smithay_clipboard::selection::write_png(
        display.display.as_ptr() as usize,
        window.surface.as_ptr() as usize,
        png,
        complete,
    );
}
