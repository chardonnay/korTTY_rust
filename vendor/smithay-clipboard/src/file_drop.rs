//! Incoming file drops on the registered Wayland clipboard worker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::worker::{self, Command, Commands};

pub(crate) const URI_LIST: &str = "text/uri-list";

/// Coordinates relative to the native Wayland surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    pub surface: usize,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug)]
pub enum Event {
    Hover { target: Target },
    Leave,
    Dropped { id: u64, target: Target, permit: Permit },
    Received { id: u64, data: Vec<u8> },
    Cancelled { id: u64 },
}

/// Watch one surface for file drops. The callback runs on the clipboard worker
/// and must return promptly. Dropping the watch cancels its offers and reads.
pub fn watch(
    display: usize,
    surface: usize,
    notify: impl Fn(Event) + Send + Sync + 'static,
) -> Option<Watch> {
    if surface == 0 {
        return None;
    }
    let commands = worker::commands(display)?;
    let registration =
        Arc::new(Registration { surface, live: Mutex::new(true), notify: Box::new(notify) });
    commands.send(Command::WatchFileDrops(Arc::clone(&registration))).ok()?;
    Some(Watch { commands, registration })
}

pub struct Watch {
    commands: Arc<Commands>,
    registration: Arc<Registration>,
}

/// Proof that an exact offer is still awaiting the application's admission.
#[derive(Clone)]
pub struct Permit {
    registration: Arc<Registration>,
    valid: Arc<AtomicBool>,
    deadline: Instant,
}

impl std::fmt::Debug for Permit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Wayland file offer permit")
    }
}

impl Permit {
    pub(crate) fn new(registration: Arc<Registration>, deadline: Instant) -> Self {
        Self { registration, valid: Arc::new(AtomicBool::new(true)), deadline }
    }

    pub fn is_live(&self) -> bool {
        self.valid.load(Ordering::Acquire)
            && self.registration.live()
            && Instant::now() < self.deadline
    }

    pub(crate) fn invalidate(&self) {
        self.valid.store(false, Ordering::Release);
    }
}

impl Watch {
    /// Withdraw this registration and cancel any outstanding offers.
    pub fn close(&self) {
        if self.registration.revoke() {
            let _ = self.commands.send(Command::UnwatchFileDrops(Arc::clone(&self.registration)));
        }
    }

    /// Start reading after the application has resolved the drop coordinates.
    /// Returns whether the request was queued.
    pub fn accept(&self, id: u64) -> bool {
        self.registration.live()
            && self
                .commands
                .send(Command::AcceptFileDrop(Arc::clone(&self.registration), id))
                .is_ok()
    }

    /// Finish only after the application has accepted the received paths. A
    /// false value rejects the drop before or after receipt. Returns whether
    /// the request was queued.
    pub fn finish(&self, id: u64, accepted: bool) -> bool {
        self.registration.live()
            && self
                .commands
                .send(Command::FinishFileDrop(Arc::clone(&self.registration), id, accepted))
                .is_ok()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.close();
    }
}

pub(crate) struct Registration {
    pub(crate) surface: usize,
    live: Mutex<bool>,
    notify: Box<dyn Fn(Event) + Send + Sync>,
}

impl Registration {
    pub(crate) fn live(&self) -> bool {
        self.live.lock().is_ok_and(|live| *live)
    }

    pub(crate) fn revoke(&self) -> bool {
        self.live.lock().is_ok_and(|mut live| std::mem::replace(&mut *live, false))
    }

    pub(crate) fn while_live(&self, action: impl FnOnce()) -> bool {
        let Ok(live) = self.live.lock() else { return false };
        if !*live {
            return false;
        }
        action();
        true
    }

    pub(crate) fn notify(&self, event: Event) {
        if self.live() {
            (self.notify)(event);
        }
    }
}
