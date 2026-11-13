use std::cmp::Reverse;
use std::collections::HashMap;
use std::path::PathBuf;

use iced::keyboard::{Key as KeyboardKey, Modifiers, key::Named};
use iced::{Task, widget, window};

use crate::clipboard_policy::ServerAccess;
use crate::connection_store::{Connection, Store, teamwork};
use crate::snippets::{self, Library, SnippetId};
use crate::ssh_dialog::LoginTarget;

use super::chrome::Action;
use super::snippet_access::PaletteTarget;
use super::ssh_access::TeamworkSelection;
use super::tab_watch::Kind as WatchKind;
use super::{Message as Desktop, TabEntry, Window, Workspace};

mod model;
mod view;

use model::{Entry, Key, Kind, Mru, Snapshot, Source};

const MAX_QUERY_BYTES: usize = 512;
const UNAVAILABLE: &str = "Not available right now";
const TEAMWORK_BADGE: &str = "Shared (Teamwork)";
const TEAMWORK_RESELECT: &str = "This Teamwork connection changed or is no longer available. Reopen the palette to select it again.";
const TEAMWORK_DENIED: &str = "Managed policy blocks Teamwork connections.";

#[derive(Clone)]
pub struct Message {
    request: u64,
    event: Event,
}

impl std::fmt::Debug for Message {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output
            .debug_struct("PaletteMessage")
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
enum Event {
    Loaded(Libraries),
    Search(String),
    Clipboard(Option<String>),
    Move(i8),
    Select(Key),
    Choose(Option<Key>, bool),
    Traverse,
    TraverseFrom(bool),
    Dismiss,
}

#[derive(Default)]
pub(super) struct Palette {
    open: Option<Picker>,
    recent: Mru,
    sequence: u64,
}

impl Palette {
    pub(super) fn is_open(&self, owner: window::Id) -> bool {
        self.open
            .as_ref()
            .is_some_and(|picker| picker.owner == owner)
    }

    pub(super) fn dismiss(&mut self, owner: window::Id) {
        if self.is_open(owner) {
            self.open = None;
        }
    }
}

struct Picker {
    owner: window::Id,
    request: u64,
    snapshot: Snapshot,
    teamwork: Option<TeamworkSnapshot>,
    snippet_target: Option<PaletteTarget>,
    field: widget::Id,
    list: widget::Id,
    query: String,
    results: Vec<Key>,
    selected: usize,
    reason: Option<String>,
    loading: bool,
    clipboard_pending: bool,
    library_problem: Option<&'static str>,
}

impl Picker {
    fn refresh(&mut self, recent: &Mru) {
        self.results = self
            .snapshot
            .query(&self.query, recent)
            .into_iter()
            .map(|entry| entry.key.clone())
            .collect();
        self.selected = 0;
        self.reason = None;
    }

    fn send(&self, event: Event) -> Desktop {
        wrap(self.owner, self.request, event)
    }

    fn focus_field(&self) -> Task<Desktop> {
        widget::operation::focus(self.field.clone())
            .chain(widget::operation::move_cursor_to_end(self.field.clone()))
    }
}

#[derive(Clone, Default)]
struct Libraries {
    connections: Vec<ConnectionRow>,
    teamwork: Option<TeamworkSnapshot>,
    snippets: Vec<Entry>,
    problem: Option<&'static str>,
}

#[derive(Clone)]
struct ConnectionRow {
    row: Entry,
    incognito: bool,
}

#[derive(Clone)]
struct TeamworkSnapshot {
    catalog: teamwork::Catalog,
    store_path: PathBuf,
}

impl TeamworkSnapshot {
    fn selection(&self, identity: &teamwork::Identity) -> Option<TeamworkSelection> {
        self.catalog
            .get(identity)
            .map(|entry| TeamworkSelection::new(entry, self.store_path.clone()))
    }
}

impl Libraries {
    fn load(catalog: teamwork::Catalog, access: &ServerAccess) -> Self {
        let connections = crate::profile::connections_path().and_then(Store::open);
        let snippets = snippets::profile_path().and_then(Library::open);
        let problem = match (connections.is_err(), snippets.is_err()) {
            (true, true) => Some("Could not read the connections and snippets libraries."),
            (true, false) => Some("Could not read saved connections."),
            (false, true) => Some("Could not read saved snippets."),
            (false, false) => None,
        };
        let (connections, teamwork) = connections
            .as_ref()
            .map(|store| connection_rows(store, catalog, access))
            .unwrap_or_default();
        Self {
            connections,
            teamwork,
            snippets: snippets.as_ref().map_or_else(|_| Vec::new(), snippet_rows),
            problem,
        }
    }
}

impl Workspace {
    /// Iced can emit several ignored events before it lays out a newly opened overlay.
    pub(super) fn palette_key_before_layout(
        &mut self,
        owner: window::Id,
        key: &KeyboardKey,
        modifiers: Modifiers,
        input: Option<&str>,
    ) -> Option<Task<Desktop>> {
        let picker = self
            .palette
            .open
            .as_ref()
            .filter(|picker| picker.owner == owner)?;
        let request = picker.request;
        if !self.palette_can_open(owner) {
            self.palette.dismiss(owner);
            return Some(Task::none());
        }
        let shortcut = self.window_settings.key_bindings.action_for(key, modifiers);
        if matches!(
            shortcut,
            Some(Action::CommandPalette | Action::SnippetPalette)
        ) {
            self.palette.dismiss(owner);
            return Some(Task::none());
        }
        if shortcut == Some(Action::Paste) {
            let picker = self.palette.open.as_mut()?;
            if std::mem::replace(&mut picker.clipboard_pending, true) {
                return Some(Task::none());
            }
            return Some(
                self.read_clipboard()
                    .map(move |contents| wrap(owner, request, Event::Clipboard(contents))),
            );
        }
        if let Some(event) = view::keyboard(key, modifiers) {
            if matches!(event, Event::Choose(..)) {
                // There is no rendered selection to confirm before the overlay handles input.
                return Some(Task::none());
            }
            return Some(self.update_palette(owner, Message { request, event }));
        }
        if modifiers.control() || modifiers.logo() {
            return Some(Task::none());
        }
        let mut query = picker.query.clone();
        if *key == KeyboardKey::Named(Named::Backspace) {
            query.pop();
        } else if let Some(input) = input.or(match key {
            KeyboardKey::Character(text) => Some(text.as_str()),
            KeyboardKey::Named(Named::Space) => Some(" "),
            _ => None,
        }) {
            append_query(&mut query, input);
        } else {
            return Some(Task::none());
        }
        Some(self.update_palette(
            owner,
            Message {
                request,
                event: Event::Search(query),
            },
        ))
    }

    pub(super) fn palette_can_open(&self, owner: window::Id) -> bool {
        self.owner_dialog(owner).is_none()
            && !self.reopen_busy(owner)
            && self.windows.get(&owner).is_some_and(|window| {
                window.pending_serial_close.is_none()
                    && window.pending_ssh.is_none()
                    && window.pending_split_connection.is_none()
                    && window.pending_ibm.is_none()
                    && window.pending_serial.is_none()
                    && window.pending_tunnels.is_none()
                    && window.pending_paste.is_none()
                    && window.pending_snippet_run.is_none()
                    && !window.clipboard_settings
                    && self.control.settings != Some(owner)
                    && !self.project_panel_open(owner)
                    && !self.security.editing(owner)
                    && self.pending_tunnel_review(window).is_none()
                    && self.window_clipboard_prompt(window).is_none()
            })
    }

    pub(super) fn toggle_palette(
        &mut self,
        owner: window::Id,
        snippets_only: bool,
    ) -> Task<Desktop> {
        if self
            .palette
            .open
            .as_ref()
            .is_some_and(|picker| picker.owner == owner)
        {
            self.palette.dismiss(owner);
            return Task::none();
        }
        if !self.palette_can_open(owner) {
            return Task::none();
        }
        self.dismiss_terminal_context(owner);
        if let Some(window) = self.windows.get_mut(&owner) {
            window.cancel_paste();
        }
        self.palette.sequence = self.palette.sequence.wrapping_add(1);
        let request = self.palette.sequence;
        let snippet_target = self.palette_snippet_target(owner);
        let snapshot = self.palette_snapshot(owner, snippet_target.as_ref(), Libraries::default());
        let query = snapshot
            .scoped_query(snippets_only.then_some(Kind::Snippet))
            .into();
        let mut picker = Picker {
            owner,
            request,
            snapshot,
            teamwork: None,
            snippet_target,
            field: widget::Id::unique(),
            list: widget::Id::unique(),
            query,
            results: Vec::new(),
            selected: 0,
            reason: None,
            loading: true,
            clipboard_pending: false,
            library_problem: None,
        };
        picker.refresh(&self.palette.recent);
        let focus = picker.focus_field();
        self.palette.open = Some(picker);
        Task::batch([
            focus,
            load(
                owner,
                request,
                self.teamwork_catalog(),
                self.clipboard.policy.server_access().clone(),
            ),
        ])
    }

    pub(super) fn update_palette(&mut self, owner: window::Id, message: Message) -> Task<Desktop> {
        let Some(picker) = self
            .palette
            .open
            .as_ref()
            .filter(|picker| picker.owner == owner && picker.request == message.request)
        else {
            return Task::none();
        };
        match message.event {
            Event::Dismiss => {
                self.palette.dismiss(owner);
                Task::none()
            }
            Event::Choose(key, alternate) => self.choose_palette(owner, key, alternate),
            Event::Loaded(mut libraries) => {
                let problem = libraries.problem;
                let teamwork = libraries
                    .teamwork
                    .take()
                    .filter(|_| self.teamwork_sync_allowed());
                let snapshot =
                    self.palette_snapshot(owner, picker.snippet_target.as_ref(), libraries);
                let picker = self.palette.open.as_mut().expect("current palette");
                picker.snapshot = snapshot;
                picker.teamwork = teamwork;
                picker.loading = false;
                picker.library_problem = problem;
                let selected = picker.results.get(picker.selected).cloned();
                picker.refresh(&self.palette.recent);
                if let Some(index) =
                    selected.and_then(|key| picker.results.iter().position(|row| *row == key))
                {
                    picker.selected = index;
                }
                picker.reveal_selection()
            }
            Event::Traverse => {
                let request = picker.request;
                widget::operation::is_focused(picker.field.clone())
                    .map(move |focused| wrap(owner, request, Event::TraverseFrom(focused)))
            }
            Event::TraverseFrom(true) if !picker.results.is_empty() => {
                widget::operation::focus(picker.list.clone())
            }
            Event::TraverseFrom(_) => widget::operation::focus(picker.field.clone()),
            event => {
                let picker = self.palette.open.as_mut().expect("current palette");
                match event {
                    Event::Search(mut value) => {
                        picker.clipboard_pending = false;
                        value.truncate(value.floor_char_boundary(MAX_QUERY_BYTES));
                        picker.query = value;
                        picker.refresh(&self.palette.recent);
                        picker.reveal_selection()
                    }
                    Event::Clipboard(contents) if std::mem::take(&mut picker.clipboard_pending) => {
                        if let Some(contents) = contents {
                            append_query(&mut picker.query, &contents);
                            picker.refresh(&self.palette.recent);
                        }
                        Task::batch([picker.focus_field(), picker.reveal_selection()])
                    }
                    Event::Move(delta) if !picker.results.is_empty() => {
                        picker.selected = picker
                            .selected
                            .saturating_add_signed(isize::from(delta))
                            .min(picker.results.len() - 1);
                        picker.reason = None;
                        picker.reveal_selection()
                    }
                    Event::Select(key) => {
                        if let Some(index) = picker.results.iter().position(|row| *row == key) {
                            picker.selected = index;
                            picker.reason = None;
                        }
                        Task::none()
                    }
                    _ => Task::none(),
                }
            }
        }
    }

    fn choose_palette(
        &mut self,
        owner: window::Id,
        key: Option<Key>,
        alternate: bool,
    ) -> Task<Desktop> {
        let Some(picker) = self.palette.open.as_ref() else {
            return Task::none();
        };
        if picker.clipboard_pending {
            return Task::none();
        }
        // A click or Enter may arrive after a newer result list was applied.
        let Some(key) = key.filter(|key| picker.results.get(picker.selected) == Some(key)) else {
            return Task::none();
        };
        let Some(entry) = picker.snapshot.entry(&key) else {
            return Task::none();
        };
        let alternate = alternate && entry.alternate;
        let mut target = None;
        let activation = match key {
            _ if !self.palette_can_open(owner) => Err(UNAVAILABLE.into()),
            Key::ReconnectTab => self
                .windows
                .get(&owner)
                .and_then(Window::tab)
                .filter(|tab| self.can_reconnect_tab(owner, tab.id))
                .map(|tab| Activation::ReconnectTab(tab.id))
                .ok_or_else(|| UNAVAILABLE.into()),
            Key::Monitor(kind) => self
                .windows
                .get(&owner)
                .and_then(Window::tab)
                .map(|tab| Activation::Monitor(tab.id, kind))
                .ok_or_else(|| UNAVAILABLE.into()),
            _ if !alternate && !entry.enabled => Err(entry
                .disabled_reason
                .as_deref()
                .unwrap_or(UNAVAILABLE)
                .into()),
            Key::Action(action) if self.chrome_enabled(owner, action) => {
                Ok(Activation::Action(action))
            }
            Key::Action(_) => Err(UNAVAILABLE.into()),
            Key::Tab(tab) => self
                .windows
                .iter()
                .find(|(_, window)| window.tabs.iter().any(|row| row.id() == tab))
                .ok_or_else(|| "That tab has closed.".to_owned())
                .and_then(|(&window, _)| {
                    self.palette_can_open(window)
                        .then_some(Activation::Tab(window, tab))
                        .ok_or_else(|| "Finish the dialog in that window first.".into())
                }),
            Key::Connection(id)
                if let Some(profile) = crate::profile::connections_path()
                    .and_then(Store::open)
                    .ok()
                    .and_then(|store| store.local(id).cloned()) =>
            {
                match self.palette_connection_reason(false) {
                    Some(reason) => Err(reason.into()),
                    None => Ok(Activation::Local(Box::new(profile))),
                }
            }
            Key::Connection(id) => self
                .saved_connection(id)
                .ok_or_else(|| "The saved connection no longer exists.".to_owned())
                .and_then(|connection| {
                    if let Some(reason) = self.blocked_saved_server(&connection) {
                        return Err(reason);
                    }
                    match self.palette_connection_reason(connection.incognito) {
                        Some(reason) => Err(reason.into()),
                        None => Ok(Activation::Connection(Box::new(connection))),
                    }
                }),
            Key::Teamwork(_) if !self.teamwork_sync_allowed() => Err(TEAMWORK_DENIED.into()),
            Key::Teamwork(ref identity) => picker
                .teamwork
                .as_ref()
                .and_then(|snapshot| snapshot.selection(identity))
                .filter(|selection| selection.matches_catalog(self))
                .ok_or_else(|| TEAMWORK_RESELECT.into())
                .and_then(|selection| {
                    if let Some(reason) = self.blocked_saved_server(selection.connection()) {
                        Err(reason)
                    } else {
                        Ok(Activation::Teamwork(selection))
                    }
                }),
            Key::Snippet(id) => {
                target = picker.snippet_target.clone();
                Ok(Activation::Snippet(id, alternate))
            }
        };
        match activation {
            Ok(action) => {
                self.palette.recent.choose(key);
                self.palette.dismiss(owner);
                match action {
                    Activation::Action(action) => self.invoke_chrome(owner, action),
                    Activation::ReconnectTab(tab) => self.reconnect_tab(owner, tab),
                    Activation::Monitor(tab, kind) => {
                        self.toggle_tab_watch(owner, tab, kind);
                        Task::none()
                    }
                    Activation::Tab(window, tab) => {
                        let select = self.update(Desktop::SelectTab(window, tab));
                        if window == owner {
                            select
                        } else {
                            Task::batch([select, window::gain_focus(window)])
                        }
                    }
                    Activation::Connection(connection) => {
                        self.show_saved_ssh(owner, *connection, false)
                    }
                    Activation::Local(profile) => self.open_saved_local(owner, *profile),
                    Activation::Teamwork(selection) => {
                        self.show_teamwork_ssh(owner, selection, false)
                    }
                    Activation::Snippet(id, alternate) => {
                        self.invoke_palette_snippet(owner, target, id, alternate)
                    }
                }
            }
            Err(reason) => {
                if let Some(picker) = &mut self.palette.open {
                    picker.reason = Some(reason);
                }
                Task::none()
            }
        }
    }

    fn palette_snapshot(
        &self,
        owner: window::Id,
        target: Option<&PaletteTarget>,
        libraries: Libraries,
    ) -> Snapshot {
        let actions = self.windows.get(&owner).map_or_else(Vec::new, |window| {
            let mut actions: Vec<_> = Action::shortcut_catalog()
                .filter(|(action, _, _)| *action != Action::CommandPalette)
                .map(|(action, title, path)| {
                    let mut entry = Entry::new(Key::Action(action), title, path);
                    entry.enabled = self.chrome_enabled(owner, action);
                    entry.checked = self.chrome_checked(owner, window, action);
                    entry.shortcut = self.window_settings.key_bindings.menu_hint(action);
                    entry
                })
                .collect();
            let mut reconnect = Entry::new(Key::ReconnectTab, "Reconnect", "Tabs");
            reconnect.enabled = window
                .tab()
                .is_some_and(|tab| self.can_reconnect_tab(owner, tab.id));
            actions.push(reconnect);
            for kind in WatchKind::ALL {
                let mut entry = Entry::new(Key::Monitor(kind), kind.label(), "Tabs");
                entry.enabled = window.tab().is_some();
                entry.checked = window.tab().is_some_and(|tab| tab.watch.enabled(kind));
                actions.push(entry);
            }
            actions
        });
        let connections = libraries
            .connections
            .into_iter()
            .filter(|entry| {
                !matches!(entry.row.key, Key::Teamwork(_)) || self.teamwork_sync_allowed()
            })
            .map(|ConnectionRow { mut row, incognito }| {
                if let Some(reason) = self.palette_connection_reason(incognito) {
                    row.disabled_reason = Some(reason.into());
                }
                row.enabled = row.disabled_reason.is_none();
                row
            })
            .collect();
        let (destination, reason) = self.palette_snippet_presentation(owner, target);
        let snippets = libraries
            .snippets
            .into_iter()
            .map(|mut row| {
                row.detail = join_details([&destination, &row.detail]);
                row.disabled_reason.clone_from(&reason);
                row.enabled = reason.is_none();
                row
            })
            .collect();
        Snapshot::new([
            Source {
                kind: Kind::Action,
                entries: actions,
            },
            Source {
                kind: Kind::Tab,
                entries: self.palette_tabs(owner),
            },
            Source {
                kind: Kind::Connection,
                entries: connections,
            },
            Source {
                kind: Kind::Snippet,
                entries: snippets,
            },
        ])
    }

    fn palette_tabs(&self, owner: window::Id) -> Vec<Entry> {
        let mut windows: Vec<_> = self.windows.iter().collect();
        windows.sort_by_key(|&(id, window)| (*id != owner, window.number));
        windows
            .into_iter()
            .flat_map(|(&id, window)| {
                let window_label = if id == owner {
                    String::new()
                } else {
                    format!("Window {}", window.number)
                };
                let enabled = self.palette_can_open(id);
                tabs_in_palette_order(window, id == owner)
                    .into_iter()
                    .map(move |tab| {
                        let (title, connection) = match tab {
                            TabEntry::Terminal(tab) => {
                                let (name, connection) = self.tab_names(tab);
                                (format!("{name} {}", tab.id), connection)
                            }
                            TabEntry::Tool(tool) => {
                                (self.hosted_tool_title(tool).into_owned(), None)
                            }
                        };
                        let current = if id == owner && window.active_tab_id() == Some(tab.id()) {
                            "Current tab"
                        } else {
                            ""
                        };
                        let mut entry = Entry::new(
                            Key::Tab(tab.id()),
                            title,
                            join_details([
                                &window_label,
                                connection.as_deref().unwrap_or(""),
                                current,
                            ]),
                        );
                        entry.enabled = enabled;
                        entry.disabled_reason =
                            (!enabled).then(|| "Finish the dialog in that window first.".into());
                        entry
                    })
            })
            .collect()
    }

    fn palette_connection_reason(&self, incognito: bool) -> Option<&'static str> {
        if self.clipboard.policy.problem.is_some() {
            Some("Managed policy locks new sessions.")
        } else if incognito && !self.clipboard.policy.incognito_allowed {
            Some("Managed policy blocks this incognito connection.")
        } else {
            None
        }
    }
}

enum Activation {
    Action(Action),
    ReconnectTab(u64),
    Monitor(u64, WatchKind),
    Tab(window::Id, u64),
    Connection(Box<Connection>),
    Local(Box<crate::connection_store::LocalProfile>),
    Teamwork(TeamworkSelection),
    Snippet(SnippetId, bool),
}

fn tabs_in_palette_order(window: &Window, own: bool) -> Vec<&TabEntry> {
    let ranks: HashMap<_, _> = window
        .tab_mru
        .iter()
        .enumerate()
        .map(|(rank, &id)| (id, rank))
        .collect();
    let current = own.then(|| window.active_tab_id()).flatten();
    let mut tabs: Vec<_> = window
        .tabs
        .iter()
        .filter(|tab| own || tab.terminal().is_some())
        .collect();
    tabs.sort_by_key(|tab| {
        (
            Some(tab.id()) == current,
            ranks.get(&tab.id()).copied().unwrap_or(usize::MAX),
        )
    });
    tabs
}

fn connection_rows(
    store: &Store,
    catalog: teamwork::Catalog,
    access: &ServerAccess,
) -> (Vec<ConnectionRow>, Option<TeamworkSnapshot>) {
    let mut saved: Vec<_> = store.connections().iter().collect();
    saved.sort_by_cached_key(|connection| {
        (
            Reverse(store.last_used_at(connection.id)),
            connection.name.to_lowercase(),
        )
    });
    let mut rows: Vec<_> = saved
        .into_iter()
        .map(|connection| {
            let detail = join_details([
                &connection_address(&connection.login.endpoint),
                store.group_of(connection.id).unwrap_or(""),
                &connection.tag,
            ]);
            let mut row = Entry::new(Key::Connection(connection.id), &connection.name, detail);
            row.disabled_reason = connection
                .blocked_server(access)
                .map(|(host, port)| super::server_access::blocked_notice(host, port));
            ConnectionRow {
                row,
                incognito: connection.incognito,
            }
        })
        .collect();
    rows.extend(store.local_profiles().iter().map(|profile| ConnectionRow {
        row: Entry::new(
            Key::Connection(profile.id),
            profile.display_name(),
            "Local Shell",
        ),
        incognito: false,
    }));
    let mut shared: Vec<_> = catalog
        .entries()
        .filter(|entry| !store.is_teamwork_hidden(entry.identity()))
        .collect();
    if shared.is_empty() {
        return (rows, None);
    }
    shared.sort_by_cached_key(|entry| entry.connection.name.to_lowercase());
    rows.extend(shared.into_iter().map(|entry| {
        let connection = &entry.connection;
        let detail = join_details([
            &connection_address(&connection.login.endpoint),
            &entry.group,
        ]);
        let mut row = Entry::new(
            Key::Teamwork(entry.identity().clone()),
            &connection.name,
            detail,
        );
        row.search_detail = Some(join_details([TEAMWORK_BADGE, &row.detail]));
        row.disabled_reason = connection
            .blocked_server(access)
            .map(|(host, port)| super::server_access::blocked_notice(host, port));
        ConnectionRow {
            row,
            incognito: connection.incognito,
        }
    }));
    (
        rows,
        Some(TeamworkSnapshot {
            catalog,
            store_path: store.path().to_owned(),
        }),
    )
}

fn connection_address(endpoint: &LoginTarget) -> String {
    let mut address = String::new();
    if !endpoint.username.is_empty() {
        address.push_str(&endpoint.username);
        address.push('@');
    }
    if endpoint.host.contains(':') {
        address.push('[');
        address.push_str(&endpoint.host);
        address.push(']');
    } else {
        address.push_str(&endpoint.host);
    }
    if endpoint.port != 22 {
        address.push(':');
        address.push_str(&endpoint.port.to_string());
    }
    address
}

fn snippet_rows(library: &Library) -> Vec<Entry> {
    let mut rows: Vec<_> = library.entries().iter().collect();
    rows.sort_by_cached_key(|entry| entry.name().to_lowercase());
    rows.into_iter()
        .map(|snippet| {
            let metadata = snippet.metadata();
            let details = join_details([
                &metadata.folder_path,
                snippet.category(),
                snippet.language(),
            ]);
            let tags = metadata.tags.join(" ");
            let mut row = Entry::new(Key::Snippet(snippet.id()), snippet.name(), &details);
            row.search_detail = Some(join_details([&details, &tags, &metadata.description]));
            row.alternate = true;
            row
        })
        .collect()
}

fn join_details<const N: usize>(parts: [&str; N]) -> String {
    let mut joined = String::new();
    for part in parts
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if !joined.is_empty() {
            joined.push_str(" · ");
        }
        joined.push_str(part);
    }
    joined
}

fn append_query(query: &mut String, input: &str) {
    for character in input.chars().filter(|character| !character.is_control()) {
        if query.len() + character.len_utf8() > MAX_QUERY_BYTES {
            break;
        }
        query.push(character);
    }
}

fn load(
    owner: window::Id,
    request: u64,
    catalog: teamwork::Catalog,
    access: ServerAccess,
) -> Task<Desktop> {
    Task::perform(
        async move {
            let (send, receive) = async_channel::bounded(1);
            if std::thread::Builder::new()
                .name("kortty-palette-load".into())
                .spawn(move || {
                    let _ = send.send_blocking(Libraries::load(catalog, &access));
                })
                .is_ok()
                && let Ok(libraries) = receive.recv().await
            {
                return libraries;
            }
            Libraries {
                problem: Some("Could not load the palette libraries."),
                ..Libraries::default()
            }
        },
        move |libraries| wrap(owner, request, Event::Loaded(libraries)),
    )
}

fn wrap(owner: window::Id, request: u64, event: Event) -> Desktop {
    Desktop::Palette(owner, Message { request, event })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;
    use iced::keyboard::Location;

    #[test]
    fn reconnect_is_a_tab_command_without_a_shortcut_and_is_disabled_without_a_tab() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.toggle_palette(owner, false);
        let request = workspace.palette.open.as_ref().unwrap().request;
        let send = |event| Message { request, event };
        let _ = workspace.update_palette(owner, send(Event::Search(">Reconnect".into())));
        let open = workspace.palette.open.as_ref().unwrap();
        assert_eq!(open.results.first(), Some(&Key::ReconnectTab));
        let row = open.snapshot.entry(&Key::ReconnectTab).unwrap();
        assert_eq!(row.title, "Reconnect");
        assert_eq!(row.detail, "Tabs");
        assert!(row.shortcut.is_none());
        assert!(!row.enabled);

        let _ =
            workspace.update_palette(owner, send(Event::Choose(Some(Key::ReconnectTab), false)));
        let open = workspace.palette.open.as_ref().unwrap();
        assert_eq!(open.reason.as_deref(), Some(UNAVAILABLE));
    }

    #[test]
    fn palette_reconnect_revalidates_the_current_tab_when_run() {
        use crate::session::{GridSize, IbmKind, Session};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let first = workspace.store_session(
            Session::disconnected_ibm(IbmKind::Tn3270e),
            GridSize::default(),
            14,
        );
        workspace.add_tab(owner, first);
        let first_tab = workspace.windows[&owner].tab().unwrap().id;
        let second = workspace.store_session(
            Session::disconnected_ibm(IbmKind::Tn3270e),
            GridSize::default(),
            14,
        );
        workspace.add_tab(owner, second);

        let _ = workspace.toggle_palette(owner, false);
        let request = workspace.palette.open.as_ref().unwrap().request;
        let send = |event| Message { request, event };
        let _ = workspace.update_palette(owner, send(Event::Search(">Reconnect".into())));
        let _ = workspace.update(Desktop::SelectTab(owner, first_tab));
        let _ =
            workspace.update_palette(owner, send(Event::Choose(Some(Key::ReconnectTab), false)));

        let window = &workspace.windows[&owner];
        assert_eq!(window.tab().unwrap().id, first_tab);
        assert_eq!(
            window
                .pending_ibm
                .as_ref()
                .and_then(|form| form.reconnect.as_ref())
                .map(|reconnect| reconnect.target.source),
            Some(first),
        );
        assert!(!workspace.palette.is_open(owner));
    }

    #[test]
    fn tab_scope_orders_recent_tabs_then_resolves_a_moved_tab_by_identity() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        // These test-only IDs model tab ownership without opening real PTYs.
        for session in [1, 2, 3] {
            workspace.add_tab(owner, SessionId(session));
        }
        workspace.add_tab(other, SessionId(4));
        let ids: Vec<_> = workspace.windows[&owner]
            .tabs
            .iter()
            .map(TabEntry::id)
            .collect();
        let other_tab = workspace.windows[&other].tab().unwrap().id;
        let _ = workspace.update(Desktop::SelectTab(owner, ids[0]));
        let _ = workspace.toggle_palette(owner, false);
        let request = workspace.palette.open.as_ref().unwrap().request;
        let send = |event| Message { request, event };
        let _ = workspace.update_palette(owner, send(Event::Search("#".into())));
        let open = workspace.palette.open.as_ref().unwrap();
        assert_eq!(
            open.results,
            [
                Key::Tab(ids[2]),
                Key::Tab(ids[1]),
                Key::Tab(ids[0]),
                Key::Tab(other_tab)
            ]
        );

        let _ = workspace.transfer(owner, Some(other), ids[2]);
        let _ = workspace.update(Desktop::SelectTab(other, other_tab));
        let _ = workspace.update_palette(owner, send(Event::Choose(Some(Key::Tab(ids[2])), false)));
        assert_eq!(workspace.windows[&other].tab().unwrap().id, ids[2]);
        assert!(workspace.palette.open.is_none());
    }

    #[test]
    fn tab_scope_lists_tools_only_in_its_own_window() {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        // Test-only IDs avoid starting PTYs while exercising the real palette projection.
        workspace.add_tab(owner, SessionId(1));
        workspace.add_tab(other, SessionId(2));
        let other_terminal = workspace.windows[&other].active_tab_id().unwrap();
        let _ = workspace.show_openpgp(owner);
        let _ = workspace.show_openpgp(other);
        let own_tool = workspace.windows[&owner].active_tab_id().unwrap();
        let other_tool = workspace.windows[&other].active_tab_id().unwrap();
        assert!(workspace.windows[&owner].tab().is_none());
        let rows = workspace.palette_tabs(owner);
        let tool = rows
            .iter()
            .find(|row| row.key == Key::Tab(own_tool))
            .unwrap();
        assert_eq!(tool.title, super::super::openpgp_window::TITLE);
        assert_eq!(tool.detail, "Current tab");
        assert!(rows.iter().any(|row| row.key == Key::Tab(other_terminal)));
        assert!(rows.iter().all(|row| row.key != Key::Tab(other_tool)));
    }

    #[test]
    fn stale_loading_cannot_replace_a_reopened_picker_and_disabled_rows_leave_it_open() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.toggle_palette(owner, false);
        let previous = workspace.palette.open.as_ref().unwrap().request;
        let _ = workspace.toggle_palette(owner, false);
        let _ = workspace.toggle_palette(owner, true);
        let open = workspace.palette.open.as_ref().unwrap();
        let request = open.request;
        assert_eq!(open.query, "$");
        let _ = workspace.update_palette(
            owner,
            Message {
                request: previous,
                event: Event::Loaded(Libraries::default()),
            },
        );
        assert!(workspace.palette.open.as_ref().unwrap().loading);

        let send = |event| Message { request, event };
        let _ = workspace.update_palette(owner, send(Event::Search(">Zoom Pane".into())));
        let _ = workspace.update_palette(
            owner,
            send(Event::Choose(Some(Key::Action(Action::ZoomPane)), false)),
        );
        assert_eq!(
            workspace.palette.open.as_ref().unwrap().reason.as_deref(),
            Some(UNAVAILABLE)
        );
    }

    #[test]
    fn a_key_batch_before_overlay_layout_routes_paste_and_enter_away_from_the_shell() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        workspace.add_tab(owner, SessionId(1));
        let paste_modifiers = if cfg!(target_os = "macos") {
            Modifiers::COMMAND
        } else {
            Modifiers::COMMAND | Modifiers::SHIFT
        };
        let _ = workspace.key(
            owner,
            KeyboardKey::Character("p".into()),
            Location::Standard,
            Modifiers::COMMAND | Modifiers::SHIFT,
            None,
        );
        let _ = workspace.key(
            owner,
            KeyboardKey::Character("v".into()),
            Location::Standard,
            paste_modifiers,
            None,
        );
        let _ = workspace.key(
            owner,
            KeyboardKey::Named(Named::Enter),
            Location::Standard,
            Modifiers::empty(),
            None,
        );
        let picker = workspace.palette.open.as_ref().unwrap();
        assert!(picker.clipboard_pending);
        assert!(workspace.windows[&owner].paste_request.is_none());

        let request = picker.request;
        let _ = workspace.update_palette(
            owner,
            Message {
                request,
                event: Event::Clipboard(Some(">global settings".into())),
            },
        );
        let picker = workspace.palette.open.as_ref().unwrap();
        assert!(!picker.clipboard_pending);
        assert_eq!(picker.query, ">global settings");
        assert_eq!(picker.results, [Key::Action(Action::Settings)]);
    }

    #[test]
    fn asynchronous_results_preserve_the_visible_choice_and_reject_an_old_enter() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.toggle_palette(owner, false);
        let request = workspace.palette.open.as_ref().unwrap().request;
        let send = |event| Message { request, event };
        let _ = workspace.update_palette(owner, send(Event::Search("security".into())));
        let old = workspace.palette.open.as_ref().unwrap().results[0].clone();
        assert!(matches!(old, Key::Action(_)));
        let snippet = Key::Snippet(SnippetId::try_from("00".repeat(16)).unwrap());
        let _ = workspace.update_palette(
            owner,
            send(Event::Loaded(Libraries {
                snippets: vec![Entry::new(snippet.clone(), "Security", "")],
                ..Libraries::default()
            })),
        );
        let picker = workspace.palette.open.as_ref().unwrap();
        assert_eq!(picker.results[picker.selected], old);
        assert!(picker.results.contains(&snippet));

        let _ = workspace.update_palette(owner, send(Event::Select(snippet.clone())));
        let _ = workspace.update_palette(owner, send(Event::Choose(Some(old), false)));
        let picker = workspace.palette.open.as_ref().unwrap();
        assert_eq!(picker.results[picker.selected], snippet);
        assert!(workspace.windows[&owner].pending_snippet_run.is_none());
    }

    #[test]
    fn connection_palette_orders_saved_rows_and_omits_only_the_hidden_teamwork_origin() {
        let profile = snippets::tests::Profile::new();
        let alpha = profile.0.join("alpha.xml");
        let beta = profile.0.join("beta.xml");
        write_shared(&alpha, "Alpha shared", "alpha.example");
        write_shared(&beta, "Beta shared", "beta.example");
        let catalog = shared_catalog(&[&alpha, &beta]);
        let (alpha_entry, beta_key) = {
            let mut entries = catalog.entries();
            let alpha_entry = entries.next().unwrap();
            let beta_key = Key::Teamwork(entries.next().unwrap().identity().clone());
            (alpha_entry, beta_key)
        };
        let mut store = Store::open(profile.0.join("connections.json")).unwrap();
        let mut local = alpha_entry.connection.clone();
        local.id = 0;
        local.name = "Alpha local".into();
        let alpha_local = store.save(local.clone()).unwrap();
        local.name = "Zeta local".into();
        let zeta_local = store.save(local).unwrap();
        store.record_used_at(zeta_local, 10).unwrap();
        store
            .hide_teamwork(&[teamwork::HiddenEntry::from(alpha_entry)])
            .unwrap();

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        workspace.set_teamwork_catalog_for_test(catalog);
        let request = populate_connection_palette(&mut workspace, owner, &store);
        let picker = workspace.palette.open.as_ref().unwrap();
        assert_eq!(
            picker.results,
            [
                Key::Connection(zeta_local),
                Key::Connection(alpha_local),
                beta_key.clone()
            ]
        );
        let beta_row = picker.snapshot.entry(&beta_key).unwrap();
        assert_eq!(beta_row.detail, "ops@beta.example:2222 · Shared labs");
        let send = |event| Message { request, event };
        let _ = workspace.update_palette(owner, send(Event::Search("@teamwork".into())));
        assert_eq!(
            workspace.palette.open.as_ref().unwrap().results.as_slice(),
            std::slice::from_ref(&beta_key)
        );
        let _ = workspace.update_palette(owner, send(Event::Choose(Some(beta_key), false)));
        assert!(workspace.windows[&owner].pending_ssh.is_some());
        assert!(!workspace.palette.is_open(owner));
    }

    #[test]
    fn connection_palette_revalidates_teamwork_route_local_visibility_and_policy() {
        let profile = snippets::tests::Profile::new();
        let source = profile.0.join("teamwork.xml");
        write_shared(&source, "Shared target", "original.example");
        let original = shared_catalog(&[&source]);
        let entry = original.entries().next().unwrap();
        let hidden = teamwork::HiddenEntry::from(entry);
        let key = Key::Teamwork(entry.identity().clone());
        let mut store = Store::open(profile.0.join("connections.json")).unwrap();
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        workspace.set_teamwork_catalog_for_test(original.clone());
        let request = populate_connection_palette(&mut workspace, owner, &store);

        let choose = || Message {
            request,
            event: Event::Choose(Some(key.clone()), false),
        };
        write_shared(&source, "Shared target", "changed.example");
        workspace.set_teamwork_catalog_for_test(shared_catalog(&[&source]));
        let _ = workspace.update_palette(owner, choose());
        assert!(workspace.windows[&owner].pending_ssh.is_none());
        assert_eq!(
            workspace.palette.open.as_ref().unwrap().reason.as_deref(),
            Some(TEAMWORK_RESELECT)
        );

        workspace.set_teamwork_catalog_for_test(original);
        store.hide_teamwork(std::slice::from_ref(&hidden)).unwrap();
        let _ = workspace.update_palette(owner, choose());
        assert!(workspace.windows[&owner].pending_ssh.is_none());
        assert_eq!(
            workspace.palette.open.as_ref().unwrap().reason.as_deref(),
            Some(TEAMWORK_RESELECT)
        );

        store.restore_teamwork(hidden.identity()).unwrap();
        workspace.clipboard.policy.teamwork_allowed = false;
        let _ = workspace.update_palette(owner, choose());
        assert!(workspace.windows[&owner].pending_ssh.is_none());
        assert_eq!(
            workspace.palette.open.as_ref().unwrap().reason.as_deref(),
            Some(TEAMWORK_DENIED)
        );
        workspace.palette.dismiss(owner);
        populate_connection_palette(&mut workspace, owner, &store);
        assert!(workspace.palette.open.as_ref().unwrap().results.is_empty());
    }

    fn write_shared(path: &std::path::Path, name: &str, host: &str) {
        std::fs::write(
            path,
            format!("<connections><connection id=\"same-id\"><name>{name}</name><host>{host}</host><port>2222</port><username>ops</username><group>Shared labs</group></connection></connections>"),
        )
        .unwrap();
    }

    fn shared_catalog(sources: &[&std::path::Path]) -> teamwork::Catalog {
        let settings = teamwork::Settings {
            sources: sources
                .iter()
                .map(|path| teamwork::Source {
                    kind: teamwork::SourceKind::File,
                    enabled: true,
                    location: path.to_str().unwrap().into(),
                    check_interval_minutes: teamwork::DEFAULT_CHECK_INTERVAL_MINUTES,
                })
                .collect(),
            ..Default::default()
        };
        teamwork::Catalog::default().refreshed(&settings.active_sources(true, &[]).unwrap())
    }

    fn populate_connection_palette(
        workspace: &mut Workspace,
        owner: window::Id,
        store: &Store,
    ) -> u64 {
        let _ = workspace.toggle_palette(owner, false);
        let request = workspace.palette.open.as_ref().unwrap().request;
        let (connections, teamwork) = connection_rows(
            store,
            workspace.teamwork_catalog(),
            workspace.clipboard.policy.server_access(),
        );
        let _ = workspace.update_palette(
            owner,
            Message {
                request,
                event: Event::Loaded(Libraries {
                    connections,
                    teamwork,
                    ..Libraries::default()
                }),
            },
        );
        let _ = workspace.update_palette(
            owner,
            Message {
                request,
                event: Event::Search("@".into()),
            },
        );
        request
    }
}
