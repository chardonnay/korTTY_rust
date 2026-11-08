//! Native image selection on the registered clipboard worker.

use std::io::{self, ErrorKind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::worker::{self, Command};

const MAX_PNG_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const PNG_MIME: &str = "image/png";
type Completion = Box<dyn FnOnce(io::Result<()>) + Send>;

/// Publishes a PNG for a focused application surface using the existing clipboard worker.
/// The completion reports submission of the selection to Wayland, which has no selection ack.
pub fn write_png(
    display: usize,
    surface: usize,
    png: Arc<[u8]>,
    complete: Box<dyn FnOnce(io::Result<()>) + Send>,
) {
    if !(33..=MAX_PNG_BYTES).contains(&png.len())
        || !png.starts_with(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR")
    {
        complete(Err(io::Error::new(ErrorKind::InvalidData, "invalid clipboard PNG")));
        return;
    }
    let Some(commands) = worker::commands(display) else {
        complete(Err(io::Error::other("Wayland clipboard is not available")));
        return;
    };
    if commands.png_pending.swap(true, Ordering::AcqRel) {
        complete(Err(io::Error::new(ErrorKind::WouldBlock, "Wayland image clipboard is busy")));
        return;
    }
    // A failed send drops the request, delivering an error and releasing the slot.
    let _ = commands.send(Command::StorePng(Request {
        surface,
        png,
        complete: Some(complete),
        pending: Arc::clone(&commands.png_pending),
    }));
}

pub(crate) struct Request {
    pub(crate) surface: usize,
    pub(crate) png: Arc<[u8]>,
    complete: Option<Completion>,
    pending: Arc<AtomicBool>,
}

impl Request {
    pub(crate) fn finish(mut self, result: io::Result<()>) {
        self.resolve(result);
    }

    fn resolve(&mut self, result: io::Result<()>) {
        if let Some(complete) = self.complete.take() {
            self.pending.store(false, Ordering::Release);
            complete(result);
        }
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        self.resolve(Err(io::Error::new(
            ErrorKind::BrokenPipe,
            "Wayland clipboard worker stopped",
        )));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use sctk::reexports::calloop::channel;

    use super::*;

    #[test]
    fn image_request_slot_is_released_when_the_worker_drops_its_queue() {
        const DISPLAY: usize = usize::MAX;
        // Test double for a registered worker that disconnects before accepting its request.
        let (send, receive) = channel::channel();
        let _registration = worker::Registration::new(DISPLAY, send).unwrap();
        let mut header = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        header.resize(33, 0);
        let png: Arc<[u8]> = Arc::from(header);
        let (first, first_result) = mpsc::channel();
        write_png(
            DISPLAY,
            1,
            Arc::clone(&png),
            Box::new(move |result| first.send(result).unwrap()),
        );
        assert!(matches!(first_result.try_recv(), Err(mpsc::TryRecvError::Empty)));

        let (second, second_result) = mpsc::channel();
        write_png(DISPLAY, 1, png, Box::new(move |result| second.send(result).unwrap()));
        assert_eq!(second_result.try_recv().unwrap().unwrap_err().kind(), ErrorKind::WouldBlock);

        drop(receive);
        assert_eq!(first_result.try_recv().unwrap().unwrap_err().kind(), ErrorKind::BrokenPipe);
        assert!(!worker::commands(DISPLAY).unwrap().png_pending.load(Ordering::Acquire));
    }
}
