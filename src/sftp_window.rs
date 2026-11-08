use std::collections::{HashSet, VecDeque};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ssh_connection::{Connected, SftpOpener};
use async_channel::Receiver;
use iced::widget::{button, checkbox, column, container, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length, Task};
use kortty_ssh::sftp::RemoteText;

use crate::clipboard_policy::RemoteEditorPolicy;
use crate::session::SessionId;
use crate::workspace::SshTarget;

mod files;
mod tree;
pub(crate) mod worker;

pub(crate) use files::nonce as staging_name;
pub(crate) use files::{LocalStamp as LocalFileStamp, list_local, validate_transfer_name};
pub(crate) use tree::local as local_tree;
pub(crate) use tree::single_file::{LocalSource, StagedDownload};
pub(crate) use tree::{FolderError, FolderTransfer, copy_scheduled as copy_scheduled_folder};
pub(crate) use worker::RemoteEditor;

const PAGE_SIZE: usize = 160;
const HISTORY_LIMIT: usize = 32;
pub(crate) const MAX_SELECTION: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Local,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EntryName {
    Local(OsString),
    Remote(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    Directory,
    File,
    Link,
    Other,
}

impl EntryKind {
    pub(crate) fn transferable(self) -> bool {
        matches!(self, Self::File | Self::Directory)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) name: EntryName,
    pub(crate) display: String,
    pub(crate) hidden: bool,
    pub(crate) kind: EntryKind,
    pub(crate) size: Option<u64>,
    pub(crate) modified: Option<u32>,
    mode: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Location {
    Local(PathBuf),
    Remote(Vec<u8>),
}

impl Location {
    pub(crate) fn display(&self) -> String {
        match self {
            Self::Local(path) => files::display(&path.to_string_lossy()),
            Self::Remote(path) => files::display(&String::from_utf8_lossy(path)),
        }
    }

    pub(crate) fn with(&self, name: &EntryName) -> Option<Self> {
        match (self, name) {
            (Self::Local(path), EntryName::Local(name)) => Some(Self::Local(path.join(name))),
            (Self::Remote(path), EntryName::Remote(name)) => {
                Some(Self::Remote(files::remote_join(path, name)))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Directory {
    pub(crate) path: Location,
    pub(crate) entries: Vec<Entry>,
}

impl Directory {
    fn sorted(path: Location, mut entries: Vec<Entry>) -> Self {
        entries.sort_by_cached_key(|entry| {
            (
                !entry.hidden,
                entry.kind != EntryKind::Directory,
                entry.display.to_lowercase(),
            )
        });
        Self { path, entries }
    }
}

#[derive(Default)]
struct Panel {
    directory: Option<Arc<Directory>>,
    path: String,
    filter: String,
    hide_hidden: bool,
    selected: HashSet<EntryName>,
    page: usize,
}

impl Panel {
    fn load(&mut self, directory: Arc<Directory>) {
        self.path = directory.path.display();
        self.directory = Some(directory);
        self.selected.clear();
        self.page = 0;
    }

    fn selected(&self) -> Option<(&Directory, &Entry)> {
        if self.selected.len() != 1 {
            return None;
        }
        let directory = self.directory.as_deref()?;
        let selected = self.selected.iter().next()?;
        Some((
            directory,
            directory
                .entries
                .iter()
                .find(|entry| &entry.name == selected)?,
        ))
    }

    fn toggle(&mut self, name: EntryName, checked: bool) {
        if !checked {
            self.selected.remove(&name);
            return;
        }
        if self.selected.len() == MAX_SELECTION
            || !self.directory.as_ref().is_some_and(|directory| {
                directory
                    .entries
                    .iter()
                    .any(|entry| entry.name == name && entry.kind.transferable())
            })
        {
            return;
        }
        if self
            .selected()
            .is_some_and(|(_, entry)| !entry.kind.transferable())
        {
            self.selected.clear();
        }
        self.selected.insert(name);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    NewFolder,
    Rename,
    Delete,
    Permissions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    Replace,
    KeepBoth,
    Skip,
}

#[derive(Debug, Clone)]
pub enum Message {
    Path(Side, String),
    Filter(Side, String),
    Hidden(Side),
    Page(Side, bool),
    Select(Side, EntryName),
    ToggleSelection(Side, EntryName, bool),
    Open(Side),
    Go(Side),
    Up(Side),
    Home(Side),
    Refresh(Side),
    Transfer(Side),
    EditRemote,
    ShowEditor(RemoteText, RemoteEditor),
    Change(Side, Change),
    EditName(String),
    ConfirmChange,
    Resolve(ConflictChoice),
    Dismiss,
    CancelTransfer,
    Reconnect,
    Close,
    ConfirmClose(bool),
    Done(u64, Result<Arc<worker::Reply>, worker::Failure>),
}

enum Prompt {
    Change {
        operation: Change,
        directory: Location,
        selected: Option<Entry>,
        input: String,
    },
    Conflict(worker::Conflict),
    Close,
}

struct Busy {
    id: u64,
    progress: Option<worker::Progress>,
    title: String,
    #[cfg(all(test, unix))]
    reply: Receiver<Result<Arc<worker::Reply>, worker::Failure>>,
}

pub struct SftpWindow {
    target: SshTarget,
    attached_to: Option<SessionId>,
    worker: Option<worker::Worker>,
    local: Panel,
    remote: Panel,
    home: Option<Vec<u8>>,
    busy: Option<Busy>,
    prompt: Option<Prompt>,
    counter: u64,
    status: Option<String>,
    history: VecDeque<String>,
    pending_transfers: VecDeque<worker::Transfer>,
    transfer_allowed: bool,
    editor_policy: RemoteEditorPolicy,
}

impl SftpWindow {
    pub fn open(
        target: SshTarget,
        ssh: Connected,
        transfer_allowed: bool,
        editor_policy: RemoteEditorPolicy,
    ) -> io::Result<(Self, Task<Message>)> {
        Self::open_connection(
            target,
            None,
            worker::Connection::Standalone(ssh),
            transfer_allowed,
            editor_policy,
            None,
        )
    }

    pub(crate) fn open_attached_at(
        target: SshTarget,
        session: SessionId,
        opener: SftpOpener,
        transfer_allowed: bool,
        editor_policy: RemoteEditorPolicy,
        path: Option<Vec<u8>>,
    ) -> io::Result<(Self, Task<Message>)> {
        Self::open_connection(
            target,
            Some(session),
            worker::Connection::Terminal(opener),
            transfer_allowed,
            editor_policy,
            path,
        )
    }

    fn open_connection(
        target: SshTarget,
        attached_to: Option<SessionId>,
        connection: worker::Connection,
        transfer_allowed: bool,
        editor_policy: RemoteEditorPolicy,
        path: Option<Vec<u8>>,
    ) -> io::Result<(Self, Task<Message>)> {
        let policy = if transfer_allowed {
            editor_policy
        } else {
            RemoteEditorPolicy::Deny
        };
        let worker = worker::Worker::start(connection, policy)?;
        Ok(Self::with_worker(
            target,
            attached_to,
            worker,
            transfer_allowed,
            policy,
            path,
        ))
    }

    pub(crate) fn with_worker(
        target: SshTarget,
        attached_to: Option<SessionId>,
        worker: worker::Worker,
        transfer_allowed: bool,
        editor_policy: RemoteEditorPolicy,
        path: Option<Vec<u8>>,
    ) -> (Self, Task<Message>) {
        let mut state = Self {
            target,
            attached_to,
            worker: None,
            local: Panel::default(),
            remote: Panel::default(),
            home: None,
            busy: None,
            prompt: None,
            counter: 0,
            status: None,
            history: VecDeque::new(),
            pending_transfers: VecDeque::new(),
            transfer_allowed,
            editor_policy,
        };
        let task = state.initialize(worker, path);
        (state, task)
    }

    fn connected(
        &mut self,
        connection: worker::Connection,
        remote_path: Option<Vec<u8>>,
    ) -> io::Result<Task<Message>> {
        let worker = worker::Worker::start(connection, self.editor_policy)?;
        Ok(self.initialize(worker, remote_path))
    }

    fn initialize(
        &mut self,
        worker: worker::Worker,
        remote_path: Option<Vec<u8>>,
    ) -> Task<Message> {
        self.worker = Some(worker);
        self.busy = None;
        self.prompt = None;
        self.pending_transfers.clear();
        let local = self
            .local
            .directory
            .as_ref()
            .and_then(|directory| match &directory.path {
                Location::Local(path) => Some(path.clone()),
                _ => None,
            });
        let remote = remote_path.or_else(|| {
            self.remote
                .directory
                .as_ref()
                .and_then(|directory| match &directory.path {
                    Location::Remote(path) => Some(path.clone()),
                    _ => None,
                })
        });
        self.dispatch(
            worker::Operation::Initial { local, remote },
            "Opening SFTP",
            None,
        )
    }

    pub fn target(&self) -> &SshTarget {
        &self.target
    }

    pub(crate) fn browse_from_sidebar(&mut self, path: Vec<u8>) -> Task<Message> {
        if self.busy.is_some() || self.prompt.is_some() {
            self.set_status("Finish the current operation before changing folders.".into());
            return Task::none();
        }
        self.dispatch(
            worker::Operation::Browse(Location::Remote(path)),
            "Listing remote files",
            None,
        )
    }

    pub fn attached_to(&self) -> Option<SessionId> {
        self.attached_to
    }

    pub(crate) fn terminal_editor_worker(&self) -> Option<&worker::Worker> {
        if self.busy.is_some() || self.prompt.is_some() {
            return None;
        }
        self.worker.as_ref().filter(|worker| !worker.is_closed())
    }

    pub(crate) fn owns_editor(&self, editor: &RemoteEditor) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| worker.owns_editor(editor))
    }

    pub fn reconnect(&mut self, target: SshTarget, ssh: Connected) -> io::Result<Task<Message>> {
        if self.target != target {
            self.remote = Panel::default();
            self.home = None;
            self.target = target;
        }
        let task = self.connected(worker::Connection::Standalone(ssh), None)?;
        self.attached_to = None;
        Ok(task)
    }

    pub fn set_status(&mut self, value: String) {
        self.status = Some(value);
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn close_confirmed(&self) -> bool {
        matches!(self.prompt, Some(Prompt::Close))
    }

    pub fn transfer_running(&self) -> bool {
        self.busy
            .as_ref()
            .is_some_and(|busy| busy.progress.is_some())
    }

    pub(crate) fn transport_busy(&self) -> bool {
        self.busy.is_some() || !self.pending_transfers.is_empty()
    }

    pub fn request_close(&mut self) -> bool {
        if self.transfer_running() {
            self.prompt = Some(Prompt::Close);
            false
        } else {
            true
        }
    }

    pub fn stop(&mut self) -> Option<Receiver<()>> {
        self.worker.take().map(worker::Worker::stop)
    }

    pub(crate) fn take_worker(&mut self) -> Option<worker::Worker> {
        self.worker.take()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Done(id, reply) => return self.finished(id, reply),
            Message::Dismiss | Message::ConfirmClose(false) => {
                if matches!(self.prompt.take(), Some(Prompt::Conflict(_))) {
                    self.pending_transfers.clear();
                }
            }
            Message::CancelTransfer if self.transfer_running() => {
                self.pending_transfers.clear();
                if let Some(worker) = self.worker.as_ref() {
                    worker.abort();
                }
                self.status = Some("Stopping transfer and closing SFTP connection…".into());
            }
            Message::EditName(value) => {
                if let Some(Prompt::Change { input, .. }) = &mut self.prompt {
                    *input = bounded(value);
                }
            }
            Message::ConfirmChange => return self.confirm_change(),
            Message::Resolve(choice) => return self.resolve(choice),
            _ if self.busy.is_some() || self.prompt.is_some() => {}
            Message::Path(side, value) => self.panel_mut(side).path = bounded(value),
            Message::Filter(side, value) => {
                let panel = self.panel_mut(side);
                panel.filter = bounded(value);
                panel.page = 0;
                panel.selected.clear();
            }
            Message::Hidden(side) => {
                let panel = self.panel_mut(side);
                panel.hide_hidden = !panel.hide_hidden;
                panel.page = 0;
                panel.selected.clear();
            }
            Message::Page(side, forward) => {
                let panel = self.panel_mut(side);
                panel.page = if forward {
                    panel.page.saturating_add(1)
                } else {
                    panel.page.saturating_sub(1)
                };
            }
            Message::Select(side, name) => {
                let selected = &mut self.panel_mut(side).selected;
                selected.clear();
                selected.insert(name);
            }
            Message::ToggleSelection(side, name, checked) if self.transfer_allowed => {
                self.panel_mut(side).toggle(name, checked);
            }
            Message::Go(side) => {
                if let Some(path) = self.entered_path(side) {
                    return self.browse(path);
                }
            }
            Message::Open(side) => {
                if let Some((directory, entry)) = self.panel(side).selected()
                    && entry.kind == EntryKind::Directory
                    && let Some(path) = directory.path.with(&entry.name)
                {
                    return self.browse(path);
                }
            }
            Message::Up(side) => {
                if let Some(directory) = self.panel(side).directory.as_ref() {
                    let path = match &directory.path {
                        Location::Local(path) => {
                            path.parent().map(|path| Location::Local(path.to_owned()))
                        }
                        Location::Remote(path) => {
                            Some(Location::Remote(files::remote_join(path, b"..")))
                        }
                    };
                    if let Some(path) = path {
                        return self.browse(path);
                    }
                }
            }
            Message::Home(Side::Local) => {
                return self.browse(Location::Local(worker::local_home()));
            }
            Message::Home(Side::Remote) => {
                if let Some(home) = &self.home {
                    return self.browse(Location::Remote(home.clone()));
                }
            }
            Message::Refresh(side) => {
                if let Some(directory) = &self.panel(side).directory {
                    return self.browse(directory.path.clone());
                }
            }
            Message::Transfer(side) if self.transfer_allowed => {
                self.pending_transfers = self.transfers(side);
                return self.start_next_transfer();
            }
            Message::EditRemote if self.editor_policy != RemoteEditorPolicy::Deny => {
                if let Some((directory, entry)) = self.remote.selected()
                    && entry.kind == EntryKind::File
                    && let Some(Location::Remote(path)) = directory.path.with(&entry.name)
                {
                    return self.dispatch(
                        worker::Operation::OpenEditor(path),
                        "Opening remote file",
                        None,
                    );
                }
            }
            Message::Change(side, operation) => self.ask_change(side, operation),
            _ => {}
        }
        Task::none()
    }

    fn panel(&self, side: Side) -> &Panel {
        match side {
            Side::Local => &self.local,
            Side::Remote => &self.remote,
        }
    }
    fn panel_mut(&mut self, side: Side) -> &mut Panel {
        match side {
            Side::Local => &mut self.local,
            Side::Remote => &mut self.remote,
        }
    }

    fn entered_path(&self, side: Side) -> Option<Location> {
        let panel = self.panel(side);
        let directory = panel.directory.as_ref()?;
        if panel.path.is_empty() || panel.path.contains('\0') {
            return None;
        }
        if panel.path == directory.path.display() {
            return Some(directory.path.clone());
        }
        match &directory.path {
            Location::Local(path) => Some(Location::Local(path.join(&panel.path))),
            Location::Remote(path) => {
                let entered = panel.path.as_bytes();
                if entered == b"~" {
                    return self.home.clone().map(Location::Remote);
                }
                let entered = if let Some(relative) = entered.strip_prefix(b"~/") {
                    files::remote_join(self.home.as_deref()?, relative)
                } else if entered.starts_with(b"/") {
                    entered.to_vec()
                } else {
                    files::remote_join(path, entered)
                };
                Some(Location::Remote(entered))
            }
        }
    }

    fn browse(&mut self, location: Location) -> Task<Message> {
        self.dispatch(
            worker::Operation::Browse(location),
            "Loading directory",
            None,
        )
    }

    fn transfers(&self, source: Side) -> VecDeque<worker::Transfer> {
        let source_panel = self.panel(source);
        let target = match source {
            Side::Local => &self.remote,
            Side::Remote => &self.local,
        };
        let (Some(directory), Some(target)) = (&source_panel.directory, &target.directory) else {
            return VecDeque::new();
        };
        directory
            .entries
            .iter()
            .filter_map(|entry| {
                if !source_panel.selected.contains(&entry.name) || !entry.kind.transferable() {
                    return None;
                }
                Some(worker::Transfer {
                    source: directory.path.with(&entry.name)?,
                    target_directory: target.path.clone(),
                    name: entry.name.clone(),
                    kind: entry.kind,
                    title: entry.display.clone(),
                    size: (entry.kind == EntryKind::File)
                        .then_some(entry.size)
                        .flatten(),
                })
            })
            .collect()
    }

    fn start_next_transfer(&mut self) -> Task<Message> {
        self.pending_transfers
            .pop_front()
            .map_or_else(Task::none, |transfer| {
                self.start_transfer(transfer, worker::Destination::Check)
            })
    }

    fn start_transfer(
        &mut self,
        transfer: worker::Transfer,
        destination: worker::Destination,
    ) -> Task<Message> {
        if !self.transfer_allowed {
            self.pending_transfers.clear();
            return Task::none();
        }
        let progress = worker::Progress::new(transfer.size);
        let title = transfer.title.clone();
        let task = self.dispatch(
            worker::Operation::Copy {
                transfer,
                destination,
                progress: progress.clone(),
            },
            &title,
            Some(progress),
        );
        if self.busy.is_none() {
            self.pending_transfers.clear();
        }
        task
    }

    fn resolve(&mut self, choice: ConflictChoice) -> Task<Message> {
        if !matches!(self.prompt, Some(Prompt::Conflict(_))) {
            return Task::none();
        }
        let Some(Prompt::Conflict(conflict)) = self.prompt.take() else {
            return Task::none();
        };
        match choice {
            ConflictChoice::Replace if conflict.can_replace => self.start_transfer(
                conflict.transfer,
                worker::Destination::Replace(conflict.existing),
            ),
            ConflictChoice::KeepBoth => {
                self.start_transfer(conflict.transfer, worker::Destination::KeepBoth)
            }
            _ => {
                self.status = Some("Existing entry kept".into());
                self.start_next_transfer()
            }
        }
    }

    fn ask_change(&mut self, side: Side, operation: Change) {
        let panel = self.panel(side);
        let Some(directory) = &panel.directory else {
            return;
        };
        let selected = panel.selected().map(|(_, entry)| entry.clone());
        if operation != Change::NewFolder && selected.is_none() {
            return;
        }
        let input = match (operation, &selected) {
            (Change::Rename, Some(entry)) => entry.display.clone(),
            (Change::Permissions, Some(entry)) => entry
                .mode
                .map_or_else(String::new, |mode| format!("{:03o}", mode & 0o777)),
            _ => String::new(),
        };
        self.prompt = Some(Prompt::Change {
            operation,
            directory: directory.path.clone(),
            selected,
            input,
        });
    }

    fn confirm_change(&mut self) -> Task<Message> {
        if !matches!(self.prompt, Some(Prompt::Change { .. })) {
            return Task::none();
        }
        let Some(Prompt::Change {
            operation,
            directory,
            selected,
            input,
        }) = self.prompt.take()
        else {
            return Task::none();
        };
        self.dispatch(
            worker::Operation::Change {
                directory,
                operation,
                selected,
                input,
            },
            "Changing file",
            None,
        )
    }

    fn dispatch(
        &mut self,
        operation: worker::Operation,
        title: &str,
        progress: Option<worker::Progress>,
    ) -> Task<Message> {
        let Some(worker) = self.worker.as_ref() else {
            return Task::none();
        };
        if self.busy.is_some() {
            return Task::none();
        }
        let receiver = match worker.submit(operation) {
            Ok(receiver) => receiver,
            Err(error) => {
                if error.kind() == io::ErrorKind::BrokenPipe {
                    self.worker.take();
                }
                self.status = Some(error.to_string());
                return Task::none();
            }
        };
        self.counter = self.counter.wrapping_add(1);
        let id = self.counter;
        self.busy = Some(Busy {
            id,
            progress,
            title: title.into(),
            #[cfg(all(test, unix))]
            reply: receiver.clone(),
        });
        Task::future(async move {
            Message::Done(
                id,
                receiver
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err(worker::Failure::disconnected("SFTP worker ended"))),
            )
        })
    }

    fn finished(
        &mut self,
        id: u64,
        reply: Result<Arc<worker::Reply>, worker::Failure>,
    ) -> Task<Message> {
        if self.busy.as_ref().is_none_or(|busy| busy.id != id) {
            return Task::none();
        }
        let Some(busy) = self.busy.take() else {
            return Task::none();
        };
        match reply {
            Ok(reply) => match reply.as_ref() {
                worker::Reply::Initial {
                    local,
                    remote,
                    home,
                } => {
                    self.local.load(local.clone());
                    self.remote.load(remote.clone());
                    self.home = Some(home.clone());
                    self.status = None;
                }
                worker::Reply::Listed(directory) => self.load_directory(directory.clone()),
                worker::Reply::EditorOpened(document) => {
                    if let Some(editor) = self.worker.as_ref().and_then(worker::Worker::editor) {
                        self.status = None;
                        return Task::done(Message::ShowEditor(document.clone(), editor));
                    }
                    self.status = Some("The original SFTP connection closed.".into());
                }
                worker::Reply::EditorSaved(_) => {}
                worker::Reply::Changed { directory, note } => {
                    if let Some(directory) = directory {
                        self.load_directory(directory.clone());
                    }
                    self.status = Some(note.clone());
                    if busy.progress.is_some() {
                        self.remember(note.clone());
                        return self.start_next_transfer();
                    }
                }
                worker::Reply::Conflict(conflict) => {
                    self.prompt = Some(Prompt::Conflict(conflict.clone()))
                }
            },
            Err(error) => {
                if error.disconnected {
                    self.worker.take();
                }
                if busy.progress.is_some() {
                    self.remember(format!("{} · {}", busy.title, error.message));
                    self.pending_transfers.clear();
                }
                self.status = Some(error.message);
            }
        }
        Task::none()
    }

    fn load_directory(&mut self, directory: Arc<Directory>) {
        let side = match directory.path {
            Location::Local(_) => Side::Local,
            Location::Remote(_) => Side::Remote,
        };
        self.panel_mut(side).load(directory);
        self.status = None;
    }

    fn remember(&mut self, message: String) {
        if self.history.len() == HISTORY_LIMIT {
            self.history.pop_back();
        }
        self.history.push_front(message);
    }

    pub fn view(&self) -> Element<'_, Message> {
        if let Some(prompt) = &self.prompt {
            return self.prompt_view(prompt);
        }
        let mut top = row![text(self.target.label()).size(18)].spacing(8);
        let disconnected = self.worker.as_ref().is_none_or(worker::Worker::is_closed);
        if disconnected {
            let label = if self.attached_to.is_some() {
                "Separate login…"
            } else {
                "Reconnect…"
            };
            top = top.push(button(label).on_press(Message::Reconnect));
        }
        if self.transfer_running() {
            top = top.push(button("Cancel transfer").on_press(Message::CancelTransfer));
        }
        top = top.push(button("Close").on_press(Message::Close));
        let mut content = column![
            top,
            row![self.panel_view(Side::Local), self.panel_view(Side::Remote)]
                .spacing(12)
                .height(Length::Fill)
        ]
        .padding(12)
        .spacing(8);
        if let Some(busy) = &self.busy {
            let label = busy.progress.as_ref().map_or_else(
                || busy.title.clone(),
                |progress| format!("{} · {}", busy.title, progress.label()),
            );
            let label = match self.pending_transfers.len() {
                0 => label,
                remaining => format!("{label} · {remaining} waiting"),
            };
            content = content.push(text(label));
        }
        if let Some(status) = &self.status {
            content = content.push(text(status));
        } else if disconnected && self.attached_to.is_some() {
            content = content.push(text("The terminal SFTP channel closed."));
        }
        if !self.history.is_empty() {
            let rows = self
                .history
                .iter()
                .fold(column![text("Transfers").size(15)], |rows, event| {
                    rows.push(text(event).size(12))
                });
            content = content.push(scrollable(rows).height(72));
        }
        content.height(Length::Fill).into()
    }

    fn panel_view(&self, side: Side) -> Element<'_, Message> {
        let panel = self.panel(side);
        let available = self.worker.is_some() && self.busy.is_none() && panel.directory.is_some();
        let filter = panel.filter.to_lowercase();
        let visible = |entry: &&Entry| {
            (!panel.hide_hidden || !entry.hidden)
                && (filter.is_empty() || entry.display.to_lowercase().contains(&filter))
        };
        let entries = panel
            .directory
            .as_ref()
            .map_or(&[][..], |directory| directory.entries.as_slice());
        let count = entries.iter().filter(visible).count();
        let last_page = count.saturating_sub(1) / PAGE_SIZE;
        let page = panel.page.min(last_page);
        let rows = entries
            .iter()
            .filter(visible)
            .skip(page * PAGE_SIZE)
            .take(PAGE_SIZE)
            .fold(column![].spacing(3), |rows, entry| {
                let kind = match entry.kind {
                    EntryKind::Directory => "Folder",
                    EntryKind::File => "File",
                    EntryKind::Link => "Link",
                    EntryKind::Other => "Other",
                };
                let size = entry
                    .size
                    .map_or_else(|| "—".into(), |size| size.to_string());
                let label = row![
                    text(&entry.display).width(Length::Fill),
                    text(kind).width(58),
                    text(size).width(90)
                ]
                .spacing(6);
                let selected = panel.selected.contains(&entry.name);
                let toggle = available
                    && self.transfer_allowed
                    && entry.kind.transferable()
                    && (selected || panel.selected.len() < MAX_SELECTION);
                rows.push(
                    row![
                        checkbox(selected && entry.kind.transferable()).on_toggle_maybe(
                            toggle.then_some(move |checked| Message::ToggleSelection(
                                side,
                                entry.name.clone(),
                                checked
                            )),
                        ),
                        button(label)
                            .style(if selected {
                                button::primary
                            } else {
                                button::secondary
                            })
                            .width(Length::Fill)
                            .on_press_maybe(
                                available.then(|| Message::Select(side, entry.name.clone())),
                            ),
                    ]
                    .spacing(5)
                    .align_y(Alignment::Center),
                )
            });
        let selected = panel.selected().map(|(_, entry)| entry);
        let other = if side == Side::Local {
            &self.remote
        } else {
            &self.local
        };
        let transfer = available
            && self.transfer_allowed
            && other.directory.is_some()
            && (panel.selected.len() > 1
                || selected.is_some_and(|entry| entry.kind.transferable()));
        let paths = row![
            button("Home").on_press_maybe(available.then_some(Message::Home(side))),
            button("Up").on_press_maybe(available.then_some(Message::Up(side))),
            text_input("Path", &panel.path)
                .on_input(move |path| Message::Path(side, path))
                .on_submit_maybe(available.then_some(Message::Go(side))),
            button("Go").on_press_maybe(available.then_some(Message::Go(side)))
        ]
        .spacing(4);
        let mut actions = row![
                button("Open").on_press_maybe(
                    (available && selected.is_some_and(|entry| entry.kind == EntryKind::Directory))
                        .then_some(Message::Open(side))
                ),
                button("Refresh").on_press_maybe(available.then_some(Message::Refresh(side))),
                button("New folder")
                    .on_press_maybe(available.then_some(Message::Change(side, Change::NewFolder))),
                button("Rename").on_press_maybe(
                    (available && selected.is_some())
                        .then_some(Message::Change(side, Change::Rename))
                ),
                button("Delete").on_press_maybe(
                    (available && selected.is_some())
                        .then_some(Message::Change(side, Change::Delete))
                ),
                button("Rights").on_press_maybe(
                    (available
                        && selected.is_some_and(
                            |entry| entry.mode.is_some() && entry.kind != EntryKind::Link
                        ))
                    .then_some(Message::Change(side, Change::Permissions))
                ),
                button(if side == Side::Local {
                    "Upload"
                } else {
                    "Download"
                })
                .on_press_maybe(transfer.then_some(Message::Transfer(side))),
            ]
        .spacing(4);
        if side == Side::Remote {
            let can_edit = available
                && self.editor_policy != RemoteEditorPolicy::Deny
                && selected.is_some_and(|entry| entry.kind == EntryKind::File);
            actions = actions.push(
                button(if self.editor_policy == RemoteEditorPolicy::ReadOnly {
                    "View text"
                } else {
                    "Edit text"
                })
                .on_press_maybe(can_edit.then_some(Message::EditRemote)),
            );
        }
        let header = if side == Side::Local {
            "Local"
        } else {
            "Server"
        };
        container(
            column![
                text(if panel.selected.len() > 1 {
                    format!("{header} · {} selected", panel.selected.len())
                } else {
                    header.into()
                })
                .size(19),
                paths,
                row![
                    text_input("Filter", &panel.filter)
                        .on_input(move |query| Message::Filter(side, query)),
                    button(if panel.hide_hidden {
                        "Show hidden"
                    } else {
                        "Hide hidden"
                    })
                    .on_press(Message::Hidden(side))
                ]
                .spacing(4),
                scrollable(rows).height(Length::Fill),
                row![
                    button("Previous")
                        .on_press_maybe((page > 0).then_some(Message::Page(side, false))),
                    text(format!("{count}/{} · page {}", entries.len(), page + 1)),
                    button("Next")
                        .on_press_maybe((page < last_page).then_some(Message::Page(side, true)))
                ]
                .spacing(4),
                scrollable(actions).direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::new()
                )),
            ]
            .spacing(6)
            .height(Length::Fill),
        )
        .width(Length::Fill)
        .into()
    }

    fn prompt_view<'a>(&self, prompt: &'a Prompt) -> Element<'a, Message> {
        match prompt {
            Prompt::Close => column![
                text("Cancel the running transfer and close?").size(20),
                row![
                    button("Keep open").on_press(Message::ConfirmClose(false)),
                    button("Cancel transfer and close").on_press(Message::ConfirmClose(true))
                ]
                .spacing(8)
            ]
            .padding(16)
            .spacing(12)
            .into(),
            Prompt::Conflict(conflict) => column![
                text("Destination already exists").size(20),
                text(conflict.description()),
                row![
                    button("Skip").on_press(Message::Resolve(ConflictChoice::Skip)),
                    button("Keep both").on_press(Message::Resolve(ConflictChoice::KeepBoth)),
                    button("Replace").on_press_maybe(
                        conflict
                            .can_replace
                            .then_some(Message::Resolve(ConflictChoice::Replace))
                    ),
                    button("Cancel").on_press(Message::Dismiss),
                ]
                .spacing(8)
            ]
            .padding(16)
            .spacing(12)
            .into(),
            Prompt::Change {
                operation,
                input,
                selected,
                ..
            } => {
                let title = match operation {
                    Change::NewFolder => "New folder",
                    Change::Rename => "Rename entry",
                    Change::Delete => "Delete entry?",
                    Change::Permissions => "Change permissions",
                };
                let mut view = column![text(title).size(20)].padding(16).spacing(12);
                if let Some(entry) = selected {
                    view = view.push(text(&entry.display));
                }
                if *operation != Change::Delete {
                    view = view.push(
                        text_input(
                            if *operation == Change::Permissions {
                                "Three octal digits"
                            } else {
                                "Name"
                            },
                            input,
                        )
                        .on_input(Message::EditName)
                        .on_submit(Message::ConfirmChange),
                    );
                }
                view.push(
                    row![
                        button("Cancel").on_press(Message::Dismiss),
                        button(if *operation == Change::Delete {
                            "Delete"
                        } else {
                            "Apply"
                        })
                        .on_press(Message::ConfirmChange)
                    ]
                    .spacing(8),
                )
                .into()
            }
        }
    }
}

fn bounded(value: String) -> String {
    if value.len() <= 4096 {
        value
    } else {
        String::new()
    }
}

fn parent(path: &Path) -> io::Result<&Path> {
    path.parent()
        .ok_or_else(|| io::Error::other("file has no parent directory"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    impl SftpWindow {
        /// Reads the actual worker response when the test does not poll Iced tasks.
        pub(crate) fn poll_worker_reply_for_test(&self) -> Option<Message> {
            let busy = self.busy.as_ref()?;
            let result = match busy.reply.try_recv() {
                Ok(result) => result,
                Err(async_channel::TryRecvError::Empty) => return None,
                Err(async_channel::TryRecvError::Closed) => {
                    Err(worker::Failure::disconnected("SFTP worker ended"))
                }
            };
            Some(Message::Done(busy.id, result))
        }
    }

    #[test]
    fn transfer_selection_keeps_files_and_folders_but_single_actions_require_one() {
        let entry = |name: &str, kind| Entry {
            name: EntryName::Remote(name.as_bytes().to_vec()),
            display: name.into(),
            hidden: false,
            kind,
            size: None,
            modified: None,
            mode: None,
        };
        let folder = entry("folder", EntryKind::Directory);
        let file = entry("file", EntryKind::File);
        let link = entry("link", EntryKind::Link);
        let directory = Arc::new(Directory::sorted(
            Location::Remote(b"/source".to_vec()),
            vec![folder.clone(), file.clone(), link.clone()],
        ));
        let mut panel = Panel::default();
        panel.load(directory.clone());
        panel.toggle(folder.name.clone(), true);
        assert_eq!(panel.selected().unwrap().1.kind, EntryKind::Directory);
        panel.toggle(file.name.clone(), true);
        panel.toggle(link.name.clone(), true);
        assert_eq!(panel.selected.len(), 2);
        assert!(panel.selected().is_none());
        panel.toggle(folder.name, false);
        assert_eq!(panel.selected().unwrap().1.name, file.name);
        panel.load(directory);
        assert!(panel.selected.is_empty());
    }
}
