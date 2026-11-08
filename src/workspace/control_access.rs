use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use async_channel::Receiver;
use iced::futures::{StreamExt, stream::BoxStream};
use iced::widget::{button, column, pane_grid, row, text};
use iced::{Element, Subscription, Task, window};
use kortty_control::{
    ErrorCode, Fault, Method, Params, ReadMode, Reply, Request, Server, profile_root,
};
use serde_json::{Value, json};

use crate::session::{Frame, SessionId, Status};

use super::{Message as Desktop, PaneSession, Tab, Window, Workspace};

mod input;

#[derive(Clone, Debug)]
pub enum Message {
    Settings(window::Id, bool),
    Enable(window::Id),
    Disable(window::Id),
    Stopped,
    Request(Request),
}

#[derive(Default)]
pub(super) struct Control {
    pub(super) settings: Option<window::Id>,
    server: Option<Server>,
    generation: u64,
    stopping: bool,
    first_write: bool,
    pending_splits: HashMap<SessionId, PendingSplit>,
}

impl Control {
    pub(super) fn subscription(&self) -> Subscription<Desktop> {
        self.server
            .as_ref()
            .map_or_else(Subscription::none, |server| {
                Subscription::run_with(Watch(self.generation, server.requests()), watch)
            })
    }

    pub(super) fn detach(&mut self) -> Option<Server> {
        let server = self.server.take()?;
        server.revoke();
        Some(server)
    }

    pub(super) fn close_settings(&mut self, id: window::Id) {
        if self.settings == Some(id) {
            self.settings = None;
        }
    }

    pub(super) fn settings_view(&self, id: window::Id) -> Element<'_, Desktop> {
        let status = if let Some(server) = &self.server {
            format!("Listening locally at {}", server.address())
        } else if self.stopping {
            "Stopping local control…".into()
        } else {
            "Local control is off.".into()
        };
        column![
            text("Local Control API").size(19),
            text(status),
            text("While enabled, local processes running as your OS user can read and type into open terminals."),
            row![
                button("Done").on_press(Desktop::Control(Message::Settings(id, false))),
                button("Disable").on_press_maybe(self.server.is_some().then_some(
                    Desktop::Control(Message::Disable(id)),
                )),
            ]
            .spacing(8),
        ]
        .padding(16)
        .spacing(12)
        .into()
    }
}

#[derive(Clone)]
struct Watch(u64, Receiver<Request>);

impl Hash for Watch {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

fn watch(watch: &Watch) -> BoxStream<'static, Desktop> {
    watch
        .1
        .clone()
        .map(|request| Desktop::Control(Message::Request(request)))
        .boxed()
}

#[derive(Clone, Copy)]
struct Address {
    window: window::Id,
    tab: usize,
    pane: pane_grid::Pane,
    session: SessionId,
}

struct PendingSplit {
    source: SessionId,
    axis: pane_grid::Axis,
    focus: bool,
    instance: Arc<str>,
    reply: Reply,
}

impl Workspace {
    pub(super) fn update_control(&mut self, message: Message) -> Task<Desktop> {
        match message {
            Message::Settings(id, true) if self.windows.contains_key(&id) => {
                self.control.settings = Some(id);
            }
            Message::Settings(id, false) => self.control.close_settings(id),
            Message::Enable(id)
                if self.control.settings == Some(id)
                    && self.clipboard.policy.control_allowed
                    && self.control.server.is_none()
                    && !self.control.stopping =>
            {
                match profile_root().and_then(|root| Server::start(&root)) {
                    Ok(server) => {
                        self.control.server = Some(server);
                        self.control.generation = self.control.generation.wrapping_add(1);
                        self.control.first_write = true;
                        self.control.close_settings(id);
                    }
                    Err(error) => {
                        self.control.close_settings(id);
                        self.notice(
                            id,
                            format!("Could not enable the local Control API ({error})"),
                        );
                    }
                }
            }
            Message::Disable(id) if self.control.settings == Some(id) => {
                if let Some(server) = self.control.detach() {
                    for (session, _) in std::mem::take(&mut self.control.pending_splits) {
                        self.stop_session(session);
                    }
                    self.control.stopping = true;
                    return Task::future(async move {
                        server.stop();
                        Desktop::Control(Message::Stopped)
                    });
                }
            }
            Message::Stopped => self.control.stopping = false,
            Message::Request(request) => return self.control_request(&request),
            _ => {}
        }
        Task::none()
    }

    pub(super) fn view_control_settings(&self, id: window::Id) -> Element<'_, Desktop> {
        let mut body = column![self.control.settings_view(id)].padding(8);
        if !self.clipboard.policy.control_allowed {
            body = body.push(text("Managed policy blocks the Control API."));
        } else if self.control.server.is_none() && !self.control.stopping {
            body = body.push(
                button("Enable for this run").on_press(Desktop::Control(Message::Enable(id))),
            );
        }
        body.into()
    }

    fn control_request(&mut self, request: &Request) -> Task<Desktop> {
        if !request.reply.active() {
            return Task::none();
        }
        if !self.clipboard.policy.control_allowed {
            request.reply.send(Err(Fault::new(
                ErrorCode::Policy,
                "Managed policy blocks local control",
            )));
            return Task::none();
        }
        let result = match request.method {
            Method::Read | Method::Split => {
                let pending = if request.method == Method::Read {
                    self.control_read(request)
                } else {
                    self.control_start_split(request)
                };
                if let Err(error) = pending {
                    request.reply.send(Err(error));
                }
                return Task::none();
            }
            _ => self.control_dispatch(request),
        };
        match result {
            Ok((value, task)) => {
                request.reply.send(Ok(value));
                task
            }
            Err(error) => {
                request.reply.send(Err(error));
                Task::none()
            }
        }
    }

    fn control_read(&self, request: &Request) -> Result<(), Fault> {
        let address = self.control_resolve(request.params.required("pane")?, &request.instance)?;
        let mode = ReadMode::parse(&request.params, ReadMode::Visible)?;
        let lines = ReadMode::lines(&request.params)?;
        self.sessions[&address.session].session.control_read(
            pane_id(address.session),
            mode,
            lines,
            request.reply.clone(),
        )
    }

    fn control_dispatch(&mut self, request: &Request) -> Result<(Value, Task<Desktop>), Fault> {
        let params = &request.params;
        let instance = &request.instance;
        let mut task = Task::none();
        let result = match request.method {
            Method::Windows => {
                let windows: Vec<_> = self
                    .control_windows(None)?
                    .into_iter()
                    .enumerate()
                    .map(|(index, (id, window))| {
                        json!({"window_id": window_id(window), "index": index, "title": self.title(id),
                            "focused": window.focused, "tab_count": window.terminal_tabs().count()})
                    })
                    .collect();
                json!({"instance": instance.as_ref(), "windows": windows})
            }
            Method::Tabs => {
                let workspace = &*self;
                let tabs: Vec<_> = self
                    .control_windows(params.string("window")?)?
                    .into_iter()
                    .flat_map(|(_, window)| {
                        indexed_terminal_tabs(window).map(move |(index, tab)| {
                            workspace.control_tab_info(window, tab, index, instance)
                        })
                    })
                    .collect();
                json!({"instance": instance.as_ref(), "tabs": tabs})
            }
            Method::Panes => self.control_pane_list(params, instance)?,
            Method::Current => json!({"pane": self.control_current().map(|address| {
                self.control_pane_info(address, instance)
            })}),
            Method::Resolve => self.control_pane_for_pids(&params.resolve_pids()?, instance)?,
            Method::Pane
            | Method::FocusPane
            | Method::FocusTab
            | Method::Close
            | Method::SendText
            | Method::Run
            | Method::SendKeys => {
                let parameter = if request.method == Method::FocusTab {
                    "tab"
                } else {
                    "pane"
                };
                let address = self.control_resolve(params.required(parameter)?, instance)?;
                if request.method.mutates() {
                    self.control_allow_mutation(address.window)?;
                }
                match request.method {
                    Method::Pane => json!({"pane": self.control_pane_info(address, instance)}),
                    Method::FocusPane | Method::FocusTab => {
                        let pane = request.method == Method::FocusPane;
                        if !pane || params.flag("raise", true)? {
                            task = window::gain_focus(address.window);
                        }
                        self.control_focus(address, pane);
                        if pane {
                            json!({"ok": true, "pane": self.control_pane_info(address, instance)})
                        } else {
                            let window = &self.windows[&address.window];
                            json!({"ok": true, "tab": self.control_tab_info(
                                window, window.tabs[address.tab].terminal().expect("resolved terminal tab"), address.tab, instance)})
                        }
                    }
                    Method::Close => self.control_close(address)?,
                    _ => {
                        let written = input::write(
                            self.sessions
                                .get_mut(&address.session)
                                .expect("resolved pane"),
                            address.session,
                            request.method,
                            params,
                        )?;
                        if self.control.first_write {
                            self.control.first_write = false;
                            self.notice(
                                address.window,
                                "A local Control API client sent terminal input.".into(),
                            );
                        }
                        written
                    }
                }
            }
            _ => {
                return Err(Fault::new(
                    ErrorCode::UnknownMethod,
                    "Method cannot be dispatched to the desktop",
                ));
            }
        };
        Ok((result, task))
    }

    fn control_windows(
        &self,
        requested: Option<&str>,
    ) -> Result<Vec<(window::Id, &Window)>, Fault> {
        let mut windows: Vec<_> = self
            .windows
            .iter()
            .filter_map(|(id, window)| {
                (requested.is_none_or(|wanted| wanted == window_id(window)))
                    .then_some((*id, window))
            })
            .collect();
        if requested.is_some() && windows.is_empty() {
            return Err(Fault::new(ErrorCode::WindowNotFound, "Window is not open"));
        }
        windows.sort_by_key(|(_, window)| window.number);
        Ok(windows)
    }

    fn control_current(&self) -> Option<Address> {
        let (id, window) = self.windows.iter().find(|(_, window)| window.focused)?;
        let tab = window.tab()?;
        Some(Address {
            window: *id,
            tab: window.active,
            pane: tab.focused,
            session: tab.focused_session()?,
        })
    }

    fn control_pane_for_pids(&self, pids: &[u32], instance: &str) -> Result<Value, Fault> {
        let mut nearest = None;
        for (id, window) in self.control_windows(None)? {
            for (index, tab) in indexed_terminal_tabs(window) {
                for (pane, session) in tab.grid.iter() {
                    let Some(pid) = self
                        .sessions
                        .get(session)
                        .and_then(|pane| pane.session.local_shell_pid())
                    else {
                        continue;
                    };
                    let Some(rank) = pids.iter().position(|&candidate| candidate == pid) else {
                        continue;
                    };
                    if nearest.as_ref().is_none_or(|(best, _, _)| rank < *best) {
                        nearest = Some((
                            rank,
                            Address {
                                window: id,
                                tab: index,
                                pane: *pane,
                                session: *session,
                            },
                            pid,
                        ));
                    }
                }
            }
        }
        if let Some((_, address, pid)) = nearest {
            let pane = self.control_pane_info(address, instance);
            if pane.get("shell_pid").and_then(Value::as_u64) == Some(u64::from(pid)) {
                return Ok(json!({"pane": pane, "matched_pid": pid}));
            }
        }
        Ok(json!({"pane": null, "matched_pid": null}))
    }

    fn control_resolve(&self, selector: &str, instance: &str) -> Result<Address, Fault> {
        let missing = || Fault::new(ErrorCode::PaneNotFound, "Pane is not open");
        if selector == "@focused" {
            return self.control_current().ok_or_else(missing);
        }
        let fields: Vec<_> = selector.splitn(4, ':').collect();
        let (requested_window, requested_tab, requested_pane) = match fields.as_slice() {
            [window, tab, pane] => (Some(*window), Some(*tab), Some(*pane)),
            [single] if single.starts_with('w') => {
                return Err(Fault::new(ErrorCode::Ambiguous, "Specify a tab or pane"));
            }
            [single] if single.starts_with('t') => (None, Some(*single), None),
            [single] if single.starts_with('p') => (None, None, Some(*single)),
            _ => return Err(Fault::param("pane")),
        };
        for (id, window) in self.control_windows(requested_window)? {
            for (index, tab) in indexed_terminal_tabs(window) {
                if requested_tab.is_some_and(|requested| requested != tab_id(instance, tab.id)) {
                    continue;
                }
                if let Some((pane, session)) = tab.grid.iter().find(|(pane, session)| {
                    requested_pane.map_or(tab.focused == **pane, |requested| {
                        requested == pane_id(**session)
                    })
                }) {
                    return Ok(Address {
                        window: id,
                        tab: index,
                        pane: *pane,
                        session: *session,
                    });
                }
            }
        }
        if requested_pane.is_none() {
            return Err(Fault::new(ErrorCode::TabNotFound, "Tab is not open"));
        }
        Err(missing())
    }

    fn control_pane_list(&self, params: &Params, instance: &str) -> Result<Value, Fault> {
        let mut panes = Vec::new();
        let wanted = params.string("tab")?;
        let mut found = wanted.is_none();
        let local_only = params.flag("local_shell_only", false)?;
        for (id, window) in self.control_windows(params.string("window")?)? {
            for (index, tab) in indexed_terminal_tabs(window) {
                if wanted.is_some_and(|wanted| wanted != tab_id(instance, tab.id)) {
                    continue;
                }
                found = true;
                for (pane, session) in ordered_panes(tab) {
                    let address = Address {
                        window: id,
                        tab: index,
                        pane,
                        session,
                    };
                    let current = &self.sessions[&session];
                    if !local_only || protocol(current, &current.frame) == "LOCAL_SHELL" {
                        panes.push(self.control_pane_info(address, instance));
                    }
                }
            }
        }
        if !found {
            return Err(Fault::new(ErrorCode::TabNotFound, "Tab is not open"));
        }
        Ok(json!({"instance": instance, "panes": panes}))
    }

    fn control_tab_info(&self, window: &Window, tab: &Tab, index: usize, instance: &str) -> Value {
        let pane = tab
            .focused_session()
            .and_then(|session| self.sessions.get(&session));
        json!({"window_id": window_id(window), "tab_id": tab_id(instance, tab.id),
            "title": self.tab_names(tab).0,
            "protocol": pane.map(|pane| protocol(pane, &pane.frame)).unwrap_or("UNKNOWN"),
            "host": pane.and_then(|pane| pane.ssh.as_ref().map(super::SshTarget::host)),
            "connected": pane.is_some_and(|pane| pane.session.frame().status == Status::Running),
            "active": window.active == index, "pane_count": tab.grid.len(),
            "agents": {"blocked": 0, "working": 0, "done": 0, "idle": 0, "total": 0, "most_urgent": null}})
    }

    fn control_pane_info(&self, address: Address, instance: &str) -> Value {
        let window = &self.windows[&address.window];
        let tab = window.tabs[address.tab]
            .terminal()
            .expect("resolved terminal tab");
        let pane = &self.sessions[&address.session];
        let frame = pane.session.frame();
        let protocol = protocol(pane, &frame);
        let window_id = window_id(window);
        let tab_id = tab_id(instance, tab.id);
        let pane_id = pane_id(address.session);
        let shell_pid = pane.session.local_shell_pid().map_or(-1, i64::from);
        let index = ordered_panes(tab)
            .iter()
            .position(|(pane, _)| *pane == address.pane);
        json!({"window_id": window_id, "tab_id": tab_id, "pane_id": pane_id,
            "index": index, "focused": address.pane == tab.focused,
            "protocol": protocol, "connected": frame.status == Status::Running,
            "local_shell": protocol == "LOCAL_SHELL", "working_directory": null, "shell_pid": shell_pid,
            "columns": frame.screen.columns, "rows": frame.screen.rows,
            "alternate_screen": frame.screen.alternate_screen, "bracketed_paste": frame.bracketed_paste,
            "isolation": pane.isolation_name(&frame.status), "incognito": tab.incognito,
            "agent": {"pane_id": pane_id, "tab_id": tab_id, "window_id": window_id,
                "detected": false, "state": "unknown", "kind": null, "display_name": null,
                "alias": null, "matched_rule_id": null, "evidence": null, "state_since_millis": 0,
                "seconds_in_state": 0, "done_until_seen": false, "pid": -1, "command": null}})
    }

    fn control_allow_mutation(&self, id: window::Id) -> Result<(), Fault> {
        let window = &self.windows[&id];
        if self.control.settings == Some(id)
            || self.journal_options_modal(id).is_some()
            || self.journal_edit_for_owner(id).is_some()
            || window.clipboard_settings
            || self.project_panel_open(id)
            || self.security.editing(id)
            || window.pending_paste.is_some()
            || window.pending_ibm.is_some()
            || window.pending_serial.is_some()
            || window.pending_ssh.is_some()
            || window.pending_split_connection.is_some()
            || window.pending_tunnels.is_some()
            || self.pending_tunnel_review(window).is_some()
            || window.pending_serial_close.is_some()
            || self.window_clipboard_prompt(window).is_some()
        {
            return Err(Fault::new(
                ErrorCode::Busy,
                "The window is waiting for user input",
            ));
        }
        Ok(())
    }

    fn control_focus(&mut self, address: Address, pane: bool) {
        self.dismiss_journal_context_for_window(address.window);
        let window = self
            .windows
            .get_mut(&address.window)
            .expect("resolved window");
        window.cancel_paste();
        window.active = address.tab;
        window.remember_active_tab();
        let tab = window.tab_mut().expect("resolved terminal tab");
        if pane {
            if tab
                .grid
                .maximized()
                .is_some_and(|active| active != address.pane)
            {
                tab.grid.restore();
            }
            tab.focused = address.pane;
        }
        self.session_restore.changed(false);
    }

    fn control_start_split(&mut self, request: &Request) -> Result<(), Fault> {
        let params = &request.params;
        let source = self.control_resolve(params.required("pane")?, &request.instance)?;
        self.control_allow_mutation(source.window)?;
        let pane = &self.sessions[&source.session];
        let frame = pane.session.frame();
        if frame.status != Status::Running {
            return Err(Fault::new(
                ErrorCode::NotConnected,
                "The source pane is disconnected",
            ));
        }
        if protocol(pane, &frame) != "LOCAL_SHELL" {
            return Err(Fault::new(
                ErrorCode::Unsupported,
                "Only local shells can be split through the Control API",
            ));
        }
        let axis = match params
            .string("orientation")?
            .unwrap_or("horizontal")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "horizontal" => pane_grid::Axis::Horizontal,
            "vertical" => pane_grid::Axis::Vertical,
            _ => return Err(Fault::param("orientation")),
        };
        let focus = params.flag("focus", true)?;
        if self.control.pending_splits.len() >= 8 {
            return Err(Fault::new(
                ErrorCode::Busy,
                "Too many pending terminal splits",
            ));
        }
        let security = pane.local_security.clone();
        let session = self
            .start_session_with_policy(source.window, frame.screen.profile, security)
            .ok_or_else(split_failed)?;
        self.control.pending_splits.insert(
            session,
            PendingSplit {
                source: source.session,
                axis,
                focus,
                instance: Arc::clone(&request.instance),
                reply: request.reply.clone(),
            },
        );
        Ok(())
    }

    pub(super) fn control_finish_split(&mut self, session: SessionId) {
        let Some(pending) = self.control.pending_splits.get(&session) else {
            return;
        };
        let status = &self.sessions[&session].frame.status;
        if pending.reply.active() && *status == Status::Starting {
            return;
        }
        let running = *status == Status::Running;
        let pending = self
            .control
            .pending_splits
            .remove(&session)
            .expect("pending split");
        let result = if running && pending.reply.active() {
            self.control_attach_split(session, &pending)
        } else {
            Err(split_failed())
        };
        if result.is_err() {
            self.stop_session(session);
        }
        pending.reply.send(result);
    }

    fn control_attach_split(
        &mut self,
        session: SessionId,
        pending: &PendingSplit,
    ) -> Result<Value, Fault> {
        let source = self.control_resolve(&pane_id(pending.source), &pending.instance)?;
        self.control_allow_mutation(source.window)?;
        let tab = self
            .windows
            .get_mut(&source.window)
            .expect("resolved window")
            .tabs[source.tab]
            .terminal_mut()
            .expect("resolved terminal tab");
        let (pane, _) = tab
            .grid
            .split(pending.axis, source.pane, session)
            .ok_or_else(split_failed)?;
        tab.grid.restore();
        if pending.focus {
            tab.focused = pane;
        }
        self.inherit_terminal_effect(source.session, session);
        self.session_restore.changed(false);
        Ok(
            json!({"pane": self.control_pane_info(Address {pane, session, ..source}, &pending.instance)}),
        )
    }

    fn control_close(&mut self, address: Address) -> Result<Value, Fault> {
        let frame = self.sessions[&address.session].session.frame();
        if frame.serial.is_some() && matches!(frame.status, Status::Starting | Status::Running) {
            return Err(Fault::new(
                ErrorCode::Unsupported,
                "Closing an open serial port requires desktop confirmation",
            ));
        }
        let tab = self
            .windows
            .get_mut(&address.window)
            .expect("resolved window")
            .tabs[address.tab]
            .terminal_mut()
            .expect("resolved terminal tab");
        if tab.grid.len() == 1 {
            return Err(Fault::new(
                ErrorCode::LastPane,
                "The tab needs at least one pane",
            ));
        }
        let session = tab
            .close_pane(address.pane)
            .ok_or_else(|| Fault::new(ErrorCode::PaneNotFound, "Pane is not open"))?;
        self.stop_session(session);
        self.session_restore.changed(false);
        Ok(json!({"ok": true}))
    }
}

fn window_id(window: &Window) -> String {
    format!("w{}", window.number)
}

fn tab_id(instance: &str, id: u64) -> String {
    let prefix = instance.get(..8).unwrap_or("00000000");
    format!(
        "t{prefix}-{:04x}-4000-8000-{:012x}",
        id >> 48,
        id & 0xffff_ffff_ffff
    )
}

fn pane_id(session: SessionId) -> String {
    format!("p{:08x}", session.0)
}

fn split_failed() -> Fault {
    Fault::new(ErrorCode::SplitFailed, "Could not split the source pane")
}

fn protocol(pane: &PaneSession, frame: &Frame) -> &'static str {
    if pane.ssh.is_some() {
        "SSH_TCP"
    } else if let Some(ibm) = &frame.ibm {
        ibm.kind.label()
    } else if frame.serial.is_some() {
        "SERIAL"
    } else {
        "LOCAL_SHELL"
    }
}

fn indexed_terminal_tabs(window: &Window) -> impl Iterator<Item = (usize, &Tab)> {
    window
        .tabs
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| entry.terminal().map(|tab| (index, tab)))
}

pub(super) fn ordered_panes(tab: &Tab) -> Vec<(pane_grid::Pane, SessionId)> {
    let mut panes = Vec::with_capacity(tab.grid.len());
    let mut nodes = vec![tab.grid.layout()];
    while let Some(node) = nodes.pop() {
        match node {
            pane_grid::Node::Pane(pane) => {
                panes.push((*pane, *tab.grid.get(*pane).expect("pane in layout")));
            }
            pane_grid::Node::Split { a, b, .. } => {
                nodes.push(b);
                nodes.push(a);
            }
        }
    }
    panes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{IbmKind, Session};
    use kortty_terminal::TerminalProfile;
    use std::thread;
    use std::time::{Duration, Instant};

    fn wait_for_shell(session: &Session) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(pid) = session.local_shell_pid() {
                return pid;
            }
            assert!(Instant::now() < deadline, "local shell did not start");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn control_projects_only_terminal_tabs_and_keeps_live_tab_indices() {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let (owner, _) = workspace.open_window(false);
        // Test-only IDs model the pane registry without starting a PTY.
        workspace.add_tab(owner, SessionId(1));
        let _ = workspace.show_openpgp(owner);
        let tool = workspace.windows[&owner].active_tab_id().unwrap();
        workspace.add_tab(owner, SessionId(2));
        let state = workspace.windows.get_mut(&owner).unwrap();
        state.focused = true;
        assert_eq!(
            indexed_terminal_tabs(state)
                .map(|(index, _)| index)
                .collect::<Vec<_>>(),
            [0, 2]
        );
        let current = workspace.control_current().unwrap();
        assert_eq!((current.tab, current.session), (2, SessionId(2)));

        let _ = workspace.update(Desktop::SelectTab(owner, tool));
        assert!(workspace.control_current().is_none());
        assert!(matches!(
            workspace.control_resolve("@focused", "12345678"),
            Err(error) if error.code == ErrorCode::PaneNotFound
        ));
        let explicit = workspace
            .control_resolve(&pane_id(SessionId(2)), "12345678")
            .unwrap();
        assert_eq!((explicit.tab, explicit.session), (2, SessionId(2)));
    }

    #[test]
    fn control_close_promotes_the_remaining_pane_to_primary_and_focus() {
        let mut workspace = Workspace::default();
        let (window, _) = workspace.open_window(false);
        let offline = || Session::disconnected_ibm(IbmKind::Tn3270e);
        let closing = workspace.store_session(offline(), Default::default(), 14);
        let remaining = workspace.store_session(offline(), Default::default(), 14);
        workspace.add_tab(window, closing);
        let tab = workspace.active_tab_mut(window).unwrap();
        let primary = tab.primary;
        let (sibling, _) = tab
            .grid
            .split(pane_grid::Axis::Vertical, primary, remaining)
            .unwrap();

        assert_eq!(
            workspace
                .control_close(Address {
                    window,
                    tab: 0,
                    pane: primary,
                    session: closing,
                })
                .unwrap(),
            json!({"ok": true})
        );

        let tab = workspace.windows[&window].tab().unwrap();
        assert_eq!((tab.primary, tab.focused), (sibling, sibling));
        assert_eq!(tab.focused_session(), Some(remaining));
        assert!(!workspace.sessions.contains_key(&closing));
    }

    #[test]
    fn resolve_picks_the_nearest_live_shell_independently_of_focus() {
        let mut workspace = Workspace::default();
        let (window, _task) = workspace.open_window(true);
        workspace.new_tab(window, TerminalProfile::Xterm);
        let state = workspace.windows.get_mut(&window).unwrap();
        let [older, focused] = {
            let mut tabs = state.terminal_tabs();
            [
                tabs.next().unwrap().focused_session().unwrap(),
                tabs.next().unwrap().focused_session().unwrap(),
            ]
        };
        state.focused = true;
        assert_eq!(workspace.control_current().unwrap().session, focused);

        let older_pid = wait_for_shell(&workspace.sessions[&older].session);
        let focused_pid = wait_for_shell(&workspace.sessions[&focused].session);
        let result = workspace
            .control_pane_for_pids(&[focused_pid, older_pid], "12345678")
            .unwrap();
        assert_eq!(result["matched_pid"], focused_pid);
        let result = workspace
            .control_pane_for_pids(&[older_pid, focused_pid], "12345678")
            .unwrap();
        assert_eq!(result["pane"]["pane_id"], pane_id(older));
        assert_eq!(result["pane"]["shell_pid"], older_pid);
        assert_eq!(result["matched_pid"], older_pid);

        let _done = workspace.sessions[&older].session.stop();
        let result = workspace
            .control_pane_for_pids(&[older_pid], "12345678")
            .unwrap();
        assert_eq!(result, json!({"pane": null, "matched_pid": null}));
    }
}
