//! Process-wide activity signal for native power management.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

static REQUESTS: Tracker = Tracker {
    count: AtomicUsize::new(0),
    watcher: Mutex::new(None),
};

struct Tracker {
    count: AtomicUsize,
    watcher: Mutex<Option<async_channel::Sender<()>>>,
}

impl Tracker {
    fn begin(&self) -> RequestActivity<'_> {
        if self.count.fetch_add(1, Ordering::AcqRel) == 0 {
            self.signal();
        }
        RequestActivity(self)
    }

    fn active(&self) -> bool {
        self.count.load(Ordering::Acquire) != 0
    }

    fn watch(&self) -> async_channel::Receiver<()> {
        let (sender, receiver) = async_channel::bounded(1);
        // Sample work that was already active when the subscription started.
        let _ = sender.try_send(());
        *self
            .watcher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
        receiver
    }

    fn signal(&self) {
        if let Some(watcher) = self
            .watcher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            // The reader samples the current count after any overlapping changes.
            let _ = watcher.try_send(());
        }
    }
}

pub(in crate::local_ai) struct RequestActivity<'a>(&'a Tracker);

impl RequestActivity<'static> {
    pub(in crate::local_ai) fn begin() -> Self {
        REQUESTS.begin()
    }
}

impl Drop for RequestActivity<'_> {
    fn drop(&mut self) {
        if self.0.count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.signal();
        }
    }
}

pub(crate) fn active() -> bool {
    REQUESTS.active()
}

pub(crate) fn watch() -> async_channel::Receiver<()> {
    REQUESTS.watch()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_notification_coalesces_overlapping_requests_and_reports_idle() {
        let tracker = Tracker {
            count: AtomicUsize::new(0),
            watcher: Mutex::new(None),
        };
        let first = tracker.begin();
        let receiver = tracker.watch();
        assert_eq!(receiver.try_recv(), Ok(()));
        assert!(tracker.active());
        let second = tracker.begin();
        assert!(receiver.is_empty());
        drop(first);
        assert!(receiver.is_empty());
        assert!(tracker.active());
        drop(second);
        assert_eq!(receiver.try_recv(), Ok(()));
        assert!(!tracker.active());
        let third = tracker.begin();
        assert_eq!(receiver.try_recv(), Ok(()));
        drop(third);
        assert_eq!(receiver.try_recv(), Ok(()));
    }
}
