//! In-process drag requests on the clipboard worker's existing data device.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use crate::worker::{self, Command, Commands};
type Progress = Arc<Mutex<DragProgress>>;

/// An opaque client surface identity with its compositor-provided local position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragTarget {
    pub surface: usize,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DragProgress {
    Pending,
    Starting,
    Hover(Option<DragTarget>),
    Finished(Option<DragTarget>),
    Unavailable,
}

/// Connects an in-process drag request to the clipboard's live Wayland worker.
pub fn start(
    display: usize,
    origin: DragTarget,
    destinations: Vec<usize>,
    token: [u8; 16],
) -> Option<DragOperation> {
    let commands = worker::commands(display)?;
    let mut mime = String::from("application/x-kortty-tab-");
    for byte in token {
        let _ = write!(mime, "{byte:02x}");
    }
    let progress = Arc::new(Mutex::new(DragProgress::Pending));
    commands
        .send(Command::StartDrag(Request {
            origin,
            destinations,
            mime,
            progress: progress.clone(),
        }))
        .ok()?;
    Some(DragOperation { commands, progress })
}

pub struct DragOperation {
    commands: Arc<Commands>,
    progress: Progress,
}

impl DragOperation {
    pub fn progress(&self) -> DragProgress {
        self.progress.lock().map_or(DragProgress::Finished(None), |progress| *progress)
    }
}

impl Drop for DragOperation {
    fn drop(&mut self) {
        let Ok(mut progress) = self.progress.lock() else { return };
        if progress.is_terminal() {
            return;
        };
        *progress = DragProgress::Finished(None);
        drop(progress);
        let _ = self.commands.send(Command::CancelDrag(self.progress.clone()));
    }
}

pub(crate) struct Request {
    pub(crate) origin: DragTarget,
    pub(crate) destinations: Vec<usize>,
    pub(crate) mime: String,
    progress: Progress,
}

impl Request {
    pub(crate) fn pending(&self) -> bool {
        self.progress.lock().is_ok_and(|progress| *progress == DragProgress::Pending)
    }

    pub(crate) fn owns(&self, progress: &Progress) -> bool {
        Arc::ptr_eq(&self.progress, progress)
    }

    pub(crate) fn live(&self) -> bool {
        self.progress.lock().is_ok_and(|progress| !progress.is_terminal())
    }

    pub(crate) fn begin(&self) -> bool {
        let Ok(mut progress) = self.progress.lock() else { return false };
        if *progress != DragProgress::Pending {
            return false;
        }
        *progress = DragProgress::Starting;
        true
    }

    pub(crate) fn acknowledge(&self) {
        let Ok(mut progress) = self.progress.lock() else { return };
        if *progress == DragProgress::Starting {
            *progress = DragProgress::Hover(None);
        }
    }

    pub(crate) fn publish(&self, event: DragProgress) -> bool {
        let Ok(mut progress) = self.progress.lock() else { return false };
        if progress.is_terminal() {
            return false;
        }
        *progress = event;
        true
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        let Ok(mut progress) = self.progress.lock() else { return };
        if !progress.is_terminal() {
            *progress = if *progress == DragProgress::Pending {
                DragProgress::Unavailable
            } else {
                DragProgress::Finished(None)
            };
        }
    }
}

impl DragProgress {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Finished(_) | Self::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_request() -> (Request, Progress) {
        let progress = Arc::new(Mutex::new(DragProgress::Pending));
        (
            Request {
                origin: DragTarget { surface: 1, x: 4.0, y: 5.0 },
                destinations: vec![1, 2],
                mime: "application/x-kortty-tab-test".into(),
                progress: progress.clone(),
            },
            progress,
        )
    }

    #[test]
    fn pending_request_remains_distinct_from_started_drag_cancellation() {
        let (request, progress) = pending_request();
        drop(request);
        assert_eq!(*progress.lock().unwrap(), DragProgress::Unavailable);

        let (request, progress) = pending_request();
        assert!(request.begin());
        assert!(!request.begin());
        drop(request);
        assert_eq!(*progress.lock().unwrap(), DragProgress::Finished(None));
    }

    #[test]
    fn a_finished_drag_cannot_publish_a_stale_hover_or_second_drop() {
        let (request, progress) = pending_request();
        let destination = DragTarget { surface: 2, x: 7.0, y: 11.0 };
        assert!(request.begin());
        request.publish(DragProgress::Finished(Some(destination)));
        request.publish(DragProgress::Hover(None));
        request.publish(DragProgress::Finished(None));
        assert!(!request.live());
        drop(request);
        assert_eq!(*progress.lock().unwrap(), DragProgress::Finished(Some(destination)));
    }
}
