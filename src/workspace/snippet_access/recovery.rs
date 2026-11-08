//! Coordinates recovery I/O and guards ownership across windows.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use iced::futures::{StreamExt, stream::BoxStream};
use iced::{Subscription, Task, window};

use crate::snippet_window::{
    self,
    draft_store::{Snapshot, Summary, Worker},
};
use crate::snippets::{self, SnippetId};

use super::{Message, Workspace};

#[derive(Default)]
pub(in crate::workspace) struct State {
    worker: Option<Worker>,
    catalog: Vec<Summary>,
    catalog_ticket: u64,
    ready: bool,
    problem: Option<String>,
    claims: HashSet<SnippetId>,
    pending: HashMap<u64, Pending>,
}

struct Pending {
    source: window::Id,
    summary: Summary,
    operation: Operation,
    orphan: bool,
    remaining: VecDeque<Summary>,
    adopted: Option<Arc<Snapshot>>,
}

#[derive(Clone, Copy)]
enum Operation {
    Restore,
    Discard,
}

#[derive(Clone, Debug)]
pub enum Event {
    Listed(u64, Result<Vec<Summary>, String>),
    Loaded(u64, Result<Option<Arc<Snapshot>>, String>),
    Adopted(u64, Result<(), String>),
    Discarded(u64, Result<(), String>),
    WriteFailed(String),
}

struct ErrorEvents(async_channel::Receiver<String>);

impl Hash for ErrorEvents {
    fn hash<H: Hasher>(&self, _: &mut H) {}
}

fn error_stream(events: &ErrorEvents) -> BoxStream<'static, Message> {
    events
        .0
        .clone()
        .map(|error| Message::SnippetRecovery(Event::WriteFailed(error)))
        .boxed()
}

impl State {
    pub(in crate::workspace) fn finish(&mut self) -> Option<async_channel::Receiver<()>> {
        self.worker.take().map(Worker::finish)
    }

    fn retire(&mut self, ticket: u64, summary: &Summary) {
        // A snapshot queued before the mutation may finish after its reply.
        self.catalog_ticket = self.catalog_ticket.max(ticket);
        self.catalog
            .retain(|entry| entry.key != summary.key || entry.revision != summary.revision);
    }
}

impl Workspace {
    pub(in crate::workspace) fn snippet_recovery_subscription(&self) -> Subscription<Message> {
        self.snippet_recovery
            .worker
            .as_ref()
            .map_or_else(Subscription::none, |worker| {
                Subscription::run_with(ErrorEvents(worker.problems()), error_stream)
            })
    }
    pub(in crate::workspace) fn initialize_snippet_recovery(&mut self) -> Task<Message> {
        let worker = match snippets::profile_path().and_then(|path| {
            Worker::start(path.with_file_name(snippet_window::draft_store::DIRECTORY_NAME))
        }) {
            Ok(worker) => worker,
            Err(error) => {
                eprintln!("kortty: could not start snippet recovery ({error})");
                return Task::none();
            }
        };
        self.snippet_recovery.worker = Some(worker);
        self.reload_snippet_recovery()
    }

    pub(in crate::workspace) fn refresh_snippet_recovery(&mut self) {
        self.flush_snippet_recovery();
        if !self.snippet_recovery.ready {
            if let Some(problem) = &self.snippet_recovery.problem {
                for surface in self.snippets.values_mut() {
                    surface.window.recovery_failed(problem.clone());
                }
            }
            return;
        }
        let mut unavailable: Vec<_> = self
            .snippets
            .values()
            .flat_map(|surface| surface.window.live_recovery_keys())
            .collect();
        unavailable.extend(self.snippet_recovery.claims.iter().copied());
        for (&id, surface) in &mut self.snippets {
            // Keep the current offer until the in-flight operation is acknowledged.
            if self
                .snippet_recovery
                .pending
                .values()
                .any(|pending| pending.source == id)
            {
                continue;
            }
            surface
                .window
                .sync_recovery(&self.snippet_recovery.catalog, &unavailable);
        }
        self.flush_snippet_recovery();
    }

    pub(in crate::workspace) fn flush_snippet_recovery(&mut self) {
        let removals: Vec<_> = self
            .snippets
            .iter_mut()
            .flat_map(|(&id, surface)| {
                surface
                    .window
                    .take_recovery_removals()
                    .into_iter()
                    .map(move |remove| (id, remove))
            })
            .collect();
        for (source, remove) in removals {
            let result = self
                .snippet_recovery
                .worker
                .as_ref()
                .map(|worker| worker.remove(remove.key, remove.revision));
            if let Some(Err(error)) = result {
                if let Some(surface) = self.snippets.get_mut(&source) {
                    surface.window.recovery_failed(error.to_string());
                }
            } else {
                self.snippet_recovery
                    .catalog
                    .retain(|entry| entry.key != remove.key);
            }
        }
    }

    pub(in crate::workspace) fn snippet_recovery_needs_tick(&self) -> bool {
        self.snippet_recovery.worker.is_some()
            && self
                .snippets
                .values()
                .any(|surface| surface.window.draft_autosave_pending())
    }

    pub(in crate::workspace) fn poll_snippet_recovery(&mut self, now: Instant) {
        if self.snippet_recovery.worker.is_none() {
            return;
        }
        self.refresh_snippet_recovery();
        for surface in self.snippets.values_mut() {
            if surface.window.busy() {
                continue;
            }
            for draft in surface.window.due_drafts(now) {
                let key = draft.key;
                if let Err(error) = self.snippet_recovery.worker.as_ref().unwrap().save(draft) {
                    surface.window.autosave_failed(key, error.to_string());
                }
            }
        }
        self.flush_snippet_recovery();
    }

    pub(super) fn update_snippet_recovery_action(
        &mut self,
        source: window::Id,
        action: snippet_window::recovery::Message,
    ) -> Task<Message> {
        if self.exiting || self.hosted_close_locks(source) || self.snippet_recovery.worker.is_none()
        {
            return Task::none();
        }
        let Some(surface) = self.snippets.get(&source) else {
            return Task::none();
        };
        if !surface.window.can_begin_recovery() {
            return Task::none();
        }
        let choices = surface.window.recovery_choices(&action);
        let operation = match action {
            snippet_window::recovery::Message::Restore(_)
            | snippet_window::recovery::Message::RestoreOrphans => Operation::Restore,
            snippet_window::recovery::Message::Discard(_)
            | snippet_window::recovery::Message::DiscardOrphans => Operation::Discard,
        };
        self.start_next_recovery(source, choices.into(), operation)
    }

    fn recovery_available(&self, source: window::Id, summary: &Summary) -> bool {
        !self.snippet_recovery.claims.contains(&summary.key)
            && self
                .snippet_recovery
                .catalog
                .iter()
                .any(|entry| entry.key == summary.key && entry.revision == summary.revision)
            && !self.snippets.iter().any(|(&id, surface)| {
                id != source && surface.window.live_recovery_keys().contains(&summary.key)
            })
    }

    fn start_next_recovery(
        &mut self,
        source: window::Id,
        mut remaining: VecDeque<Summary>,
        operation: Operation,
    ) -> Task<Message> {
        while let Some(summary) = remaining.pop_front() {
            if !self.recovery_available(source, &summary) {
                continue;
            }
            let Some(surface) = self.snippets.get_mut(&source) else {
                return Task::none();
            };
            let orphan = surface.window.recovery_is_orphan(&summary);
            surface.window.set_recovery_busy(true);
            let request = self.allocate();
            let worker = self.snippet_recovery.worker.as_ref().unwrap();
            let task = match operation {
                Operation::Restore => receive(
                    worker.load(summary.key, Some(summary.revision.clone())),
                    move |result| {
                        Message::SnippetRecovery(Event::Loaded(
                            request,
                            result.map(|snapshot| snapshot.map(Arc::new)),
                        ))
                    },
                ),
                Operation::Discard => receive(
                    worker.remove_confirmed(summary.key, summary.revision.clone()),
                    move |result| Message::SnippetRecovery(Event::Discarded(request, result)),
                ),
            };
            self.snippet_recovery.claims.insert(summary.key);
            self.snippet_recovery.pending.insert(
                request,
                Pending {
                    source,
                    summary,
                    operation,
                    orphan,
                    remaining,
                    adopted: None,
                },
            );
            return task;
        }
        if let Some(surface) = self.snippets.get_mut(&source) {
            surface.window.set_recovery_busy(false);
        }
        self.refresh_snippet_recovery();
        self.update_snippets(source, snippet_window::Message::RecoveryReady)
    }

    pub(in crate::workspace) fn update_snippet_recovery(&mut self, event: Event) -> Task<Message> {
        if self.exiting {
            return Task::none();
        }
        match event {
            Event::WriteFailed(error) => {
                for surface in self.snippets.values_mut() {
                    surface.window.recovery_failed(error.clone());
                }
                self.reload_snippet_recovery()
            }
            Event::Listed(ticket, result) if ticket >= self.snippet_recovery.catalog_ticket => {
                self.snippet_recovery.catalog_ticket = ticket;
                match result {
                    Ok(catalog) => {
                        self.snippet_recovery.catalog = catalog;
                        self.snippet_recovery.ready = true;
                        self.snippet_recovery.problem = None;
                    }
                    Err(error) => {
                        self.snippet_recovery.problem = Some(error.clone());
                        for surface in self.snippets.values_mut() {
                            surface.window.recovery_failed(error.clone());
                        }
                    }
                }
                self.refresh_snippet_recovery();
                Task::none()
            }
            Event::Listed(..) => Task::none(),
            Event::Loaded(request, result) => {
                let Some(mut pending) = self.snippet_recovery.pending.remove(&request) else {
                    return Task::none();
                };
                let snapshot = match result {
                    Ok(Some(snapshot)) => snapshot,
                    Ok(None) => {
                        return self.finish_recovery(
                            pending,
                            Err(String::from("The draft no longer exists")),
                        );
                    }
                    Err(error) => return self.finish_recovery(pending, Err(error)),
                };
                if pending.orphan {
                    let mut adopted = (*snapshot).clone();
                    adopted.key = match SnippetId::new() {
                        Ok(key) => key,
                        Err(error) => return self.finish_recovery(pending, Err(error.to_string())),
                    };
                    adopted.saved_id = None;
                    adopted.base_hash = None;
                    adopted.saved_at = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis()
                        .min(i64::MAX as u128) as i64;
                    // A listing can observe the new file before the adoption reply
                    // binds its identity to a live editor.
                    if !self.snippet_recovery.claims.insert(adopted.key) {
                        return self.finish_recovery(
                            pending,
                            Err(String::from("The recovery identity is already in use")),
                        );
                    }
                    let adopted = Arc::new(adopted);
                    let request = self.allocate();
                    let reply = self.snippet_recovery.worker.as_ref().unwrap().adopt(
                        adopted.clone(),
                        pending.summary.key,
                        pending.summary.revision.clone(),
                    );
                    pending.adopted = Some(adopted);
                    self.snippet_recovery.pending.insert(request, pending);
                    receive(reply, move |result| {
                        Message::SnippetRecovery(Event::Adopted(request, result))
                    })
                } else {
                    let result = self
                        .snippets
                        .get_mut(&pending.source)
                        .ok_or_else(|| String::from("The editor was closed"))
                        .and_then(|surface| {
                            surface
                                .window
                                .apply_recovered(&snapshot, false)
                                .map_err(|error| error.to_string())
                        });
                    self.finish_recovery(pending, result)
                }
            }
            Event::Adopted(request, result) => {
                let Some(pending) = self.snippet_recovery.pending.remove(&request) else {
                    return Task::none();
                };
                let result = result.and_then(|()| {
                    self.snippet_recovery.retire(request, &pending.summary);
                    let snapshot = pending
                        .adopted
                        .as_ref()
                        .ok_or_else(|| String::from("Missing adopted draft"))?;
                    self.snippets
                        .get_mut(&pending.source)
                        .ok_or_else(|| String::from("The editor was closed"))?
                        .window
                        .apply_recovered(snapshot, true)
                        .map_err(|error| error.to_string())
                });
                self.finish_recovery(pending, result)
            }
            Event::Discarded(request, result) => {
                let Some(pending) = self.snippet_recovery.pending.remove(&request) else {
                    return Task::none();
                };
                if result.is_ok() {
                    let summary = &pending.summary;
                    self.snippet_recovery.retire(request, summary);
                    if !self
                        .snippet_recovery
                        .catalog
                        .iter()
                        .any(|entry| entry.key == summary.key)
                        && let Some(surface) = self.snippets.get_mut(&pending.source)
                    {
                        surface.window.discard_offered(summary);
                    }
                }
                self.finish_recovery(pending, result)
            }
        }
    }

    fn finish_recovery(
        &mut self,
        mut pending: Pending,
        result: Result<(), String>,
    ) -> Task<Message> {
        self.snippet_recovery.claims.remove(&pending.summary.key);
        if let Some(adopted) = &pending.adopted {
            self.snippet_recovery.claims.remove(&adopted.key);
        }
        let reload = result.is_err();
        if let Err(error) = result {
            pending.remaining.clear();
            if let Some(surface) = self.snippets.get_mut(&pending.source) {
                surface.window.recovery_failed(error);
            }
        }
        let resume = self.start_next_recovery(pending.source, pending.remaining, pending.operation);
        if reload {
            Task::batch([resume, self.reload_snippet_recovery()])
        } else {
            resume
        }
    }

    fn reload_snippet_recovery(&mut self) -> Task<Message> {
        let ticket = self.allocate();
        self.snippet_recovery
            .worker
            .as_ref()
            .map_or_else(Task::none, |worker| {
                receive(worker.list(), move |result| {
                    Message::SnippetRecovery(Event::Listed(ticket, result))
                })
            })
    }
}

fn receive<T: Send + 'static>(
    reply: std::io::Result<async_channel::Receiver<Result<T, String>>>,
    message: impl FnOnce(Result<T, String>) -> Message + Send + 'static,
) -> Task<Message> {
    Task::perform(
        async move {
            reply
                .map_err(|error| error.to_string())?
                .recv()
                .await
                .map_err(|_| String::from("The snippet recovery worker stopped"))?
        },
        message,
    )
}

#[cfg(test)]
mod tests;
