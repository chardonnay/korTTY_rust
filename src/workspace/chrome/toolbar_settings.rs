//! Profile-backed toolbar preferences and the per-window editing transaction.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use iced::{Task, window};
use serde::{Deserialize, Serialize};

use crate::connection_store::{Store, group_suffix};
use crate::connection_window::FolderRename;
use crate::profile;

use super::toolbar_item_context::{Context, Event as ItemEvent};
use super::{Action, Icon, WorkspaceMessage, settings_window, wrap};

#[cfg(test)]
mod tests;

pub(super) const MIN_ICON: u16 = 16;
pub(super) const MAX_ICON: u16 = 64;
const DEFAULT_ICON: u16 = 24;
const MAX_PROFILE_BYTES: u64 = 64 * 1024;
const MAX_ITEMS: usize = 128;
const MAX_FOLDER_PATH_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Group {
    Terminal,
    Tools,
    Settings,
    Connections,
}

impl Group {
    pub(super) const ACTIONS: [Self; 3] = [Self::Terminal, Self::Tools, Self::Settings];
    pub(super) const ALL: [Self; 4] = [
        Self::Terminal,
        Self::Tools,
        Self::Settings,
        Self::Connections,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::Tools => "Tools",
            Self::Settings => "Settings",
            Self::Connections => "Connections",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Terminal => 0,
            Self::Tools => 1,
            Self::Settings => 2,
            Self::Connections => 3,
        }
    }
}

impl std::fmt::Display for Group {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Catalog {
    Actions,
    Settings,
    Connections,
}

impl Catalog {
    pub(super) const ALL: [Self; 3] = [Self::Actions, Self::Settings, Self::Connections];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Actions => "Actions",
            Self::Settings => "Settings",
            Self::Connections => "Connections",
        }
    }
}

/// Reference slots retained for layouts saved before their native actions existed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reference {
    PowerShell,
    CommandPrompt,
    Journal,
    Logfile,
    Ai,
    Colors,
    Keyboard,
    Logging,
}

impl Reference {
    pub(super) fn presentation(self) -> (Icon, &'static str) {
        match self {
            Self::PowerShell => (Icon::PowerShell, "New PowerShell"),
            Self::CommandPrompt => (Icon::CommandPrompt, "New Command Prompt"),
            Self::Journal => (Icon::Journal, "Session Journals…"),
            Self::Logfile => (Icon::Logfile, "Logfile Manager…"),
            Self::Ai => (Icon::Ai, "AI Manager…"),
            Self::Colors => (Icon::Colors, "Colors"),
            Self::Keyboard => (Icon::Keyboard, "Keyboard"),
            Self::Logging => (Icon::Journal, "Logging"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Item {
    Action(Action),
    Reference(Reference),
    Connection(u64),
    Folder(String),
    Separator,
}

impl Item {
    fn rename_folder(&mut self, rename: &FolderRename) -> bool {
        let Self::Folder(path) = self else {
            return false;
        };
        let Some(suffix) = group_suffix(path, &rename.old) else {
            return false;
        };
        if rename.new.len() + suffix.len() > MAX_FOLDER_PATH_BYTES {
            return false;
        }
        *path = format!("{}{suffix}", rename.new);
        true
    }

    pub(super) fn accepts(&self, group: Group) -> bool {
        match self {
            Self::Action(_) | Self::Reference(_) => group != Group::Connections,
            Self::Connection(_) | Self::Folder(_) => group == Group::Connections,
            Self::Separator => true,
        }
    }

    fn valid(&self, group: Group) -> bool {
        self.accepts(group)
            && match self {
                Self::Connection(id) => *id != 0,
                Self::Folder(path) => {
                    path.len() <= MAX_FOLDER_PATH_BYTES
                        && !path.chars().any(char::is_control)
                        && path.split('/').all(|part| !part.trim().is_empty())
                }
                _ => true,
            }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Section {
    visible: bool,
    items: Vec<Item>,
}

impl Section {
    fn new(items: Vec<Item>) -> Self {
        Self {
            visible: true,
            items,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Layout {
    icon_size: u16,
    actions_visible: bool,
    terminal: Section,
    tools: Section,
    settings: Section,
    connections: Section,
}

impl Default for Layout {
    fn default() -> Self {
        use Action::*;
        use Item::Action as Button;

        Self {
            icon_size: DEFAULT_ICON,
            actions_visible: true,
            terminal: Section::new(vec![
                Button(QuickConnect),
                Button(LocalShell),
                #[cfg(windows)]
                Button(PowerShell),
                #[cfg(windows)]
                Button(CommandPrompt),
            ]),
            tools: Section::new(vec![
                Button(ManageConnections),
                Button(Sftp),
                Button(Snippets),
                Button(SessionJournals),
                Button(LogfileManager),
                Button(AiManager),
                Button(Scheduler),
                Button(Recordings),
            ]),
            settings: Section::new(vec![
                Button(FontSettings),
                Button(ColorSettings),
                Button(KeyboardSettings),
                Button(LoggingSettings),
                Button(WindowSettings),
                Button(TerminalSettings),
            ]),
            connections: Section::new(Vec::new()),
        }
    }
}

impl Layout {
    fn rename_folder(&mut self, rename: &FolderRename) -> bool {
        let mut changed = false;
        for item in &mut self.connections.items {
            changed |= item.rename_folder(rename);
        }
        changed
    }

    pub(super) fn icon_size(&self) -> u16 {
        self.icon_size
    }

    pub(super) fn actions_visible(&self) -> bool {
        self.actions_visible
    }

    pub(super) fn visible(&self, group: Group) -> bool {
        self.section(group).visible
    }

    pub(super) fn items(&self, group: Group) -> &[Item] {
        &self.section(group).items
    }

    fn section(&self, group: Group) -> &Section {
        [
            &self.terminal,
            &self.tools,
            &self.settings,
            &self.connections,
        ][group.index()]
    }

    fn section_mut(&mut self, group: Group) -> &mut Section {
        [
            &mut self.terminal,
            &mut self.tools,
            &mut self.settings,
            &mut self.connections,
        ][group.index()]
    }

    pub(super) fn has_room(&self) -> bool {
        self.item_count() < MAX_ITEMS
    }

    fn item_count(&self) -> usize {
        Group::ALL
            .into_iter()
            .map(|group| self.items(group).len())
            .sum()
    }

    fn valid(&self) -> bool {
        (MIN_ICON..=MAX_ICON).contains(&self.icon_size)
            && self.item_count() <= MAX_ITEMS
            && Group::ALL
                .into_iter()
                .all(|group| self.items(group).iter().all(|item| item.valid(group)))
    }

    fn set_row(&mut self, row: Row, visible: bool) {
        match row {
            Row::Actions => self.actions_visible = visible,
            Row::Group(group) => self.section_mut(group).visible = visible,
        }
    }

    fn add(&mut self, group: Group, item: Item) -> Option<Location> {
        self.insert(group, self.items(group).len(), item)
    }

    fn insert(&mut self, group: Group, index: usize, item: Item) -> Option<Location> {
        if !self.has_room() || !item.valid(group) {
            return None;
        }
        let items = &mut self.section_mut(group).items;
        if index > items.len() {
            return None;
        }
        items.insert(index, item);
        Some(Location { group, index })
    }

    fn item(&self, location: Location) -> Option<&Item> {
        self.items(location.group).get(location.index)
    }

    fn remove(&mut self, location: Location) -> Option<Item> {
        self.item(location)?;
        Some(
            self.section_mut(location.group)
                .items
                .remove(location.index),
        )
    }

    fn place(&mut self, source: Location, group: Group, before: Option<usize>) -> Option<Location> {
        if !self.item(source)?.accepts(group) {
            return None;
        }
        let destination = before.unwrap_or_else(|| self.items(group).len());
        if destination > self.items(group).len() {
            return None;
        }
        let index = if source.group == group && source.index < destination {
            destination - 1
        } else {
            destination
        };
        let item = self.remove(source)?;
        self.section_mut(group).items.insert(index, item);
        Some(Location { group, index })
    }

    fn step(&mut self, source: Location, forward: bool) -> Option<Location> {
        self.item(source)?;
        let next = source
            .index
            .checked_add_signed(if forward { 1 } else { -1 })?;
        if next >= self.items(source.group).len() {
            return None;
        }
        self.section_mut(source.group)
            .items
            .swap(source.index, next);
        Some(Location {
            group: source.group,
            index: next,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Row {
    Actions,
    Group(Group),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Location {
    pub(super) group: Group,
    pub(super) index: usize,
}

#[derive(Clone, Debug)]
pub enum Edit {
    Search(String),
    Catalog(Catalog),
    IconSize(u16),
    Add(Group, Item),
    Pick(Location),
    PickCatalog(Group, Item),
    ReleaseCatalog(Group, Item),
    Drop(Group, Option<usize>),
    CancelDrag,
    ItemContext(ItemEvent),
    Remove,
    Step(bool),
    Move(Group),
    Reset,
}

#[derive(Clone, Debug)]
pub enum Message {
    Open(window::Id, Catalog),
    Edit(window::Id, Edit),
    Visible(window::Id, Row, bool),
    Finish(window::Id),
    Cancel(window::Id),
    Saved(Result<(), String>),
    Connect(window::Id, u64),
    ConnectFolder(window::Id, String),
}

impl Message {
    pub(super) fn owner(&self) -> Option<window::Id> {
        match self {
            Self::Open(owner, _)
            | Self::Edit(owner, _)
            | Self::Visible(owner, _, _)
            | Self::Finish(owner)
            | Self::Cancel(owner)
            | Self::Connect(owner, _)
            | Self::ConnectFolder(owner, _) => Some(*owner),
            Self::Saved(_) => None,
        }
    }
}

pub(super) struct Draft {
    owner: window::Id,
    pub(super) layout: Layout,
    pub(super) catalog: Catalog,
    pub(super) search: String,
    pub(super) search_input: iced::widget::Id,
    pub(super) catalog_scroll: iced::widget::Id,
    pub(super) selected: Option<Location>,
    pub(super) item_context: Context,
    drag: Option<Drag>,
}

enum Drag {
    Existing(Location),
    Catalog(Group, Item),
}

impl Draft {
    pub(super) fn edit(&mut self, edit: Edit) {
        match edit {
            Edit::Search(value) => self.search = value.chars().take(96).collect(),
            Edit::Catalog(catalog) => self.catalog = catalog,
            Edit::IconSize(size) => self.layout.icon_size = size.clamp(MIN_ICON, MAX_ICON),
            Edit::Add(group, item) => {
                let placed = self.layout.add(group, item);
                self.update_order_selection(placed);
            }
            Edit::Pick(location) => {
                self.item_context.dismiss();
                self.selected = self.layout.item(location).map(|_| location);
                self.drag = self.selected.map(Drag::Existing);
            }
            Edit::PickCatalog(group, item) => self.drag = Some(Drag::Catalog(group, item)),
            Edit::ReleaseCatalog(group, item) => {
                if matches!(self.drag.take(), Some(Drag::Catalog(origin, entry)) if origin == group && entry == item)
                {
                    self.edit(Edit::Add(group, item));
                }
            }
            Edit::Drop(group, before) => {
                let placed = match self.drag.take() {
                    Some(Drag::Existing(location)) => self.layout.place(location, group, before),
                    Some(Drag::Catalog(_, item)) => {
                        let index = before.unwrap_or_else(|| self.layout.items(group).len());
                        self.layout.insert(group, index, item)
                    }
                    None => None,
                };
                self.update_order_selection(placed);
            }
            Edit::CancelDrag => self.drag = None,
            Edit::ItemContext(_) => unreachable!("toolbar routes stamped item events first"),
            Edit::Remove => {
                if let Some(location) = self.selected.take()
                    && self.layout.remove(location).is_some()
                {
                    self.item_context.invalidate();
                }
            }
            Edit::Step(forward) => {
                if let Some(location) = self.selected {
                    let placed = self.layout.step(location, forward);
                    self.update_order_selection(placed);
                }
            }
            Edit::Move(group) => {
                if let Some(location) = self.selected {
                    let placed = self.layout.place(location, group, None);
                    self.update_order_selection(placed);
                }
            }
            Edit::Reset => {
                let mut defaults = Layout::default();
                for group in Group::ALL {
                    std::mem::swap(
                        &mut self.layout.section_mut(group).items,
                        &mut defaults.section_mut(group).items,
                    );
                }
                self.selected = None;
                self.drag = None;
                self.item_context.invalidate();
            }
        }
    }

    fn update_order_selection(&mut self, placed: Option<Location>) {
        if let Some(placed) = placed {
            self.selected = Some(placed);
            self.item_context.invalidate();
        }
    }

    pub(super) fn selected_item(&self) -> Option<(Location, &Item)> {
        let location = self.selected?;
        self.layout.item(location).map(|item| (location, item))
    }

    pub(super) fn can_drop(&self, group: Group) -> bool {
        let item = match self.drag.as_ref() {
            Some(Drag::Existing(location)) => self.layout.item(*location),
            Some(Drag::Catalog(_, item)) => Some(item),
            None => None,
        };
        item.is_some_and(|item| item.accepts(group))
    }
}

#[derive(Default)]
pub(super) struct Toolbar {
    layout: Layout,
    draft: Option<Draft>,
    pending: Option<Pending>,
    unsaved_folder_paths: bool,
    problem: Option<String>,
    connections: Vec<ConnectionChoice>,
    folders: Vec<FolderChoice>,
}

pub(super) struct ConnectionChoice {
    pub(super) id: u64,
    pub(super) name: String,
    pub(super) incognito: bool,
}

pub(super) struct FolderChoice {
    pub(super) path: String,
    pub(super) populated: bool,
}

struct Pending {
    owner: window::Id,
    layout: Layout,
    intent: SaveIntent,
    needs_rewrite: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SaveIntent {
    Finish,
    Visibility,
    Rename,
    SettingsIconSize(window::Id),
}

impl Toolbar {
    pub(super) fn load() -> Self {
        let mut toolbar = Self::default();
        match path().and_then(|path| load_from(&path)) {
            Ok(layout) => toolbar.layout = layout,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => toolbar.problem = Some(format!("Could not load the toolbar ({error}).")),
        }
        if let Ok(store) = profile::connections_path().and_then(Store::open) {
            toolbar.refresh_connections(&store);
        }
        toolbar
    }

    pub(super) fn refresh_connections(&mut self, store: &Store) {
        self.connections = store
            .connections()
            .iter()
            .map(|connection| ConnectionChoice {
                id: connection.id,
                name: connection.name.clone(),
                incognito: connection.incognito,
            })
            .collect();
        self.folders = store
            .groups()
            .map(|path| FolderChoice {
                path: path.to_owned(),
                populated: store
                    .in_group(path)
                    .any(|connection| store.group_of(connection.id) == Some(path)),
            })
            .collect();
    }

    pub(super) fn rename_folder(
        &mut self,
        owner: window::Id,
        rename: &FolderRename,
    ) -> Task<WorkspaceMessage> {
        let changed = self.layout.rename_folder(rename);
        self.unsaved_folder_paths |= changed;
        if let Some(draft) = &mut self.draft {
            if draft.layout.rename_folder(rename) {
                draft.item_context.invalidate();
            }
            if let Some(Drag::Catalog(_, item)) = &mut draft.drag {
                item.rename_folder(rename);
            }
        }
        if let Some(pending) = &mut self.pending {
            // The worker owns an earlier snapshot. Apply the rename again in the next write.
            pending.needs_rewrite |= pending.layout.rename_folder(rename) || changed;
            return Task::none();
        }
        self.save(owner, self.layout.clone(), SaveIntent::Rename)
    }

    pub(super) fn connections(&self) -> &[ConnectionChoice] {
        &self.connections
    }

    pub(super) fn folders(&self) -> &[FolderChoice] {
        &self.folders
    }

    pub(super) fn can_edit(&self, owner: window::Id) -> bool {
        self.pending.is_none() && self.draft.as_ref().is_none_or(|draft| draft.owner == owner)
    }

    pub(super) fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn pending_owner(&self) -> Option<window::Id> {
        self.pending.as_ref().map(|pending| pending.owner)
    }

    pub(super) fn committed_icon_size(&self) -> u16 {
        self.layout.icon_size()
    }

    pub(super) fn save_icon_size(
        &mut self,
        owner: window::Id,
        dialog: window::Id,
        size: u16,
    ) -> Task<WorkspaceMessage> {
        if self.pending() {
            return settings_icon_size_saved(
                dialog,
                Err("The toolbar is still being saved. Try again shortly.".into()),
            );
        }
        let mut layout = self.layout.clone();
        layout.icon_size = size.clamp(MIN_ICON, MAX_ICON);
        self.save(owner, layout, SaveIntent::SettingsIconSize(dialog))
    }

    pub(super) fn draft(&self, owner: window::Id) -> Option<&Draft> {
        self.draft.as_ref().filter(|draft| draft.owner == owner)
    }

    pub(super) fn layout(&self, owner: window::Id) -> &Layout {
        self.draft(owner)
            .map_or(&self.layout, |draft| &draft.layout)
    }

    pub(super) fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    pub(super) fn close(&mut self, owner: window::Id) {
        if self.draft(owner).is_some() {
            self.draft = None;
        }
    }

    pub(super) fn dismiss_item_menu(&mut self, owner: window::Id) {
        if let Some(draft) = self.draft.as_mut().filter(|draft| draft.owner == owner) {
            draft.item_context.dismiss();
        }
    }

    pub(super) fn update(&mut self, message: Message) -> Task<WorkspaceMessage> {
        match message {
            Message::Open(owner, catalog) if self.can_edit(owner) => {
                let draft = self.draft.get_or_insert_with(|| Draft {
                    owner,
                    layout: self.layout.clone(),
                    catalog,
                    search: String::new(),
                    search_input: iced::widget::Id::unique(),
                    catalog_scroll: iced::widget::Id::unique(),
                    selected: None,
                    item_context: Context::default(),
                    drag: None,
                });
                draft.catalog = catalog;
                draft.search.clear();
                draft.drag = None;
                draft.item_context.dismiss();
                return Task::batch([
                    iced::widget::operation::focus(draft.search_input.clone()),
                    iced::widget::operation::snap_to(
                        draft.catalog_scroll.clone(),
                        iced::widget::operation::RelativeOffset::START,
                    ),
                ]);
            }
            Message::Edit(owner, Edit::ItemContext(event)) if self.can_edit(owner) => {
                if let Some(draft) = self.draft.as_mut() {
                    return draft.item_context_event(event);
                }
            }
            Message::Edit(owner, edit) if self.can_edit(owner) => {
                if let Some(draft) = self.draft.as_mut() {
                    let reset_scroll = matches!(edit, Edit::Search(_) | Edit::Catalog(_));
                    draft.edit(edit);
                    if reset_scroll {
                        return iced::widget::operation::snap_to(
                            draft.catalog_scroll.clone(),
                            iced::widget::operation::RelativeOffset::START,
                        );
                    }
                }
            }
            Message::Visible(owner, row, visible) if self.can_edit(owner) => {
                if let Some(draft) = self.draft.as_mut() {
                    draft.layout.set_row(row, visible);
                } else {
                    let mut layout = self.layout.clone();
                    layout.set_row(row, visible);
                    return self.save(owner, layout, SaveIntent::Visibility);
                }
            }
            Message::Finish(owner) if self.can_edit(owner) => {
                if let Some(draft) = self.draft.as_mut() {
                    draft.item_context.dismiss();
                    let layout = draft.layout.clone();
                    return self.save(owner, layout, SaveIntent::Finish);
                }
            }
            Message::Cancel(owner) if self.can_edit(owner) => self.close(owner),
            Message::Saved(result) => {
                let Some(pending) = self.pending.take() else {
                    return Task::none();
                };
                let report = match result {
                    Ok(()) => {
                        let previous_icon_size = self.layout.icon_size();
                        self.layout = pending.layout;
                        if let Some(draft) = &mut self.draft
                            && draft.layout.icon_size() == previous_icon_size
                        {
                            draft.layout.icon_size = self.layout.icon_size();
                        }
                        self.unsaved_folder_paths = pending.needs_rewrite;
                        if pending.intent != SaveIntent::Rename || self.draft.is_none() {
                            self.problem = None;
                        }
                        if pending.intent == SaveIntent::Finish {
                            self.close(pending.owner);
                        }
                        if let SaveIntent::SettingsIconSize(dialog) = pending.intent {
                            settings_icon_size_saved(dialog, Ok(()))
                        } else {
                            Task::none()
                        }
                    }
                    Err(error) => {
                        if let SaveIntent::SettingsIconSize(dialog) = pending.intent {
                            settings_icon_size_saved(dialog, Err(error))
                        } else {
                            let error = if pending.intent == SaveIntent::Rename {
                                format!(
                                    "Folder renamed, but the toolbar could not be saved ({error})."
                                )
                            } else {
                                format!("Could not save the toolbar ({error}).")
                            };
                            self.problem = Some(error.clone());
                            if self.draft(pending.owner).is_none() {
                                Task::done(WorkspaceMessage::Notice(pending.owner, error))
                            } else {
                                Task::none()
                            }
                        }
                    }
                };
                if pending.needs_rewrite && self.unsaved_folder_paths {
                    let write = self.save(pending.owner, self.layout.clone(), SaveIntent::Rename);
                    return Task::batch([report, write]);
                }
                return report;
            }
            _ => {}
        }
        Task::none()
    }

    fn save(
        &mut self,
        owner: window::Id,
        layout: Layout,
        intent: SaveIntent,
    ) -> Task<WorkspaceMessage> {
        if layout == self.layout && !self.unsaved_folder_paths {
            if intent == SaveIntent::Finish {
                self.close(owner);
            }
            if let SaveIntent::SettingsIconSize(dialog) = intent {
                return settings_icon_size_saved(dialog, Ok(()));
            }
            return Task::none();
        }
        let to_write = layout.clone();
        self.pending = Some(Pending {
            owner,
            layout,
            intent,
            needs_rewrite: false,
        });
        Task::perform(
            async move {
                path()
                    .and_then(|path| save_to(&path, to_write))
                    .map_err(|error| error.to_string())
            },
            |result| wrap(super::Message::Toolbar(Message::Saved(result))),
        )
    }
}

fn settings_icon_size_saved(
    dialog: window::Id,
    result: Result<(), String>,
) -> Task<WorkspaceMessage> {
    Task::done(WorkspaceMessage::Settings(
        settings_window::Message::ToolbarIconSizeSaved(dialog, result),
    ))
}

fn path() -> io::Result<PathBuf> {
    Ok(profile::root()?.join("toolbar.json"))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    version: u32,
    layout: Layout,
}

fn load_from(path: &Path) -> io::Result<Layout> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_PROFILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let mut profile = decode_profile(&bytes)?;
    for group in Group::ACTIONS {
        for item in &mut profile.layout.section_mut(group).items {
            match item {
                Item::Reference(Reference::Journal) => {
                    *item = Item::Action(Action::SessionJournals)
                }
                Item::Reference(Reference::Logfile) => *item = Item::Action(Action::LogfileManager),
                Item::Reference(Reference::Keyboard) => {
                    *item = Item::Action(Action::KeyboardSettings)
                }
                Item::Reference(Reference::Colors) => *item = Item::Action(Action::ColorSettings),
                Item::Reference(Reference::Logging) => {
                    *item = Item::Action(Action::LoggingSettings)
                }
                #[cfg(windows)]
                Item::Reference(Reference::PowerShell) => *item = Item::Action(Action::PowerShell),
                #[cfg(windows)]
                Item::Reference(Reference::CommandPrompt) => {
                    *item = Item::Action(Action::CommandPrompt)
                }
                _ => {}
            }
        }
    }
    Ok(profile.layout)
}

pub(in crate::workspace) fn validate_backup(bytes: &[u8]) -> io::Result<()> {
    decode_profile(bytes).map(|_| ())
}

fn decode_profile(bytes: &[u8]) -> io::Result<Profile> {
    if bytes.len() as u64 > MAX_PROFILE_BYTES {
        return Err(invalid_profile());
    }
    let profile: Profile = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    if profile.version != 1 || !profile.layout.valid() {
        return Err(invalid_profile());
    }
    Ok(profile)
}

fn save_to(path: &Path, layout: Layout) -> io::Result<()> {
    if !layout.valid() {
        return Err(invalid_profile());
    }
    let bytes =
        serde_json::to_vec_pretty(&Profile { version: 1, layout }).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_PROFILE_BYTES {
        return Err(invalid_profile());
    }
    profile::save_atomic(path, &bytes)
}

fn invalid_profile() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid toolbar layout or unsupported profile version",
    )
}
