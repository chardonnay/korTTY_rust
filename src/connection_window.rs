use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use iced::keyboard::Modifiers;
use iced::{Point, Task};

use crate::connection_store::custom_os_badges::Catalog;
use crate::connection_store::{Connection, Search, Store, TabColor};
use crate::terminal_effects::Effect;

mod color;
mod form;
mod os_badge;
pub(crate) mod overview;
mod selection;
#[cfg(test)]
mod tests;
mod tree_context;
mod view;
use color::ColorDraft;
use form::Draft;
pub use form::Edit;
use selection::{RowKey, Selection};
pub(crate) use view::ViewChoices;

#[derive(Clone, Debug)]
pub enum Message {
    Search(String),
    Scope(Scope),
    Select(u64),
    SelectGroup(String),
    OpenTreeContext(RowKey, Point),
    TreeContext(tree_context::Message),
    ToggleGroup(String),
    NewGroup,
    RenameGroup,
    DeleteGroup,
    EditGroupColor,
    GroupColorEnabled(bool),
    GroupColorInput(String),
    GroupName(String),
    CommitGroup,
    CancelGroup,
    DraftGroup(Option<String>),
    ToggleFavorite,
    ToggleOsBadgePicker(u64),
    SetOsBadge(u64, Option<String>),
    ManageOsBadges,
    QuickName(String),
    QuickTag(String),
    SaveQuick,
    CancelQuick,
    New,
    EditSelected,
    Duplicate,
    Edit(Edit),
    Save,
    CancelEdit,
    Delete,
    ConfirmDelete(bool),
    Reload,
    ImportConnections,
    ExportConnections,
    ExportOverview,
    Completed(Result<Box<Persisted>, String>),
    Connect(bool),
    OpenVault,
    Close,
    KeepEditing,
    DiscardAndClose,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    #[default]
    All,
    Favorites,
    Recent,
    Open,
    Group(String),
    Tag(String),
}

#[derive(Clone, Debug)]
pub struct Persisted {
    store: Store,
    selection: SelectionChange,
    scope: Option<Scope>,
    status: &'static str,
    folder_rename: Option<FolderRename>,
}

#[derive(Clone, Copy, Debug)]
enum SelectionChange {
    Preserve,
    Replace(Option<u64>),
}

#[derive(Clone, Debug)]
pub(crate) struct FolderRename {
    pub(crate) old: String,
    pub(crate) new: String,
}

impl Persisted {
    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    pub(crate) fn folder_rename(&self) -> Option<&FolderRename> {
        self.folder_rename.as_ref()
    }

    pub(crate) fn saved(store: Store, id: u64) -> Self {
        Self {
            store,
            selection: SelectionChange::Replace(Some(id)),
            scope: None,
            status: "Connection saved.",
            folder_rename: None,
        }
    }

    fn preserving_selection(store: Store, status: &'static str) -> Self {
        Self {
            store,
            selection: SelectionChange::Preserve,
            scope: None,
            status,
            folder_rename: None,
        }
    }
}

#[derive(Clone)]
enum FolderDialog {
    Create {
        parent: Option<String>,
        name: String,
    },
    Rename {
        path: String,
        name: String,
    },
    Delete {
        path: String,
        count: usize,
    },
    Color {
        path: String,
        original: Option<TabColor>,
        draft: ColorDraft,
    },
}

struct QuickEdit {
    name: String,
    tag: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialChoice {
    pub id: String,
    pub label: String,
    pub username: String,
}

impl fmt::Display for CredentialChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

impl CredentialChoice {
    fn options(
        credentials: &[Self],
        current: &str,
        allow_empty: bool,
    ) -> (Vec<Self>, Option<Self>) {
        let mut choices = credentials.to_vec();
        if allow_empty {
            choices.insert(
                0,
                Self {
                    id: String::new(),
                    label: "No stored passphrase".into(),
                    username: String::new(),
                },
            );
        }
        if !current.is_empty() && choices.iter().all(|credential| credential.id != current) {
            choices.push(Self {
                id: current.to_owned(),
                label: "Existing vault reference (locked or missing)".into(),
                username: String::new(),
            });
        }
        let selected = choices
            .iter()
            .find(|credential| credential.id == current)
            .cloned();
        (choices, selected)
    }
}

pub struct ConnectionWindow {
    store: Store,
    custom_badges: Arc<Catalog>,
    path: PathBuf,
    selection: Selection,
    modifiers: Modifiers,
    tree_context: tree_context::TreeContext,
    badge_picker: Option<u64>,
    scope: Scope,
    collapsed: BTreeSet<String>,
    open_counts: Option<BTreeMap<u64, usize>>,
    draft: Option<Draft>,
    draft_group: Option<String>,
    draft_conflict: bool,
    quick: Option<QuickEdit>,
    folder: Option<FolderDialog>,
    query: String,
    notice: Option<String>,
    confirming_delete: Vec<u64>,
    confirming_close: bool,
    busy: bool,
}

pub(crate) struct OverviewSources<'a> {
    pub(crate) store: &'a Store,
    pub(crate) custom_badges: &'a Catalog,
    pub(crate) selected: Vec<u64>,
    pub(crate) shown: Vec<u64>,
    pub(crate) all: Vec<u64>,
}

impl ConnectionWindow {
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let store = Store::open(path.clone()).map_err(|error| error.to_string())?;
        let profile_root = path
            .parent()
            .ok_or_else(|| "The connection store has no profile directory.".to_owned())?;
        let (custom_badges, notice) = match Catalog::load(profile_root) {
            Ok(catalog) => (catalog, None),
            Err(error) => (
                Catalog::empty(profile_root),
                Some(format!("Could not load custom OS symbols ({error}).")),
            ),
        };
        let mut selection = Selection::default();
        selection.replace(
            store
                .connections()
                .first()
                .map(|entry| RowKey::Connection(entry.id)),
        );
        Ok(Self {
            selection,
            modifiers: Modifiers::empty(),
            tree_context: tree_context::TreeContext::default(),
            badge_picker: None,
            scope: Scope::All,
            collapsed: BTreeSet::new(),
            open_counts: None,
            store,
            custom_badges: Arc::new(custom_badges),
            path,
            draft: None,
            draft_group: None,
            draft_conflict: false,
            quick: None,
            folder: None,
            query: String::new(),
            notice,
            confirming_delete: Vec::new(),
            confirming_close: false,
            busy: false,
        })
    }

    pub(crate) fn custom_badges(&self) -> Arc<Catalog> {
        Arc::clone(&self.custom_badges)
    }

    pub(crate) fn replace_custom_badges(&mut self, catalog: Arc<Catalog>) {
        self.custom_badges = catalog;
    }

    pub(crate) fn set_notice(&mut self, text: String) {
        self.notice = Some(text);
    }

    fn valid_os_badge(&self, reference: Option<&str>) -> bool {
        reference.is_none_or(|id| os_badge::Badge::resolve(Some(id), &self.custom_badges).is_some())
    }

    fn connection_badge(&self, id: u64) -> Option<os_badge::Badge<'_>> {
        os_badge::Badge::resolve(self.store.os_badge_id(id), &self.custom_badges)
    }

    pub fn selected(&self) -> Option<&Connection> {
        if !self.can_browse() {
            return None;
        }
        self.single_connection()
    }

    fn single_connection(&self) -> Option<&Connection> {
        self.selection
            .single_connection()
            .and_then(|id| self.store.get(id))
    }

    pub(crate) fn connect_selection(&self) -> Option<&Connection> {
        if !self.can_browse() {
            return None;
        }
        if let Some(connection) = self.single_connection() {
            return Some(connection);
        }
        let id = self.selected_connection_ids().into_iter().next()?;
        self.store.get(id)
    }

    fn selected_connection_ids(&self) -> Vec<u64> {
        if !self.can_browse() {
            return Vec::new();
        }
        if let Some(id) = self.selection.single_connection() {
            return vec![id];
        }
        if self.selection.connections().next().is_none() {
            return Vec::new();
        }
        self.visual_rows(self.visible_connections())
            .into_iter()
            .filter_map(|row| match row {
                selection::VisualRow::Connection(entry, _)
                    if self.selection.contains(&RowKey::Connection(entry.id)) =>
                {
                    Some(entry.id)
                }
                _ => None,
            })
            .collect()
    }

    pub(crate) fn set_modifiers(&mut self, modifiers: Modifiers) {
        self.modifiers = modifiers;
    }

    pub(crate) fn dismiss_tree_context(&mut self) {
        self.tree_context.dismiss();
    }

    fn select_context_group(&mut self, path: &str) -> bool {
        let row = RowKey::Group(path.to_owned());
        if !self.can_browse() || !self.selection.contains(&row) {
            return false;
        }
        self.selection.replace(Some(row));
        true
    }

    pub(crate) fn overview_sources(&self) -> Option<OverviewSources<'_>> {
        if !self.can_export_overview() {
            return None;
        }
        Some(OverviewSources {
            store: &self.store,
            custom_badges: &self.custom_badges,
            selected: self.selected_export_ids(),
            shown: self
                .visible_connections()
                .into_iter()
                .map(|connection| connection.id)
                .collect(),
            all: self
                .store
                .connections()
                .iter()
                .map(|connection| connection.id)
                .collect(),
        })
    }

    fn can_export_overview(&self) -> bool {
        self.can_browse() && !self.store.connections().is_empty()
    }

    pub(crate) fn can_import_connections(&self) -> bool {
        self.can_browse()
    }

    pub(crate) fn export_connections_sources(&self) -> Option<(&Store, Vec<u64>)> {
        if !self.can_browse() {
            return None;
        }
        let ids = self.selected_export_ids();
        (!ids.is_empty()).then_some((&self.store, ids))
    }

    fn can_export_connections(&self) -> bool {
        self.can_browse()
            && (self.selection.connections().next().is_some()
                || self
                    .selection
                    .single_group()
                    .is_some_and(|path| self.store.in_group(path).next().is_some()))
    }

    fn selected_export_ids(&self) -> Vec<u64> {
        let selected = self.selected_connection_ids();
        if !selected.is_empty() {
            return selected;
        }
        self.selection
            .single_group()
            .map(|path| self.store.in_group(path).map(|entry| entry.id).collect())
            .unwrap_or_default()
    }

    fn visible_connections(&self) -> Vec<&Connection> {
        let search = Search::new(&self.query);
        if self.scope == Scope::Recent {
            return self
                .store
                .recent()
                .filter(|entry| search.manager(entry))
                .collect();
        }
        let mut rows = self
            .store
            .connections()
            .iter()
            .filter(|entry| self.scope_accepts(entry) && search.manager(entry))
            .collect::<Vec<_>>();
        rows.sort_by_cached_key(|entry| entry.name.to_lowercase());
        rows
    }

    pub(crate) fn editing_terminal_effect(&self) -> Option<Effect> {
        self.draft.as_ref()?.terminal_effect()
    }

    pub fn request_close(&mut self) -> bool {
        self.dismiss_tree_context();
        if self.busy {
            self.notice = Some("A profile update is in progress.".into());
            return false;
        }
        self.confirming_close = self.tree_context.has_unsaved_prompt()
            || self.draft.as_ref().is_some_and(|draft| draft.changed)
            || self.quick.is_some()
            || self.folder.as_ref().is_some_and(|action| match action {
                FolderDialog::Create { name, .. } => !name.is_empty(),
                FolderDialog::Rename { path, name } => path.rsplit('/').next() != Some(name),
                FolderDialog::Delete { .. } => false,
                FolderDialog::Color {
                    original, draft, ..
                } => draft.value() != Ok(*original),
            });
        !self.confirming_close
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn update_store(&mut self, store: Store) {
        if self.busy || self.store.path() != store.path() {
            return;
        }
        let edit_id = self
            .draft
            .as_ref()
            .map(Draft::id)
            .filter(|id| *id != 0)
            .or(self
                .selection
                .single_connection()
                .filter(|_| self.quick.is_some()));
        if let Some(id) = edit_id
            && (self.store.get(id) != store.get(id)
                || self.store.group_of(id) != store.group_of(id))
        {
            self.draft_conflict = true;
            self.notice = Some("This connection changed in another window. Discard the draft and reopen it before saving.".into());
        }
        if self.confirming_delete.iter().any(|&id| {
            self.store.get(id) != store.get(id) || self.store.group_of(id) != store.group_of(id)
        }) {
            self.confirming_delete.clear();
            self.notice =
                Some("The selection changed. Review the connections before deleting them.".into());
        }
        if let Some(FolderDialog::Delete { path, .. }) = &self.folder
            && (!self.store.groups().eq(store.groups())
                || !self.store.in_group(path).eq(store.in_group(path)))
        {
            self.folder = None;
            self.notice =
                Some("The folder changed. Review its contents before deleting it.".into());
        }
        if let Some(FolderDialog::Color { path, original, .. }) = &self.folder
            && (!store.has_group(path) || store.group_color(path) != *original)
        {
            self.folder = None;
            self.notice =
                Some("The folder's tab color changed. Reopen the editor before saving.".into());
        }
        self.store = store;
        self.reconcile_selection();
    }

    pub fn set_open_counts(&mut self, counts: BTreeMap<u64, usize>) {
        self.open_counts = Some(counts);
        self.reconcile_selection();
    }

    fn can_browse(&self) -> bool {
        !self.busy
            && !self.confirming_close
            && self.confirming_delete.is_empty()
            && !self.tree_context.has_prompt()
            && self.draft.is_none()
            && self.folder.is_none()
            && self.quick.is_none()
    }

    fn scope_accepts(&self, connection: &Connection) -> bool {
        match &self.scope {
            Scope::All => true,
            Scope::Favorites => self.store.is_favorite(connection.id),
            Scope::Recent => self.store.recent().any(|entry| entry.id == connection.id),
            Scope::Open => self.open_count(connection.id) > 0,
            Scope::Group(group) => self.store.is_in_group(connection.id, group),
            Scope::Tag(tag) => connection.tag.to_lowercase() == tag.to_lowercase(),
        }
    }

    fn open_count(&self, id: u64) -> usize {
        self.open_counts
            .as_ref()
            .and_then(|counts| counts.get(&id))
            .copied()
            .unwrap_or(0)
    }

    fn reconcile_selection(&mut self) {
        self.dismiss_tree_context();
        if let Scope::Group(path) = &self.scope
            && !self.store.has_group(path)
        {
            self.scope = Scope::All;
        }
        if self.draft.is_none() && self.quick.is_none() {
            let visible: BTreeSet<_> = self.visible_row_keys().into_iter().collect();
            self.selection.retain(|row| {
                visible.contains(row)
                    || matches!((row, &self.scope),
                        (RowKey::Group(folder), Scope::Group(root)) if folder == root)
            });
        }
    }

    fn set_scope(&mut self, scope: Scope) {
        self.badge_picker = None;
        if let Scope::Group(path) = &scope {
            self.selection.replace(Some(RowKey::Group(path.clone())));
        } else {
            self.selection
                .retain(|row| matches!(row, RowKey::Connection(_)));
        }
        self.scope = scope;
        self.reconcile_selection();
    }

    fn active_group(&self) -> Option<&str> {
        self.selection.single_group().or(match &self.scope {
            Scope::Group(path) => Some(path.as_str()),
            _ => None,
        })
    }

    fn select_row(&mut self, row: RowKey) {
        let visible = self.visible_row_keys();
        if visible.contains(&row) {
            self.badge_picker = None;
            self.dismiss_tree_context();
            self.selection.press(row, &visible, self.modifiers);
            self.notice = None;
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        if (self.busy && !matches!(message, Message::Completed(_)))
            || (self.confirming_close && !matches!(message, Message::KeepEditing))
            || (!self.confirming_delete.is_empty() && !matches!(message, Message::ConfirmDelete(_)))
            || (self.tree_context.has_prompt()
                && !matches!(
                    message,
                    Message::TreeContext(_) | Message::Completed(_) | Message::KeepEditing
                ))
            || (self.folder.is_some()
                && !matches!(
                    message,
                    Message::GroupName(_)
                        | Message::GroupColorEnabled(_)
                        | Message::GroupColorInput(_)
                        | Message::CommitGroup
                        | Message::CancelGroup
                        | Message::Completed(_)
                        | Message::KeepEditing
                ))
            || (self.quick.is_some()
                && !matches!(
                    message,
                    Message::QuickName(_)
                        | Message::QuickTag(_)
                        | Message::SaveQuick
                        | Message::CancelQuick
                        | Message::Completed(_)
                        | Message::KeepEditing
                ))
        {
            return Task::none();
        }
        match message {
            Message::Search(query) if self.can_browse() && query.len() <= 240 => {
                self.badge_picker = None;
                self.query = query;
                self.reconcile_selection();
            }
            Message::Scope(scope) if self.can_browse() => self.set_scope(scope),
            Message::Select(id) if self.can_browse() => self.select_row(RowKey::Connection(id)),
            Message::SelectGroup(path) if self.can_browse() => {
                self.select_row(RowKey::Group(path));
            }
            Message::OpenTreeContext(row, point) if self.can_browse() => {
                if row != RowKey::Empty && !self.visible_row_keys().contains(&row) {
                    return Task::none();
                }
                if row != RowKey::Empty && !self.selection.contains(&row) {
                    self.selection.replace(Some(row.clone()));
                }
                self.badge_picker = None;
                self.tree_context.open(row, point);
            }
            Message::TreeContext(message) => return self.update_tree_context(message),
            Message::ToggleGroup(path) if self.can_browse() => {
                if !self.collapsed.remove(&path) && self.store.has_group(&path) {
                    self.collapsed.insert(path);
                }
                self.reconcile_selection();
            }
            Message::New if self.can_browse() => {
                self.badge_picker = None;
                self.dismiss_tree_context();
                self.draft_group = self.active_group().map(str::to_owned);
                self.draft = Some(Draft::new());
                self.notice = None;
            }
            Message::EditSelected | Message::Duplicate => {
                if let Some(selected) = self.selected() {
                    let draft = Draft::from_profile(
                        selected,
                        self.store.os_badge_id(selected.id),
                        matches!(message, Message::Duplicate),
                    );
                    self.draft_group = self.store.group_of(selected.id).map(str::to_owned);
                    self.draft = Some(draft);
                    self.badge_picker = None;
                    self.dismiss_tree_context();
                    self.notice = None;
                }
            }
            Message::ToggleOsBadgePicker(id)
                if self.selected().is_some_and(|selected| selected.id == id) =>
            {
                self.badge_picker = (self.badge_picker != Some(id)).then_some(id);
            }
            Message::SetOsBadge(id, badge)
                if self.badge_picker == Some(id)
                    && self.selected().is_some_and(|selected| selected.id == id)
                    && self.valid_os_badge(badge.as_deref()) =>
            {
                self.badge_picker = None;
                if self.store.os_badge_id(id) != badge.as_deref() {
                    return self.persist(move |mut store| {
                        store.set_os_badge_id(id, badge.as_deref())?;
                        let mut done = Persisted::saved(store, id);
                        done.status = "OS symbol saved.";
                        Ok(done)
                    });
                }
            }
            Message::Edit(Edit::OsBadge(Some(ref id))) if !self.valid_os_badge(Some(id)) => {}
            Message::Edit(edit) => {
                if let Some(draft) = &mut self.draft {
                    draft.update(edit);
                    if !self.draft_conflict {
                        self.notice = None;
                    }
                }
            }
            Message::DraftGroup(group) => {
                if let Some(draft) = &mut self.draft
                    && group
                        .as_deref()
                        .is_none_or(|path| self.store.has_group(path))
                    && self.draft_group != group
                {
                    self.draft_group = group;
                    draft.changed = true;
                }
            }
            Message::CancelEdit => {
                self.draft = None;
                self.draft_group = None;
                self.draft_conflict = false;
                self.notice = None;
                self.reconcile_selection();
            }
            Message::Save if !self.draft_conflict => {
                let Some(draft) = &self.draft else {
                    return Task::none();
                };
                let Ok(connection) = draft.connection() else {
                    return Task::none();
                };
                let group = self.draft_group.clone();
                let badge = draft.os_badge_id().map(str::to_owned);
                let select_folder =
                    connection.id == 0 || group.as_deref() != self.store.group_of(connection.id);
                return self.persist(move |mut store| {
                    let id = store.save_in_group_with_badge(
                        connection,
                        group.as_deref(),
                        badge.as_deref(),
                    )?;
                    let mut done = Persisted::saved(store, id);
                    if select_folder {
                        done.scope = Some(group.map_or(Scope::All, Scope::Group));
                    }
                    Ok(done)
                });
            }
            Message::QuickName(value) => self.edit_quick(value, true),
            Message::QuickTag(value) => self.edit_quick(value, false),
            Message::SaveQuick if !self.draft_conflict => {
                let Some((quick, entry)) = self.quick.as_ref().zip(self.single_connection()) else {
                    return Task::none();
                };
                let mut entry = entry.clone();
                entry.name = quick.name.trim().into();
                entry.tag = quick.tag.trim().into();
                if let Err(error) = entry.validate() {
                    self.notice = Some(error.to_string());
                    return Task::none();
                }
                return self.persist(move |mut store| {
                    let id = store.save(entry)?;
                    Ok(Persisted::saved(store, id))
                });
            }
            Message::CancelQuick => {
                self.quick = None;
                self.draft_conflict = false;
                self.notice = None;
                self.reconcile_selection();
            }
            Message::ToggleFavorite => {
                let ids = self.selected_connection_ids();
                if !ids.is_empty() {
                    return self.persist(move |mut store| {
                        let favorite = store.toggle_favorites(&ids)?;
                        Ok(Persisted::preserving_selection(
                            store,
                            if favorite {
                                "Added to Favorites."
                            } else {
                                "Removed from Favorites."
                            },
                        ))
                    });
                }
            }
            Message::NewGroup if self.can_browse() => {
                self.folder = Some(FolderDialog::Create {
                    parent: self.active_group().map(str::to_owned),
                    name: String::new(),
                });
                self.notice = None;
            }
            Message::RenameGroup if self.can_browse() => {
                if let Some(path) = self.selection.single_group() {
                    self.folder = Some(FolderDialog::Rename {
                        path: path.into(),
                        name: path.rsplit('/').next().unwrap_or_default().into(),
                    });
                    self.notice = None;
                }
            }
            Message::DeleteGroup if self.can_browse() => {
                if let Some(path) = self.selection.single_group() {
                    self.folder = Some(FolderDialog::Delete {
                        path: path.into(),
                        count: self.store.in_group(path).count(),
                    });
                    self.notice = None;
                }
            }
            Message::EditGroupColor if self.can_browse() => {
                if let Some(path) = self.selection.single_group() {
                    let original = self.store.group_color(path);
                    let inherited = self
                        .store
                        .ancestor_group_color(path)
                        .map(|(_, color)| color);
                    self.folder = Some(FolderDialog::Color {
                        path: path.into(),
                        original,
                        draft: ColorDraft::new(original, inherited),
                    });
                    self.notice = None;
                }
            }
            Message::GroupColorEnabled(enabled) => {
                if let Some(FolderDialog::Color { draft, .. }) = &mut self.folder {
                    draft.set_enabled(enabled);
                    self.notice = None;
                }
            }
            Message::GroupColorInput(input) => {
                if let Some(FolderDialog::Color { draft, .. }) = &mut self.folder
                    && draft.set_input(input)
                {
                    self.notice = None;
                }
            }
            Message::GroupName(value) if value.len() <= 96 => {
                if let Some(FolderDialog::Create { name, .. } | FolderDialog::Rename { name, .. }) =
                    &mut self.folder
                {
                    *name = value;
                    self.notice = None;
                }
            }
            Message::CommitGroup => return self.commit_group(),
            Message::CancelGroup => {
                self.folder = None;
                self.notice = None;
            }
            Message::Delete if self.can_browse() => {
                self.dismiss_tree_context();
                self.confirming_delete = self.selected_connection_ids();
            }
            Message::ConfirmDelete(confirmed) if !self.confirming_delete.is_empty() => {
                let ids = std::mem::take(&mut self.confirming_delete);
                if confirmed {
                    return self.persist(move |mut store| {
                        store.delete_connections(&ids)?;
                        Ok(Persisted::preserving_selection(
                            store,
                            if ids.len() == 1 {
                                "Connection deleted."
                            } else {
                                "Connections deleted."
                            },
                        ))
                    });
                }
            }
            Message::Reload if self.can_browse() => {
                let path = self.path.clone();
                return self.persist(move |_| {
                    let store = Store::open(path)?;
                    Ok(Persisted::preserving_selection(
                        store,
                        "Connections reloaded.",
                    ))
                });
            }
            Message::Completed(result) => {
                self.busy = false;
                match result {
                    Ok(done) => {
                        self.tree_context.clear_prompt();
                        self.draft = None;
                        self.draft_group = None;
                        self.draft_conflict = false;
                        self.quick = None;
                        self.folder = None;
                        self.store = done.store;
                        if let Some(scope) = done.scope {
                            self.set_scope(scope);
                        }
                        if let SelectionChange::Replace(id) = done.selection {
                            self.selection.replace(id.map(RowKey::Connection));
                        }
                        self.reconcile_selection();
                        self.notice = Some(done.status.into());
                    }
                    Err(error) => {
                        self.notice = Some(format!("Connection store operation failed ({error})"));
                    }
                }
            }
            Message::KeepEditing => self.confirming_close = false,
            _ => {}
        }
        Task::none()
    }

    fn edit_quick(&mut self, value: String, name_field: bool) {
        if value.len() > if name_field { 160 } else { 96 } || self.draft_conflict {
            return;
        }
        if self.quick.is_none() {
            let Some(entry) = self.selected() else {
                return;
            };
            self.quick = Some(QuickEdit {
                name: entry.name.clone(),
                tag: entry.tag.clone(),
            });
        }
        let Some(quick) = &mut self.quick else {
            return;
        };
        if name_field {
            quick.name = value;
        } else {
            quick.tag = value;
        }
        if self
            .selection
            .single_connection()
            .and_then(|id| self.store.get(id))
            .is_some_and(|entry| entry.name == quick.name && entry.tag == quick.tag)
        {
            self.quick = None;
        }
        self.notice = None;
    }

    fn commit_group(&mut self) -> Task<Message> {
        let Some(action) = self.folder.clone() else {
            return Task::none();
        };
        if let FolderDialog::Color {
            original, draft, ..
        } = &action
        {
            match draft.value() {
                Ok(value) if value == *original => {
                    self.folder = None;
                    self.notice = None;
                    return Task::none();
                }
                Err(error) => {
                    self.notice = Some(error.into());
                    return Task::none();
                }
                Ok(_) => {}
            }
        }
        self.persist(move |mut store| {
            let (scope, status, folder_rename) = match action {
                FolderDialog::Create { parent, name } => (
                    Scope::Group(store.create_group(parent.as_deref(), &name)?),
                    "Folder created.",
                    None,
                ),
                FolderDialog::Rename { path, name } => {
                    let new = store.rename_group(&path, &name)?;
                    let rename = (path != new).then(|| FolderRename {
                        old: path,
                        new: new.clone(),
                    });
                    (Scope::Group(new), "Folder renamed.", rename)
                }
                FolderDialog::Delete { path, .. } => {
                    store.delete_group(&path)?;
                    (Scope::All, "Folder deleted.", None)
                }
                FolderDialog::Color { path, draft, .. } => {
                    store.set_group_color(&path, draft.value().map_err(std::io::Error::other)?)?;
                    (Scope::Group(path), "Folder tab color saved.", None)
                }
            };
            Ok(Persisted {
                store,
                selection: SelectionChange::Preserve,
                scope: Some(scope),
                status,
                folder_rename,
            })
        })
    }

    fn persist(
        &mut self,
        mutate: impl FnOnce(Store) -> std::io::Result<Persisted> + Send + 'static,
    ) -> Task<Message> {
        self.badge_picker = None;
        self.dismiss_tree_context();
        self.busy = true;
        let store = self.store.clone();
        Task::future(async move {
            Message::Completed(
                mutate(store)
                    .map(Box::new)
                    .map_err(|error| error.to_string()),
            )
        })
    }
}
