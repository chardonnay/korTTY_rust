//! Short-lived process activities that allow the system to sleep normally.

use std::io;
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::NSObject;
use objc2_foundation::{NSActivityOptions, NSProcessInfo, ns_string};

/// Keeps the process responsive while this thread owns a request or job scope.
#[must_use]
pub struct AppNapActivity(Retained<NSObject>);

impl AppNapActivity {
    pub fn begin() -> Self {
        let token = autoreleasepool(|_| {
            // SAFETY: NSProcessInfo is thread-safe. The retained token remains on
            // this thread and is handed back exactly once by Drop.
            unsafe {
                NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                    NSActivityOptions::NSActivityUserInitiatedAllowingIdleSystemSleep,
                    ns_string!("kortty terminal, scheduled or AI work"),
                )
            }
        });
        #[cfg(debug_assertions)]
        LIVE_ACTIVITIES.with(|count| count.set(count.get() + 1));
        Self(token)
    }

    /// Reports activity scopes owned by the current thread in debug builds.
    #[cfg(debug_assertions)]
    pub fn current_thread_has_activity() -> bool {
        LIVE_ACTIVITIES.with(|count| count.get() != 0)
    }
}

impl Drop for AppNapActivity {
    fn drop(&mut self) {
        autoreleasepool(|_| {
            // SAFETY: This token was acquired from the shared process info object.
            unsafe { NSProcessInfo::processInfo().endActivity(&self.0) };
        });
        #[cfg(debug_assertions)]
        LIVE_ACTIVITIES.with(|count| count.set(count.get() - 1));
    }
}

#[cfg(debug_assertions)]
thread_local! {
    static LIVE_ACTIVITIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

const TERMINAL_LEASE: Duration = Duration::from_secs(60);
static TERMINAL: Mutex<Weak<Lease>> = Mutex::new(Weak::new());

#[derive(Default)]
struct Lease {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    deadline: Option<Instant>,
    stopped: bool,
}

/// Owns the terminal lease timer for one desktop lifetime.
pub struct TerminalActivity {
    lease: Arc<Lease>,
    worker: Option<JoinHandle<()>>,
}

impl TerminalActivity {
    pub fn start() -> io::Result<Self> {
        let mut current = TERMINAL.lock().unwrap_or_else(PoisonError::into_inner);
        if current.upgrade().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "terminal activity timer is running",
            ));
        }
        let lease = Arc::new(Lease::default());
        let worker_lease = Arc::clone(&lease);
        let worker = thread::Builder::new()
            .name("kortty-terminal-app-nap".into())
            .spawn(move || worker_lease.run(AppNapActivity::begin))?;
        *current = Arc::downgrade(&lease);
        Ok(Self {
            lease,
            worker: Some(worker),
        })
    }
}

impl Drop for TerminalActivity {
    fn drop(&mut self) {
        self.lease.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Records successful nonempty terminal I/O from its existing worker.
pub fn record_terminal_activity(at: Instant) {
    let lease = TERMINAL
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .upgrade();
    if let Some(lease) = lease {
        lease.extend(at + TERMINAL_LEASE);
    }
}

impl Lease {
    fn extend(&self, deadline: Instant) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.stopped || state.deadline.is_some_and(|current| current >= deadline) {
            return;
        }
        // The worker can keep its original wait after a renewal. It checks the
        // latest deadline on waking. Only an empty lease needs an early wake.
        if state.deadline.replace(deadline).is_none() {
            self.changed.notify_one();
        }
    }

    fn stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.stopped = true;
        self.changed.notify_one();
    }

    fn run<T>(&self, begin: impl Fn() -> T) {
        let mut activity = None;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        while !state.stopped {
            if let Some(remaining) = state.deadline.and_then(|deadline| {
                deadline
                    .checked_duration_since(Instant::now())
                    .filter(|time| !time.is_zero())
            }) {
                activity.get_or_insert_with(&begin);
                state = self
                    .changed
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            } else {
                state.deadline = None;
                drop(activity.take());
                state = self
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, RecvTimeoutError, Sender};

    use super::*;

    struct Token(Sender<bool>);

    impl Drop for Token {
        fn drop(&mut self) {
            let _ = self.0.send(false);
        }
    }

    #[test]
    fn terminal_activity_renews_one_scope_and_releases_it_on_idle_and_stop() {
        let lease = Arc::new(Lease::default());
        let worker_lease = Arc::clone(&lease);
        let (event, observed) = mpsc::channel();
        let worker = thread::spawn(move || {
            worker_lease.run(|| {
                event.send(true).unwrap();
                Token(event.clone())
            })
        });
        let mut activity = TerminalActivity {
            lease,
            worker: Some(worker),
        };
        let short = Duration::from_millis(50);
        let timeout = Duration::from_secs(3);
        assert_eq!(observed.recv_timeout(short), Err(RecvTimeoutError::Timeout));

        activity
            .lease
            .extend(Instant::now() + Duration::from_secs(1));
        assert!(observed.recv_timeout(timeout).unwrap());
        let renewed = Instant::now() + Duration::from_millis(1500);
        activity.lease.extend(renewed);
        activity.lease.extend(Instant::now() + short);
        assert!(!observed.recv_timeout(timeout).unwrap());
        assert!(Instant::now() >= renewed);

        activity.lease.extend(Instant::now() + TERMINAL_LEASE);
        assert!(observed.recv_timeout(timeout).unwrap());
        activity.lease.stop();
        activity.worker.take().unwrap().join().unwrap();
        assert!(!observed.recv_timeout(timeout).unwrap());
    }

    #[cfg(debug_assertions)]
    #[test]
    fn native_activity_scopes_release_independently() {
        let first = AppNapActivity::begin();
        let second = AppNapActivity::begin();
        assert!(AppNapActivity::current_thread_has_activity());
        drop(first);
        assert!(AppNapActivity::current_thread_has_activity());
        drop(second);
        assert!(!AppNapActivity::current_thread_has_activity());
    }
}
