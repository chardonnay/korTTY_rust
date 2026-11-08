use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use super::{Abort, Process, lock};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);
static WORKERS: Mutex<BTreeMap<Id, Weak<Process>>> = Mutex::new(BTreeMap::new());

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Id(u64);

pub(super) struct Registration {
    id: Id,
    pid: u32,
    started: Instant,
    label: String,
    sandbox: AtomicBool,
}

impl Registration {
    pub(super) fn new(label: String, pid: u32) -> Self {
        Self {
            id: Id(NEXT_ID.fetch_add(1, Ordering::Relaxed)),
            pid,
            started: Instant::now(),
            label,
            sandbox: AtomicBool::new(false),
        }
    }

    pub(super) fn sandbox_verified(&self) {
        self.sandbox.store(true, Ordering::Release);
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        lock(&WORKERS).remove(&self.id);
    }
}

pub(super) fn register(abort: &Abort) {
    if let Some(registration) = &abort.0.monitor {
        lock(&WORKERS).insert(registration.id, Arc::downgrade(&abort.0));
    }
}

#[derive(Clone, Debug)]
pub struct Sample {
    pub id: Id,
    pub pid: Option<u32>,
    pub label: String,
    pub sandbox: bool,
    pub running_for: Duration,
    pub cpu_time: Option<Duration>,
    pub resident_bytes: Option<u64>,
}

pub(crate) fn sample() -> Vec<Sample> {
    let workers: Vec<_> = lock(&WORKERS).values().cloned().collect();
    workers.into_iter().filter_map(measure).collect()
}

fn measure(weak: Weak<Process>) -> Option<Sample> {
    let process = weak.upgrade()?;
    if process.closed.load(Ordering::Acquire) {
        return None;
    }
    let registration = process.monitor.as_ref()?;
    let usage = if let Ok(mut child) = process.child.try_lock() {
        let child = child.as_mut()?;
        if child.exited().unwrap_or(false) {
            return None;
        }
        child.usage()
    } else {
        // Keep the row selectable while startup or shutdown briefly holds the child.
        kortty_pty::ProcessUsage::default()
    };
    Some(Sample {
        id: registration.id,
        pid: (registration.pid != 0).then_some(registration.pid),
        label: registration.label.clone(),
        sandbox: registration.sandbox.load(Ordering::Acquire),
        running_for: registration.started.elapsed(),
        cpu_time: usage.cpu_time,
        resident_bytes: usage.resident_bytes,
    })
}

/// Terminates the original owned child only, even if its numeric PID was reused.
pub(crate) fn end(id: Id) -> io::Result<()> {
    let process = lock(&WORKERS).get(&id).and_then(Weak::upgrade);
    let Some(process) = process else {
        return Ok(());
    };
    {
        let mut child = lock(&process.child);
        let Some(child) = child.as_mut() else {
            return Ok(());
        };
        if !child.exited()? {
            child.kill()?;
        }
        process.closed.store(true, Ordering::Release);
    }
    #[cfg(any(windows, target_os = "macos"))]
    process.stop_relay();
    Ok(())
}
