use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use iced::widget::{pane_grid, text};
use iced::{Element, Task, window};

use crate::connection_store::{Connection, Store};
use crate::session::{SessionId, Status};
use crate::session_security::IsolationLevel;

use super::chrome::Navigation;
use super::{Message as Desktop, PaneSession, Tab, Window, Workspace, multi_exec_access, profile};

#[cfg(test)]
mod tests;
mod view;

#[derive(Default)]
pub(super) struct State {
    visible: bool,
    collapsed: HashSet<Branch>,
    context: Option<(Target, usize)>,
    request: Option<u64>,
    catalog: Option<Arc<Catalog>>,
}

impl State {
    pub(super) fn visible(&self) -> bool {
        self.visible
    }

    fn expanded(&self, branch: &Branch) -> bool {
        !self.collapsed.contains(branch)
    }
}

pub struct Catalog(Store);

impl std::fmt::Debug for Catalog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DashboardCatalog")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Toggle,
    Refresh,
    Loaded(u64, Result<Arc<Catalog>, String>),
    Disclosure(Branch),
    ExpandAll,
    Focus(Target),
    Menu(Target),
    DismissMenu,
    NavigateMenu(Navigation),
    Perform(Target, Action),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Branch {
    Window,
    Environment(String),
    Group(String),
    Tab(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Target {
    tab: u64,
    session: Option<SessionId>,
}

impl Target {
    pub(super) fn tab(tab: u64) -> Self {
        Self { tab, session: None }
    }

    fn multi_exec(self) -> multi_exec_access::Action {
        self.session.map_or(
            multi_exec_access::Action::TabAt(self.tab),
            multi_exec_access::Action::PaneAt,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Focus,
    Duplicate,
    Reconnect,
    Sftp,
    Close,
    MultiExec,
}

impl Action {
    fn label(self, target: Target) -> &'static str {
        match self {
            Self::Focus => "Focus",
            Self::Duplicate => "Duplicate",
            Self::Reconnect => "Reconnect",
            Self::Sftp => "SFTP Manager…",
            Self::Close => "Close",
            Self::MultiExec if target.session.is_some() => "Multi-exec: Include This Pane",
            Self::MultiExec => "Multi-exec: Include All Panes of This Tab",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionState {
    Connecting,
    Connected,
    Disconnected,
    Ended,
    Error,
}

impl From<&Status> for ConnectionState {
    fn from(status: &Status) -> Self {
        match status {
            Status::Disconnected => Self::Disconnected,
            Status::Starting => Self::Connecting,
            Status::Running => Self::Connected,
            Status::Exited(_) => Self::Ended,
            Status::Error(_) => Self::Error,
        }
    }
}

impl ConnectionState {
    fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting",
            Self::Connected => "Connected",
            Self::Disconnected => "Disconnected",
            Self::Ended => "Ended",
            Self::Error => "Connection error",
        }
    }

    fn indicator(self) -> Element<'static, Desktop> {
        let symbol = match self {
            Self::Connected | Self::Error => "●",
            Self::Connecting => "◐",
            Self::Disconnected | Self::Ended => "○",
        };
        text(symbol)
            .size(13)
            .style(move |theme: &iced::Theme| {
                let palette = theme.extended_palette();
                let color = match self {
                    Self::Connected => palette.success.base.color,
                    Self::Error => palette.danger.base.color,
                    Self::Connecting => palette.primary.base.color,
                    Self::Disconnected | Self::Ended => palette.background.weak.text,
                };
                text::Style { color: Some(color) }
            })
            .into()
    }
}

struct ConnectionRow {
    id: u64,
    label: String,
    endpoint: Option<String>,
    protocol: &'static str,
    state: ConnectionState,
    panes: Vec<PaneRow>,
}

struct PaneRow {
    session: SessionId,
    number: usize,
    state: ConnectionState,
}

#[derive(Default)]
struct Tree {
    direct: Vec<ConnectionRow>,
    environments: BTreeMap<String, Vec<ConnectionRow>>,
    groups: BTreeMap<String, Vec<ConnectionRow>>,
    total: usize,
    connected: usize,
    preferred_width: f32,
}

impl Tree {
    fn branch_ids(&self) -> impl Iterator<Item = Branch> + '_ {
        std::iter::once(Branch::Window)
            .chain(self.environments.keys().cloned().map(Branch::Environment))
            .chain(self.groups.keys().cloned().map(Branch::Group))
            .chain(
                self.direct
                    .iter()
                    .chain(self.environments.values().flatten())
                    .chain(self.groups.values().flatten())
                    .filter(|item| !item.panes.is_empty())
                    .map(|item| Branch::Tab(item.id)),
            )
    }

    fn measure(&mut self, label: &str, inset: f32) {
        // A bounded estimate keeps the sidebar, drawing and native tab-drag
        // coordinates in agreement without consulting the renderer from update.
        let width = label.chars().take(60).count() as f32 * 7.5 + inset;
        self.preferred_width = self.preferred_width.max(width);
    }
}

impl Workspace {
    pub(super) fn update_dashboard(
        &mut self,
        owner: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        let Some(window) = self.windows.get_mut(&owner) else {
            return Task::none();
        };
        if let Message::Loaded(request, result) = message {
            if window.dashboard.request == Some(request) {
                window.dashboard.request = None;
                match result {
                    Ok(catalog) => window.dashboard.catalog = Some(catalog),
                    Err(error) if window.dashboard.visible => self.notice(
                        owner,
                        format!("Could not refresh dashboard connection groups ({error})"),
                    ),
                    Err(_) => {}
                }
            }
            return Task::none();
        }
        if matches!(message, Message::Toggle) {
            let visible = !window.dashboard.visible;
            return self.set_dashboard_visible(owner, visible);
        }
        if !window.dashboard.visible {
            return Task::none();
        }
        match message {
            Message::Refresh => self.refresh_dashboard(owner),
            Message::Disclosure(branch) => {
                if !window.dashboard.collapsed.remove(&branch) {
                    window.dashboard.collapsed.insert(branch);
                }
                window.dashboard.context = None;
                Task::none()
            }
            Message::ExpandAll => {
                let window = &self.windows[&owner];
                let tree = self.dashboard_tree(window);
                let collapse = tree
                    .branch_ids()
                    .any(|branch| window.dashboard.expanded(&branch));
                let state = &mut self
                    .windows
                    .get_mut(&owner)
                    .expect("dashboard owner")
                    .dashboard;
                state.collapsed = if collapse {
                    tree.branch_ids().collect()
                } else {
                    HashSet::new()
                };
                state.context = None;
                Task::none()
            }
            Message::Focus(target) => self.perform_dashboard_action(owner, target, Action::Focus),
            Message::Perform(target, action) => {
                self.perform_dashboard_action(owner, target, action)
            }
            Message::Menu(target) => {
                window.dashboard.context = (window.dashboard.context.map(|item| item.0)
                    != Some(target))
                .then_some((target, 0));
                Task::none()
            }
            Message::DismissMenu => {
                window.dashboard.context = None;
                Task::none()
            }
            Message::NavigateMenu(direction) => self.dashboard_menu_key(owner, direction),
            Message::Loaded(..) | Message::Toggle => Task::none(),
        }
    }

    pub(super) fn set_dashboard_visible(
        &mut self,
        owner: window::Id,
        visible: bool,
    ) -> Task<Desktop> {
        let Some(window) = self.windows.get_mut(&owner) else {
            return Task::none();
        };
        if window.dashboard.visible == visible {
            return Task::none();
        }
        window.dashboard.visible = visible;
        self.session_restore.changed(false);
        if visible {
            self.refresh_dashboard(owner)
        } else {
            window.dashboard.context = None;
            Task::none()
        }
    }

    fn refresh_dashboard(&mut self, owner: window::Id) -> Task<Desktop> {
        if self.windows[&owner].dashboard.request.is_some() {
            return Task::none();
        }
        let request = self.allocate();
        if let Some(window) = self.windows.get_mut(&owner) {
            window.dashboard.request = Some(request);
        }
        Task::perform(
            async {
                let (send, receive) = async_channel::bounded(1);
                std::thread::Builder::new()
                    .name("kortty-dashboard-catalog".into())
                    .spawn(move || {
                        let result = profile::connections_path()
                            .and_then(Store::open)
                            .map(|store| Arc::new(Catalog(store)))
                            .map_err(|error| error.to_string());
                        let _ = send.send_blocking(result);
                    })
                    .map_err(|error| error.to_string())?;
                receive.recv().await.map_err(|error| error.to_string())?
            },
            move |result| Desktop::Dashboard(owner, Message::Loaded(request, result)),
        )
    }

    fn dashboard_menu_key(&mut self, owner: window::Id, direction: Navigation) -> Task<Desktop> {
        let Some((target, selected)) = self.windows[&owner].dashboard.context else {
            return Task::none();
        };
        let actions = self.dashboard_actions(owner, target);
        if actions.is_empty() || matches!(direction, Navigation::Escape | Navigation::Left) {
            self.windows
                .get_mut(&owner)
                .expect("dashboard owner")
                .dashboard
                .context = None;
        } else if matches!(direction, Navigation::Activate | Navigation::Right) {
            if let Some(&action) = actions.get(selected) {
                return self.perform_dashboard_action(owner, target, action);
            }
        } else {
            let index = if matches!(direction, Navigation::Previous) {
                (selected + actions.len() - 1) % actions.len()
            } else {
                (selected + 1) % actions.len()
            };
            self.windows
                .get_mut(&owner)
                .expect("dashboard owner")
                .dashboard
                .context = Some((target, index));
        }
        Task::none()
    }

    pub(super) fn dashboard_actions(&self, owner: window::Id, target: Target) -> Vec<Action> {
        let Some((tab, _, pane)) = self.dashboard_target(owner, target) else {
            return Vec::new();
        };
        if !self.palette_can_open(owner) {
            return Vec::new();
        }
        let mut actions = vec![Action::Focus];
        let reconnect = self.can_reconnect_tab(owner, tab.id);
        let multi_exec = self.multi_exec_available(Some(owner), target.multi_exec());
        if target.session.is_some() {
            if reconnect {
                actions.push(Action::Reconnect);
            }
            actions.extend(multi_exec.then_some(Action::MultiExec));
            return actions;
        }

        let allowed = self.clipboard.policy.problem.is_none()
            && (!tab.incognito || self.clipboard.policy.incognito_allowed);
        let frame = pane.session.frame();
        let local = pane.ssh.is_none() && frame.serial.is_none() && frame.ibm.is_none();
        if allowed && (local || pane.ssh.is_some()) {
            actions.push(Action::Duplicate);
        }
        if reconnect {
            actions.push(Action::Reconnect);
        }
        if allowed {
            let minimum = self.clipboard.policy.minimum_isolation;
            if frame.status == Status::Running
                && self.clipboard.policy.file_transfer_allowed
                && pane
                    .ssh
                    .as_ref()
                    .is_some_and(|target| target.isolation >= minimum)
                && (minimum != IsolationLevel::Sandbox || pane.session.sandbox_backend().is_some())
                && pane.session.sftp_opener().is_some()
            {
                actions.push(Action::Sftp);
            }
        }
        actions.push(Action::Close);
        actions.extend(multi_exec.then_some(Action::MultiExec));
        actions
    }

    fn dashboard_target(
        &self,
        owner: window::Id,
        target: Target,
    ) -> Option<(&Tab, pane_grid::Pane, &PaneSession)> {
        let tab = self
            .windows
            .get(&owner)?
            .terminal_tabs()
            .find(|tab| tab.id == target.tab)?;
        let pane = if let Some(id) = target.session {
            tab.grid
                .iter()
                .find_map(|(pane, session)| (*session == id).then_some(*pane))?
        } else {
            tab.focused
        };
        let session = tab.grid.get(pane)?;
        Some((tab, pane, self.sessions.get(session)?))
    }

    pub(super) fn perform_dashboard_action(
        &mut self,
        owner: window::Id,
        target: Target,
        action: Action,
    ) -> Task<Desktop> {
        if let Some(window) = self.windows.get_mut(&owner) {
            window.dashboard.context = None;
        }
        if !self.dashboard_actions(owner, target).contains(&action) {
            return Task::none();
        }
        let Some((tab, pane_id, pane)) = self.dashboard_target(owner, target) else {
            return Task::none();
        };
        let profile = pane.frame.screen.profile;
        match action {
            Action::Focus => {
                let selected = self.update(Desktop::SelectTab(owner, target.tab));
                if let Some(tab) = self.active_tab_mut(owner)
                    && tab.id == target.tab
                {
                    if tab
                        .grid
                        .maximized()
                        .is_some_and(|current| current != pane_id)
                    {
                        tab.grid.restore();
                    }
                    tab.focused = pane_id;
                }
                selected
            }
            Action::Close => self.update(Desktop::CloseTab(owner, target.tab)),
            Action::MultiExec => self.update(Desktop::MultiExec(Some(owner), target.multi_exec())),
            Action::Sftp => {
                let select = self.update(Desktop::SelectTab(owner, target.tab));
                Task::batch([
                    select,
                    self.open_attached_sftp(owner).unwrap_or_else(Task::none),
                ])
            }
            Action::Reconnect => self.reconnect_tab(owner, target.tab),
            Action::Duplicate => {
                let incognito = tab.incognito;
                let font = pane.font_size_override();
                if let Some(ssh) = pane.ssh.clone() {
                    return self.duplicate_dashboard_ssh(
                        owner, target.tab, &ssh, profile, incognito, font,
                    );
                }
                let security = pane.local_security.clone();
                match self.store_local_session(profile, font, security) {
                    Ok(id) => {
                        self.add_tab_with_privacy(owner, id, incognito);
                        self.place_duplicate_tab(owner, target.tab);
                    }
                    Err(error) => {
                        self.notice(owner, format!("Could not duplicate terminal ({error})"))
                    }
                }
                Task::none()
            }
        }
    }

    fn dashboard_tree(&self, window: &Window) -> Tree {
        let mut tree = Tree::default();
        let catalog = window
            .dashboard
            .catalog
            .as_deref()
            .map(|catalog| &catalog.0);
        for tab in window.terminal_tabs() {
            let Some(session) = tab.focused_session() else {
                continue;
            };
            let Some(pane) = self.sessions.get(&session) else {
                continue;
            };
            let saved = pane.ssh.as_ref().and_then(|target| {
                catalog?
                    .get(target.saved_id()?)
                    .filter(|connection| target.matches(connection))
            });
            let environment = pane
                .credential_environment_source
                .as_deref()
                .and_then(|credential| self.vault.as_ref()?.credential_environment(credential))
                .and_then(|environment| self.environments.catalog().label(environment));
            let group = tab.group.as_deref();
            let label = self.dashboard_label(tab, pane, saved);
            let status = ConnectionState::from(&pane.frame.status);
            tree.total += 1;
            tree.connected += usize::from(status == ConnectionState::Connected);
            tree.measure(
                &label,
                if group.or(environment).is_some() {
                    145.0
                } else {
                    128.0
                },
            );
            if let Some(branch) = group.or(environment) {
                tree.measure(branch, 100.0);
            }
            let panes = if tab.grid.len() < 2 {
                Vec::new()
            } else {
                ordered_sessions(tab)
                    .enumerate()
                    .filter_map(|(index, session)| {
                        let pane = self.sessions.get(&session)?;
                        Some(PaneRow {
                            session,
                            number: index + 1,
                            state: ConnectionState::from(&pane.frame.status),
                        })
                    })
                    .collect()
            };
            let item = ConnectionRow {
                id: tab.id,
                label,
                endpoint: pane.ssh.as_ref().map(|target| target.label()),
                protocol: if pane.ssh.is_some() {
                    "ssh"
                } else if pane.frame.serial.is_some() {
                    "serial"
                } else if let Some(ibm) = &pane.frame.ibm {
                    if ibm.kind.model_5250().is_some() {
                        "tn5250"
                    } else {
                        "tn3270"
                    }
                } else {
                    "local"
                },
                state: status,
                panes,
            };
            if let Some(group) = group {
                tree.groups.entry(group.to_owned()).or_default().push(item);
            } else if let Some(environment) = environment {
                tree.environments
                    .entry(environment.to_owned())
                    .or_default()
                    .push(item);
            } else {
                tree.direct.push(item);
            }
        }
        tree
    }

    fn dashboard_label(&self, tab: &Tab, pane: &PaneSession, saved: Option<&Connection>) -> String {
        let label = if let Some(title) = &tab.custom_title {
            title.clone()
        } else if let Some(saved) = saved {
            saved.name.clone()
        } else if let Some(ssh) = &pane.ssh {
            ssh.host().into()
        } else {
            let (label, _) = self.tab_names(tab);
            if label == "Terminal" {
                "Local Shell".into()
            } else {
                label
            }
        };
        bounded_label(&label)
    }
}

fn ordered_sessions(tab: &Tab) -> impl Iterator<Item = SessionId> + '_ {
    let mut pending = vec![tab.grid.layout()];
    std::iter::from_fn(move || {
        while let Some(node) = pending.pop() {
            match node {
                pane_grid::Node::Pane(pane) => {
                    if let Some(&session) = tab.grid.get(*pane) {
                        return Some(session);
                    }
                }
                pane_grid::Node::Split { a, b, .. } => {
                    pending.push(b);
                    pending.push(a);
                }
            }
        }
        None
    })
}

fn bounded_label(label: &str) -> String {
    let mut chars = label.chars().filter(|ch| {
        !ch.is_control() && !matches!(ch, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    });
    let mut label: String = chars.by_ref().take(120).collect();
    if chars.next().is_some() {
        label.push('…');
    }
    label
}
