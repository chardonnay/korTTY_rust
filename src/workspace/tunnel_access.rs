use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use iced::widget::{button, checkbox, column, pick_list, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length, Task, window};
use kortty_ssh::Endpoint;

use crate::connection_store::{MAX_TUNNELS, TunnelPlan};
use crate::session::{SessionId, Status};
use crate::ssh_connection::{DirectOpener, RemoteOpener};
use crate::ssh_dialog::normalize_host;
use crate::ssh_tunnel::{Capacity, Destination, Listener};

use super::{Message as WorkspaceMessage, Workspace};

mod saved;
pub(super) use saved::ClosingOwner;
pub use saved::SavedReview;
use saved::SavedTunnels;

const MAX_LISTENERS: usize = 16;

#[derive(Debug, Clone)]
pub enum Message {
    Show(window::Id, SessionId),
    Close(window::Id),
    Edit(window::Id, Field),
    Start(window::Id),
    Stop(window::Id, u64),
    ListenerChanged(SessionId),
    Review(window::Id, SavedReview, bool),
    Approved(SavedReview, Result<(), String>),
}

#[derive(Debug, Clone)]
pub enum Field {
    Mode(Mode),
    Bind(String),
    BindPort(String),
    Destination(String),
    DestinationPort(String),
    Exposed(bool),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Local,
    Remote,
    Dynamic,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Local => "Local",
            Self::Remote => "Remote",
            Self::Dynamic => "Dynamic (SOCKS)",
        })
    }
}

#[derive(Default)]
struct Form {
    mode: Mode,
    bind: String,
    bind_port: String,
    destination: String,
    destination_port: String,
    exposed: bool,
}

impl Form {
    fn update(&mut self, field: Field) {
        match field {
            Field::Mode(mode) => {
                self.mode = mode;
                self.exposed = false;
            }
            Field::Bind(bind) => {
                self.bind = bounded(bind, 64);
                self.exposed = false;
            }
            Field::BindPort(port) => {
                self.bind_port = bounded(port, 8);
                self.exposed = false;
            }
            Field::Destination(host) => {
                self.destination = bounded(host, 256);
                self.exposed = false;
            }
            Field::DestinationPort(port) => {
                self.destination_port = bounded(port, 8);
                self.exposed = false;
            }
            Field::Exposed(exposed) => self.exposed = exposed,
        }
    }

    fn bind_ip(&self) -> Result<IpAddr, &'static str> {
        let bind = self.bind.trim();
        if bind.is_empty() || bind.eq_ignore_ascii_case("localhost") {
            return Ok(Ipv4Addr::LOCALHOST.into());
        }
        bind.parse()
            .map_err(|_| "Use localhost or a numeric bind address.")
    }

    fn validate(&self) -> Result<(SocketAddr, Destination), &'static str> {
        let bind = self.bind_ip()?;
        if !bind.is_loopback() && !self.exposed {
            return Err("Confirm access from other devices before starting this listener.");
        }
        let bind_port = port(&self.bind_port).ok_or("Enter a listen port from 1 to 65535.")?;
        let destination = match self.mode {
            Mode::Local | Mode::Remote => {
                let host = normalize_host(&self.destination)
                    .map_err(|_| "Enter a destination hostname or IP address.")?;
                let port = port(&self.destination_port)
                    .ok_or("Enter a destination port from 1 to 65535.")?;
                let endpoint = Endpoint::new(&host, port).map_err(|_| "Invalid destination.")?;
                if self.mode == Mode::Remote {
                    Destination::Remote(endpoint)
                } else {
                    Destination::Fixed(endpoint)
                }
            }
            Mode::Dynamic => Destination::Socks,
        };
        Ok((SocketAddr::new(bind, bind_port), destination))
    }
}

fn bounded(input: String, max: usize) -> String {
    if input.len() <= max {
        input
    } else {
        String::new()
    }
}

fn port(input: &str) -> Option<u16> {
    let input = input.trim();
    if !input.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    input.parse().ok().filter(|&port| port != 0)
}

pub(super) struct Forwarding {
    direct: DirectOpener,
    remote: RemoteOpener,
    capacity: Capacity,
    active: BTreeMap<u64, ActiveListener>,
    next_listener: u64,
    form: Form,
    notice: Option<String>,
    saved: Option<SavedTunnels>,
    retiring_saved: bool,
}

struct ActiveListener {
    listener: Listener,
    saved: Option<usize>,
}

impl Forwarding {
    pub(super) fn new(direct: DirectOpener, remote: RemoteOpener) -> Self {
        Self {
            direct,
            remote,
            capacity: Capacity::default(),
            active: BTreeMap::new(),
            next_listener: 0,
            form: Form::default(),
            notice: None,
            saved: None,
            retiring_saved: false,
        }
    }

    pub(super) fn active_count(&self) -> usize {
        self.active
            .values()
            .filter(|entry| entry.listener.is_listening())
            .count()
    }

    fn start(&mut self, session: SessionId) -> Task<WorkspaceMessage> {
        if self.active.len() >= MAX_LISTENERS {
            self.notice = Some(format!(
                "This session can manage up to {MAX_LISTENERS} listeners."
            ));
            return Task::none();
        }
        let (address, destination) = match self.form.validate() {
            Ok(valid) => valid,
            Err(error) => {
                self.notice = Some(error.into());
                return Task::none();
            }
        };
        match self.start_listener(session, address, destination, None) {
            Ok(task) => {
                self.notice = None;
                self.form = Form::default();
                task
            }
            Err(error) => {
                self.notice = Some(format!("Could not start the SSH tunnel ({error})"));
                Task::none()
            }
        }
    }

    fn start_listener(
        &mut self,
        session: SessionId,
        address: SocketAddr,
        destination: Destination,
        saved: Option<usize>,
    ) -> std::io::Result<Task<WorkspaceMessage>> {
        let capacity = self.capacity.clone();
        let (listener, updates) = match destination {
            Destination::Remote(target) => {
                Listener::start_remote(self.remote.clone(), address, target, capacity)
            }
            _ => Listener::start(self.direct.clone(), address, destination, capacity),
        }?;
        self.next_listener += 1;
        self.active
            .insert(self.next_listener, ActiveListener { listener, saved });
        Ok(Task::run(updates, move |_| {
            WorkspaceMessage::Tunnel(Message::ListenerChanged(session))
        }))
    }

    fn start_saved(
        &mut self,
        session: SessionId,
        plan: TunnelPlan,
        stopped: [bool; MAX_TUNNELS],
    ) -> Task<WorkspaceMessage> {
        let mut tasks = Vec::new();
        let mut failures = Vec::new();
        for (index, (rule, stopped)) in plan.rules.into_iter().zip(stopped).enumerate() {
            if stopped {
                continue;
            }
            if self.active.len() >= MAX_LISTENERS {
                failures.push("Listener limit reached.".into());
                break;
            }
            match rule.destination().and_then(|destination| {
                self.start_listener(session, rule.bind, destination, Some(index))
            }) {
                Ok(task) => tasks.push(task),
                Err(error) => failures.push(format!("{} ({error})", rule.label())),
            }
        }
        if let Some(first) = failures.first() {
            self.notice = Some(format!(
                "Could not start {} saved tunnel(s). {first}",
                failures.len()
            ));
        }
        Task::batch(tasks)
    }
}

impl Workspace {
    pub(super) fn tunnel_review_view(
        &self,
        id: window::Id,
        review: &SavedReview,
    ) -> Element<'_, WorkspaceMessage> {
        let mut content = column![
            text("Open saved SSH tunnels?").size(22),
            text(review.plan.login.label()),
            text("Review the addresses before allowing forwarding. Remote listener exposure depends on the SSH server.").size(13),
        ].spacing(10);
        for rule in &review.plan.rules {
            content = content.push(text(rule.label()));
        }
        if self.tunnel_approval_pending(review) {
            content = content.push(text("Saving tunnel approval…"));
        } else {
            let decision =
                |allowed| WorkspaceMessage::Tunnel(Message::Review(id, review.clone(), allowed));
            content = content.push(
                row![
                    button("Not now").on_press(decision(false)),
                    button("Open tunnels").on_press(decision(true)),
                ]
                .spacing(8),
            );
        }
        scrollable(content.padding(16)).into()
    }

    pub(super) fn update_tunnel(&mut self, message: Message) -> Task<WorkspaceMessage> {
        let window = match message {
            Message::Show(window, _)
            | Message::Close(window)
            | Message::Edit(window, _)
            | Message::Start(window)
            | Message::Stop(window, _) => window,
            Message::ListenerChanged(session) => return self.tunnel_listener_changed(session),
            Message::Review(window, review, allowed) => {
                return self.review_tunnels(window, review, allowed);
            }
            Message::Approved(review, result) => {
                return self.tunnel_approval_result(review, result);
            }
        };
        let Some(state) = self.windows.get_mut(&window) else {
            return Task::none();
        };
        if let Message::Close(_) = message {
            state.pending_tunnels = None;
            return Task::none();
        }
        if !self.clipboard.policy.port_forwarding_allowed {
            return Task::none();
        }
        if let Message::Show(_, session) = message {
            if state.pane_location(session).is_some()
                && self
                    .sessions
                    .get(&session)
                    .is_some_and(|pane| pane.forwarding.is_some())
            {
                state.cancel_paste();
                state.pending_tunnels = Some(session);
            }
            return Task::none();
        }
        let Some(session) = state.pending_tunnels else {
            return Task::none();
        };
        if state.pane_location(session).is_none() {
            state.pending_tunnels = None;
            return Task::none();
        }
        let Some(pane) = self.sessions.get_mut(&session) else {
            return Task::none();
        };
        let Some(forwarding) = &mut pane.forwarding else {
            return Task::none();
        };
        match message {
            Message::Edit(_, field) => {
                forwarding.form.update(field);
                forwarding.notice = None;
            }
            Message::Start(_) if pane.frame.status == Status::Running => {
                return forwarding.start(session);
            }
            Message::Stop(_, id) => {
                if let Some(entry) = forwarding.active.get(&id) {
                    let saved_rule = entry.saved;
                    if entry.listener.is_finished() {
                        forwarding.active.remove(&id);
                    } else {
                        entry.listener.stop();
                    }
                    if !forwarding.retiring_saved
                        && let (Some(saved), Some(rule)) = (&mut forwarding.saved, saved_rule)
                    {
                        saved.stop_rule(rule);
                    }
                }
            }
            _ => {}
        }
        Task::none()
    }

    pub(super) fn tunnel_view(
        &self,
        window: window::Id,
        session: SessionId,
    ) -> Element<'_, WorkspaceMessage> {
        let close = button("Close").on_press(WorkspaceMessage::Tunnel(Message::Close(window)));
        let Some(pane) = self.sessions.get(&session) else {
            return close.into();
        };
        let Some(forwarding) = &pane.forwarding else {
            return close.into();
        };
        let form = &forwarding.form;
        let edit = move |field| WorkspaceMessage::Tunnel(Message::Edit(window, field));
        let mut content = column![
            row![
                text("SSH tunnels").size(22),
                iced::widget::space::horizontal(),
                close
            ]
            .align_y(Alignment::Center),
            pick_list(
                [Mode::Local, Mode::Remote, Mode::Dynamic],
                Some(form.mode),
                move |mode| { edit(Field::Mode(mode)) }
            ),
            row![
                text_input("Listen address (localhost)", &form.bind)
                    .on_input(move |address| edit(Field::Bind(address))),
                text_input("Listen port", &form.bind_port)
                    .on_input(move |port| edit(Field::BindPort(port)))
                    .width(120),
            ]
            .spacing(8),
        ]
        .spacing(10);
        if form.mode != Mode::Dynamic {
            let label = if form.mode == Mode::Remote {
                "Local service hostname or IP"
            } else {
                "Remote service hostname or IP"
            };
            content = content.push(
                row![
                    text_input(label, &form.destination)
                        .on_input(move |host| edit(Field::Destination(host))),
                    text_input("Destination port", &form.destination_port)
                        .on_input(move |port| edit(Field::DestinationPort(port)))
                        .width(120),
                ]
                .spacing(8),
            );
        }
        if form.mode == Mode::Remote {
            content = content.push(
                text("The SSH server listens on this address. Its GatewayPorts setting controls exposure.")
                    .size(13),
            );
        }
        if form.bind_ip().is_ok_and(|ip| !ip.is_loopback()) {
            content = content.push(
                checkbox(form.exposed)
                    .label("Allow other devices to reach this listener")
                    .on_toggle(move |allowed| edit(Field::Exposed(allowed))),
            );
        }
        let validation = form.validate();
        let can_start = pane.frame.status == Status::Running
            && validation.is_ok()
            && self.clipboard.policy.port_forwarding_allowed;
        content =
            content.push(button("Start tunnel").on_press_maybe(
                can_start.then_some(WorkspaceMessage::Tunnel(Message::Start(window))),
            ));
        if let Some(notice) = &forwarding.notice {
            content = content.push(text(notice).size(13));
        } else if !can_start && let Err(error) = validation {
            content = content.push(text(error).size(13));
        }
        if pane.frame.status != Status::Running {
            content = content.push(text("The SSH session is closed.").size(13));
        }
        for (&id, entry) in &forwarding.active {
            let listener = &entry.listener;
            content = content.push(
                row![
                    column![text(listener.label()), text(listener.status()).size(13)].spacing(2),
                    iced::widget::space::horizontal(),
                    button(if listener.is_finished() {
                        "Remove"
                    } else {
                        "Stop"
                    })
                    .on_press_maybe(
                        (!listener.is_stopping() || listener.is_finished())
                            .then_some(WorkspaceMessage::Tunnel(Message::Stop(window, id)))
                    ),
                ]
                .align_y(Alignment::Center),
            );
        }
        scrollable(content.padding(16).width(Length::Fill)).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listener_requires_fresh_exposure_confirmation_when_the_scope_changes() {
        let mut form = Form {
            mode: Mode::Dynamic,
            bind_port: "1080".into(),
            ..Form::default()
        };
        assert!(form.validate().unwrap().0.ip().is_loopback());
        form.update(Field::Bind("0.0.0.0".into()));
        assert!(form.validate().is_err());
        form.update(Field::Exposed(true));
        assert!(form.validate().unwrap().0.ip().is_unspecified());
        form.update(Field::BindPort("1081".into()));
        assert!(form.validate().is_err());
    }
}
