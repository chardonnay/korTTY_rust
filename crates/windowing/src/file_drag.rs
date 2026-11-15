//! Authority for a native desktop file offer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Copied,
    Cancelled,
    Unavailable,
}

/// The application can revoke a queued or live offer while the OS owns the pointer.
#[derive(Clone)]
pub struct Permit(Arc<Authority>);

struct Authority {
    live: AtomicBool,
    check: Box<dyn Fn() -> bool + Send + Sync>,
}

impl Permit {
    /// The caller's check runs on the native drag thread and must return promptly.
    pub fn new(check: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(Authority {
            live: AtomicBool::new(true),
            check: Box::new(check),
        }))
    }

    pub fn revoke(&self) {
        self.0.live.store(false, Ordering::Release);
    }

    pub fn allows(&self) -> bool {
        self.is_live()
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(&self.0.check))
                .unwrap_or(false)
            && self.is_live()
    }

    pub(super) fn is_live(&self) -> bool {
        self.0.live.load(Ordering::Acquire)
    }
}
