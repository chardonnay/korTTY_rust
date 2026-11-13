use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use async_channel::Receiver;
use iced::futures::{StreamExt, stream::BoxStream};
use iced::widget::{button, checkbox, column, pane_grid, row, text};
use iced::{Element, Subscription, Task, window};
use kortty_control::{
    ErrorCode, Fault, Method, Params, ReadMode, Reply, Request, Server, TabCreateParams,
    tab_create_result,
};
use serde_json::{Value, json};

use crate::clipboard_policy::ControlApiPolicy;
use crate::session::{Frame, SessionId, Status};
use crate::session_security::IsolationLevel;

use super::ssh_access::CreateOptions;
use super::{Message as Desktop, PaneSession, Tab, Window, Workspace};

mod agents;
mod create_target;
mod input;
mod mcp;
mod preferences;
use create_target::CreateTarget;

#[derive(Clone, Debug)]
pub enum Message {
    Settings(window::Id, bool),
    Enable(window::Id),
    Disable(window::Id),
    McpServer(window::Id, bool),
    McpWrites(window::Id, bool),
    McpChecked(u64, Result<(), &'static str>),
    McpAnswer(window::Id, u64, mcp::Decision),
    McpWritten(u64, Result<usize, &'static str>),
    Stopped,
    Request(Request),
    AgentWritten(u64, Result<usize, &'static str>),
    Tick,
}

#[derive(Default)]
pub(super) struct Control {
    pub(super) settings: Option<window::Id>,
    server: Option<Server>,
    generation: u64,
    stopping: bool,
    first_write: bool,
    pending_splits: HashMap<SessionId, PendingSplit>,
    agents: agents::AgentControl,
    preferences: preferences::Preferences,
    consent: mcp::Consent,
    mcp_delivery: HashMap<u64, mcp::Delivery>,
}

impl Control {
    pub(super) fn subscription(&self) -> Subscription<Desktop> {
        let requests = self
            .server
            .as_ref()
            .map_or_else(Subscription::none, |server| {
                Subscription::run_with(Watch(self.generation, server.requests()), watch)
            });
        if self.agents.needs_tick() || self.consent.active() || !self.mcp_delivery.is_empty() {
            Subscription::batch([requests, Subscription::run(agent_clock)])
        } else {
            requests
        }
    }

    pub(super) fn detach(&mut self) -> Option<Server> {
        let server = self.server.take()?;
        server.revoke();
        self.agents = agents::AgentControl::default();
        self.consent.clear();
        self.mcp_delivery.clear();
        Some(server)
    }

    pub(super) fn close_settings(&mut self, id: window::Id) {
        if self.settings == Some(id) {
            self.settings = None;
        }
    }

    fn settings_view(&self, id: window::Id, editable: bool) -> Element<'_, Desktop> {
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
                button("Disable").on_press_maybe((editable && self.server.is_some()).then_some(
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

fn agent_clock() -> BoxStream<'static, Desktop> {
    let (sender, receiver) = async_channel::bounded(1);
    let _ = std::thread::Builder::new()
        .name("kortty-control-agent-waits".into())
        .spawn(move || {
            while !sender.is_closed() {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let _ = sender.try_send(());
            }
        });
    receiver.map(|()| Desktop::Control(Message::Tick)).boxed()
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
    pub(super) fn control_boot(&mut self) {
        let result = crate::profile::root().and_then(|root| self.control_boot_from(&root));
        if let Err(error) = result {
            eprintln!("kortty: could not start saved Control API settings ({error})");
        }
    }

    fn control_boot_from(&mut self, root: &std::path::Path) -> std::io::Result<()> {
        self.control.preferences = preferences::Preferences::load(root)?;
        let policy = &self.clipboard.policy;
        if policy.control_allowed()
            && (policy.control_api == ControlApiPolicy::Allow || self.control.preferences.enabled)
        {
            self.control_start(root)?;
        }
        Ok(())
    }

    fn control_editable(&self) -> bool {
        let policy = &self.clipboard.policy;
        policy.control_allowed() && policy.control_api == ControlApiPolicy::Personal
    }

    fn control_start(&mut self, root: &std::path::Path) -> std::io::Result<()> {
        let server = Server::start(root)?;
        server.set_mcp_mode(self.control_mcp_mode());
        self.control.server = Some(server);
        self.control.generation = self.control.generation.wrapping_add(1);
        self.control.first_write = true;
        Ok(())
    }

    fn control_save(&mut self, id: window::Id) {
        let preferences = self.control.preferences;
        if let Err(error) = crate::profile::root().and_then(|root| preferences.save(&root)) {
            self.notice(id, format!("Could not save Control API settings ({error})"));
        }
    }

    pub(super) fn update_control(&mut self, message: Message) -> Task<Desktop> {
        match message {
            Message::Settings(id, true) if self.windows.contains_key(&id) => {
                self.control.settings = Some(id);
            }
            Message::Settings(id, false) => self.control.close_settings(id),
            Message::Enable(id)
                if self.control.settings == Some(id)
                    && self.control_editable()
                    && self.control.server.is_none()
                    && !self.control.stopping =>
            {
                match crate::profile::root().and_then(|root| self.control_start(&root)) {
                    Ok(()) => {
                        self.control.preferences.enabled = true;
                        self.control_save(id);
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
            Message::Disable(id)
                if self.control.settings == Some(id) && self.control_editable() =>
            {
                self.control.preferences.enabled = false;
                let server = self.control.detach();
                self.control_save(id);
                if let Some(server) = server {
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
            Message::McpServer(id, enabled)
                if self.control.settings == Some(id)
                    && self.control.server.is_some()
                    && self.clipboard.policy.control_allowed()
                    && self.clipboard.policy.mcp_server_allowed =>
            {
                self.control.preferences.mcp_server_enabled = enabled;
                self.control_update_mcp(id);
            }
            Message::McpWrites(id, enabled)
                if self.control.settings == Some(id)
                    && self.control.server.is_some()
                    && self.control.preferences.mcp_server_enabled
                    && self.clipboard.policy.control_allowed()
                    && self.clipboard.policy.mcp_server_allowed =>
            {
                self.control.preferences.mcp_write_tools_enabled = enabled;
                self.control_update_mcp(id);
            }
            Message::McpChecked(ticket, result) => return self.control_mcp_checked(ticket, result),
            Message::McpAnswer(owner, ticket, decision) => {
                return self.control_mcp_answer(owner, ticket, decision);
            }
            Message::McpWritten(ticket, result) => return self.control_mcp_written(ticket, result),
            Message::Stopped => self.control.stopping = false,
            Message::Request(request) => return self.control_request(&request),
            Message::AgentWritten(ticket, result) => {
                return self.control_agent_written(ticket, result);
            }
            Message::Tick => {
                self.control_agent_tick();
                self.control_mcp_tick();
            }
            _ => {}
        }
        Task::none()
    }

    fn control_update_mcp(&mut self, owner: window::Id) {
        let mode = self.control_mcp_mode();
        self.control.consent.clear();
        self.control.mcp_delivery.clear();
        if let Some(server) = &self.control.server {
            server.set_mcp_mode(mode);
        }
        self.control_save(owner);
    }

    pub(super) fn view_control_settings(&self, id: window::Id) -> Element<'_, Desktop> {
        let editable = self.control_editable();
        let mut body = column![self.control.settings_view(id, editable)].padding(8);
        if !self.clipboard.policy.control_allowed() {
            body = body.push(text("Managed policy blocks the Control API."));
        } else if !editable {
            body = body.push(text("Managed policy requires the Control API."));
        } else if self.control.server.is_none() && !self.control.stopping {
            body = body
                .push(button("Enable Control API").on_press(Desktop::Control(Message::Enable(id))));
        }
        let policy =
            self.clipboard.policy.control_allowed() && self.clipboard.policy.mcp_server_allowed;
        let editable = policy && self.control.server.is_some();
        let preferences = self.control.preferences;
        body = body.push(
            column![
                checkbox(policy && preferences.mcp_server_enabled)
                    .label("MCP server")
                    .on_toggle_maybe(editable.then_some(move |enabled| {
                        Desktop::Control(Message::McpServer(id, enabled))
                    })),
                checkbox(policy && preferences.mcp_write_tools_enabled)
                    .label("Allow MCP write tools")
                    .on_toggle_maybe((editable && preferences.mcp_server_enabled).then_some(
                        move |enabled| { Desktop::Control(Message::McpWrites(id, enabled)) }
                    )),
                text(if !policy {
                    "Managed policy blocks MCP access."
                } else if !editable {
                    "Enable the Control API to configure MCP access."
                } else {
                    "MCP reads mask known secrets. Terminal writes require desktop consent."
                }),
            ]
            .spacing(8),
        );
        body.into()
    }

    fn control_request(&mut self, request: &Request) -> Task<Desktop> {
        if !request.reply.active() {
            return Task::none();
        }
        if !self.clipboard.policy.control_allowed() {
            request.reply.send(Err(Fault::new(
                ErrorCode::Policy,
                "Managed policy blocks local control",
            )));
            return Task::none();
        }
        if request.reply.mcp_client().is_some() {
            return self.control_mcp_route(request);
        }
        let result = match request.method {
            Method::AgentPrompt | Method::AgentKeys => {
                return self.control_agent_write(request);
            }
            Method::AgentWait => {
                if let Err(error) = self.control_agent_wait(request, None) {
                    request.reply.send(Err(error));
                }
                return Task::none();
            }
            Method::Agents | Method::Agent | Method::AgentExplain | Method::AgentRename => self
                .control_agents(request)
                .map(|value| (value, Task::none())),
            Method::AgentStart => (|| {
                let address =
                    self.control_resolve(request.params.required("pane")?, &request.instance)?;
                let value = self.control_agent_start(request)?;
                Ok((
                    value,
                    self.control_first_mutation(
                        address.window,
                        "A local Control API client started a coding agent.",
                    ),
                ))
            })(),
            Method::Notify => {
                return match self.control_notification(request) {
                    Ok(task) => task,
                    Err(error) => {
                        request.reply.send(Err(error));
                        Task::none()
                    }
                };
            }
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

    fn control_first_mutation(&mut self, owner: window::Id, message: &str) -> Task<Desktop> {
        if !std::mem::take(&mut self.control.first_write) {
            return Task::none();
        }
        self.notice(owner, message.into());
        self.notify_control_activity("Local Control API", message)
    }

    fn control_notification(&self, request: &Request) -> Result<Task<Desktop>, Fault> {
        if !self.clipboard.policy.desktop_notifications_allowed {
            return Err(Fault::new(
                ErrorCode::Policy,
                "Managed policy blocks desktop notifications",
            ));
        }
        if !self.terminal_notifications_enabled() {
            return Err(Fault::new(
                ErrorCode::Unsupported,
                "Desktop notifications are disabled",
            ));
        }
        let title = request.params.required("title")?.to_owned();
        let body = request.params.required("body")?.to_owned();
        Ok(self.notify_control_request(title, body, request.reply.clone()))
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
                let mcp = request.reply.mcp_client().is_some();
                let tabs: Vec<_> = self
                    .control_windows(params.string("window")?)?
                    .into_iter()
                    .flat_map(|(_, window)| {
                        indexed_terminal_tabs(window)
                            .filter(move |(_, tab)| !mcp || !tab.incognito)
                            .map(move |(index, tab)| {
                                workspace.control_tab_info(window, tab, index, instance)
                            })
                    })
                    .collect();
                json!({"instance": instance.as_ref(), "tabs": tabs})
            }
            Method::CreateTab => return self.control_create_tab(request),
            Method::Panes => {
                self.control_pane_list(params, instance, request.reply.mcp_client().is_some())?
            }
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
                        task = Task::batch([
                            task,
                            self.control_first_mutation(
                                address.window,
                                "A local Control API client changed terminal focus.",
                            ),
                        ]);
                        if pane {
                            json!({"ok": true, "pane": self.control_pane_info(address, instance)})
                        } else {
                            let window = &self.windows[&address.window];
                            json!({"ok": true, "tab": self.control_tab_info(
                                window, window.tabs[address.tab].terminal().expect("resolved terminal tab"), address.tab, instance)})
                        }
                    }
                    Method::Close => {
                        let result = self.control_close(address)?;
                        task = self.control_first_mutation(
                            address.window,
                            "A local Control API client closed a terminal pane.",
                        );
                        result
                    }
                    _ => {
                        let aliases = self.control_shortcut_aliases();
                        let written = input::write(
                            self.sessions
                                .get_mut(&address.session)
                                .expect("resolved pane"),
                            address.session,
                            request.method,
                            params,
                            aliases.as_ref(),
                        )?;
                        task = self.control_first_mutation(
                            address.window,
                            "A local Control API client sent terminal input.",
                        );
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

    fn control_create_tab(&mut self, request: &Request) -> Result<(Value, Task<Desktop>), Fault> {
        let params = TabCreateParams::parse(&request.params)?;
        let windows = self
            .control_windows(params.window)
            .map_err(|error| error.with("window", json!(params.window)))?;
        let owner = windows
            .iter()
            .find(|(_, window)| window.focused)
            .or_else(|| windows.first())
            .map(|(id, _)| *id)
            .ok_or_else(|| Fault::new(ErrorCode::Unavailable, "No kortty window is open"))?;
        self.control_allow_mutation(owner)?;
        if self.exiting
            || self.owner_dialog(owner).is_some()
            || self.reopen_busy(owner)
            || self.palette.is_open(owner)
        {
            return Err(Fault::new(
                ErrorCode::Busy,
                "The window is waiting for user input",
            ));
        }
        if self.clipboard.policy.problem.is_some() {
            return Err(Fault::new(
                ErrorCode::Policy,
                "Managed policy blocks the connection",
            ));
        }
        let isolation = params.isolation.and_then(IsolationLevel::from_id);
        let minimum = self.clipboard.policy.minimum_isolation;
        if isolation.is_some_and(|requested| requested < minimum) {
            return Err(Fault::new(
                ErrorCode::Policy,
                "Managed policy requires stronger isolation",
            )
            .with("isolation", json!(isolation))
            .with("minimum", json!(minimum)));
        }
        if params.incognito && !self.clipboard.policy.incognito_allowed {
            return Err(Fault::new(
                ErrorCode::Policy,
                "Managed policy blocks incognito sessions",
            ));
        }
        let target = self.control_create_target(params.connection)?;
        if !request.reply.active() {
            return Err(Fault::new(
                ErrorCode::Timeout,
                "The tab creation request expired",
            ));
        }
        let options = CreateOptions {
            isolation,
            incognito: params.incognito,
        };
        let (tab, task) = match target {
            CreateTarget::Ssh(connection, path) => {
                self.create_control_saved_ssh(owner, *connection, path, options)?
            }
            CreateTarget::Teamwork(selection) => {
                self.create_control_teamwork_ssh(owner, selection, options)?
            }
            CreateTarget::Serial(profile) => {
                if !self.serial_profiles_allowed() {
                    return Err(Fault::new(
                        ErrorCode::Policy,
                        "Managed policy blocks serial connections",
                    ));
                }
                if isolation.is_some_and(|level| level != IsolationLevel::None) {
                    return Err(Fault::new(
                        ErrorCode::Unsupported,
                        "Serial connections do not support session isolation",
                    ));
                }
                let previous = self.windows[&owner].active_tab_id();
                let task = self.open_saved_serial_with_privacy(owner, profile, options.incognito);
                let tab = self.windows[&owner]
                    .active_tab_id()
                    .filter(|id| Some(*id) != previous)
                    .ok_or_else(|| {
                        Fault::new(ErrorCode::Unavailable, "Could not open the serial tab")
                    })?;
                (Some(tab), task)
            }
        };
        let tab = tab
            .map(|id| {
                let window = &self.windows[&owner];
                indexed_terminal_tabs(window)
                    .find(|(_, tab)| tab.id == id)
                    .map(|(index, tab)| {
                        self.control_tab_info(window, tab, index, &request.instance)
                    })
                    .ok_or_else(|| Fault::new(ErrorCode::Unavailable, "The new tab is unavailable"))
            })
            .transpose()?;
        let notice = self.control_first_mutation(
            owner,
            "A local Control API client requested a terminal tab.",
        );
        Ok((
            tab_create_result(&request.instance, tab),
            Task::batch([task, notice]),
        ))
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

    fn control_pane_list(
        &self,
        params: &Params,
        instance: &str,
        mcp: bool,
    ) -> Result<Value, Fault> {
        let mut panes = Vec::new();
        let wanted = params.string("tab")?;
        let mut found = wanted.is_none();
        let local_only = params.flag("local_shell_only", false)?;
        for (id, window) in self.control_windows(params.string("window")?)? {
            for (index, tab) in indexed_terminal_tabs(window) {
                if mcp && tab.incognito {
                    continue;
                }
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
        let agents = self.control_agent_totals(Some(tab));
        json!({"window_id": window_id(window), "tab_id": tab_id(instance, tab.id),
            "title": self.tab_names(tab).0,
            "protocol": pane.map(|pane| protocol(pane, &pane.frame)).unwrap_or("UNKNOWN"),
            "host": pane.and_then(|pane| pane.ssh.as_ref().map(super::SshTarget::host)),
            "connected": pane.is_some_and(|pane| pane.session.frame().status == Status::Running),
            "active": window.active == index, "pane_count": tab.grid.len(),
            "agents": agents})
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
        let agent = self.control_agent_info(address, instance);
        json!({"window_id": window_id, "tab_id": tab_id, "pane_id": pane_id,
            "index": index, "focused": address.pane == tab.focused,
            "protocol": protocol, "connected": frame.status == Status::Running,
            "local_shell": protocol == "LOCAL_SHELL", "working_directory": null, "shell_pid": shell_pid,
            "columns": frame.screen.columns, "rows": frame.screen.rows,
            "alternate_screen": frame.screen.alternate_screen, "bracketed_paste": frame.bracketed_paste,
            "isolation": pane.isolation_name(&frame.status), "incognito": tab.incognito,
            "stream_generation":pane.stream_generation,
            "agent": agent})
    }

    fn control_allow_mutation(&self, id: window::Id) -> Result<(), Fault> {
        let window = &self.windows[&id];
        if self.control.settings == Some(id)
            || self.control.consent.owner() == Some(id)
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

    pub(super) fn control_finish_split(&mut self, session: SessionId) -> Task<Desktop> {
        let Some(pending) = self.control.pending_splits.get(&session) else {
            return Task::none();
        };
        let status = &self.sessions[&session].frame.status;
        if pending.reply.active() && *status == Status::Starting {
            return Task::none();
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
        let owner = result
            .is_ok()
            .then(|| {
                self.windows
                    .iter()
                    .find(|(_, window)| window.pane_location(session).is_some())
                    .map(|(&id, _)| id)
            })
            .flatten();
        pending.reply.send(result);
        owner.map_or_else(Task::none, |owner| {
            self.control_first_mutation(owner, "A local Control API client split a terminal pane.")
        })
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
    use std::path::PathBuf;
    use std::time::{Duration, Instant};
    use std::{fs, thread};

    struct TestProfile(PathBuf);

    impl TestProfile {
        fn new() -> Self {
            // The profile must leave room for the control socket in sockaddr_un.
            let mut nonce = [0; 8];
            getrandom::fill(&mut nonce).unwrap();
            let path = std::env::temp_dir().join(format!("kc-{:016x}", u64::from_ne_bytes(nonce)));
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            }
            #[cfg(windows)]
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestProfile {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn managed_control_api_boot_applies_policy_and_preserves_personal_preferences() {
        use kortty_control::mcp::Mode;

        for (rule, enabled, listening, editable) in [
            ("", false, false, true),
            ("", true, true, true),
            ("control-api = 'allow'", false, true, false),
            ("control-api = 'deny'", true, false, false),
        ] {
            let profile = TestProfile::new();
            let preferences = preferences::Preferences {
                enabled,
                mcp_server_enabled: false,
                mcp_write_tools_enabled: true,
            };
            preferences.save(&profile.0).unwrap();
            let mut workspace = Workspace::default();
            workspace.clipboard.policy = crate::clipboard_policy::parse(&format!(
                "[meta]\nschema-version=1\n[[rule]]\n[rule.features]\nmcp-server = 'allow'\n{rule}"
            ))
            .unwrap();
            workspace.control_boot_from(&profile.0).unwrap();
            assert_eq!(workspace.control_editable(), editable, "{rule}");
            assert_eq!(workspace.control.preferences, preferences);
            assert_eq!(
                preferences::Preferences::load(&profile.0).unwrap(),
                preferences
            );
            assert_eq!(
                workspace.control_mcp_mode(),
                if workspace.clipboard.policy.control_allowed() {
                    Mode::Off
                } else {
                    Mode::PolicyDenied
                }
            );
            assert_eq!(workspace.control.server.is_some(), listening, "{rule}");
            if let Some(server) = workspace.control.server.take() {
                let mut client =
                    kortty_control::Client::connect(&profile.0, Duration::from_secs(5)).unwrap();
                let response = client.call("ping", &json!({})).unwrap();
                assert_eq!(response["pong"], true);
                assert_eq!(response["instance"], server.instance());
                drop(client);
                server.stop();
            }
        }
    }

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
    fn control_reports_host_shortcut_conflicts_and_allows_literal_input() {
        let profile = TestProfile::new();
        let server = Server::start(&profile.0).unwrap();
        let requests = server.requests();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(
            serde_json::from_value(json!({
                "general":{"agent_shortcut":{"enabled":true,"name":"fixture","case_insensitive":true}}
            }))
            .unwrap(),
        );
        let (owner, _) = workspace.open_window(false);
        workspace.new_tab(owner, TerminalProfile::Xterm);
        let session = workspace.windows[&owner]
            .tab()
            .unwrap()
            .focused_session()
            .unwrap();
        wait_for_shell(&workspace.sessions[&session].session);
        let pane = pane_id(session);
        let call = |workspace: &mut Workspace, method, params, dispatched: &[Method]| {
            thread::scope(|scope| {
                let root = &profile.0;
                let client = scope.spawn(move || {
                    kortty_control::Client::connect(root, Duration::from_secs(5))
                        .unwrap()
                        .call(method, &params)
                });
                for &expected in dispatched {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let request = loop {
                        match requests.try_recv() {
                            Ok(request) => break request,
                            Err(async_channel::TryRecvError::Empty)
                                if Instant::now() < deadline =>
                            {
                                thread::sleep(Duration::from_millis(5));
                            }
                            Err(error) => panic!("Missing {expected:?} request: {error}"),
                        }
                    };
                    assert_eq!(request.method, expected);
                    let _ = workspace.update(Desktop::Control(Message::Request(request)));
                }
                client.join().unwrap()
            })
        };
        let command = "FIXTURE-ask safe question";
        for (method, params, dispatched) in [
            (
                "pane.send_text",
                json!({"pane":pane,"text":format!("{command}\r\nother"),"bracketed":"never"}),
                &[Method::SendText][..],
            ),
            (
                "pane.run",
                json!({"pane":pane,"command":command}),
                &[Method::Run][..],
            ),
            (
                "agent.start",
                json!({"pane":pane,"kind":"gemini-cli","command":["FIXTURE-ask", "safe question"]}),
                &[Method::Pane, Method::AgentStart][..],
            ),
        ] {
            let error = call(&mut workspace, method, params, dispatched).unwrap_err();
            assert_eq!(error.exit_code(), 2);
            let kortty_control::ClientError::Server { rpc, .. } = error else {
                panic!("Expected a Control API fault");
            };
            assert_eq!(rpc["code"], -32024);
            assert_eq!(
                rpc["data"],
                json!({"code":"host_shortcut_conflict","retryable":false,"exit":2,
                    "pane":pane,"shortcut":"fixture",
                    "hint":"pass allow_shortcut_conflict:true to write it anyway"})
            );
        }

        for (params, length, bracketed) in [
            (
                json!({"pane":pane,"text":command,"allow_shortcut_conflict":true}),
                command.len(),
                false,
            ),
            (
                json!({"pane":pane,"text":command,"bracketed":"always"}),
                command.len() + 12,
                true,
            ),
            (json!({"pane":pane,"text":"fixture"}), 7, false),
        ] {
            let reply = call(
                &mut workspace,
                "pane.send_text",
                params,
                &[Method::SendText],
            )
            .unwrap();
            assert_eq!(reply["bytes_written"], length);
            assert_eq!(reply["bracketed"], bracketed);
            assert_eq!(reply["submitted"], false);
        }
        server.stop();
    }

    #[cfg(windows)]
    #[test]
    fn control_notification_reports_unavailable_when_the_owner_disappears() {
        let fixture = TestProfile::new();
        let server = Server::start(&fixture.0).unwrap();
        let root = fixture.0.clone();
        let client = thread::spawn(move || {
            kortty_control::Client::connect(&root, Duration::from_secs(5))
                .unwrap()
                .call("notification.show", &json!({"title":"Fixture","body":""}))
                .unwrap()
        });
        let request = server.requests().recv_blocking().unwrap();
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let task = workspace.control_request(&request);
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        let action = iced::futures::executor::block_on(stream.next()).unwrap();
        assert!(matches!(
            &action,
            iced_runtime::Action::Window(iced_runtime::window::Action::Run(id, _)) if *id == owner
        ));
        // The runtime drops the callback when its owner is no longer present.
        drop(action);
        assert!(iced::futures::executor::block_on(stream.next()).is_none());
        assert_eq!(
            client.join().unwrap(),
            json!({"shown":true,"supported":false})
        );
        server.stop();
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
