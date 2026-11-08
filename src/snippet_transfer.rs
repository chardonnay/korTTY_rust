//! Prepared, cancellable copies of a snippet layout into a terminal directory.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::ssh_connection::SftpOpener;
use async_channel::{Receiver, Sender, bounded};

mod local;
mod remote;

pub(crate) struct Layout {
    pub(crate) directories: Vec<String>,
    pub(crate) entries: Vec<Entry>,
}

pub(crate) struct Entry {
    pub(crate) path: String,
    pub(crate) content: String,
    pub(crate) mode: u32,
}

#[derive(Clone, Copy)]
pub(crate) enum Choice {
    Copy,
    Skip,
    Overwrite,
}

#[derive(Clone)]
pub(crate) struct Progress {
    pub(crate) done: usize,
    pub(crate) total: usize,
    pub(crate) path: String,
}

#[derive(Clone, Default)]
pub(crate) struct Outcome {
    pub(crate) written: usize,
    pub(crate) skipped: usize,
    pub(crate) cancelled: bool,
}

pub(crate) enum View {
    Preparing,
    Confirm {
        label: String,
        target: String,
        files: usize,
        conflicts: Vec<String>,
        overwrite: bool,
    },
    Copying {
        target: String,
        done: usize,
        total: usize,
        current: String,
        cancelling: bool,
    },
    Finished {
        target: String,
        written: usize,
        skipped: usize,
        cancelled: bool,
    },
    Failed {
        message: String,
    },
}

pub(crate) type DirectoryReply = Receiver<io::Result<PathBuf>>;

pub(crate) enum Source {
    Local(DirectoryReply),
    Remote(SftpOpener, Option<Vec<u8>>),
}

pub(crate) struct Request {
    pub(crate) choice: Choice,
    pub(crate) directory: Option<DirectoryReply>,
}

#[derive(Clone)]
pub(crate) enum Event {
    Prepared {
        target: String,
        files: usize,
        conflicts: Vec<String>,
        overwrite: bool,
    },
    Progress(Progress),
    Finished(Result<Outcome, String>),
}

impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Snippet transfer event")
    }
}

pub(crate) struct Worker {
    cancel: Arc<AtomicBool>,
    confirm: Sender<Request>,
}

impl Worker {
    pub(crate) fn start(
        source: Source,
        layout: Layout,
        released: Option<Receiver<()>>,
    ) -> io::Result<(Self, Receiver<Event>, Receiver<()>)> {
        let (confirm, decision) = bounded(1);
        let (events, receive) = bounded(8);
        let (stopped, done) = bounded(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&cancel);
        std::thread::Builder::new()
            .name("kortty-snippet-transfer".into())
            .spawn(move || {
                let result = execute(source, layout, released, decision, &events, &cancelled);
                let outcome = match result {
                    Err(error)
                        if error.kind() == io::ErrorKind::Interrupted
                            && cancelled.load(Ordering::Acquire) =>
                    {
                        Ok(Outcome {
                            cancelled: true,
                            ..Outcome::default()
                        })
                    }
                    result => result.map_err(|error| error.to_string()),
                };
                let _ = events.send_blocking(Event::Finished(outcome));
                drop(stopped);
            })?;
        Ok((Self { cancel, confirm }, receive, done))
    }

    pub(crate) fn confirm(&self, request: Request) -> bool {
        self.confirm.try_send(request).is_ok()
    }

    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        self.confirm.close();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn execute(
    source: Source,
    layout: Layout,
    released: Option<Receiver<()>>,
    decision: Receiver<Request>,
    events: &Sender<Event>,
    cancelled: &AtomicBool,
) -> io::Result<Outcome> {
    if let Some(released) = released {
        let _ = released.recv_blocking();
    }
    check_cancelled(cancelled)?;
    let plan = match source {
        Source::Local(directory) => {
            let cwd = shell_directory(directory)?;
            Plan::Local(local::prepare(cwd.clone(), &layout, cancelled)?, cwd)
        }
        Source::Remote(opener, cwd) => {
            Plan::Remote(remote::prepare(opener, cwd, &layout, cancelled)?)
        }
    };
    check_cancelled(cancelled)?;
    events
        .send_blocking(plan.preview(layout.entries.len()))
        .map_err(|_| interrupted())?;
    let request = decision.recv_blocking().map_err(|_| interrupted())?;
    check_cancelled(cancelled)?;
    let mut last_progress = None::<Instant>;
    let mut progress = |progress| {
        let now = Instant::now();
        if last_progress.is_none_or(|last| now.duration_since(last) >= Duration::from_millis(100)) {
            let _ = events.try_send(Event::Progress(progress));
            last_progress = Some(now);
        }
    };
    match plan {
        Plan::Local(plan, cwd) => {
            let directory = request.directory.ok_or_else(interrupted)?;
            if shell_directory(directory)? != cwd {
                return Err(io::Error::other(
                    "The terminal working directory changed. Review the destination again.",
                ));
            }
            plan.run(&layout, request.choice, cancelled, &mut progress)
        }
        Plan::Remote(plan) => plan.run(&layout, request.choice, cancelled, &mut progress),
    }
}

enum Plan {
    Local(local::Plan, PathBuf),
    Remote(remote::Plan),
}

impl Plan {
    fn preview(&self, files: usize) -> Event {
        let (target, conflicts, overwrite) = match self {
            Self::Local(plan, _) => (plan.target(), plan.conflicts(), plan.can_overwrite()),
            Self::Remote(plan) => (plan.target(), plan.conflicts(), plan.can_overwrite()),
        };
        Event::Prepared {
            target,
            files,
            conflicts: conflicts.to_vec(),
            overwrite,
        }
    }
}

fn shell_directory(directory: DirectoryReply) -> io::Result<PathBuf> {
    directory
        .recv_blocking()
        .map_err(|_| io::Error::other("The original shell closed."))?
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(interrupted())
    } else {
        Ok(())
    }
}

fn interrupted() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "Snippet transfer cancelled")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::snippets::tests::Profile;

    fn reported_directory(path: PathBuf) -> DirectoryReply {
        // Test double for the directory returned by the foreground PTY shell.
        let (send, receive) = bounded(1);
        send.try_send(Ok(path)).unwrap();
        receive
    }

    fn layout() -> Layout {
        Layout {
            directories: vec!["operations".into()],
            entries: vec![Entry {
                path: "operations/check.sh".into(),
                content: "echo test\n".into(),
                mode: 0o755,
            }],
        }
    }

    #[test]
    fn snippet_transfer_preflight_does_not_write_and_rejects_a_changed_shell_directory() {
        let first = Profile::new();
        let changed = Profile::new();
        let original = first.0.canonicalize().unwrap();
        let other = changed.0.canonicalize().unwrap();
        let (worker, events, done) = Worker::start(
            Source::Local(reported_directory(original.clone())),
            layout(),
            None,
        )
        .unwrap();
        assert!(matches!(
            events.recv_blocking().unwrap(),
            Event::Prepared { .. }
        ));
        assert!(!original.join("operations").exists());
        assert!(worker.confirm(Request {
            choice: Choice::Copy,
            directory: Some(reported_directory(other.clone())),
        }));
        assert!(matches!(
            events.recv_blocking().unwrap(),
            Event::Finished(Err(reason)) if reason.contains("working directory changed")
        ));
        assert!(done.recv_blocking().is_err());
        assert!(!original.join("operations").exists());
        assert!(!other.join("operations").exists());
    }

    #[test]
    fn dropping_a_prepared_snippet_transfer_releases_the_worker_without_writing() {
        let profile = Profile::new();
        let original = profile.0.canonicalize().unwrap();
        let (worker, events, done) = Worker::start(
            Source::Local(reported_directory(original.clone())),
            layout(),
            None,
        )
        .unwrap();
        assert!(matches!(
            events.recv_blocking().unwrap(),
            Event::Prepared { .. }
        ));
        drop(worker);
        assert!(matches!(
            events.recv_blocking().unwrap(),
            Event::Finished(Ok(Outcome {
                cancelled: true,
                written: 0,
                ..
            }))
        ));
        assert!(done.recv_blocking().is_err());
        assert!(!original.join("operations").exists());
    }
}
