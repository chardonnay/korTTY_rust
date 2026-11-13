//! Timestamps for the desktop's activity watches. No session content is kept.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

#[derive(Clone, Copy, Default)]
pub(crate) struct OutputSample {
    pub(crate) first: Option<Instant>,
    pub(crate) last: Option<Instant>,
    pub(crate) mirrored: Option<Instant>,
}

#[derive(Default)]
pub(super) struct Clock(Mutex<OutputSample>, AtomicU64);

impl Clock {
    pub(super) fn output(&self, at: Instant) {
        #[cfg(target_os = "macos")]
        kortty_macos::record_terminal_activity(at);
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

    pub(super) fn last_output(&self) -> Option<Instant> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).last
    }

    pub(super) fn note_input(&self) {
        self.1.fetch_add(1, Ordering::Release);
    }

    pub(super) fn input_revision(&self) -> u64 {
        self.1.load(Ordering::Acquire)
    }

    pub(super) fn last_mirrored_input(&self) -> Option<Instant> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .mirrored
    }

    pub(super) fn take(&self) -> OutputSample {
        let mut sample = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let current = *sample;
        sample.first = None;
        current
    }
}
