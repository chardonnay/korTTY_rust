use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use iced::keyboard::Modifiers;
use iced::{Point, Task};

use crate::connection_store::custom_os_badges::Catalog;
use crate::connection_store::teamwork;
use crate::connection_store::{Connection, LocalProfile, Search, SerialProfile, Store, TabColor};
use crate::serial_dialog;
use crate::terminal_effects::Effect;

mod color;
mod form;
mod journal_settings;
mod local;
pub(crate) mod network_map;
mod os_badge;
pub(crate) mod overview;
mod selection;
mod terminal_overrides;
#[cfg(test)]
mod tests;
mod tree_context;
mod view;
use color::ColorDraft;
use form::Draft;
pub use form::Edit;
use selection::{RowKey, Selection};
pub(crate) use view::{HighlightChoices, ViewChoices};

const TEAMWORK_USERNAME_DRAFT_BYTES: usize = 259;

#[derive(Clone, Debug)]
pub enum Message {
    Search(String),
    Scope(Scope),
    MapView(bool),
    MapJumpRoutes(bool),
    MapTunnels(bool),
    MapZoom(f32),
    MapFit,
    MapSelect(RowKey),
    MapMove(u64, Option<String>),
    MapUndo,
    ExportMapPdf,
    Select(u64),
    SelectShared(teamwork::Identity),
    TeamworkAuthEdit(teamwork::DefaultAuth),
    SaveTeamworkAuth,
    ResetTeamworkAuth,
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
    NewSerial,
    NewLocal,
    SelectLocal(u64),
    Local(local::Edit),
    SelectSerial(u64),
    SerialName(String),
    SerialTag(String),
    SerialJournal(journal_settings::Edit),
    SerialPasteLineDelay(String),
    SerialStrictTerminalMode(Option<bool>),
    ToggleSerialPin,
    Serial(serial_dialog::Message),
    EditSelected,
    Duplicate,
    Edit(Edit),
    Save,
    CancelEdit,
    Delete,
    ConfirmDelete(bool),
    ConfirmTeamworkHide(bool),
    RefreshTeamwork,
    RestoreTeamwork,
    Reload,
    ImportConnections,
    ExportConnections,
    ExportOverview,
    Completed(Result<Box<Persisted>, String>),
    Connect(bool),
    ConnectInNewWindow,
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
    Teamwork,
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

struct TeamworkAuthDraft {
    saved: teamwork::DefaultAuth,
    draft: teamwork::DefaultAuth,
    conflict: bool,
}

impl TeamworkAuthDraft {
    fn new(saved: teamwork::DefaultAuth) -> Self {
        Self {
            draft: saved.clone(),
            saved,
            conflict: false,
        }
    }

    fn dirty(&self) -> bool {
        self.draft != self.saved
    }

    fn refresh(&mut self, saved: teamwork::DefaultAuth) {
        if self.saved == saved {
            return;
        }
        if !self.dirty() || self.draft == saved {
            *self = Self::new(saved);
        } else {
            self.saved = saved;
            self.conflict = true;
        }
    }

    fn change(&self) -> Option<(teamwork::DefaultAuth, teamwork::DefaultAuth)> {
        (self.dirty() && !self.conflict && self.draft.validate().is_ok())
            .then(|| (self.saved.clone(), self.draft.clone()))
    }
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
    server_access: crate::clipboard_policy::ServerAccess,
    teamwork: teamwork::Catalog,
    teamwork_available: bool,
    teamwork_auth: Option<TeamworkAuthDraft>,
    teamwork_auth_saving: bool,
    custom_badges: Arc<Catalog>,
    path: PathBuf,
    selection: Selection,
    modifiers: Modifiers,
    tree_context: tree_context::TreeContext,
    badge_picker: Option<u64>,
    scope: Scope,
    network_map: network_map::State,
    collapsed: BTreeSet<String>,
    open_counts: Option<BTreeMap<u64, usize>>,
    draft: Option<Draft>,
    serial_draft: Option<serial::Draft>,
    local_draft: Option<local::Draft>,
    serial_generation: u64,
    draft_group: Option<String>,
    draft_conflict: bool,
    quick: Option<QuickEdit>,
    folder: Option<FolderDialog>,
    query: String,
    notice: Option<String>,
    teamwork_notice: Option<String>,
    confirming_delete: Vec<u64>,
    confirming_teamwork_hide: Vec<teamwork::HiddenEntry>,
    confirming_close: bool,
    busy: bool,
}

mod serial;

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
                .map(|entry| RowKey::Connection(entry.id))
                .or_else(|| {
                    store
                        .serial_profiles()
                        .first()
                        .map(|entry| RowKey::Serial(entry.id))
                })
                .or_else(|| {
                    store
                        .local_profiles()
                        .first()
                        .map(|entry| RowKey::Local(entry.id))
                }),
        );
        Ok(Self {
            selection,
            modifiers: Modifiers::empty(),
            tree_context: tree_context::TreeContext::default(),
            badge_picker: None,
            scope: Scope::All,
            network_map: network_map::State::default(),
            collapsed: BTreeSet::new(),
            open_counts: None,
            store,
            server_access: Default::default(),
            teamwork: teamwork::Catalog::default(),
            teamwork_available: false,
            teamwork_auth: None,
            teamwork_auth_saving: false,
            custom_badges: Arc::new(custom_badges),
            path,
            draft: None,
            serial_draft: None,
            local_draft: None,
            serial_generation: 0,
            draft_group: None,
            draft_conflict: false,
            quick: None,
            folder: None,
            query: String::new(),
            notice,
            teamwork_notice: None,
            confirming_delete: Vec::new(),
            confirming_teamwork_hide: Vec::new(),
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

    pub(crate) fn set_server_access(&mut self, access: crate::clipboard_policy::ServerAccess) {
        self.server_access = access;
    }

    pub(crate) fn set_teamwork(&mut self, catalog: teamwork::Catalog, notice: Option<String>) {
        let changed = !self.teamwork.same_entries(&catalog);
        self.teamwork = catalog;
        self.teamwork_notice = notice;
        if changed {
            if self
                .confirming_teamwork_hide
                .iter()
                .any(|entry| self.teamwork.get(entry.identity()).is_none())
            {
                self.cancel_changed_teamwork_hide();
            }
            self.reconcile_selection();
        }
    }

    pub(crate) fn set_teamwork_available(&mut self, available: bool) {
        self.teamwork_available = available;
    }

    pub(crate) fn can_restore_teamwork(&self) -> bool {
        self.teamwork_available && self.can_browse()
    }

    pub(crate) fn set_teamwork_auth(&mut self, value: Option<teamwork::DefaultAuth>) {
        match (&mut self.teamwork_auth, value) {
            (Some(draft), Some(saved)) => draft.refresh(saved),
            (_, value) => self.teamwork_auth = value.map(TeamworkAuthDraft::new),
        }
    }

    pub(crate) fn teamwork_auth_change(
        &self,
    ) -> Option<(teamwork::DefaultAuth, teamwork::DefaultAuth)> {
        if !self.can_edit_teamwork_auth() {
            return None;
        }
        self.teamwork_auth.as_ref()?.change()
    }

    pub(crate) fn set_teamwork_auth_saving(&mut self, saving: bool) {
        self.teamwork_auth_saving = saving;
    }

    fn can_edit_teamwork_auth(&self) -> bool {
        !self.teamwork_auth_saving && self.scope == Scope::Teamwork && self.can_browse()
    }

    pub(crate) fn selected_shared(&self) -> Option<&teamwork::Entry> {
        self.can_browse()
            .then(|| self.selection.single_shared())
            .flatten()
            .and_then(|identity| self.shared_entry(identity))
    }

    fn shared_entry(&self, identity: &teamwork::Identity) -> Option<&teamwork::Entry> {
        self.teamwork
            .get(identity)
            .filter(|_| !self.store.is_teamwork_hidden(identity))
    }

    fn shared_entries(&self) -> impl Iterator<Item = &teamwork::Entry> {
        self.teamwork
            .entries()
            .filter(|entry| !self.store.is_teamwork_hidden(entry.identity()))
    }

    fn selected_shared_entries(&self) -> impl Iterator<Item = &teamwork::Entry> {
        self.selection
            .shared()
            .filter_map(|identity| self.shared_entry(identity))
    }

    fn favorite_count(&self) -> usize {
        self.store
            .connections()
            .iter()
            .filter(|entry| self.store.is_favorite(entry.id))
            .count()
            + self
                .store
                .serial_profiles()
                .iter()
                .filter(|entry| self.store.is_favorite(entry.id))
                .count()
            + self
                .shared_entries()
                .filter(|entry| self.store.is_teamwork_favorite(entry.identity()))
                .count()
    }

    fn visible_shared(&self) -> Vec<&teamwork::Entry> {
        let search = Search::new(&self.query);
        let mut entries: Vec<_> = self
            .shared_entries()
            .filter(|entry| {
                let in_scope = match &self.scope {
                    Scope::All | Scope::Teamwork => true,
                    Scope::Favorites => self.store.is_teamwork_favorite(entry.identity()),
                    Scope::Tag(tag) => entry.connection.tag.to_lowercase() == *tag,
                    _ => false,
                };
                in_scope && search.manager(&entry.connection)
            })
            .collect();
        entries.sort_by_cached_key(|entry| {
            (
                entry.group.to_lowercase(),
                &entry.group,
                entry.connection.name.to_lowercase(),
            )
        });
        entries
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
        if !self.can_browse() || self.selection.serials().next().is_some() {
            return None;
        }
        if let Some(connection) = self.single_connection() {
            return Some(connection);
        }
        let id = self.selected_connection_ids().into_iter().next()?;
        self.store.get(id)
    }

    pub(crate) fn selected_serial(&self) -> Option<&SerialProfile> {
        if !self.can_browse() {
            return None;
        }
        self.single_serial()
    }

    fn single_serial(&self) -> Option<&SerialProfile> {
        self.store.serial(self.selection.single_serial()?)
    }

    pub(crate) fn selected_local(&self) -> Option<&LocalProfile> {
        if !self.can_browse() {
            return None;
        }
        self.single_local()
    }

    fn single_local(&self) -> Option<&LocalProfile> {
        self.store.local(self.selection.single_local()?)
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
        self.visible_rows(self.visible_entries())
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

    fn selected_profile_ids(&self) -> Vec<u64> {
        if !self.can_browse() {
            return Vec::new();
        }
        self.visible_rows(self.visible_entries())
            .into_iter()
            .filter(|row| self.selection.contains(&row.key()))
            .filter_map(|row| match row {
                selection::VisualRow::Connection(entry, _) => Some(entry.id),
                selection::VisualRow::Serial(entry, _) => Some(entry.id),
                _ => None,
            })
            .collect()
    }

    fn profile_tag(&self, id: u64) -> Option<&str> {
        self.store
            .get(id)
            .map(|entry| entry.tag.as_str())
            .or_else(|| self.store.serial(id).map(|entry| entry.tag.as_str()))
    }

    fn library_tags(&self) -> impl Iterator<Item = &str> {
        self.store
            .connections()
            .iter()
            .map(|entry| entry.tag.as_str())
            .chain(
                self.store
                    .serial_profiles()
                    .iter()
                    .map(|entry| entry.tag.as_str()),
            )
            .chain(
                self.shared_entries()
                    .map(|entry| entry.connection.tag.as_str()),
            )
            .filter(|tag| !tag.is_empty())
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
            selected: self
                .selected_export_ids()
                .into_iter()
                .filter(|id| self.store.get(*id).is_some())
                .collect(),
            shown: self
                .visible_entries()
                .into_iter()
                .filter_map(|row| match row {
                    selection::VisualRow::Connection(connection, _) => Some(connection.id),
                    _ => None,
                })
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
            && (self.selection.single_local().is_some()
                || self.selection.serials().next().is_some()
                || self.selection.connections().next().is_some()
                || self
                    .selection
                    .single_group()
                    .is_some_and(|path| self.folder_profile_count(path) != 0))
    }

    fn selected_export_ids(&self) -> Vec<u64> {
        if let Some(id) = self.selection.single_local() {
            return vec![id];
        }
        let selected = self.selected_profile_ids();
        if !selected.is_empty() {
            return selected;
        }
        self.selection
            .single_group()
            .map(|path| {
                self.store
                    .in_group(path)
                    .map(|entry| entry.id)
                    .chain(self.store.serial_in_group(path).map(|entry| entry.id))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn folder_profile_count(&self, path: &str) -> usize {
        self.store.in_group(path).count() + self.store.serial_in_group(path).count()
    }

    fn visible_connections(&self) -> Vec<&Connection> {
        let search = Search::new(&self.query);
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
        if self.busy() {
            self.notice = Some("A profile update is in progress.".into());
            return false;
        }
        self.confirming_close = self.tree_context.has_unsaved_prompt()
            || self
                .teamwork_auth
                .as_ref()
                .is_some_and(TeamworkAuthDraft::dirty)
            || self.draft.as_ref().is_some_and(|draft| draft.changed)
            || self
                .serial_draft
                .as_ref()
                .is_some_and(|draft| draft.changed)
            || self.local_draft.as_ref().is_some_and(|draft| draft.changed)
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
        self.store_busy() || self.teamwork_auth_saving
    }

    pub(crate) fn store_busy(&self) -> bool {
        self.busy
    }

    pub(crate) fn discard_editor(&mut self) {
        if self.busy
            || (self.draft.is_none() && self.serial_draft.is_none() && self.local_draft.is_none())
        {
            return;
        }
        self.confirming_close = false;
        let _ = self.update(Message::CancelEdit);
    }

    pub(crate) fn store_path(&self) -> &std::path::Path {
        &self.path
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
        if self.serial_draft.as_ref().is_some_and(|draft| {
            self.store.serial(draft.id) != store.serial(draft.id)
                || self.store.group_of(draft.id) != store.group_of(draft.id)
                || self
                    .draft_group
                    .as_deref()
                    .is_some_and(|group| !store.has_group(group))
        }) || self
            .local_draft
            .as_ref()
            .is_some_and(|draft| self.store.local(draft.id) != store.local(draft.id))
        {
            self.draft_conflict = true;
            self.notice = Some("This connection changed in another window. Discard the draft and reopen it before saving.".into());
        }
        if self.confirming_delete.iter().any(|&id| {
            self.store.get(id) != store.get(id)
                || self.store.serial(id) != store.serial(id)
                || self.store.local(id) != store.local(id)
                || self.store.group_of(id) != store.group_of(id)
        }) {
            self.confirming_delete.clear();
            self.notice =
                Some("The selection changed. Review the connections before deleting them.".into());
        }
        if self
            .confirming_teamwork_hide
            .iter()
            .any(|entry| store.is_teamwork_hidden(entry.identity()))
        {
            self.cancel_changed_teamwork_hide();
        }
        if let Some(FolderDialog::Delete { path, .. }) = &self.folder
            && (!self.store.groups().eq(store.groups())
                || !self.store.in_group(path).eq(store.in_group(path))
                || !self
                    .store
                    .serial_in_group(path)
                    .eq(store.serial_in_group(path)))
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
        if self.open_counts.as_ref() != Some(&counts) {
            self.open_counts = Some(counts);
            self.reconcile_selection();
        }
    }

    fn can_browse(&self) -> bool {
        !self.busy
            && !self.confirming_close
            && self.confirming_delete.is_empty()
            && self.confirming_teamwork_hide.is_empty()
            && !self.tree_context.has_prompt()
            && self.draft.is_none()
            && self.serial_draft.is_none()
            && self.local_draft.is_none()
            && self.folder.is_none()
            && self.quick.is_none()
    }

    fn scope_accepts(&self, connection: &Connection) -> bool {
        match &self.scope {
            Scope::All => true,
            Scope::Teamwork | Scope::Recent => false,
            Scope::Favorites => self.store.is_favorite(connection.id),
            Scope::Open => self.open_count(connection.id) > 0,
            Scope::Group(group) => self.store.is_in_group(connection.id, group),
            Scope::Tag(tag) => connection.tag.to_lowercase() == tag.to_lowercase(),
        }
    }

    fn cancel_changed_teamwork_hide(&mut self) {
        self.confirming_teamwork_hide.clear();
        self.notice = Some(
            "The Teamwork selection changed. Review the connections before hiding them.".into(),
        );
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
        if self.draft.is_none()
            && self.serial_draft.is_none()
            && self.local_draft.is_none()
            && self.quick.is_none()
        {
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
            self.selection.retain(|row| {
                matches!(
                    row,
                    RowKey::Connection(_)
                        | RowKey::Shared(_)
                        | RowKey::Serial(_)
                        | RowKey::Local(_)
                )
            });
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
        let mut visible = self.visible_row_keys();
        if visible.contains(&row) {
            self.badge_picker = None;
            self.dismiss_tree_context();
            if matches!(row, RowKey::Local(_)) || self.selection.single_local().is_some() {
                self.selection.replace(Some(row));
            } else {
                visible.retain(|key| !matches!(key, RowKey::Local(_)));
                self.selection.press(row, &visible, self.modifiers);
            }
            self.notice = None;
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        if (self.busy && !matches!(message, Message::Completed(_)))
            || (self.confirming_close && !matches!(message, Message::KeepEditing))
            || (!self.confirming_teamwork_hide.is_empty()
                && !matches!(message, Message::ConfirmTeamworkHide(_)))
            || (self.confirming_teamwork_hide.is_empty()
                && !self.confirming_delete.is_empty()
                && !matches!(message, Message::ConfirmDelete(_) | Message::Completed(_)))
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
            Message::SelectShared(id) if self.can_browse() => {
                self.select_row(RowKey::Shared(id));
            }
            Message::TeamworkAuthEdit(value) if self.can_edit_teamwork_auth() => {
                if let Some(auth) = &mut self.teamwork_auth
                    && !matches!(
                        &value,
                        teamwork::DefaultAuth::Key { username, .. }
                            | teamwork::DefaultAuth::TemporaryKey { username }
                            if username.len() > TEAMWORK_USERNAME_DRAFT_BYTES
                    )
                {
                    auth.draft = value;
                    if !auth.dirty() {
                        auth.conflict = false;
                    }
                }
            }
            Message::ResetTeamworkAuth if self.can_edit_teamwork_auth() => {
                if let Some(auth) = &mut self.teamwork_auth {
                    auth.draft = auth.saved.clone();
                    auth.conflict = false;
                }
            }
            Message::MapView(enabled) if self.can_browse() => {
                self.dismiss_tree_context();
                self.network_map.visible = enabled;
            }
            Message::MapJumpRoutes(enabled) if self.network_map.visible => {
                self.network_map.jumps = enabled;
            }
            Message::MapTunnels(enabled) if self.network_map.visible => {
                self.network_map.tunnels = enabled;
            }
            Message::MapZoom(scale) if self.network_map.visible && scale.is_finite() => {
                self.network_map.zoom = network_map::Zoom::Manual(network_map::clamp_zoom(scale));
            }
            Message::MapFit if self.network_map.visible => {
                self.network_map.zoom = network_map::Zoom::Fit
            }
            Message::MapSelect(row) if self.network_map.visible && self.can_browse() => {
                self.select_map_row(row);
            }
            Message::MapMove(id, folder) if self.network_map.visible && self.can_browse() => {
                return self.move_map_connection(id, folder);
            }
            Message::MapUndo if self.network_map.visible && self.can_browse() => {
                return self.undo_map_move();
            }
            Message::Select(id) if self.can_browse() => self.select_row(RowKey::Connection(id)),
            Message::SelectSerial(id) if self.can_browse() => self.select_row(RowKey::Serial(id)),
            Message::SelectLocal(id) if self.can_browse() => self.select_row(RowKey::Local(id)),
            Message::SelectGroup(path) if self.can_browse() => {
                self.select_row(RowKey::Group(path));
            }
            Message::OpenTreeContext(row, point) if self.can_browse() => {
                if row == RowKey::Empty && self.scope == Scope::Teamwork {
                    return Task::none();
                }
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
            Message::New if self.can_browse() && self.scope != Scope::Teamwork => {
                self.badge_picker = None;
                self.dismiss_tree_context();
                self.draft_group = self.active_group().map(str::to_owned);
                self.draft = Some(Draft::new());
                self.notice = None;
            }
            Message::NewSerial if self.can_browse() && self.scope != Scope::Teamwork => {
                return self.begin_serial(None, false);
            }
            Message::NewLocal if self.can_browse() && self.scope != Scope::Teamwork => {
                self.begin_local(None, false);
            }
            Message::EditSelected | Message::Duplicate => {
                if let Some(selected) = self.selected_local() {
                    self.begin_local(
                        Some(selected.clone()),
                        matches!(message, Message::Duplicate),
                    );
                } else if let Some(selected) = self.selected_serial() {
                    return self.begin_serial(
                        Some(selected.clone()),
                        matches!(message, Message::Duplicate),
                    );
                }
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
                if group
                    .as_deref()
                    .is_none_or(|path| self.store.has_group(path))
                    && self.draft_group != group
                {
                    if let Some(draft) = &mut self.draft {
                        self.draft_group = group;
                        draft.changed = true;
                    } else if let Some(draft) = &mut self.serial_draft {
                        self.draft_group = group;
                        draft.changed = true;
                    }
                }
            }
            Message::CancelEdit => {
                self.draft = None;
                self.serial_draft = None;
                self.local_draft = None;
                self.draft_group = None;
                self.draft_conflict = false;
                self.notice = None;
                self.reconcile_selection();
            }
            Message::Save if !self.draft_conflict => {
                if self.local_draft.is_some() {
                    return self.save_local();
                }
                if self.serial_draft.is_some() {
                    return self.save_serial();
                }
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
            Message::SerialName(name) if name.len() <= 160 => {
                if let Some(draft) = &mut self.serial_draft {
                    draft.changed |= draft.name != name;
                    draft.name = name;
                }
            }
            Message::SerialTag(tag) if tag.len() <= 96 => {
                if let Some(draft) = &mut self.serial_draft {
                    draft.changed |= draft.tag != tag;
                    draft.tag = tag;
                }
            }
            Message::ToggleSerialPin if self.can_browse() => {
                if let Some(mut profile) = self.selected_serial().cloned() {
                    profile.pinned = !profile.pinned;
                    return self.persist(move |mut store| {
                        let status = if profile.pinned {
                            "Serial profile pinned."
                        } else {
                            "Serial profile unpinned."
                        };
                        store.save_serial(profile)?;
                        Ok(Persisted::preserving_selection(store, status))
                    });
                }
            }
            Message::SerialJournal(edit) => {
                if let Some(draft) = &mut self.serial_draft {
                    draft.journal.update(edit);
                    draft.changed = true;
                }
            }
            Message::SerialPasteLineDelay(value) => {
                if let Some(draft) = &mut self.serial_draft {
                    draft.paste_line_delay = value.chars().take(8).collect();
                    draft.changed = true;
                }
            }
            Message::SerialStrictTerminalMode(value) => {
                if let Some(draft) = &mut self.serial_draft {
                    draft.strict_terminal_mode = value;
                    draft.changed = true;
                }
            }
            Message::Serial(message) => return self.update_serial(message),
            Message::Local(edit) => {
                if let Some(draft) = &mut self.local_draft {
                    draft.update(edit);
                    if !self.draft_conflict {
                        self.notice = None;
                    }
                }
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
            Message::ToggleFavorite if self.can_browse() => {
                let local = self.selected_profile_ids();
                let shared = self
                    .selected_shared_entries()
                    .map(|entry| entry.identity().clone())
                    .collect::<Vec<_>>();
                if !local.is_empty() || !shared.is_empty() {
                    return self.persist(move |mut store| {
                        let favorite = store.toggle_connection_favorites(&local, &shared)?;
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
            Message::NewGroup if self.can_browse() && self.scope != Scope::Teamwork => {
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
                        count: self.folder_profile_count(path),
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
                let local = self
                    .selection
                    .single_local()
                    .map_or_else(|| self.selected_profile_ids(), |id| vec![id]);
                self.confirming_teamwork_hide = self
                    .selected_shared_entries()
                    .map(teamwork::HiddenEntry::from)
                    .collect();
                self.confirming_delete = local;
            }
            Message::ConfirmTeamworkHide(confirmed)
                if !self.confirming_teamwork_hide.is_empty() =>
            {
                let entries = std::mem::take(&mut self.confirming_teamwork_hide);
                if confirmed {
                    return self.persist(move |mut store| {
                        store.hide_teamwork(&entries)?;
                        Ok(Persisted::preserving_selection(
                            store,
                            if entries.len() == 1 {
                                "Teamwork connection hidden."
                            } else {
                                "Teamwork connections hidden."
                            },
                        ))
                    });
                }
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
                self.network_map.finish_move(result.is_ok());
                match result {
                    Ok(done) => {
                        self.tree_context.clear_prompt();
                        self.draft = None;
                        self.serial_draft = None;
                        self.local_draft = None;
                        self.draft_group = None;
                        self.draft_conflict = false;
                        self.quick = None;
                        self.folder = None;
                        self.store = done.store;
                        if let Some(scope) = done.scope {
                            self.set_scope(scope);
                        }
                        if let SelectionChange::Replace(id) = done.selection {
                            self.selection.replace(id.map(|id| {
                                if self.store.serial(id).is_some() {
                                    RowKey::Serial(id)
                                } else if self.store.local(id).is_some() {
                                    RowKey::Local(id)
                                } else {
                                    RowKey::Connection(id)
                                }
                            }));
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

#[cfg(test)]
#[test]
fn teamwork_auth_draft_keeps_conflicting_edits_and_accepts_a_matching_commit() {
    use teamwork::DefaultAuth;

    let key = DefaultAuth::Key {
        id: 4,
        username: "operator".into(),
    };
    let other = DefaultAuth::TemporaryKey {
        username: "reviewer".into(),
    };
    let mut draft = TeamworkAuthDraft::new(DefaultAuth::Prompt);
    draft.draft = key.clone();
    assert_eq!(draft.change(), Some((DefaultAuth::Prompt, key.clone())));
    draft.refresh(DefaultAuth::Prompt);
    assert!(draft.change().is_some());

    draft.refresh(other.clone());
    assert_eq!(draft.saved, other);
    assert_eq!(draft.draft, key);
    assert!(draft.conflict);
    assert!(draft.change().is_none());

    draft.refresh(key.clone());
    assert_eq!(draft.saved, key);
    assert!(!draft.dirty());
    assert!(!draft.conflict);
}
