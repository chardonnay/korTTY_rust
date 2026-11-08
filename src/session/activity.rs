//! Timestamps for the desktop's activity watches. No session content is kept.

use std::sync::{Mutex, PoisonError};
use std::time::Instant;

#[derive(Clone, Copy, Default)]
pub(crate) struct OutputSample {
    pub(crate) first: Option<Instant>,
    pub(crate) last: Option<Instant>,
    pub(crate) mirrored: Option<Instant>,
}

#[derive(Default)]
pub(super) struct Clock(Mutex<OutputSample>);

impl Clock {
    pub(super) fn output(&self, at: Instant) {
        let mut sample = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        sample.first.get_or_insert(at);
        sample.last = Some(at);
    }

    pub(super) fn mirrored_write(&self, at: Instant) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .mirrored = Some(at);
    }

    pub(super) fn take(&self) -> OutputSample {
        let mut sample = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let current = *sample;
        sample.first = None;
        current
    }
}
