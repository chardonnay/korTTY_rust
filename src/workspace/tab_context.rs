use std::collections::HashMap;

use iced::keyboard::{Key, key::Named};
use iced::widget::{
    self, Column, button, column, container, mouse_area, row, rule, scrollable, space, text,
    text_input,
};
use iced::{Element, Length, Point, Size, Task, window};

use super::{
    Message as Desktop, SerialClose, Tab, Window, Workspace,
    chrome::{Navigation, popup},
    dashboard::{Action as DashboardAction, Target},
    journal_capture_access::{Note, TabJournalAction},
    multi_exec_access::Action as MultiExecAction,
    tab_watch::Kind as WatchKind,
};
use crate::workspace::dialog_placement::Kind as DialogKind;
use crate::{profile, session::SessionId};

const RENAME_TITLE: &str = "Rename Tab";
const JOURNAL_NOTE_TITLE: &str = "Journal Note";

#[derive(Default)]
pub(super) struct State {
    menu: Option<Menu>,
    renames: HashMap<window::Id, Rename>,
    journal_notes: HashMap<window::Id, JournalNote>,
    closing: HashMap<window::Id, Batch>,
}

impl State {
    pub(super) fn has_dialogs(&self) -> bool {
        !self.renames.is_empty() || !self.journal_notes.is_empty()
    }
}

struct Menu {
    owner: window::Id,
    tab: u64,
    request: u64,
    point: Point,
    selected: Option<Action>,
    journal: bool,
}

struct JournalNote {
    owner: window::Id,
    tab: u64,
    run: u64,
    input: widget::Id,
}

struct Rename {
    owner: window::Id,
    tab: u64,
    kind: NameField,
    draft: String,
    input: widget::Id,
}

enum NameField {
    Tab {
        automatic: String,
        connection: Option<String>,
    },
    NewGroup,
    RenameGroup(String),
}

impl NameField {
    fn title(&self) -> &'static str {
        match self {
            Self::Tab { .. } => RENAME_TITLE,
            Self::NewGroup => "New Group",
            Self::RenameGroup(_) => "Rename Group",
        }
    }
}

struct Batch {
    request: u64,
    anchor: Option<u64>,
    targets: Vec<CloseTarget>,
}

struct CloseTarget {
    tab: u64,
    sessions: Option<Vec<SessionId>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Action {
    Rename,
    Duplicate,
    Reconnect,
    Sftp,
    JournalMenu,
    Journal(TabJournalAction),
    MultiExec,
    Monitor(WatchKind),
    CloseOthers,
    CloseRight,
    ReopenClosed,
    NoGroup,
    AssignGroup(String),
    NewGroup,
    RenameGroup,
}

impl Action {
    const PRIMARY: [Self; 5] = [
        Self::Rename,
        Self::Duplicate,
        Self::Reconnect,
        Self::Sftp,
        Self::MultiExec,
    ];

    fn label(&self) -> &str {
        match self {
            Self::Rename => "Rename Tab…",
            Self::Duplicate => "Duplicate",
            Self::Reconnect => "Reconnect",
            Self::Sftp => "Open SFTP here",
            Self::JournalMenu => "Session Journal",
            Self::Journal(action) => action.label(),
            Self::MultiExec => "Multi-exec: Include All Panes of This Tab",
            Self::Monitor(kind) => kind.label(),
            Self::CloseOthers => "Close Other Tabs",
            Self::CloseRight => "Close Tabs to the Right",
            Self::ReopenClosed => "Reopen Closed Tab",
            Self::NoGroup => "No Group",
            Self::AssignGroup(group) => group,
            Self::NewGroup => "New Group…",
            Self::RenameGroup => "Rename Group…",
        }
    }

    fn dashboard(&self) -> Option<DashboardAction> {
        match self {
            Self::Duplicate => Some(DashboardAction::Duplicate),
            Self::Sftp => Some(DashboardAction::Sftp),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Message {
    Open(u64, Point),
    Dismiss(u64),
    Navigate(u64, Navigation),
    OpenJournal(u64),
    PointAtRoot(u64, Action),
    Invoke(u64, Action),
    JournalNoteFocus,
    JournalNote(Note),
    RenameFocus,
    RenameInput(String),
    RenameFinish(bool),
}

impl Workspace {
    pub(super) fn apply_saved_tab_groups(&mut self, store: &crate::connection_store::Store) {
        let mut persistent = false;
        for window in self.windows.values_mut() {
            if let Some(pending) = &mut window.pending_ssh {
                pending.refresh_connection_group(store);
            }
            let mut changed = false;
            for tab in window.terminal_tabs_mut() {
                let Some(target) = tab
                    .grid
                    .get(tab.primary)
                    .and_then(|session| self.sessions.get(session))
                    .and_then(|pane| pane.ssh.as_ref())
                else {
                    continue;
                };
                let Some(id) = target.saved_id().filter(|id| {
                    store
                        .get(*id)
                        .is_some_and(|connection| target.matches(connection))
                }) else {
                    continue;
                };
                let group = store.group_of(id).and_then(profile::normalize_tab_group);
                if tab.connection_group_baseline != group {
                    tab.connection_group_baseline = group.clone();
                    if tab.group != group {
                        tab.group = group;
                        changed = true;
                        persistent |= !tab.incognito;
                    }
                }
            }
            if changed {
                window.arrange_tab_groups();
            }
        }
        if persistent {
            self.session_restore.changed(false);
        }
    }

    pub(super) fn place_duplicate_tab(&mut self, owner: window::Id, source: u64) {
        let Some(window) = self.windows.get_mut(&owner) else {
            return;
        };
        let Some(source_tab) = window.terminal_tabs().find(|tab| tab.id == source) else {
            return;
        };
        let group = source_tab.group.clone();
        if window.active_tab_id() == Some(source) || window.tab().is_none() {
            return;
        }
        let mut duplicate = window.tabs.remove(window.active);
        if let Some(tab) = duplicate.terminal_mut() {
            tab.group = group;
        }
        let index = window
            .tabs
            .iter()
            .position(|tab| tab.id() == source)
            .map_or(window.tabs.len(), |index| index + 1);
        window.tabs.insert(index, duplicate);
        window.active = index;
        window.arrange_tab_groups();
        self.session_restore.changed(false);
    }

    pub(super) fn update_tab_context(&mut self, id: window::Id, message: Message) -> Task<Desktop> {
        match message {
            Message::Open(tab, point) => {
                if !self.palette_can_open(id)
                    || self.palette.is_open(id)
                    || self.terminal_tab(id, tab).is_none()
                    || !point.x.is_finite()
                    || !point.y.is_finite()
                {
                    return Task::none();
                }
                self.chrome.dismiss(id);
                self.dismiss_terminal_context(id);
                let request = self.allocate();
                self.tab_context.menu = Some(Menu {
                    owner: id,
                    tab,
                    point,
                    request,
                    selected: None,
                    journal: false,
                });
                Task::none()
            }
            Message::Dismiss(request) => {
                self.tab_context
                    .menu
                    .take_if(|menu| menu.owner == id && menu.request == request);
                Task::none()
            }
            Message::Navigate(request, direction) => {
                self.navigate_tab_context(id, request, direction)
            }
            Message::OpenJournal(request) => {
                self.open_tab_journal_menu(id, request);
                Task::none()
            }
            Message::PointAtRoot(request, action) => {
                if let Some(menu) = self
                    .tab_context
                    .menu
                    .as_mut()
                    .filter(|menu| menu.owner == id && menu.request == request && menu.journal)
                {
                    menu.journal = false;
                    menu.selected = Some(action);
                }
                Task::none()
            }
            Message::Invoke(request, action) => self.invoke_tab_context(id, request, action),
            Message::JournalNoteFocus => self.focus_tab_journal_note(id),
            Message::JournalNote(action) => self.update_tab_journal_note(id, action),
            Message::RenameFocus => {
                self.tab_context
                    .renames
                    .get(&id)
                    .map_or_else(Task::none, |rename| {
                        Task::batch([
                            window::gain_focus(id),
                            widget::operation::focus(rename.input.clone()),
                        ])
                    })
            }
            Message::RenameInput(value) => {
                if let Some(rename) = self.tab_context.renames.get_mut(&id) {
                    rename.draft = value.chars().take(1024).collect();
                }
                Task::none()
            }
            Message::RenameFinish(commit) => self.finish_rename_tab(id, commit),
        }
    }

    pub(super) fn dismiss_tab_context(&mut self, id: window::Id) {
        self.tab_context.menu.take_if(|menu| menu.owner == id);
    }

    pub(super) fn tab_context_key(&mut self, id: window::Id, key: &Key) -> Option<Task<Desktop>> {
        let menu = self
            .tab_context
            .menu
            .as_ref()
            .filter(|menu| menu.owner == id)?;
        if !self.palette_can_open(id)
            || self.palette.is_open(id)
            || self.terminal_tab(id, menu.tab).is_none()
        {
            self.dismiss_tab_context(id);
            return None;
        }
        let request = menu.request;
        let direction = match key {
            Key::Named(Named::Escape) => Navigation::Escape,
            Key::Named(Named::ArrowUp) => Navigation::Previous,
            Key::Named(Named::ArrowDown) => Navigation::Next,
            Key::Named(Named::ArrowLeft) => Navigation::Left,
            Key::Named(Named::ArrowRight) => Navigation::Right,
            Key::Named(Named::Enter) => Navigation::Activate,
            _ => return Some(Task::none()),
        };
        Some(self.navigate_tab_context(id, request, direction))
    }

    fn navigate_tab_context(
        &mut self,
        id: window::Id,
        request: u64,
        direction: Navigation,
    ) -> Task<Desktop> {
        let Some(menu) = self
            .tab_context
            .menu
            .as_ref()
            .filter(|menu| menu.owner == id && menu.request == request)
        else {
            return Task::none();
        };
        if menu.journal && matches!(direction, Navigation::Escape | Navigation::Left) {
            if let Some(menu) = self.tab_context.menu.as_mut() {
                menu.journal = false;
                menu.selected = Some(Action::JournalMenu);
            }
            return Task::none();
        }
        if matches!(direction, Navigation::Escape | Navigation::Left) {
            return self.update_tab_context(id, Message::Dismiss(request));
        }
        if matches!(direction, Navigation::Right | Navigation::Activate)
            && menu.selected == Some(Action::JournalMenu)
        {
            self.open_tab_journal_menu(id, request);
            return Task::none();
        }
        if matches!(direction, Navigation::Activate) {
            return menu.selected.clone().map_or_else(Task::none, |action| {
                self.invoke_tab_context(id, request, action)
            });
        }
        if !matches!(direction, Navigation::Previous | Navigation::Next) {
            return Task::none();
        }
        let actions = self.tab_context_level_actions(menu);
        let enabled = actions
            .into_iter()
            .filter_map(|(action, enabled)| enabled.then_some(action))
            .collect::<Vec<_>>();
        let selected = menu
            .selected
            .as_ref()
            .and_then(|action| enabled.iter().position(|item| item == action));
        let next = if enabled.is_empty() {
            None
        } else {
            let last = enabled.len() - 1;
            let index = match (direction, selected) {
                (Navigation::Previous, None | Some(0)) => last,
                (Navigation::Previous, Some(index)) => index - 1,
                (_, None) => 0,
                (_, Some(index)) => (index + 1) % enabled.len(),
            };
            Some(enabled[index].clone())
        };
        if let Some(menu) = self.tab_context.menu.as_mut() {
            menu.selected = next;
        }
        Task::none()
    }

    fn open_tab_journal_menu(&mut self, owner: window::Id, request: u64) {
        if let Some(menu) = self
            .tab_context
            .menu
            .as_ref()
            .filter(|menu| menu.owner == owner && menu.request == request && !menu.journal)
            && self
                .tab_context_menu_actions(owner, menu.tab)
                .contains(&(Action::JournalMenu, true))
            && let Some(menu) = &mut self.tab_context.menu
        {
            menu.journal = true;
            menu.selected = None;
        }
    }

    fn invoke_tab_context(
        &mut self,
        owner: window::Id,
        request: u64,
        action: Action,
    ) -> Task<Desktop> {
        let Some(menu) = self
            .tab_context
            .menu
            .take_if(|menu| menu.owner == owner && menu.request == request)
        else {
            return Task::none();
        };
        let actions = if matches!(action, Action::Journal(_)) {
            self.tab_context_level_actions(&menu)
        } else {
            self.tab_context_menu_actions(owner, menu.tab)
        };
        if !actions
            .iter()
            .any(|(candidate, enabled)| candidate == &action && *enabled)
        {
            return Task::none();
        }
        if let Some(action) = action.dashboard() {
            return self.perform_dashboard_action(owner, Target::tab(menu.tab), action);
        }
        match action {
            Action::Rename => self.show_rename_tab(owner, menu.tab),
            Action::Reconnect => self.reconnect_tab(owner, menu.tab),
            Action::Journal(TabJournalAction::Start(_) | TabJournalAction::Stop(_)) => {
                self.toggle_journal_for(owner, menu.tab)
            }
            Action::Journal(TabJournalAction::Screenshot(Some(run))) => {
                self.dock_journal_screenshot(owner, menu.tab, run)
            }
            Action::Journal(TabJournalAction::Note(Some(run))) => {
                self.show_tab_journal_note(owner, menu.tab, run)
            }
            Action::MultiExec => self.update(Desktop::MultiExec(
                Some(owner),
                MultiExecAction::TabAt(menu.tab),
            )),
            Action::Monitor(kind) => {
                self.toggle_tab_watch(owner, menu.tab, kind);
                Task::none()
            }
            Action::ReopenClosed => self.reopen_closed_tab(Some(owner), None),
            Action::NoGroup => {
                self.set_tab_group(owner, menu.tab, None);
                Task::none()
            }
            Action::AssignGroup(group) => {
                self.set_tab_group(owner, menu.tab, Some(group));
                Task::none()
            }
            Action::NewGroup => self.show_tab_name_dialog(owner, menu.tab, NameField::NewGroup),
            Action::RenameGroup => {
                let Some(group) = self
                    .terminal_tab(owner, menu.tab)
                    .and_then(|tab| tab.group.clone())
                else {
                    return Task::none();
                };
                self.show_tab_name_dialog(owner, menu.tab, NameField::RenameGroup(group))
            }
            Action::CloseOthers | Action::CloseRight => {
                self.request_tab_batch_close(owner, menu.tab, action == Action::CloseRight)
            }
            _ => Task::none(),
        }
    }

    fn tab_context_actions(&self, owner: window::Id, tab: u64) -> [(Action, bool); 8] {
        let allowed = self.palette_can_open(owner) && !self.palette.is_open(owner);
        let index = allowed
            .then(|| {
                self.windows
                    .get(&owner)?
                    .tabs
                    .iter()
                    .position(|entry| entry.id() == tab && entry.terminal().is_some())
            })
            .flatten();
        let dashboard = if index.is_some() {
            self.dashboard_actions(owner, Target::tab(tab))
        } else {
            Vec::new()
        };
        let count = self
            .windows
            .get(&owner)
            .map_or(0, |window| window.tabs.len());
        [
            (Action::Rename, index.is_some()),
            (
                Action::Duplicate,
                dashboard.contains(&DashboardAction::Duplicate),
            ),
            (
                Action::Reconnect,
                index.is_some() && self.can_reconnect_tab(owner, tab),
            ),
            (Action::Sftp, dashboard.contains(&DashboardAction::Sftp)),
            (
                Action::MultiExec,
                index.is_some()
                    && self.multi_exec_available(Some(owner), MultiExecAction::TabAt(tab)),
            ),
            (Action::CloseOthers, index.is_some() && count > 1),
            (
                Action::CloseRight,
                index.is_some_and(|index| index + 1 < count),
            ),
            (
                Action::ReopenClosed,
                index.is_some() && self.has_recently_closed(),
            ),
        ]
    }

    fn tab_context_menu_actions(&self, owner: window::Id, tab: u64) -> Vec<(Action, bool)> {
        let mut actions = Vec::from(self.tab_context_actions(owner, tab));
        if actions[0].1 {
            actions.splice(
                Action::PRIMARY.len()..Action::PRIMARY.len(),
                monitor_actions(),
            );
            if self.tab_journal_actions(owner, tab).is_some() {
                actions.insert(Action::PRIMARY.len() - 1, (Action::JournalMenu, true));
            }
            actions.extend(self.tab_group_actions(owner, tab));
        }
        actions
    }

    fn tab_context_level_actions(&self, menu: &Menu) -> Vec<(Action, bool)> {
        if !menu.journal {
            return self.tab_context_menu_actions(menu.owner, menu.tab);
        }
        if !self.palette_can_open(menu.owner) || self.palette.is_open(menu.owner) {
            return Vec::new();
        }
        self.tab_journal_actions(menu.owner, menu.tab)
            .into_iter()
            .flatten()
            .map(|(action, enabled)| (Action::Journal(action), enabled))
            .collect()
    }

    fn tab_group_actions(&self, owner: window::Id, tab: u64) -> Vec<(Action, bool)> {
        let Some(target) = self.terminal_tab(owner, tab) else {
            return Vec::new();
        };
        let mut groups: Vec<_> = self.windows[&owner]
            .terminal_tabs()
            .filter(|tab| target.incognito || !tab.incognito)
            .filter_map(|tab| tab.group.as_ref())
            .collect();
        groups.sort_by_cached_key(|group| (group.to_lowercase(), *group));
        groups.dedup();
        let current = target.group.as_ref();
        let mut actions = Vec::with_capacity(groups.len() + 3);
        actions.push((Action::NoGroup, current.is_some()));
        actions.extend(
            groups
                .into_iter()
                .map(|group| (Action::AssignGroup(group.clone()), Some(group) != current)),
        );
        actions.push((Action::NewGroup, true));
        if current.is_some() {
            actions.push((Action::RenameGroup, true));
        }
        actions
    }

    pub(super) fn tab_context_header<'a>(
        &'a self,
        owner: window::Id,
        tab: u64,
        header: Element<'a, Desktop>,
    ) -> Element<'a, Desktop> {
        let Some(menu) = self
            .tab_context
            .menu
            .as_ref()
            .filter(|menu| menu.owner == owner && menu.tab == tab)
        else {
            return header;
        };
        let actions = self.tab_context_actions(owner, tab);
        if !actions[0].1 {
            return header;
        }
        let request = menu.request;
        let mut primary = actions[..Action::PRIMARY.len()].to_vec();
        if self.tab_journal_actions(owner, tab).is_some() {
            primary.insert(Action::PRIMARY.len() - 1, (Action::JournalMenu, true));
        }
        let rows = |entries| self.tab_context_rows(menu, entries, false);
        let groups = self.tab_group_actions(owner, tab);
        let content = column![
            rows(primary),
            rule::horizontal(1),
            rows(monitor_actions().collect()),
            rule::horizontal(1),
            rows(actions[Action::PRIMARY.len()..].to_vec()),
            rule::horizontal(1),
            rows(groups),
        ]
        .spacing(4);
        popup::at_point(
            header,
            menu.point,
            container(scrollable(content).height(Length::Shrink))
                .width(288)
                .padding(5)
                .style(container::rounded_box)
                .into(),
            Desktop::TabContext(owner, Message::Dismiss(request)),
            move |direction| Desktop::TabContext(owner, Message::Navigate(request, direction)),
        )
    }

    fn tab_context_rows<'a>(
        &'a self,
        menu: &'a Menu,
        entries: Vec<(Action, bool)>,
        nested: bool,
    ) -> Column<'a, Desktop> {
        let send = |message| Desktop::TabContext(menu.owner, message);
        entries
            .into_iter()
            .fold(column![].spacing(2), |items, (action, enabled)| {
                let branch = action == Action::JournalMenu;
                let checked = match action {
                    Action::MultiExec => {
                        self.multi_exec_checked(menu.owner, MultiExecAction::TabAt(menu.tab))
                    }
                    Action::Monitor(kind) => self
                        .terminal_tab(menu.owner, menu.tab)
                        .is_some_and(|tab| tab.watch.enabled(kind)),
                    _ => false,
                };
                let selected = menu.selected.as_ref() == Some(&action) || (branch && menu.journal);
                let invoke = if branch {
                    Message::OpenJournal(menu.request)
                } else {
                    Message::Invoke(menu.request, action.clone())
                };
                let item = button(
                    row![
                        text(if checked { "✓" } else { "" }).width(15),
                        text(action.label().to_owned()).size(13),
                        space::horizontal(),
                        branch.then(|| text("›").size(15)),
                    ]
                    .align_y(iced::Alignment::Center),
                )
                .width(Length::Fill)
                .padding([5, 6])
                .style(if selected {
                    button::secondary
                } else {
                    button::text
                })
                .on_press_maybe(enabled.then_some(send(invoke)));
                if branch {
                    let trigger: Element<'_, Desktop> = mouse_area(item)
                        .on_enter(send(Message::OpenJournal(menu.request)))
                        .into();
                    if menu.journal {
                        let contents =
                            self.tab_context_rows(menu, self.tab_context_level_actions(menu), true);
                        return items.push(popup::submenu(
                            trigger,
                            container(contents)
                                .width(198)
                                .padding(5)
                                .style(container::rounded_box)
                                .into(),
                        ));
                    }
                    return items.push(trigger);
                }
                if menu.journal && !nested {
                    return items.push(
                        mouse_area(item).on_enter(send(Message::PointAtRoot(menu.request, action))),
                    );
                }
                items.push(item)
            })
    }

    fn window_has_tab(&self, owner: window::Id, tab: u64) -> bool {
        self.windows
            .get(&owner)
            .is_some_and(|window| window.tabs.iter().any(|entry| entry.id() == tab))
    }

    fn terminal_tab(&self, owner: window::Id, id: u64) -> Option<&Tab> {
        self.windows
            .get(&owner)?
            .terminal_tabs()
            .find(|tab| tab.id == id)
    }

    pub(super) fn request_close_all_tabs(&mut self, owner: window::Id) -> Task<Desktop> {
        self.request_tab_batch(owner, None)
    }

    pub(super) fn request_tab_batch_close(
        &mut self,
        owner: window::Id,
        anchor: u64,
        right: bool,
    ) -> Task<Desktop> {
        self.request_tab_batch(owner, Some((anchor, right)))
    }

    fn request_tab_batch(
        &mut self,
        owner: window::Id,
        around: Option<(u64, bool)>,
    ) -> Task<Desktop> {
        if !self.palette_can_open(owner) {
            return Task::none();
        }
        let Some(window) = self.windows.get(&owner) else {
            return Task::none();
        };
        let cutoff = if let Some((anchor, right)) = around {
            let Some(index) = window.tabs.iter().position(|entry| entry.id() == anchor) else {
                return Task::none();
            };
            Some((index, right))
        } else {
            None
        };
        let targets = window
            .tabs
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                cutoff.is_none_or(|(index, right)| if right { *i > index } else { *i != index })
            })
            .map(|(_, entry)| CloseTarget {
                tab: entry.id(),
                sessions: entry
                    .terminal()
                    .map(|tab| tab.grid.iter().map(|(_, id)| *id).collect()),
            })
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return Task::none();
        }
        let request = self.allocate();
        self.tab_context.closing.insert(
            owner,
            Batch {
                request,
                anchor: around.map(|(tab, _)| tab),
                targets,
            },
        );
        let confirm = self.request_close(owner, SerialClose::TabBatch(request));
        if around.is_none() {
            return Task::batch([confirm, self.show_close_all_dialog(owner, request)]);
        }
        confirm
    }

    pub(super) fn tab_batch_all_count(&self, owner: window::Id, request: u64) -> Option<usize> {
        let batch = self.tab_context.closing.get(&owner)?;
        (batch.request == request && batch.anchor.is_none()).then_some(batch.targets.len())
    }

    fn live_batch(&self, owner: window::Id, request: u64) -> Option<&Batch> {
        self.tab_context
            .closing
            .get(&owner)
            .filter(|batch| batch.request == request)
            .filter(|batch| {
                self.windows.contains_key(&owner)
                    && batch
                        .anchor
                        .is_none_or(|anchor| self.window_has_tab(owner, anchor))
            })
    }

    pub(super) fn tab_batch_still_applies(&self, owner: window::Id, request: u64) -> bool {
        self.live_batch(owner, request).is_some()
    }

    pub(super) fn tool_targets_for_batch(&self, owner: window::Id, request: u64) -> Vec<u64> {
        self.live_batch(owner, request)
            .into_iter()
            .flat_map(|batch| &batch.targets)
            .filter(|target| target.sessions.is_none() && self.window_has_tab(owner, target.tab))
            .map(|target| target.tab)
            .collect()
    }

    pub(super) fn tab_batch_needs_close_confirmation(
        &self,
        owner: window::Id,
        request: u64,
    ) -> bool {
        let Some(batch) = self
            .tab_context
            .closing
            .get(&owner)
            .filter(|batch| batch.request == request)
        else {
            return false;
        };
        batch.targets.iter().any(|target| {
            self.terminal_tab(owner, target.tab)
                .is_some_and(|tab| self.tab_needs_close_confirmation(tab))
        })
    }

    pub(super) fn cancel_tab_batch_close(&mut self, owner: window::Id, request: u64) {
        if self
            .tab_context
            .closing
            .get(&owner)
            .is_some_and(|batch| batch.request == request)
        {
            self.tab_context.closing.remove(&owner);
        }
    }

    pub(super) fn apply_tab_batch_close(
        &mut self,
        owner: window::Id,
        request: u64,
    ) -> Task<Desktop> {
        if !self.tab_batch_still_applies(owner, request) {
            self.cancel_tab_batch_close(owner, request);
            return Task::none();
        }
        if !self.tool_targets_for_batch(owner, request).is_empty() {
            return Task::none();
        }
        let Some(batch) = self.tab_context.closing.remove(&owner) else {
            return Task::none();
        };
        let focus = batch.anchor.map_or_else(Task::none, |anchor| {
            self.update(Desktop::SelectTab(owner, anchor))
        });
        let targets = batch
            .targets
            .iter()
            .filter_map(|target| {
                let sessions = target.sessions.as_ref()?;
                self.terminal_tab(owner, target.tab).filter(|tab| {
                    tab.grid.len() == sessions.len()
                        && tab
                            .grid
                            .iter()
                            .all(|(_, session)| sessions.contains(session))
                })
            })
            .collect::<Vec<_>>();
        let remembered = targets
            .iter()
            .filter_map(|tab| self.remember_tab(tab))
            .collect();
        let targets = targets.into_iter().map(|tab| tab.id).collect::<Vec<_>>();
        self.remember_closed_tabs(false, remembered);
        for tab in targets {
            self.close_tab(owner, tab);
        }
        self.refresh_connection_counts();
        focus
    }

    pub(super) fn show_rename_tab(&mut self, owner: window::Id, tab: u64) -> Task<Desktop> {
        let Some(tab) = self.terminal_tab(owner, tab) else {
            return Task::none();
        };
        let (automatic, connection) = self.automatic_tab_names(tab);
        self.show_tab_name_dialog(
            owner,
            tab.id,
            NameField::Tab {
                automatic,
                connection,
            },
        )
    }

    fn show_tab_name_dialog(
        &mut self,
        owner: window::Id,
        target: u64,
        kind: NameField,
    ) -> Task<Desktop> {
        if !self.palette_can_open(owner) || self.palette.is_open(owner) {
            return Task::none();
        }
        let Some(tab) = self.terminal_tab(owner, target) else {
            return Task::none();
        };
        let draft = match &kind {
            NameField::Tab { automatic, .. } => {
                tab.custom_title.as_ref().unwrap_or(automatic).clone()
            }
            NameField::NewGroup => String::new(),
            NameField::RenameGroup(group) => group.clone(),
        };
        let dialog_kind = if matches!(kind, NameField::Tab { .. }) {
            DialogKind::TabRename
        } else {
            DialogKind::TabGroupName
        };
        let (id, opened) = self.open_owned_placed_dialog(
            owner,
            dialog_kind,
            window::Settings {
                size: Size::new(475.0, 200.0),
                min_size: Some(Size::new(360.0, 185.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            Some(|id| Desktop::TabContext(id, Message::RenameFocus)),
        );
        self.tab_context.renames.insert(
            id,
            Rename {
                owner,
                tab: target,
                kind,
                draft,
                input: widget::Id::unique(),
            },
        );
        opened
    }

    fn finish_rename_tab(&mut self, id: window::Id, commit: bool) -> Task<Desktop> {
        let Some(rename) = self.tab_context.renames.remove(&id) else {
            return Task::none();
        };
        if commit {
            match rename.kind {
                NameField::Tab { automatic, .. } => {
                    if let Some(tab) = self.windows.get_mut(&rename.owner).and_then(|window| {
                        window.terminal_tabs_mut().find(|tab| tab.id == rename.tab)
                    }) {
                        let title = profile::normalize_tab_title(&rename.draft);
                        let title = (!title.is_empty() && title != automatic).then_some(title);
                        if title != tab.custom_title {
                            tab.custom_title = title;
                            if !tab.incognito {
                                self.session_restore.changed(false);
                            }
                        }
                    }
                }
                NameField::NewGroup => {
                    if let Some(group) = profile::normalize_tab_group(&rename.draft) {
                        self.set_tab_group(rename.owner, rename.tab, Some(group));
                    }
                }
                NameField::RenameGroup(old) => {
                    if let Some(new) = profile::normalize_tab_group(&rename.draft) {
                        self.rename_tab_group(rename.owner, rename.tab, &old, new);
                    }
                }
            }
        }
        if self.windows.contains_key(&rename.owner) {
            self.close_placed_dialog_and_focus(id, rename.owner)
        } else {
            self.close_placed_dialog(id)
        }
    }

    pub(super) fn rename_dialog_title(&self, id: window::Id) -> Option<&'static str> {
        self.tab_context
            .renames
            .get(&id)
            .map(|rename| rename.kind.title())
    }

    pub(super) fn rename_dialog_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.tab_context
            .renames
            .iter()
            .find_map(|(&id, rename)| (rename.owner == owner).then_some(id))
    }

    pub(super) fn remove_rename_dialog_for_owner(&mut self, owner: window::Id) {
        if let Some(id) = self.rename_dialog_for_owner(owner) {
            self.tab_context.renames.remove(&id);
        }
    }

    pub(super) fn close_rename_dialog(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        self.tab_context
            .renames
            .contains_key(&id)
            .then(|| self.finish_rename_tab(id, false))
    }

    pub(super) fn close_tab_context_owner(&mut self, owner: window::Id) -> Task<Desktop> {
        self.dismiss_tab_context(owner);
        self.tab_context.closing.remove(&owner);
        self.tab_journal_note_for_owner(owner)
            .and_then(|id| {
                self.remove_tab_journal_note(id)
                    .map(|_| self.close_placed_dialog(id))
            })
            .unwrap_or_else(Task::none)
    }

    fn show_tab_journal_note(&mut self, owner: window::Id, tab: u64, run: u64) -> Task<Desktop> {
        if !self.palette_can_open(owner) || !self.dock_note_enabled(owner, tab, run) {
            return Task::none();
        }
        let draft = self.dock_journal_note(owner, tab, run, Note::Edit);
        if !self.journal_note_draft_open(tab, run) {
            return draft;
        }
        let (id, opened) = self.open_placed_dialog(
            DialogKind::JournalNote,
            window::Settings {
                size: Size::new(585.0, 290.0),
                min_size: Some(Size::new(420.0, 250.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.tab_context.journal_notes.insert(
            id,
            JournalNote {
                owner,
                tab,
                run,
                input: widget::Id::unique(),
            },
        );
        Task::batch([
            draft,
            opened.map(move |_| Desktop::TabContext(id, Message::JournalNoteFocus)),
        ])
    }

    fn update_tab_journal_note(&mut self, id: window::Id, action: Note) -> Task<Desktop> {
        if matches!(action, Note::Cancel) {
            return self.close_tab_journal_note(id).unwrap_or_else(Task::none);
        }
        let Some(note) = self.tab_context.journal_notes.get(&id) else {
            return Task::none();
        };
        let (owner, tab, run) = (note.owner, note.tab, note.run);
        if !self
            .owner_dialog(owner)
            .is_some_and(|(dialog, _)| dialog == id)
            || !self.dock_note_enabled(owner, tab, run)
            || !self.journal_note_draft_open(tab, run)
        {
            return Task::none();
        }
        let saving = matches!(action, Note::Save);
        let task = self.dock_journal_note(owner, tab, run, action);
        if saving && !self.journal_note_draft_open(tab, run) {
            return Task::batch([
                task,
                self.close_tab_journal_note(id).unwrap_or_else(Task::none),
            ]);
        }
        task
    }

    fn focus_tab_journal_note(&self, id: window::Id) -> Task<Desktop> {
        self.tab_context
            .journal_notes
            .get(&id)
            .map_or_else(Task::none, |note| {
                Task::batch([
                    window::gain_focus(id),
                    widget::operation::focus(note.input.clone()),
                ])
            })
    }

    pub(super) fn tab_journal_note_title(&self, id: window::Id) -> Option<&'static str> {
        self.tab_context
            .journal_notes
            .contains_key(&id)
            .then_some(JOURNAL_NOTE_TITLE)
    }

    pub(super) fn tab_journal_note_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.tab_context
            .journal_notes
            .iter()
            .find_map(|(&id, note)| (note.owner == owner).then_some(id))
    }

    fn remove_tab_journal_note(&mut self, id: window::Id) -> Option<JournalNote> {
        let note = self.tab_context.journal_notes.remove(&id)?;
        self.discard_journal_note(note.tab, note.run);
        Some(note)
    }

    pub(super) fn close_tab_journal_note(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        let note = self.remove_tab_journal_note(id)?;
        let close = self.close_placed_dialog(id);
        Some(if self.windows.contains_key(&note.owner) {
            close.chain(window::gain_focus(note.owner))
        } else {
            close
        })
    }

    pub(super) fn tab_journal_note_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let note = self.tab_context.journal_notes.get(&id)?;
        let send = move |action| Desktop::TabContext(id, Message::JournalNote(action));
        let editor = self
            .journal_note_editor(
                note.owner,
                note.tab,
                note.run,
                send,
                Some(note.input.clone()),
            )
            .unwrap_or_else(|| {
                column![
                    text("This journal recording is unavailable."),
                    button("Close").on_press(send(Note::Cancel)),
                ]
                .spacing(8)
                .into()
            });
        Some(
            container(
                column![text("Add a note to the session journal").size(18), editor].spacing(12),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(16)
            .into(),
        )
    }

    pub(super) fn rename_dialog_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let rename = self.tab_context.renames.get(&id)?;
        let send = move |message| Desktop::TabContext(id, message);
        let (header, label, placeholder, explanation) = match &rename.kind {
            NameField::Tab {
                automatic,
                connection,
            } => {
                let explanation = match connection {
                    Some(connection) => format!(
                        "Leave it empty to show the shell title again. The connection is named {connection}."
                    ),
                    None => {
                        format!("Leave it empty to show the connection name ({automatic}) again.")
                    }
                };
                (
                    "Enter a name for this tab.",
                    "Tab name",
                    automatic.as_str(),
                    Some(explanation),
                )
            }
            NameField::NewGroup => (
                "Enter group name",
                "Group Name",
                "e.g. Production, Development",
                None,
            ),
            NameField::RenameGroup(group) => (
                "Enter new group name",
                "New Group Name",
                group.as_str(),
                None,
            ),
        };
        let submit = send(Message::RenameFinish(true));
        let can_submit = matches!(rename.kind, NameField::Tab { .. })
            || profile::normalize_tab_group(&rename.draft).is_some();
        Some(
            container(
                column![
                    text(header).size(18),
                    explanation.map(|explanation| text(explanation).size(13)),
                    row![
                        text(label).size(13),
                        text_input(placeholder, &rename.draft)
                            .id(rename.input.clone())
                            .on_input(move |input| send(Message::RenameInput(input)))
                            .on_submit_maybe(can_submit.then_some(submit.clone())),
                    ]
                    .spacing(8)
                    .align_y(iced::Alignment::Center),
                    space::vertical(),
                    row![
                        space::horizontal(),
                        button("Cancel").on_press(send(Message::RenameFinish(false))),
                        button("OK").on_press_maybe(can_submit.then_some(submit)),
                    ]
                    .spacing(8),
                ]
                .spacing(12),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(16)
            .into(),
        )
    }
}

fn monitor_actions() -> impl Iterator<Item = (Action, bool)> {
    WatchKind::ALL
        .into_iter()
        .map(|kind| (Action::Monitor(kind), true))
}

impl Workspace {
    pub(super) fn set_tab_group(&mut self, owner: window::Id, id: u64, group: Option<String>) {
        let Some(window) = self.windows.get_mut(&owner) else {
            return;
        };
        let Some(tab) = window.terminal_tabs_mut().find(|tab| tab.id == id) else {
            return;
        };
        if tab.group != group {
            tab.group = group;
            let persistent = !tab.incognito;
            window.arrange_tab_groups();
            if persistent {
                self.session_restore.changed(false);
            }
        }
    }

    fn rename_tab_group(&mut self, owner: window::Id, id: u64, old: &str, new: String) {
        if old == new
            || self
                .terminal_tab(owner, id)
                .and_then(|tab| tab.group.as_deref())
                != Some(old)
        {
            return;
        }
        if let Some(window) = self.windows.get_mut(&owner) {
            let mut persistent = false;
            for tab in window
                .terminal_tabs_mut()
                .filter(|tab| tab.group.as_deref() == Some(old))
            {
                tab.group = Some(new.clone());
                persistent |= !tab.incognito;
            }
            window.arrange_tab_groups();
            if persistent {
                self.session_restore.changed(false);
            }
        }
    }
}

impl Window {
    pub(super) fn arrange_tab_groups(&mut self) {
        if !self.terminal_tabs().any(|tab| tab.group.is_some()) {
            return;
        }
        let active = self.active_tab_id();
        self.tabs
            .sort_by_cached_key(|entry| match entry.terminal() {
                Some(tab) => match &tab.group {
                    Some(group) => (1, group.to_lowercase()),
                    None => (0, String::new()),
                },
                None => (2, String::new()),
            });
        if let Some(index) = self
            .tabs
            .iter()
            .position(|entry| Some(entry.id()) == active)
        {
            self.active = index;
        }
    }
}

#[cfg(test)]
mod tests;
