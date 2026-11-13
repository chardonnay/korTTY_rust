//! Copy-only desktop file drags started from an observed pointer press.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::drag::DragTarget;
use crate::worker::{self, Command, Commands};

const MAX_PAYLOAD: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Copied,
    Cancelled,
    Unavailable,
}

pub fn start(
    display: usize,
    origin: DragTarget,
    payload: Arc<[u8]>,
    marker: String,
    notify: impl FnOnce(Outcome) + Send + 'static,
) -> Option<Operation> {
    if payload.is_empty() || payload.len() > MAX_PAYLOAD {
        return None;
    }
    let commands = worker::commands(display)?;
    let live = Arc::new(AtomicBool::new(true));
    commands
        .send(Command::StartFileDrag(Request {
            origin,
            payload,
            marker,
            live: Arc::clone(&live),
            notify: Some(Box::new(notify)),
        }))
        .ok()?;
    Some(Operation { commands, live })
}

pub struct Operation {
    commands: Arc<Commands>,
    live: Arc<AtomicBool>,
}

impl Drop for Operation {
    fn drop(&mut self) {
        if self.live.swap(false, Ordering::AcqRel) {
            let _ = self
                .commands
                .send(Command::CancelFileDrag(Arc::clone(&self.live)));
        }
    }
}

impl Operation {
    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }
}

pub(crate) struct Request {
    pub(crate) origin: DragTarget,
    pub(crate) payload: Arc<[u8]>,
    pub(crate) marker: String,
    live: Arc<AtomicBool>,
    notify: Option<Box<dyn FnOnce(Outcome) + Send>>,
}

impl Request {
    pub(crate) fn live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }

    pub(crate) fn owns(&self, live: &Arc<AtomicBool>) -> bool {
        Arc::ptr_eq(live, &self.live)
    }

    pub(crate) fn finish(&mut self, outcome: Outcome) {
        let live = self.live.swap(false, Ordering::AcqRel);
        if let Some(notify) = self.notify.take().filter(|_| live) {
            notify(outcome);
        }
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        self.finish(Outcome::Cancelled);
    }
}
