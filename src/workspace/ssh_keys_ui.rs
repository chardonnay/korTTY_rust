//! Native SSH private-key catalog management and its staged edits.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use iced::widget::text_editor;
use iced::{Size, Task, window};
use kortty_vault::{Secret, Vault};
use regex::{Regex, RegexBuilder};

use crate::sftp_window::{Directory, EntryKind, EntryName, Location, list_local};
use crate::ssh_dialog::Password;
use crate::ssh_key_catalog::{Catalog, CommitReport, Edit, KeyDraft, KeyInput, KeySource};
use crate::vault_window::{Kind, VaultWindow};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message as Desktop, Workspace, tool_tabs, window_lifecycle};

#[cfg(test)]
mod tests;
mod transaction;
mod view;

pub(super) const TITLE: &str = "SSH Key Management";
const PAGE: usize = 80;
const PATH_LIMIT: usize = 4096;
const QUERY_LIMIT: usize = 256;
const PASSPHRASE_LIMIT: usize = 65536;
const DESCRIPTION_LIMIT: usize = 4096;

#[derive(Clone, Debug)]
pub enum Message {
    Open,
    Focus,
    Search(String),
    Page(bool),
    Select(RowId),
    Add,
    Edit,
    Delete,
    Copy,
    Name(String),
    Path(String),
    Description(text_editor::Action),
    Passphrase(Password),
    Credential(Option<String>),
    OpenVault,
    Browse,
    BrowseBack,
    BrowsePage(bool),
    BrowseUp,
    BrowseEntry(u64, usize),
    Browsed(u64, Result<Arc<Directory>, String>),
    Accept,
    Cancel,
    KeepEditing,
    Discard,
    HostedClose(u64, bool),
    Completed(Reply),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowId {
    Saved(u64),
    New(u64),
}

#[derive(Clone)]
pub struct Reply(Arc<Mutex<Option<Completed>>>);

impl fmt::Debug for Reply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SshKeyCatalogReply(..)")
    }
}

enum Completed {
    Loaded(Result<(Catalog, Vec<Key>), String>),
    Saved {
        catalog: Catalog,
        vault: Option<Vault>,
        result: Result<CommitReport, String>,
    },
}

#[derive(Default)]
pub(super) struct Tool {
    native: Option<Manager>,
    hosted: HashMap<window::Id, Manager>,
}

impl Tool {
    pub(super) fn is_open(&self) -> bool {
        self.native.is_some() || !self.hosted.is_empty()
    }

    fn route_mut(&mut self, id: window::Id) -> Option<&mut Manager> {
        if self.native.as_ref().is_some_and(|manager| manager.owns(id)) {
            self.native.as_mut()
        } else {
            self.hosted.values_mut().find(|manager| manager.owns(id))
        }
    }

    fn instance_mut(&mut self, id: window::Id) -> Option<&mut Manager> {
        if self.native.as_ref().is_some_and(|manager| manager.id == id) {
            self.native.as_mut()
        } else {
            self.hosted.get_mut(&id)
        }
    }
}

#[derive(Clone, Copy)]
enum Placement {
    Native { owner: window::Id },
    Hosted,
}

struct Manager {
    id: window::Id,
    placement: Placement,
    catalog: Option<Catalog>,
    entries: Vec<Key>,
    removed: Vec<u64>,
    next_draft: u64,
    selected: Option<RowId>,
    filter: Filter,
    page: usize,
    child: Option<Child>,
    discard: Option<Discard>,
    hosted_close: Option<u64>,
    vault_child: Option<window::Id>,
    status: Option<String>,
    loading: bool,
    saving: bool,
}

struct Key {
    id: RowId,
    name: String,
    description: String,
    path: PathEdit,
    passphrase: Option<String>,
    new_passphrase: Secret,
    dirty: bool,
}

enum PathEdit {
    Keep {
        path: Option<PathBuf>,
        managed: bool,
    },
    External(PathBuf),
    Copy(PathBuf),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Discard {
    Close,
    Quit,
}

struct Child {
    id: window::Id,
    kind: ChildKind,
}

enum ChildKind {
    Edit(Box<Form>),
    Delete { key: RowId, name: String },
}

struct Form {
    key: Option<RowId>,
    name: String,
    path: String,
    description: text_editor::Content,
    passphrase: Option<String>,
    new_passphrase: Secret,
    browser: Option<Browser>,
    browse_revision: u64,
    problem: Option<String>,
}

struct Browser {
    folder: PathBuf,
    result: Option<Arc<Directory>>,
    loading: bool,
    page: usize,
}

#[derive(Default)]
struct Filter {
    query: String,
    matcher: Matcher,
}

#[derive(Default)]
enum Matcher {
    #[default]
    All,
    Text(String),
    Glob(Regex),
}

impl Filter {
    fn new(query: String) -> Self {
        let value = query.trim();
        let matcher = if value.is_empty() {
            Matcher::All
        } else if value.contains('*') {
            let pattern = value
                .split('*')
                .map(regex::escape)
                .collect::<Vec<_>>()
                .join(".*");
            RegexBuilder::new(&format!(r"\A{pattern}\z"))
                .case_insensitive(true)
                .build()
                .map_or_else(|_| Matcher::Text(value.to_lowercase()), Matcher::Glob)
        } else {
            Matcher::Text(value.to_lowercase())
        };
        Self { query, matcher }
    }

    fn matches(&self, key: &Key) -> bool {
        let path = || key.path.path().map(Path::to_string_lossy);
        match &self.matcher {
            Matcher::All => true,
            Matcher::Text(needle) => {
                key.name.to_lowercase().contains(needle)
                    || path().is_some_and(|path| path.to_lowercase().contains(needle))
            }
            Matcher::Glob(pattern) => {
                pattern.is_match(&key.name) || path().is_some_and(|path| pattern.is_match(&path))
            }
        }
    }
}

impl PathEdit {
    fn path(&self) -> Option<&Path> {
        match self {
            Self::Keep { path, .. } => path.as_deref(),
            Self::External(path) | Self::Copy(path) => Some(path),
        }
    }

    fn managed(&self) -> bool {
        matches!(self, Self::Keep { managed: true, .. })
    }

    fn input(&self) -> KeyInput {
        match self {
            Self::Keep { .. } => KeyInput::Keep,
            Self::External(path) => KeyInput::External(path.clone()),
            Self::Copy(path) => KeyInput::Copy(path.clone()),
        }
    }
}

impl Manager {
    fn new(id: window::Id, placement: Placement) -> Self {
        Self {
            id,
            placement,
            catalog: None,
            entries: Vec::new(),
            removed: Vec::new(),
            next_draft: 0,
            selected: None,
            filter: Filter::default(),
            page: 0,
            child: None,
            discard: None,
            hosted_close: None,
            vault_child: None,
            status: None,
            loading: true,
            saving: false,
        }
    }

    fn hosted(&self) -> bool {
        matches!(self.placement, Placement::Hosted)
    }

    fn native_owner(&self) -> Option<window::Id> {
        match self.placement {
            Placement::Native { owner } => Some(owner),
            Placement::Hosted => None,
        }
    }

    fn owns(&self, id: window::Id) -> bool {
        self.id == id || self.focused_window() == id
    }

    fn busy(&self) -> bool {
        self.loading || self.saving
    }

    fn focused_window(&self) -> window::Id {
        self.child.as_ref().map_or(self.id, |child| child.id)
    }

    fn ready(&self) -> bool {
        self.catalog.is_some() && !self.saving
    }

    fn dirty(&self) -> bool {
        !self.removed.is_empty() || self.entries.iter().any(|key| key.dirty)
    }

    fn visible_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|key| self.filter.matches(key))
            .count()
    }

    fn selected(&self) -> Option<&Key> {
        self.entries
            .iter()
            .find(|key| Some(key.id) == self.selected)
    }

    fn reset_selection(&mut self) {
        if self.selected().is_none_or(|key| !self.filter.matches(key)) {
            self.selected = None;
        }
        self.page = self.page.min(self.visible_count().saturating_sub(1) / PAGE);
    }

    fn take_child(&mut self) -> Option<window::Id> {
        let child = self.child.take()?;
        (!self.hosted()).then_some(child.id)
    }

    fn form_mut(&mut self, id: window::Id) -> Option<&mut Form> {
        match &mut self.child.as_mut().filter(|child| child.id == id)?.kind {
            ChildKind::Edit(form) => Some(form),
            ChildKind::Delete { .. } => None,
        }
    }

    fn accept_child(&mut self, id: window::Id) -> Option<window::Id> {
        let child = self.child.as_mut().filter(|child| child.id == id)?;
        match &mut child.kind {
            ChildKind::Delete { key, .. } => {
                if let Some(index) = self.entries.iter().position(|entry| entry.id == *key) {
                    let removed = self.entries.remove(index);
                    if let RowId::Saved(id) = removed.id {
                        self.removed.push(id);
                    }
                }
                self.reset_selection();
            }
            ChildKind::Edit(form) if form.browser.is_none() => {
                let existing = form
                    .key
                    .and_then(|id| self.entries.iter_mut().find(|key| key.id == id));
                let edited = match form.key_data(existing.as_deref()) {
                    Ok(edited) => edited,
                    Err(problem) => {
                        form.problem = Some(problem.into());
                        return None;
                    }
                };
                if let Some(key) = existing {
                    edited.apply(key);
                    self.selected = Some(key.id);
                } else {
                    self.next_draft += 1;
                    let id = RowId::New(self.next_draft);
                    self.entries.push(edited.into_key(id));
                    self.selected = Some(id);
                }
                self.reset_selection();
            }
            ChildKind::Edit(_) => return None,
        }
        self.status = None;
        self.take_child()
    }
}

impl Form {
    fn new(key: Option<&Key>) -> Self {
        Self {
            key: key.map(|key| key.id),
            name: key.map_or_else(String::new, |key| key.name.clone()),
            path: key
                .and_then(|key| key.path.path())
                .map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
            description: key.map_or_else(text_editor::Content::new, |key| {
                text_editor::Content::with_text(&key.description)
            }),
            passphrase: key.and_then(|key| key.passphrase.clone()),
            new_passphrase: key.map_or_else(Secret::default, |key| key.new_passphrase.clone()),
            browser: None,
            browse_revision: 0,
            problem: None,
        }
    }

    fn key_data(&self, key: Option<&Key>) -> Result<EditedKey, &'static str> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Name is required.");
        }
        let path = self.path.trim();
        let same_path = key.is_some_and(|key| {
            key.path
                .path()
                .map_or(path.is_empty(), |previous| previous == Path::new(path))
        });
        if !same_path && !Path::new(path).is_absolute() {
            return Err("Choose an absolute path to the private SSH key.");
        }
        Ok(EditedKey {
            name: name.into(),
            description: self.description.text().trim().into(),
            path: (!same_path).then(|| PathEdit::External(PathBuf::from(path))),
            passphrase: self.passphrase.clone(),
            new_passphrase: self.new_passphrase.clone(),
        })
    }

    fn browse(&mut self, id: window::Id, folder: PathBuf) -> Task<Desktop> {
        self.browse_revision = self.browse_revision.wrapping_add(1);
        let revision = self.browse_revision;
        self.browser = Some(Browser {
            folder: folder.clone(),
            result: None,
            loading: true,
            page: 0,
        });
        Task::future(async move {
            wrap(
                id,
                Message::Browsed(
                    revision,
                    list_local(&folder)
                        .map(Arc::new)
                        .map_err(|error| error.to_string()),
                ),
            )
        })
    }

    fn browse_start(&mut self, id: window::Id) -> Task<Desktop> {
        let current = Path::new(self.path.trim());
        let folder = if current.is_absolute() {
            current.parent().map(Path::to_path_buf)
        } else {
            std::env::home_dir().map(|home| {
                let ssh = home.join(".ssh");
                if ssh.is_dir() { ssh } else { home }
            })
        };
        folder.map_or_else(Task::none, |folder| self.browse(id, folder))
    }
}

struct EditedKey {
    name: String,
    description: String,
    path: Option<PathEdit>,
    passphrase: Option<String>,
    new_passphrase: Secret,
}

impl EditedKey {
    fn apply(self, key: &mut Key) {
        key.dirty |= self.path.is_some()
            || self.name != key.name
            || self.description != key.description
            || self.passphrase != key.passphrase
            || *self.new_passphrase != *key.new_passphrase;
        key.name = self.name;
        key.description = self.description;
        if let Some(path) = self.path {
            key.path = path;
        }
        key.passphrase = self.passphrase;
        key.new_passphrase = self.new_passphrase;
    }

    fn into_key(self, id: RowId) -> Key {
        Key {
            id,
            name: self.name,
            description: self.description,
            path: self.path.unwrap_or(PathEdit::Keep {
                path: None,
                managed: false,
            }),
            passphrase: self.passphrase,
            new_passphrase: self.new_passphrase,
            dirty: true,
        }
    }
}

fn load() -> Result<(Catalog, Vec<Key>), String> {
    let catalog = Catalog::open_profile().map_err(|error| error.to_string())?;
    let entries = catalog_entries(&catalog);
    Ok((catalog, entries))
}

fn catalog_entries(catalog: &Catalog) -> Vec<Key> {
    catalog
        .records()
        .iter()
        .map(|record| {
            let (path, managed) = match &record.source {
                KeySource::External(path) => (Some(path.clone()), false),
                KeySource::Managed(_) => (catalog.display_path(record.id), true),
            };
            Key {
                id: RowId::Saved(record.id),
                name: record.name.clone(),
                description: record.description.clone(),
                path: PathEdit::Keep { path, managed },
                passphrase: record.passphrase.clone(),
                new_passphrase: Secret::default(),
                dirty: false,
            }
        })
        .collect()
}

impl Workspace {
    fn show_ssh_keys(&mut self, owner: window::Id) -> Task<Desktop> {
        if self.exiting || self.scheduler_draining || !self.windows.contains_key(&owner) {
            return Task::none();
        }
        if self.window_settings.open_tool_windows_as_tabs {
            if let Some(id) = self.hosted_tool_in(owner, tool_tabs::Kind::SshKeys) {
                return self.focus_hosted_tool(id).unwrap_or_else(Task::none);
            }
            let id = window::Id::unique();
            self.ssh_keys
                .hosted
                .insert(id, Manager::new(id, Placement::Hosted));
            let placed = self.host_tool(owner, tool_tabs::Kind::SshKeys, id);
            if self.hosted_tool_in(owner, tool_tabs::Kind::SshKeys) != Some(id) {
                self.ssh_keys.hosted.remove(&id);
                return placed;
            }
            return Task::batch([placed, load_task(id)]);
        }
        if let Some(manager) = &self.ssh_keys.native {
            return window::gain_focus(manager.focused_window());
        }
        let Some(owner_state) = self.windows.get_mut(&owner) else {
            return Task::none();
        };
        owner_state.cancel_paste();
        let (id, opened) = self.open_placed_dialog(
            DialogKind::SshKeyManager,
            dialog_settings(Size::new(820.0, 650.0), Size::new(580.0, 420.0)),
        );
        self.ssh_keys.native = Some(Manager::new(id, Placement::Native { owner }));
        Task::batch([opened.map(|_| Desktop::Noop), load_task(id)])
    }

    pub(super) fn ssh_keys_title(&self, id: window::Id) -> Option<&'static str> {
        let manager = self.ssh_keys.native.as_ref()?;
        if manager.id == id {
            return Some(TITLE);
        }
        let child = manager.child.as_ref().filter(|child| child.id == id)?;
        Some(match &child.kind {
            ChildKind::Delete { .. } => "Confirm Deletion",
            ChildKind::Edit(form) if form.browser.is_some() => "Select Private SSH Key",
            ChildKind::Edit(form) if form.key.is_some() => "Edit SSH Key",
            ChildKind::Edit(_) => "Add SSH Key",
        })
    }

    pub(super) fn ssh_keys_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.ssh_keys
            .native
            .as_ref()
            .filter(|manager| manager.native_owner() == Some(owner))
            .map(Manager::focused_window)
    }

    pub(super) fn hosted_ssh_keys_vault_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        let vault = self.vault_id?;
        self.ssh_keys
            .hosted
            .values()
            .any(|manager| {
                manager.vault_child == Some(vault)
                    && !self.hosted_close_locks(manager.id)
                    && self.hosted_tool_owner(manager.id) == Some(owner)
            })
            .then_some(vault)
    }

    pub(super) fn close_ssh_keys(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        let manager = self.ssh_keys.native.as_mut()?;
        if manager.child.as_ref().is_some_and(|child| child.id == id) {
            return Some(self.cancel_ssh_keys(id));
        }
        if manager.id != id {
            return None;
        }
        if manager.saving {
            return Some(Task::none());
        }
        if let Some(child) = &manager.child {
            return Some(window::gain_focus(child.id));
        }
        if manager.dirty() {
            manager.discard.get_or_insert(Discard::Close);
            return Some(Task::none());
        }
        Some(self.discard_ssh_keys(true))
    }

    pub(super) fn close_ssh_keys_owner(&mut self, owner: window::Id) -> Task<Desktop> {
        if self
            .ssh_keys
            .native
            .as_ref()
            .is_some_and(|manager| manager.native_owner() == Some(owner) && !manager.saving)
        {
            self.discard_ssh_keys(false)
        } else {
            Task::none()
        }
    }

    pub(super) fn request_ssh_keys_quit(&mut self) -> Option<Task<Desktop>> {
        let manager = self.ssh_keys.native.as_mut()?;
        if manager.saving {
            window_lifecycle::reply_to_native_quit(false);
        } else if manager.dirty() || manager.child.is_some() {
            manager.discard = Some(Discard::Quit);
        } else {
            return None;
        }
        Some(window::gain_focus(manager.focused_window()))
    }

    fn discard_ssh_keys(&mut self, refocus: bool) -> Task<Desktop> {
        let Some(manager) = self.ssh_keys.native.take() else {
            return Task::none();
        };
        let focus = manager
            .native_owner()
            .filter(|owner| refocus && self.windows.contains_key(owner))
            .map_or_else(Task::none, window::gain_focus);
        let child = manager
            .child
            .map_or_else(Task::none, |child| self.close_placed_dialog(child.id));
        let close = Task::batch([child, self.close_placed_dialog(manager.id), focus]);
        if manager.discard == Some(Discard::Quit) || self.should_exit_after_window_close() {
            Task::batch([close, self.quit()])
        } else {
            close
        }
    }

    fn cancel_ssh_keys(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(manager) = self.ssh_keys.route_mut(id).filter(|manager| {
            manager.focused_window() == id && !manager.saving && manager.hosted_close.is_none()
        }) else {
            return Task::none();
        };
        if let Some(vault) = manager
            .vault_child
            .filter(|vault| self.vault_id == Some(*vault))
        {
            return window::gain_focus(vault);
        }
        if let Some(discard) = manager.discard.take() {
            if discard == Discard::Quit {
                window_lifecycle::reply_to_native_quit(false);
            }
            return Task::none();
        }
        if let Some(form) = manager.form_mut(id)
            && form.browser.take().is_some()
        {
            form.problem = None;
            return Task::none();
        }
        if manager.child.is_some() {
            let manager_id = manager.id;
            let child = manager.take_child();
            return self.finish_ssh_key_child(manager_id, child);
        }
        self.finish_ssh_keys_manager(id)
    }

    fn finish_ssh_key_child(
        &mut self,
        manager: window::Id,
        child: Option<window::Id>,
    ) -> Task<Desktop> {
        child.map_or_else(Task::none, |child| {
            Task::batch([self.close_placed_dialog(child), window::gain_focus(manager)])
        })
    }

    fn finish_ssh_keys_manager(&mut self, id: window::Id) -> Task<Desktop> {
        if self.ssh_keys.hosted.contains_key(&id) {
            self.request_hosted_tool_close(id)
        } else if self
            .ssh_keys
            .native
            .as_ref()
            .is_some_and(|manager| manager.id == id)
        {
            self.discard_ssh_keys(true)
        } else {
            Task::none()
        }
    }

    pub(super) fn dismiss_hosted_ssh_keys(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(manager) = self.ssh_keys.hosted.get(&id) else {
            return Task::none();
        };
        if let Some(token) = manager.hosted_close {
            return if manager.busy() {
                Task::none()
            } else {
                self.hosted_tool_close_answer(id, token, false)
            };
        }
        let surface = manager.focused_window();
        self.cancel_ssh_keys(surface)
    }

    pub(super) fn hosted_ssh_keys_busy(&self, id: window::Id) -> bool {
        self.ssh_keys.hosted.get(&id).is_some_and(Manager::busy)
    }

    pub(super) fn hosted_ssh_keys_dirty(&self, id: window::Id) -> bool {
        self.ssh_keys
            .hosted
            .get(&id)
            .is_some_and(|manager| manager.dirty() || manager.child.is_some())
    }

    pub(super) fn prompt_hosted_ssh_keys_close(&mut self, id: window::Id, token: u64) {
        if let Some(manager) = self.ssh_keys.hosted.get_mut(&id) {
            manager.hosted_close = Some(token);
        }
    }

    pub(super) fn cancel_hosted_ssh_keys_close(&mut self, id: window::Id) {
        if let Some(manager) = self.ssh_keys.hosted.get_mut(&id) {
            manager.hosted_close = None;
        }
    }

    pub(super) fn remove_hosted_ssh_keys(&mut self, id: window::Id) {
        self.ssh_keys.hosted.remove(&id);
    }

    fn open_ssh_key_child(&mut self, manager_id: window::Id, kind: ChildKind) -> Task<Desktop> {
        let Some(hosted) = self
            .ssh_keys
            .instance_mut(manager_id)
            .map(|manager| manager.hosted())
        else {
            return Task::none();
        };
        let (id, opened) = if hosted {
            (window::Id::unique(), Task::none())
        } else {
            let (dialog_kind, size) = match &kind {
                ChildKind::Edit(_) => (DialogKind::SshKeyEdit, Size::new(690.0, 420.0)),
                ChildKind::Delete { .. } => (DialogKind::SshKeyDelete, Size::new(480.0, 235.0)),
            };
            let (id, opened) = self
                .open_placed_dialog(dialog_kind, dialog_settings(size, Size::new(420.0, 225.0)));
            (id, opened.map(|_| Desktop::Noop))
        };
        if let Some(manager) = self.ssh_keys.instance_mut(manager_id) {
            manager.child = Some(Child { id, kind });
        }
        opened
    }

    pub(super) fn update_ssh_keys(&mut self, id: window::Id, message: Message) -> Task<Desktop> {
        match message {
            Message::Open => return self.show_ssh_keys(id),
            Message::Completed(reply) => return self.finish_ssh_keys(id, reply),
            Message::Focus if self.vault_id == Some(id) => return window::gain_focus(id),
            Message::HostedClose(token, approve) => {
                return if self
                    .ssh_keys
                    .hosted
                    .get(&id)
                    .is_some_and(|manager| manager.hosted_close == Some(token) && !manager.busy())
                {
                    self.hosted_tool_close_answer(id, token, approve)
                } else {
                    Task::none()
                };
            }
            Message::Cancel => return self.cancel_ssh_keys(id),
            Message::KeepEditing => {
                return if self
                    .ssh_keys
                    .native
                    .as_ref()
                    .is_some_and(|manager| manager.owns(id) && manager.discard.is_some())
                {
                    self.cancel_ssh_keys(id)
                } else {
                    Task::none()
                };
            }
            _ => {}
        }
        if self.exiting || self.scheduler_draining {
            return Task::none();
        }
        let vault_ready = self
            .vault
            .as_ref()
            .is_some_and(VaultWindow::can_manage_environments);
        if let Message::Credential(Some(credential)) = &message
            && !self.vault.as_ref().is_some_and(|vault| {
                vault
                    .choices(Kind::KeyPassphrase)
                    .iter()
                    .any(|choice| &choice.id == credential)
            })
        {
            return Task::none();
        }
        let Some(manager) = self.ssh_keys.route_mut(id) else {
            return Task::none();
        };
        if matches!(message, Message::Focus) {
            let instance = manager.id;
            return if manager.hosted() {
                self.focus_hosted_tool(instance).unwrap_or_else(Task::none)
            } else {
                window::gain_focus(manager.focused_window())
            };
        }
        if manager.saving || manager.focused_window() != id || manager.hosted_close.is_some() {
            return Task::none();
        }
        if let Some(vault) = manager
            .vault_child
            .filter(|vault| self.vault_id == Some(*vault))
        {
            return window::gain_focus(vault);
        }
        if manager.discard.is_some() {
            return if matches!(message, Message::Discard) {
                self.discard_ssh_keys(true)
            } else {
                Task::none()
            };
        }
        if id == manager.id {
            if !manager.ready() {
                return Task::none();
            }
            match message {
                Message::Search(query) if query.len() <= QUERY_LIMIT => {
                    manager.filter = Filter::new(query);
                    manager.page = 0;
                    manager.reset_selection();
                }
                Message::Page(forward) => {
                    manager.page = page(manager.page, manager.visible_count(), forward)
                }
                Message::Select(id) => {
                    manager.selected = manager
                        .entries
                        .iter()
                        .find(|key| key.id == id && manager.filter.matches(key))
                        .map(|key| key.id);
                }
                Message::Add => {
                    return self.open_ssh_key_child(id, ChildKind::Edit(Box::new(Form::new(None))));
                }
                Message::Edit => {
                    if let Some(key) = manager.selected() {
                        let kind = ChildKind::Edit(Box::new(Form::new(Some(key))));
                        return self.open_ssh_key_child(id, kind);
                    }
                }
                Message::Delete => {
                    if let Some(key) = manager.selected() {
                        let kind = ChildKind::Delete {
                            key: key.id,
                            name: key.name.clone(),
                        };
                        return self.open_ssh_key_child(id, kind);
                    }
                }
                Message::Copy => {
                    if let Some(key) = manager
                        .entries
                        .iter_mut()
                        .find(|key| Some(key.id) == manager.selected)
                        && !key.path.managed()
                        && !matches!(key.path, PathEdit::Copy(_))
                        && let Some(path) = key.path.path()
                    {
                        key.path = PathEdit::Copy(path.to_path_buf());
                        key.dirty = true;
                        manager.status = Some(
                            "The key will be copied to your user directory when you save.".into(),
                        );
                    }
                }
                Message::Accept => return self.save_ssh_keys(id),
                _ => {}
            }
            return Task::none();
        }
        if matches!(message, Message::Accept) {
            let manager_id = manager.id;
            let child = manager.accept_child(id);
            return self.finish_ssh_key_child(manager_id, child);
        }
        if matches!(message, Message::OpenVault) && manager.form_mut(id).is_some() {
            let instance = manager.id;
            return self.show_ssh_keys_vault(instance);
        }
        let Some(form) = manager.form_mut(id) else {
            return Task::none();
        };
        form.update(id, message, vault_ready)
    }

    fn show_ssh_keys_vault(&mut self, id: window::Id) -> Task<Desktop> {
        let hosted = self.ssh_keys.hosted.contains_key(&id);
        let owner = if hosted {
            self.hosted_tool_owner(id)
        } else {
            self.ssh_keys
                .native
                .as_ref()
                .filter(|manager| manager.id == id)
                .and_then(Manager::native_owner)
        };
        let Some(owner) = owner else {
            return Task::none();
        };
        let shown = self.show_vault(owner);
        if hosted && let Some(vault) = self.vault_id {
            for manager in self.ssh_keys.hosted.values_mut() {
                manager.vault_child = (manager.id == id).then_some(vault);
            }
        }
        shown
    }
}

impl Form {
    fn update(&mut self, id: window::Id, message: Message, vault_ready: bool) -> Task<Desktop> {
        if let Some(browser) = &mut self.browser {
            return match message {
                Message::BrowseBack => {
                    self.browser = None;
                    self.problem = None;
                    Task::none()
                }
                Message::BrowsePage(forward) => {
                    browser.page = page(
                        browser.page,
                        browser
                            .result
                            .as_ref()
                            .map_or(0, |directory| directory.entries.len()),
                        forward,
                    );
                    Task::none()
                }
                Message::BrowseUp if !browser.loading => {
                    let parent = browser.folder.parent().map(Path::to_path_buf);
                    parent.map_or_else(Task::none, |parent| self.browse(id, parent))
                }
                Message::BrowseEntry(revision, index) if revision == self.browse_revision => {
                    let Some(directory) = &browser.result else {
                        return Task::none();
                    };
                    let Location::Local(folder) = &directory.path else {
                        return Task::none();
                    };
                    let Some(entry) = directory.entries.get(index) else {
                        return Task::none();
                    };
                    let EntryName::Local(name) = &entry.name else {
                        return Task::none();
                    };
                    let path = folder.join(name);
                    match entry.kind {
                        EntryKind::Directory => self.browse(id, path),
                        EntryKind::File
                            if path.to_str().is_some_and(|path| path.len() <= PATH_LIMIT) =>
                        {
                            self.path = path.to_string_lossy().into_owned();
                            self.browser = None;
                            self.problem = None;
                            Task::none()
                        }
                        _ => Task::none(),
                    }
                }
                Message::Browsed(revision, result) if revision == self.browse_revision => {
                    browser.loading = false;
                    match result {
                        Ok(directory) => browser.result = Some(directory),
                        Err(error) => {
                            self.problem = Some(format!("Could not open folder ({error})."))
                        }
                    }
                    Task::none()
                }
                _ => Task::none(),
            };
        }
        match message {
            Message::Name(value) if value.len() <= 256 => self.name = value,
            Message::Path(value) if value.len() <= PATH_LIMIT => self.path = value,
            Message::Description(action) if action.is_edit() => {
                let oversized = matches!(
                    &action,
                    text_editor::Action::Edit(text_editor::Edit::Paste(text))
                        if text.len() > DESCRIPTION_LIMIT
                );
                if !oversized {
                    let previous = self.description.text();
                    self.description.perform(action);
                    if self.description.text().len() <= DESCRIPTION_LIMIT {
                        self.problem = None;
                        return Task::none();
                    }
                    self.description = text_editor::Content::with_text(&previous);
                }
                self.problem = Some(format!(
                    "Description is limited to {DESCRIPTION_LIMIT} bytes."
                ));
                return Task::none();
            }
            Message::Description(action) => self.description.perform(action),
            Message::Passphrase(value)
                if vault_ready && value.as_str().len() <= PASSPHRASE_LIMIT =>
            {
                self.new_passphrase = Secret::new(value.into_string());
            }
            Message::Credential(id) => {
                self.passphrase = id;
                self.new_passphrase = Secret::default();
            }
            Message::Browse => return self.browse_start(id),
            _ => return Task::none(),
        }
        self.problem = None;
        Task::none()
    }
}

fn page(current: usize, count: usize, forward: bool) -> usize {
    if forward {
        (current + 1).min(count.saturating_sub(1) / PAGE)
    } else {
        current.saturating_sub(1)
    }
}

fn dialog_settings(size: Size, min_size: Size) -> window::Settings {
    window::Settings {
        size,
        min_size: Some(min_size),
        position: window::Position::Centered,
        exit_on_close_request: false,
        ..window::Settings::default()
    }
}

fn wrap(id: window::Id, message: Message) -> Desktop {
    Desktop::SshKeys(id, message)
}

fn load_task(id: window::Id) -> Task<Desktop> {
    Task::future(async move { completed(id, Completed::Loaded(load())) })
}

fn completed(id: window::Id, result: Completed) -> Desktop {
    wrap(
        id,
        Message::Completed(Reply(Arc::new(Mutex::new(Some(result))))),
    )
}
