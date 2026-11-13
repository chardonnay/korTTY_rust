//! Operation deadlines interrupt blocked requests on their original SFTP worker.

use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{Control, Operation};

#[derive(Default)]
struct State {
    finished: bool,
    expired: bool,
}

pub(super) struct Guard {
    state: Arc<(Mutex<State>, Condvar)>,
    watcher: Option<JoinHandle<()>>,
}

impl Guard {
    pub(super) fn for_operation(
        operation: &Operation,
        control: &Arc<Control>,
    ) -> io::Result<Option<Self>> {
        if matches!(
            operation,
            Operation::ReviewRemoteOwnership(..) | Operation::ApplyRemoteOwnership(..)
        ) {
            // The bounded tree review uses several SFTP round trips per entry.
            return Self::start(control, Duration::from_secs(300)).map(Some);
        }
        if operation.ownership_cancel().is_some() {
            return Self::start(control, super::ownership::TIMEOUT).map(Some);
        }
        match operation {
            Operation::ReviewDraftFile(request, _) if request.is_replacement() => {
                Self::start(control, Duration::from_secs(105)).map(Some)
            }
            Operation::PublishDraftFile(plan, _) if plan.is_replacement() => {
                Self::start(control, Duration::from_secs(105)).map(Some)
            }
            Operation::BrowseDraftDestination(..)
            | Operation::ReviewDraftFile(..)
            | Operation::PublishDraftFile(..) => {
                Self::start(control, Duration::from_secs(60)).map(Some)
            }
            Operation::MeasureFolders(request) if !request.is_allocated() => {
                Self::start(control, crate::sftp_window::browser::SCAN_TIMEOUT).map(Some)
            }
            Operation::ReviewServerArchive(..) | Operation::ReviewServerExtract(..) => {
                Self::start(control, Duration::from_secs(300)).map(Some)
            }
            Operation::ApplyServerArchive(..) | Operation::ApplyServerExtract(..) => {
                Self::start(control, Duration::from_secs(1800)).map(Some)
            }
            _ => Ok(None),
        }
    }

    fn start(control: &Arc<Control>, timeout: Duration) -> io::Result<Self> {
        Self::watch(control, timeout, None)
    }

    pub(super) fn start_cancellable(
        control: &Arc<Control>,
        timeout: Duration,
        cancel: super::sync::Cancel,
    ) -> io::Result<Self> {
        Self::watch(control, timeout, Some(cancel))
    }

    fn watch(
        control: &Arc<Control>,
        timeout: Duration,
        cancel: Option<super::sync::Cancel>,
    ) -> io::Result<Self> {
        let state = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let signal = Arc::clone(&state);
        let owner = Arc::downgrade(control);
        let watcher = thread::Builder::new()
            .name("kortty-sftp-operation-timeout".into())
            .spawn(move || {
                let (state, complete) = &*signal;
                let started = Instant::now();
                let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                while !state.finished {
                    let remaining = timeout.saturating_sub(started.elapsed());
                    if remaining.is_zero()
                        || cancel.as_ref().is_some_and(super::sync::Cancel::stopped)
                    {
                        state.expired = remaining.is_zero();
                        if let Some(control) = owner.upgrade() {
                            control.abort();
                        }
                        break;
                    }
                    let wait = if cancel.is_some() {
                        remaining.min(Duration::from_millis(40))
                    } else {
                        remaining
                    };
                    (state, _) = complete
                        .wait_timeout(state, wait)
                        .unwrap_or_else(|error| error.into_inner());
                }
            })?;
        Ok(Self {
            state,
            watcher: Some(watcher),
        })
    }

    pub(super) fn finish(mut self) -> bool {
        self.stop()
    }

    fn stop(&mut self) -> bool {
        let (state, complete) = &*self.state;
        let expired = {
            let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
            state.finished = true;
            state.expired
        };
        complete.notify_all();
        if let Some(watcher) = self.watcher.take() {
            // A later command cannot outlive this command's timeout watcher.
            let _ = watcher.join();
        }
        expired
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn sftp_measurement_deadline_aborts_only_its_live_owner_and_reaps_completed_watchers() {
        let timeout = Duration::from_millis(25);
        let expired = Arc::new(Control::default());
        let replacement = Arc::new(Control::default());
        let guard = Guard::start(&expired, timeout).unwrap();
        let wait = Instant::now() + Duration::from_secs(3);
        while !expired.is_closed() && Instant::now() < wait {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            expired.is_closed(),
            "the deadline must abort a stalled operation"
        );
        assert!(guard.finish());
        assert!(!replacement.is_closed());

        let finished = Arc::new(Control::default());
        let guard = Guard::start(&finished, Duration::from_secs(5)).unwrap();
        assert!(!guard.finish());
        assert_eq!(Arc::weak_count(&finished), 0, "the watcher must be joined");
        let dropped = Guard::start(&finished, Duration::from_secs(5)).unwrap();
        drop(dropped);
        assert_eq!(Arc::weak_count(&finished), 0);
        assert!(
            !finished.is_closed(),
            "an old timer must not abort a following operation"
        );
    }
}
