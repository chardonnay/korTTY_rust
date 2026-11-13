//! Explicit review and one-use publication of a draft in the selected server folder.

use std::sync::Arc;

use iced::widget::{button, checkbox, column, row, scrollable, text, text_input};
use iced::{Element, Length, Task};

use crate::clipboard_policy::{ClipboardPolicy, RemoteEditorPolicy};
use crate::sftp_window::worker::{self, draft_file};
use crate::sftp_window::{Directory, EntryName, Location, RemoteEditor, files};

use super::{Scope, Source};

#[derive(Clone)]
pub enum Event {
    Directory(String),
    Name(String),
    Browse,
    Listed(u64, Listing),
    Navigate(Vec<u8>),
    Page(bool),
    ChooseFolder,
    Review,
    Backup(bool),
    ReviewReplacement,
    Reviewed(u64, Result<Arc<draft_file::Plan>, String>),
    Publish(u64),
    Published(u64, Result<Published, String>),
    Cancel,
}

#[derive(Clone)]
pub struct Listing(Result<Arc<Directory>, String>);

#[derive(Clone)]
pub struct Published(String, Option<draft_file::Publication>);

pub(super) struct State {
    name: String,
    directory: Folder,
    connection: Option<RemoteEditor>,
    phase: Phase,
    backup: bool,
    replacement_channel: bool,
}

struct Folder {
    path: Vec<u8>,
    text: String,
}

impl Folder {
    fn new(path: Vec<u8>) -> Self {
        let text = match std::str::from_utf8(&path) {
            Ok(path) => path.to_owned(),
            Err(_) => path.escape_ascii().to_string(),
        };
        Self { path, text }
    }

    fn from_directory(directory: &Directory) -> Option<&[u8]> {
        match &directory.path {
            Location::Remote(path) if draft_file::valid_directory(path) => Some(path),
            _ => None,
        }
    }
}

const PAGE_SIZE: usize = 48;

#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    Listing(u64, Vec<u8>),
    Browsing(Arc<Directory>, usize),
    Reviewing(u64, Pending),
    Review(u64, Arc<draft_file::Plan>),
    Publishing(u64, Pending),
}

struct Pending(worker::sync::Cancel);

impl Drop for Pending {
    fn drop(&mut self) {
        self.0.stop_local();
    }
}

impl State {
    pub(super) fn new(source: &Source<'_>, scope: &Scope<'_>) -> Self {
        Self {
            name: String::new(),
            directory: Folder::new(
                source
                    .remote_directory
                    .filter(|path| draft_file::valid_directory(path))
                    .unwrap_or(b"/")
                    .to_vec(),
            ),
            connection: source
                .worker
                .filter(|worker| worker.verified_host_key().is_some())
                .and_then(worker::Worker::editor)
                .filter(|editor| editor.writable() && destination_policy(source, scope)),
            phase: Phase::Idle,
            backup: true,
            replacement_channel: source
                .worker
                .is_some_and(|worker| worker.exec_opener().is_some()),
        }
    }

    pub(super) fn available(&self) -> bool {
        self.connection.is_some()
    }

    pub(super) fn publishing(&self) -> bool {
        matches!(self.phase, Phase::Publishing(..))
    }

    pub(super) fn publication_note<'a>(&self, event: &'a Event) -> Option<&'a str> {
        match (&self.phase, event) {
            (Phase::Publishing(current, _), Event::Published(id, result)) if current == id => {
                Some(match result {
                    Ok(Published(note, _)) | Err(note) => note,
                })
            }
            _ => None,
        }
    }

    pub(super) fn publication<'a>(&self, event: &'a Event) -> Option<&'a draft_file::Publication> {
        match (&self.phase, event) {
            (Phase::Publishing(current, _), Event::Published(id, Ok(Published(_, saved))))
                if current == id =>
            {
                saved.as_ref()
            }
            _ => None,
        }
    }

    pub(super) fn revoke(&self) {
        if let Phase::Publishing(_, pending) = &self.phase {
            pending.0.stop_local();
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn update(
        &mut self,
        event: Event,
        source: &Source<'_>,
        scope: &Scope<'_>,
        draft: &Arc<str>,
        authorize: draft_file::Authorize,
        next_ticket: &mut u64,
        status: &mut Option<String>,
    ) -> Option<Task<Event>> {
        let worker = self.worker(source, scope);
        // A publication receipt may report an already committed file after its
        // connection closed. Keep the receipt for the matching in-flight ticket.
        let finishing = self.publication_note(&event).is_some();
        if worker.is_err() && !finishing {
            self.connection = None;
            self.phase = Phase::Idle;
            *status = Some("The original verified SFTP destination is unavailable.".into());
            return None;
        }
        match event {
            Event::Directory(path)
                if matches!(self.phase, Phase::Idle)
                    && path.len() <= draft_file::MAX_DIRECTORY_BYTES
                    && !path.chars().any(char::is_control) =>
            {
                self.directory = Folder::new(path.into_bytes());
                *status = None;
            }
            Event::Name(name) if name.len() <= 255 && matches!(self.phase, Phase::Idle) => {
                self.name = name;
                *status = None;
            }
            Event::Browse if matches!(self.phase, Phase::Idle) => {
                return self.browse(
                    self.directory.path.clone(),
                    source,
                    scope,
                    next_ticket,
                    status,
                );
            }
            Event::Listed(id, Listing(result)) if matches!(&self.phase, Phase::Listing(current, _) if id == *current) =>
            {
                let Phase::Listing(_, expected) = std::mem::take(&mut self.phase) else {
                    unreachable!("the browse ticket matched");
                };
                match result {
                    Ok(directory)
                        if Folder::from_directory(&directory) == Some(expected.as_slice()) =>
                    {
                        self.phase = Phase::Browsing(directory, 0);
                    }
                    Ok(_) => *status = Some("The server returned an invalid folder.".into()),
                    Err(error) => *status = Some(error),
                }
            }
            Event::Navigate(path) => {
                if let Phase::Browsing(directory, _) = &self.phase
                    && browse_path_allowed(directory, &path)
                {
                    return self.browse(path, source, scope, next_ticket, status);
                }
            }
            Event::Page(next) => {
                if let Phase::Browsing(directory, offset) = &mut self.phase {
                    if next && *offset + PAGE_SIZE < directory.entries.len() {
                        *offset += PAGE_SIZE;
                    } else if !next {
                        *offset = offset.saturating_sub(PAGE_SIZE);
                    }
                }
            }
            Event::ChooseFolder => {
                if let Phase::Browsing(directory, _) = &self.phase
                    && let Some(path) = Folder::from_directory(directory)
                {
                    self.directory = Folder::new(path.to_vec());
                    self.phase = Phase::Idle;
                }
            }
            Event::Backup(backup) if matches!(self.phase, Phase::Idle) => self.backup = backup,
            Event::Review | Event::ReviewReplacement if matches!(self.phase, Phase::Idle) => {
                let replacement = matches!(event, Event::ReviewReplacement);
                match self.prepare(source, scope, draft, authorize, next_ticket, replacement) {
                    Ok(task) => {
                        *status = None;
                        return Some(task);
                    }
                    Err(error) => *status = Some(error),
                }
            }
            Event::Reviewed(id, result) if matches!(self.phase, Phase::Reviewing(current, _) if current == id) =>
            {
                self.phase = Phase::Idle;
                match result {
                    Ok(plan) if self.matches(&plan, source, draft) => {
                        self.phase = Phase::Review(id, plan);
                    }
                    Ok(_) => {
                        *status = Some("The selected server folder, name or draft changed.".into())
                    }
                    Err(error) => *status = Some(error),
                }
            }
            Event::Publish(id) => {
                let Phase::Review(current, plan) = &self.phase else {
                    return None;
                };
                if *current != id {
                    return None;
                }
                let worker = self.worker(source, scope).ok()?;
                if !self.matches(plan, source, draft) {
                    *status = Some("The selected server folder, name or draft changed.".into());
                    return None;
                }
                match publish(worker, plan, id) {
                    Ok((pending, task)) => {
                        self.phase = Phase::Publishing(id, pending);
                        *status = None;
                        return Some(task);
                    }
                    Err(error) => *status = Some(error),
                }
            }
            Event::Published(id, result) if matches!(self.phase, Phase::Publishing(current, _) if current == id) =>
            {
                self.phase = Phase::Idle;
                if worker.is_err() {
                    self.connection = None;
                }
                if matches!(&result, Ok(Published(_, None))) {
                    self.name.clear();
                }
                *status = Some(match result {
                    Ok(Published(note, _)) | Err(note) => note,
                });
            }
            Event::Cancel => {
                if let Phase::Reviewing(_, pending) | Phase::Publishing(_, pending) = &self.phase
                    && let Some(worker) = source.worker
                {
                    pending.0.stop(worker);
                    *status = Some("Cancelling the server operation…".into());
                } else {
                    self.phase = Phase::Idle;
                    *status = None;
                }
            }
            _ => {}
        }
        None
    }

    fn matches(&self, plan: &draft_file::Plan, source: &Source<'_>, draft: &Arc<str>) -> bool {
        if plan.replacement().is_some() {
            source
                .right_remote
                .is_some_and(|original| plan.matches_replacement(original, self.backup, draft))
        } else {
            plan.matches(&self.directory.path, &self.name, draft)
        }
    }

    fn prepare(
        &mut self,
        source: &Source<'_>,
        scope: &Scope<'_>,
        draft: &Arc<str>,
        authorize: draft_file::Authorize,
        next_ticket: &mut u64,
        replacement: bool,
    ) -> Result<Task<Event>, String> {
        let worker = self.worker(source, scope)?;
        let request = if replacement {
            let original = source
                .right_remote
                .ok_or("There is no eligible compared server file.")?;
            draft_file::Request::replacing(
                original,
                self.backup,
                Arc::clone(draft),
                source.target,
                worker,
                authorize,
            )
        } else {
            draft_file::Request::new(
                &self.directory.path,
                &self.name,
                Arc::clone(draft),
                source.target,
                worker,
                authorize,
            )
        }
        .map_err(|error| error.to_string())?;
        let pending = Pending(worker::sync::Cancel::default());
        let reply = worker
            .submit(worker::Operation::ReviewDraftFile(
                Box::new(request),
                pending.0.clone(),
            ))
            .map_err(|error| error.to_string())?;
        *next_ticket = next_ticket.wrapping_add(1);
        let id = *next_ticket;
        self.phase = Phase::Reviewing(id, pending);
        Ok(Task::perform(receive(reply), move |result| {
            Event::Reviewed(
                id,
                result.and_then(|reply| match reply.as_ref() {
                    worker::Reply::DraftFileReviewed(plan) => Ok(Arc::clone(plan)),
                    _ => Err("The server returned no destination review.".into()),
                }),
            )
        }))
    }

    fn worker<'a>(
        &self,
        source: &Source<'a>,
        scope: &Scope<'_>,
    ) -> Result<&'a worker::Worker, String> {
        if destination_policy(source, scope)
            && let (Some(worker), Some(connection)) = (source.worker, &self.connection)
            && worker.owns_editor(connection)
        {
            Ok(worker)
        } else {
            Err("The original verified SFTP destination is unavailable.".into())
        }
    }

    fn browse(
        &mut self,
        path: Vec<u8>,
        source: &Source<'_>,
        scope: &Scope<'_>,
        next_ticket: &mut u64,
        status: &mut Option<String>,
    ) -> Option<Task<Event>> {
        if !draft_file::valid_directory(&path) {
            *status = Some("Enter an absolute canonical server folder.".into());
            return None;
        }
        let worker = self.worker(source, scope).ok()?;
        let reply = match worker.submit(worker::Operation::BrowseDraftDestination(path.clone())) {
            Ok(reply) => reply,
            Err(error) => {
                *status = Some(error.to_string());
                return None;
            }
        };
        *next_ticket = next_ticket.wrapping_add(1);
        let id = *next_ticket;
        self.phase = Phase::Listing(id, path);
        *status = None;
        Some(Task::perform(receive(reply), move |result| {
            Event::Listed(
                id,
                Listing(result.and_then(|reply| match reply.as_ref() {
                    worker::Reply::Listed(directory) => Ok(Arc::clone(directory)),
                    _ => Err("The selected server folder changed.".into()),
                })),
            )
        }))
    }

    pub(super) fn view(&self, right_remote: bool) -> Element<'_, Event> {
        match &self.phase {
            Phase::Idle => column![
                row![
                    text_input("Absolute server folder", &self.directory.text)
                        .on_input(Event::Directory),
                    button("Browse server…").on_press_maybe(
                        draft_file::valid_directory(&self.directory.path).then_some(Event::Browse),
                    ),
                ]
                .spacing(8),
                (right_remote && self.replacement_channel).then(|| row![
                    checkbox(self.backup)
                        .label("Keep a .bak copy")
                        .on_toggle(Event::Backup),
                    button("Review replacement of the right server file…")
                        .on_press(Event::ReviewReplacement),
                ]
                .spacing(8)
                .wrap()),
                row![
                    text_input("New file name", &self.name).on_input(Event::Name),
                    button("Review server save…").on_press_maybe(
                        (draft_file::valid_directory(&self.directory.path)
                            && draft_file::valid_name(&self.name))
                        .then_some(Event::Review),
                    ),
                ]
                .spacing(8),
            ]
            .spacing(8)
            .into(),
            Phase::Listing(..) => row![
                text("Loading server folders…"),
                button("Cancel").on_press(Event::Cancel),
            ]
            .spacing(8)
            .into(),
            Phase::Browsing(directory, offset) => browse_view(directory, *offset),
            Phase::Reviewing(..) => row![
                text("Checking the server destination…"),
                button("Cancel").on_press(Event::Cancel)
            ]
            .spacing(8)
            .into(),
            Phase::Review(id, plan) => column![
                scrollable(
                    text(if let Some(spec) = plan.replacement() {
                        format!(
                            "Replace server file · {} · {} bytes · mode {:04o} · {}",
                            plan.destination(),
                            plan.bytes(),
                            spec.original.mode,
                            if spec.backup {
                                "Keep a new .bak copy"
                            } else {
                                "No backup"
                            }
                        )
                    } else {
                        format!(
                            "Create new server file · {} · {} bytes · mode 0600",
                            plan.destination(),
                            plan.bytes()
                        )
                    })
                    .wrapping(text::Wrapping::WordOrGlyph),
                )
                .height(Length::Fixed(72.0)),
                row![
                    button(if plan.replacement().is_some() {
                        "Replace reviewed server file"
                    } else {
                        "Create reviewed server file"
                    })
                    .on_press(Event::Publish(*id)),
                    button("Back").on_press(Event::Cancel)
                ]
                .spacing(8),
            ]
            .spacing(5)
            .into(),
            Phase::Publishing(..) => row![
                text("Publishing the reviewed file…"),
                button("Cancel").on_press(Event::Cancel)
            ]
            .spacing(8)
            .into(),
        }
    }
}

fn browse_view(directory: &Directory, offset: usize) -> Element<'_, Event> {
    let mut entries = column![].spacing(3);
    let path = Folder::from_directory(directory).expect("verified browse result");
    for entry in directory.entries.iter().skip(offset).take(PAGE_SIZE) {
        if let EntryName::Remote(name) = &entry.name {
            entries = entries.push(
                button(text(&entry.display))
                    .style(button::subtle)
                    .width(Length::Fill)
                    .on_press(Event::Navigate(files::remote_join(path, name))),
            );
        }
    }
    column![
        text(format!("Server folder · {}", directory.path.display())),
        scrollable(entries).height(Length::Fixed(160.0)),
        row![
            button("Up").on_press_maybe(parent(path).map(|path| Event::Navigate(path.to_vec()))),
            button("Previous").on_press_maybe((offset > 0).then_some(Event::Page(false))),
            button("Next").on_press_maybe(
                (offset + PAGE_SIZE < directory.entries.len()).then_some(Event::Page(true)),
            ),
            button("Use this folder").on_press(Event::ChooseFolder),
            button("Cancel").on_press(Event::Cancel),
        ]
        .spacing(8),
    ]
    .spacing(5)
    .into()
}

fn parent(path: &[u8]) -> Option<&[u8]> {
    if path == b"/" {
        return None;
    }
    let end = path.iter().rposition(|&byte| byte == b'/')?.max(1);
    Some(&path[..end])
}

fn browse_path_allowed(directory: &Directory, candidate: &[u8]) -> bool {
    let Some(path) = Folder::from_directory(directory) else {
        return false;
    };
    parent(path) == Some(candidate)
        || directory.entries.iter().any(|entry| {
            let EntryName::Remote(name) = &entry.name else {
                return false;
            };
            entry.kind == crate::sftp_window::EntryKind::Directory
                && files::remote_join(path, name) == candidate
        })
}

fn publish(
    worker: &worker::Worker,
    plan: &Arc<draft_file::Plan>,
    id: u64,
) -> Result<(Pending, Task<Event>), String> {
    let pending = Pending(worker::sync::Cancel::default());
    let reply = worker
        .submit(worker::Operation::PublishDraftFile(
            Arc::clone(plan),
            pending.0.clone(),
        ))
        .map_err(|error| error.to_string())?;
    let task = Task::perform(receive(reply), move |result| {
        Event::Published(
            id,
            result.and_then(|reply| match reply.as_ref() {
                worker::Reply::DraftFilePublished(note, saved) => {
                    Ok(Published(note.clone(), saved.clone()))
                }
                _ => Err("The server returned no publication receipt.".into()),
            }),
        )
    });
    Ok((pending, task))
}

fn destination_policy(source: &Source<'_>, scope: &Scope<'_>) -> bool {
    let policy = ClipboardPolicy::load();
    let allows = |policy: &ClipboardPolicy| {
        policy.problem.is_none()
            && policy.file_transfer_allowed
            && policy.remote_editor == RemoteEditorPolicy::Allow
            && source.target.workflow_route_allowed(policy)
    };
    !source.private && source.transfer_allowed && allows(scope.policy) && allows(&policy)
}

async fn receive(
    reply: async_channel::Receiver<Result<Arc<worker::Reply>, worker::Failure>>,
) -> Result<Arc<worker::Reply>, String> {
    reply
        .recv()
        .await
        .map_err(|_| {
            "The SFTP worker stopped. Check the server destination before retrying.".to_owned()
        })?
        .map_err(|error| error.message)
}

#[cfg(test)]
mod tests {
    use crate::sftp_window::{Entry, EntryKind};

    use super::*;

    #[test]
    fn sftp_ai_server_browse_navigates_only_listed_children_and_the_parent() {
        let entry = |name: &[u8], kind| Entry {
            display: files::display(&String::from_utf8_lossy(name)),
            hidden: false,
            name: EntryName::Remote(name.to_vec()),
            kind,
            size: None,
            modified: None,
            owner: None,
            group: None,
            mode: None,
        };
        let listing = Directory::sorted(
            Location::Remote(b"/home/current".to_vec()),
            vec![
                entry(b"other", EntryKind::Directory),
                entry(b"raw-\xff", EntryKind::Directory),
                entry(b"link", EntryKind::Link),
            ],
        );
        for path in [
            b"/home".as_slice(),
            b"/home/current/other",
            b"/home/current/raw-\xff",
        ] {
            assert!(browse_path_allowed(&listing, path));
        }
        for path in [b"/".as_slice(), b"/home/current/link", b"/home/not-listed"] {
            assert!(!browse_path_allowed(&listing, path));
        }
        let folder = Folder::new(b"/home/current/raw-\xff".to_vec());
        assert_eq!(folder.path, b"/home/current/raw-\xff");
        assert_eq!(folder.text, "/home/current/raw-\\xff");
    }
}
