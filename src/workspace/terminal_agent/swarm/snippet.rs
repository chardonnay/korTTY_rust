//! Saved scripts are prepared off the UI thread and reviewed per target.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use iced::{Task, window};

use crate::snippets::{self, Library, SnippetId};
use crate::terminal_agent::swarm::script::Prepared;

use super::{Desktop, Message, RunMessage};

pub(super) const MAX_ARGUMENT_BYTES: usize = 4096;
const MAX_CHOICES: usize = 2048;

#[derive(Clone, Eq, PartialEq)]
pub struct Choice {
    pub(super) id: SnippetId,
    label: String,
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

#[derive(Clone)]
pub struct Review {
    pub(in crate::workspace::terminal_agent) path: PathBuf,
    pub(in crate::workspace::terminal_agent) prepared: Arc<Prepared>,
}

pub(super) fn catalog(id: window::Id) -> Task<Desktop> {
    work(
        "kortty-swarm-snippets",
        || {
            let path = snippets::profile_path()
                .map_err(|_| "The saved snippet library is unavailable.")?;
            let library = Library::open(path).map_err(|_| "Could not open the snippet library.")?;
            Ok(library
                .entries()
                .iter()
                .filter(|entry| Prepared::supports_language(entry.language()))
                .take(MAX_CHOICES)
                .map(|entry| Choice {
                    id: entry.id(),
                    label: format!("{} · {}", entry.name(), entry.language()),
                })
                .collect())
        },
        move |choices| Desktop::TerminalAgent(id, RunMessage::Swarm(Message::Snippets(choices))),
    )
}

pub(super) fn prepare(
    id: window::Id,
    round: u64,
    snippet: SnippetId,
    arguments: String,
) -> Task<Desktop> {
    work(
        "kortty-swarm-snippet-review",
        move || {
            let path = snippets::profile_path()
                .map_err(|_| "The saved snippet library is unavailable.")?;
            let library =
                Library::open(path.clone()).map_err(|_| "Could not open the snippet library.")?;
            Ok(Review {
                path,
                prepared: Arc::new(Prepared::capture(&library, snippet, &arguments)?),
            })
        },
        move |result| {
            Desktop::TerminalAgent(
                id,
                RunMessage::Swarm(Message::SnippetPrepared(round, result)),
            )
        },
    )
}

pub(super) fn work<T: Send + 'static>(
    worker: &'static str,
    run: impl FnOnce() -> Result<T, &'static str> + Send + 'static,
    complete: impl Fn(Result<T, &'static str>) -> Desktop + Send + 'static,
) -> Task<Desktop> {
    Task::perform(
        async move {
            let (send, receive) = async_channel::bounded(1);
            std::thread::Builder::new()
                .name(worker.into())
                .spawn(move || {
                    let _ = send.send_blocking(run());
                })
                .map_err(|_| "Could not start the swarm file worker.")?;
            receive
                .recv()
                .await
                .unwrap_or(Err("The swarm file worker stopped."))
        },
        complete,
    )
}

pub(in crate::workspace::terminal_agent) fn verify(
    id: window::Id,
    epoch: u64,
    review: Review,
) -> Task<Desktop> {
    work(
        "kortty-swarm-snippet-verify",
        move || review.prepared.verify(&review.path),
        move |result| Desktop::TerminalAgent(id, RunMessage::ScriptRevision(epoch, result)),
    )
}
