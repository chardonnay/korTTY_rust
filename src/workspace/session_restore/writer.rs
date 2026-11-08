use std::hash::{Hash, Hasher};
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender, bounded};
use iced::Subscription;
use iced::futures::{StreamExt, stream::BoxStream};

use crate::profile::session::{RecentlyClosed, RestoreMode, Snapshot, Store};

use super::{Message, WorkspaceMessage};

const QUIET_TIME: Duration = Duration::from_secs(2);
const MAX_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub enum Event {
    Capture,
    WriteFailed(String),
    HistoryWriteFailed(String),
}

pub(super) struct Writer {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    events: Receiver<Event>,
    done: Receiver<()>,
}

#[derive(Default)]
struct Queue {
    dirty: Option<Dirty>,
    mode: Option<RestoreMode>,
    // Some(None) is an explicit removal of the stored session.
    snapshot: Option<Option<Snapshot>>,
    history: Option<RecentlyClosed>,
    closed: bool,
}

struct Dirty {
    first: Instant,
    last: Instant,
}

impl Dirty {
    fn deadline(&self) -> Instant {
        (self.first + MAX_WAIT).min(self.last + QUIET_TIME)
    }
}

impl Writer {
    pub(super) fn start(store: Store) -> io::Result<Self> {
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let (events_tx, events) = bounded(8);
        let (done_tx, done) = bounded(1);
        let worker_queue = Arc::clone(&queue);
        thread::Builder::new()
            .name("kortty-session-save".into())
            .spawn(move || {
                run(&worker_queue, &store, &events_tx);
                drop(store);
                let _ = done_tx.try_send(());
            })?;
        Ok(Self {
            queue,
            events,
            done,
        })
    }

    pub(super) fn changed(&self) {
        self.enqueue(|queue| {
            let now = Instant::now();
            let first = queue.dirty.as_ref().map_or(now, |dirty| dirty.first);
            queue.dirty = Some(Dirty { first, last: now });
        });
    }

    pub(super) fn mode(&self, mode: RestoreMode) {
        self.enqueue(|queue| queue.mode = Some(mode));
    }

    pub(super) fn write(&self, snapshot: Option<Snapshot>) {
        self.enqueue(|queue| queue.snapshot = Some(snapshot));
    }

    pub(super) fn history(&self, history: RecentlyClosed) {
        self.enqueue(|queue| queue.history = Some(history));
    }

    pub(super) fn finish(self, final_write: Option<Option<Snapshot>>) -> Receiver<()> {
        self.enqueue(|queue| {
            if let Some(snapshot) = final_write {
                queue.snapshot = Some(snapshot);
            }
            queue.closed = true;
        });
        self.done.clone()
    }

    pub(super) fn subscription(&self) -> Subscription<WorkspaceMessage> {
        Subscription::run_with(Events(self.events.clone()), event_stream)
    }

    fn enqueue(&self, operation: impl FnOnce(&mut Queue)) {
        let (queue, ready) = &*self.queue;
        let mut queue = queue.lock().unwrap_or_else(|error| error.into_inner());
        if !queue.closed {
            operation(&mut queue);
            ready.notify_one();
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.enqueue(|queue| queue.closed = true);
    }
}

struct Events(Receiver<Event>);

impl Hash for Events {
    fn hash<H: Hasher>(&self, _: &mut H) {
        // One writer per process; the receiver stays the same during the run.
    }
}

fn event_stream(events: &Events) -> BoxStream<'static, WorkspaceMessage> {
    events
        .0
        .clone()
        .map(|event| WorkspaceMessage::SessionRestore(Message::Worker(event)))
        .boxed()
}

enum Operation {
    SaveMode(RestoreMode),
    SaveSnapshot(Option<Snapshot>),
    SaveHistory(RecentlyClosed),
    Capture,
}

fn next_operation(queue: &(Mutex<Queue>, Condvar)) -> Option<Operation> {
    let (pending, ready) = queue;
    let mut queue = pending.lock().unwrap_or_else(|error| error.into_inner());
    loop {
        if let Some(mode) = queue.mode.take() {
            return Some(Operation::SaveMode(mode));
        }
        if let Some(snapshot) = queue.snapshot.take() {
            return Some(Operation::SaveSnapshot(snapshot));
        }
        if let Some(history) = queue.history.take() {
            return Some(Operation::SaveHistory(history));
        }
        if queue.closed {
            return None;
        }
        if let Some(dirty) = &queue.dirty {
            let delay = dirty.deadline().saturating_duration_since(Instant::now());
            if delay.is_zero() {
                queue.dirty = None;
                return Some(Operation::Capture);
            }
            queue = ready
                .wait_timeout(queue, delay)
                .unwrap_or_else(|error| error.into_inner())
                .0;
        } else {
            queue = ready.wait(queue).unwrap_or_else(|error| error.into_inner());
        }
    }
}

fn run(queue: &(Mutex<Queue>, Condvar), store: &Store, events: &Sender<Event>) {
    while let Some(operation) = next_operation(queue) {
        let result = match operation {
            Operation::SaveMode(mode) => store.save_mode(mode),
            Operation::SaveSnapshot(snapshot) => store.write(snapshot.as_ref()),
            Operation::SaveHistory(history) => {
                if let Err(error) = store.save_recently_closed(&history) {
                    let _ = events.try_send(Event::HistoryWriteFailed(error.to_string()));
                }
                continue;
            }
            Operation::Capture => {
                let _ = events.try_send(Event::Capture);
                continue;
            }
        };
        if let Err(error) = result {
            let _ = events.try_send(Event::WriteFailed(error.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debounce_honors_quiet_time_and_the_maximum_wait() {
        let first = Instant::now() - Duration::from_secs(31);
        let recent = first + Duration::from_secs(1);
        let mut dirty = Dirty {
            first,
            last: recent,
        };
        assert_eq!(dirty.deadline(), recent + QUIET_TIME);
        dirty.last = Instant::now();
        assert_eq!(dirty.deadline(), first + MAX_WAIT);

        let pending = (
            Mutex::new(Queue {
                dirty: Some(dirty),
                ..Queue::default()
            }),
            Condvar::new(),
        );
        assert!(matches!(next_operation(&pending), Some(Operation::Capture)));
        assert!(pending.0.lock().unwrap().dirty.is_none());
    }
}
