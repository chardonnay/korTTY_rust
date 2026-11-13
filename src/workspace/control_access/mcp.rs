use std::collections::HashSet;
use std::sync::Arc;

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Element, Font, Length, Task, window};
use kortty_control::mcp::{MAX_READ_CHARS, MAX_READ_LINES, Mode, display_client};
use kortty_control::{ErrorCode, Fault, Method, Request, Screen};
use serde_json::{Value, json};

use crate::session::{Frame, McpIdentity, McpInput, SessionId, Status};
use crate::terminal_logging::{Redactor, Secrets};

use super::{Address, Desktop, Message, Workspace, input, pane_id};
use crate::workspace::terminal_ai::mask;

const MAX_GRANTS: usize = 128;

#[derive(Clone, Copy, Debug)]
pub enum Decision {
    Deny,
    Once,
    Session,
}

#[derive(Default)]
pub(super) struct Consent {
    next: u64,
    pending: Option<Pending>,
    grants: HashSet<Grant>,
}

#[derive(Eq, Hash, PartialEq)]
struct Grant {
    peer: String,
    session: SessionId,
    stream: u64,
}

struct Pending {
    ticket: u64,
    request: Request,
    owner: window::Id,
    tab: u64,
    grant: Grant,
    input: McpInput,
    written: Value,
    preview: String,
    label: String,
    client: String,
    offers_session: bool,
    submits: bool,
    ready: bool,
}

impl Consent {
    pub(super) fn active(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn owner(&self) -> Option<window::Id> {
        self.pending.as_ref().map(|pending| pending.owner)
    }

    pub(super) fn clear(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.request.reply.send(Err(denied("settings_changed")));
        }
        self.grants.clear();
    }

    pub(super) fn close_owner(&mut self, owner: window::Id) {
        if self.owner() == Some(owner)
            && let Some(pending) = self.pending.take()
        {
            pending.request.reply.send(Err(denied("window_closed")));
        }
    }
}

impl Workspace {
    pub(super) fn control_mcp_mode(&self) -> Mode {
        if !self.clipboard.policy.control_allowed() || !self.clipboard.policy.mcp_server_allowed {
            Mode::PolicyDenied
        } else if !self.control.preferences.mcp_server_enabled {
            Mode::Off
        } else if self.control.preferences.mcp_write_tools_enabled {
            Mode::Write
        } else {
            Mode::Read
        }
    }

    pub(super) fn control_mcp_route(&mut self, request: &Request) -> Task<Desktop> {
        let result = self.control_mcp_mode().access().and_then(|writes| {
            if request.method.mutates() {
                if !writes {
                    return Err(Fault::new(
                        ErrorCode::McpMethod,
                        "MCP write tools are disabled",
                    ));
                }
                return self.control_mcp_prepare(request);
            }
            self.control_mcp_read(request)
        });
        match result {
            Ok(task) => task,
            Err(fault) => {
                request.reply.send(Err(fault));
                Task::none()
            }
        }
    }

    fn control_mcp_read(&mut self, request: &Request) -> Result<Task<Desktop>, Fault> {
        let result = match request.method {
            Method::Read => {
                let address = self.control_mcp_resolve(request)?;
                let pane = &self.sessions[&address.session];
                let frame = pane.session.frame();
                if frame.password_prompt {
                    return Err(Fault::new(
                        ErrorCode::Busy,
                        "The pane is waiting for a password",
                    ));
                }
                let redactor = self.control_mcp_redactor(Some(address.session))?;
                if request.params.string("mode")? == Some("detection") {
                    let mut agent = self.control_agent_info(address, &request.instance);
                    let mut count = 0;
                    mask_value(&redactor, &mut agent, &mut count)?;
                    json!({"pane_id":pane_id(address.session),"mode":"detection","agent":agent,"masked_count":count})
                } else {
                    let mode = kortty_control::ReadMode::parse(
                        &request.params,
                        kortty_control::ReadMode::Visible,
                    )?;
                    let lines =
                        kortty_control::ReadMode::lines(&request.params)?.min(MAX_READ_LINES);
                    pane.session.control_read_masked(
                        pane_id(address.session),
                        mode,
                        lines,
                        request.reply.clone(),
                        move |screen, wraps, partial| {
                            mask_screen(&redactor, screen, &wraps, partial)
                        },
                    )?;
                    return Ok(Task::none());
                }
            }
            Method::Panes | Method::Tabs | Method::Agents => self.control_mcp_list(request)?,
            _ => {
                return Err(Fault::new(
                    ErrorCode::McpMethod,
                    "This method is outside the MCP allowlist",
                ));
            }
        };
        request.reply.send(Ok(result));
        Ok(Task::none())
    }

    fn control_mcp_list(&mut self, request: &Request) -> Result<Value, Fault> {
        // Exclude private entries and mask projected metadata before transmission.
        let mut result = if request.method == Method::Agents {
            self.control_agents(request)?
        } else {
            self.control_dispatch(request)?.0
        };
        let key = match request.method {
            Method::Panes => "panes",
            Method::Tabs => "tabs",
            _ => "agents",
        };
        if request.method == Method::Agents {
            // The CLI's global totals include panes outside the requested or visible scope.
            result
                .as_object_mut()
                .expect("agent result")
                .remove("totals");
        }
        let mut count = 0;
        let mut budget = MAX_READ_CHARS;
        let mut truncated = false;
        let entries = result[key].as_array_mut().expect("control list");
        let mut retained = Vec::with_capacity(entries.len().min(128));
        for mut entry in std::mem::take(entries) {
            let Some(selector) = entry["pane_id"]
                .as_str()
                .or_else(|| entry["tab_id"].as_str())
            else {
                continue;
            };
            let Ok(address) = self.control_resolve(selector, &request.instance) else {
                continue;
            };
            let tab = self.windows[&address.window].tabs[address.tab]
                .terminal()
                .expect("resolved tab");
            if tab.incognito {
                continue;
            }
            if self.sessions[&address.session]
                .session
                .frame()
                .password_prompt
            {
                continue;
            }
            let Ok(redactor) = self.control_mcp_redactor(Some(address.session)) else {
                truncated = true;
                continue;
            };
            mask_value(&redactor, &mut entry, &mut count)?;
            let size = entry.to_string().len() + 1;
            if size > budget || retained.len() >= 128 {
                truncated = true;
                break;
            }
            budget -= size;
            retained.push(entry);
        }
        *entries = retained;
        result["masked_count"] = json!(count);
        result["truncated"] = json!(truncated);
        Ok(result)
    }

    fn control_mcp_redactor(&self, session: Option<SessionId>) -> Result<Redactor, Fault> {
        let secrets = session.and_then(|id| self.sessions[&id].session.journal_redaction_secrets());
        let secrets = match secrets {
            Some(secrets) => secrets,
            // A connected SSH session can know credentials that a policy-only pass would miss.
            None if session.is_some_and(|id| self.sessions[&id].ssh.is_some()) => {
                return Err(mask_failure());
            }
            None => Secrets::default().seal().map_err(|_| mask_failure())?,
        };
        Ok(Redactor::for_journal(
            secrets,
            self.clipboard.policy.terminal_logging.clone(),
        ))
    }

    fn control_mcp_resolve(&self, request: &Request) -> Result<Address, Fault> {
        let address = self.control_resolve(request.params.required("pane")?, &request.instance)?;
        if self.windows[&address.window].tabs[address.tab]
            .terminal()
            .expect("resolved tab")
            .incognito
        {
            return Err(Fault::new(
                ErrorCode::PaneNotFound,
                "Pane is not available to MCP",
            ));
        }
        Ok(address)
    }

    fn control_mcp_state(&self, address: Address) -> Result<Arc<Frame>, Fault> {
        let window = &self.windows[&address.window];
        let tab = window.tabs[address.tab].terminal().expect("resolved tab");
        let pane = &self.sessions[&address.session];
        let frame = pane.session.frame();
        let reason = if frame.status != Status::Running
            || frame.ibm.is_some()
            || frame.serial.is_some()
            || matches!(
                frame.screen.profile,
                kortty_terminal::TerminalProfile::Hp2392
                    | kortty_terminal::TerminalProfile::Hp70092
                    | kortty_terminal::TerminalProfile::Petscii
            ) {
            "pane_state_unknown"
        } else if window.paste_request.is_some()
            || window.pending_paste.is_some()
            || pane.session.mcp_pacing()
        {
            "paste_pacing"
        } else if frame.screen.alternate_screen || window.fullscreen() {
            "alternate_screen"
        } else if frame.password_prompt {
            "password_prompt"
        } else if self.tab_broadcast_active(tab) {
            "broadcast"
        } else if self.multi_exec_member(address.session) {
            "multi_exec"
        } else if frame.coding_agent_visible()
            || frame.coding_agent().is_some()
            || self.terminal_agents.owns_session(address.session)
        {
            "coding_agent"
        } else {
            self.control_allow_mutation(address.window)?;
            if self.exiting
                || window.pending_snippet_run.is_some()
                || self.owner_dialog(address.window).is_some()
                || self.palette.is_open(address.window)
                || self.reopen_busy(address.window)
            {
                return Err(Fault::new(
                    ErrorCode::Busy,
                    "The window is waiting for user input",
                ));
            }
            return Ok(frame);
        };
        Err(blocked(reason))
    }

    fn control_mcp_prepare(&mut self, request: &Request) -> Result<Task<Desktop>, Fault> {
        self.control_mcp_tick();
        if self.control.consent.active() {
            return Err(Fault::new(
                ErrorCode::Busy,
                "Another MCP write is awaiting review",
            ));
        }
        let (peer, name) = request.reply.mcp_client().expect("MCP reply");
        let address = self.control_mcp_resolve(request)?;
        let frame = self.control_mcp_state(address)?;
        let pane = &self.sessions[&address.session];
        let identity = if let Some(ssh) = &pane.ssh {
            McpIdentity::Ssh {
                user: ssh.username().into(),
                host: ssh.host().into(),
            }
        } else if pane.session.local_shell_pid().is_some() {
            McpIdentity::Local
        } else {
            return Err(blocked("pane_state_unknown"));
        };
        let payload = input::prepare(&frame, request.method, &request.params)?;
        let plain = request.method == Method::SendText
            && !payload.submit
            && !payload.bracketed
            && request.params.string("text")?.is_some_and(|text| {
                text.chars().all(|ch| !hidden(ch)) && text.as_bytes() == payload.bytes
            });
        let submits =
            payload.submit || payload.bytes.contains(&b'\r') || payload.bytes.contains(&b'\n');
        let written = payload.result(address.session);
        let preview = visible(&payload.bytes);
        let binding = Grant {
            peer: peer.into(),
            session: address.session,
            stream: pane.stream_generation,
        };
        let tab = self.windows[&address.window].tabs[address.tab]
            .terminal()
            .expect("resolved tab");
        let title = self.tab_names(tab).0;
        let route = pane.ssh.as_ref().map_or_else(
            || "Local shell".into(),
            |ssh| format!("{}@{}", ssh.username(), ssh.host()),
        );
        let label = format!(
            "{} · {} · {}",
            visible(title.as_bytes()),
            visible(route.as_bytes()),
            pane_id(address.session)
        );
        let ticket = self.control.consent.next.wrapping_add(1);
        self.control.consent.next = ticket;
        let input = McpInput::new(
            payload.bytes,
            identity,
            frame.search_revision,
            pane.session.mcp_input_revision(),
            submits,
            !plain,
            request.reply.clone(),
        );
        let checked = pane
            .session
            .mcp_preflight(input.guard())
            .map_err(input_failure)?;
        self.control.consent.pending = Some(Pending {
            ticket,
            request: request.clone(),
            owner: address.window,
            tab: tab.id,
            grant: binding,
            input,
            written,
            preview,
            label,
            client: display_client(name),
            offers_session: plain,
            submits,
            ready: false,
        });
        Ok(Task::future(async move {
            let result = checked.recv().await.unwrap_or(Err("pane_state_unknown"));
            Desktop::Control(Message::McpChecked(ticket, result))
        }))
    }

    pub(super) fn control_mcp_checked(
        &mut self,
        ticket: u64,
        result: Result<(), &'static str>,
    ) -> Task<Desktop> {
        let Some(mut pending) = self
            .control
            .consent
            .pending
            .take_if(|pending| pending.ticket == ticket)
        else {
            return Task::none();
        };
        if !pending.request.reply.active() {
            return Task::none();
        }
        if let Err(reason) = result {
            pending.request.reply.send(Err(blocked(reason)));
            return Task::none();
        }
        let current = self
            .control_mcp_resolve(&pending.request)
            .and_then(|address| {
                if address.window != pending.owner
                    || self.sessions[&address.session].stream_generation != pending.grant.stream
                    || self.windows[&address.window].tabs[address.tab]
                        .terminal()
                        .is_none_or(|tab| tab.id != pending.tab)
                {
                    return Err(Fault::new(
                        ErrorCode::Stale,
                        "The terminal pane or connection changed",
                    ));
                }
                self.control_mcp_state(address)
            });
        if let Err(fault) = current {
            pending.request.reply.send(Err(fault));
            return Task::none();
        }
        pending.ready = true;
        let owner = pending.owner;
        let granted =
            pending.offers_session && self.control.consent.grants.contains(&pending.grant);
        self.control.consent.pending = Some(pending);
        if granted {
            return self.control_mcp_answer(owner, ticket, Decision::Once);
        }
        window::gain_focus(owner)
    }

    pub(super) fn control_mcp_answer(
        &mut self,
        owner: window::Id,
        ticket: u64,
        decision: Decision,
    ) -> Task<Desktop> {
        if !self
            .control
            .consent
            .pending
            .as_ref()
            .is_some_and(|pending| {
                pending.ticket == ticket && pending.owner == owner && pending.ready
            })
        {
            return Task::none();
        }
        let pending = self.control.consent.pending.take().expect("reviewed write");
        if matches!(decision, Decision::Deny) {
            pending.request.reply.send(Err(denied("denied")));
            return Task::none();
        }
        if !pending.request.reply.active() {
            return Task::none();
        }
        let reply = pending.request.reply.clone();
        let result = (|| {
            if self.control_mcp_mode() != Mode::Write {
                return Err(denied("settings_changed"));
            }
            let address = self.control_mcp_resolve(&pending.request)?;
            if address.window != owner
                || address.session != pending.grant.session
                || self.windows[&owner].tabs[address.tab]
                    .terminal()
                    .is_none_or(|tab| tab.id != pending.tab)
                || self.sessions[&address.session].stream_generation != pending.grant.stream
            {
                return Err(Fault::new(
                    ErrorCode::Stale,
                    "The terminal pane or connection changed",
                ));
            }
            let frame = self.control_mcp_state(address)?;
            let payload = input::prepare(&frame, pending.request.method, &pending.request.params)?;
            if !pending.input.matches(&payload.bytes, frame.search_revision) {
                return Err(Fault::new(
                    ErrorCode::Stale,
                    "The terminal changed after review",
                ));
            }
            payload.check_shortcut(
                address.session,
                pending.request.method,
                &pending.request.params,
                self.control_shortcut_aliases().as_ref(),
            )?;
            let pane = self
                .sessions
                .get_mut(&address.session)
                .expect("resolved pane");
            let guard = pending.input.guard();
            let written = pane
                .session
                .send_mcp_input(pending.input)
                .map_err(input_failure)?;
            pane.clear_selection();
            pane.session.follow_output();
            Ok((written, guard))
        })();
        match result {
            Ok((written, guard)) => {
                // Install session grants only after the worker confirms that delivery succeeded.
                let allow_session = pending.offers_session && matches!(decision, Decision::Session);
                self.control.mcp_delivery.insert(
                    ticket,
                    Delivery {
                        owner,
                        tab: pending.tab,
                        reply,
                        value: pending.written,
                        binding: Some(pending.grant),
                        allow_session,
                        guard,
                    },
                );
                Task::future(async move {
                    let result = written.recv().await.unwrap_or(Err("pane_state_unknown"));
                    Desktop::Control(Message::McpWritten(ticket, result))
                })
            }
            Err(error) => {
                reply.send(Err(error));
                Task::none()
            }
        }
    }

    pub(super) fn control_mcp_written(
        &mut self,
        ticket: u64,
        result: Result<usize, &'static str>,
    ) -> Task<Desktop> {
        let Some(mut delivery) = self.control.mcp_delivery.remove(&ticket) else {
            return Task::none();
        };
        if result.is_ok() {
            if delivery.reply.active()
                && delivery.allow_session
                && let Some(grant) = delivery.binding.take()
            {
                if self.control.consent.grants.len() >= MAX_GRANTS {
                    self.control.consent.grants.clear();
                }
                self.control.consent.grants.insert(grant);
            }
            delivery.reply.send(Ok(std::mem::take(&mut delivery.value)));
            self.control_first_mutation(delivery.owner, "An MCP client sent terminal input.")
        } else {
            delivery
                .reply
                .send(Err(blocked(result.expect_err("failed write"))));
            Task::none()
        }
    }

    pub(super) fn control_mcp_tick(&mut self) {
        if self
            .control
            .consent
            .pending
            .as_ref()
            .is_some_and(|pending| !pending.request.reply.active())
        {
            self.control.consent.pending = None;
        }
        for delivery in self.control.mcp_delivery.values() {
            if !delivery.reply.active() {
                delivery.guard.cancel();
            }
        }
    }

    pub(in crate::workspace) fn control_mcp_revalidate(&mut self) {
        if self.control.mcp_delivery.is_empty() {
            return;
        }
        let mode = self.control_mcp_mode();
        let instance = self
            .control
            .server
            .as_ref()
            .map(kortty_control::Server::instance);
        for delivery in self.control.mcp_delivery.values() {
            let valid = mode == Mode::Write
                && instance.is_some_and(|instance| {
                    let Some(binding) = &delivery.binding else {
                        return false;
                    };
                    self.control_resolve(&pane_id(binding.session), instance)
                        .is_ok_and(|address| {
                            address.window == delivery.owner
                                && self.sessions[&address.session].stream_generation
                                    == binding.stream
                                && self.windows[&address.window].tabs[address.tab]
                                    .terminal()
                                    .is_some_and(|tab| tab.id == delivery.tab && !tab.incognito)
                                && self.control_mcp_state(address).is_ok()
                        })
                });
            if !valid {
                delivery.guard.cancel();
            }
        }
    }

    pub(in crate::workspace) fn control_mcp_close_owner(&mut self, owner: window::Id) {
        self.control.consent.close_owner(owner);
        self.control
            .mcp_delivery
            .retain(|_, delivery| delivery.owner != owner);
    }

    pub(in crate::workspace) fn control_mcp_key(&mut self, owner: window::Id, deny: bool) -> bool {
        if self.control.consent.owner() != Some(owner) {
            return false;
        }
        if deny {
            self.control.consent.close_owner(owner);
        }
        true
    }

    pub(in crate::workspace) fn control_mcp_view(
        &self,
        owner: window::Id,
    ) -> Option<Element<'_, Desktop>> {
        let pending = self.control.consent.pending.as_ref().filter(|pending| {
            pending.owner == owner && pending.ready && pending.request.reply.active()
        })?;
        let answer =
            |decision| Desktop::Control(Message::McpAnswer(owner, pending.ticket, decision));
        let action = match pending.request.method {
            Method::Run => "Run a command",
            Method::SendKeys => "Send terminal keys",
            _ => "Type text",
        };
        let mut buttons = column![
            row![
                button("Deny").on_press(answer(Decision::Deny)),
                button("Allow once").on_press(answer(Decision::Once)),
            ]
            .spacing(8)
        ]
        .spacing(8);
        if pending.offers_session {
            buttons = buttons.push(
                button("Allow plain text for this pane and session")
                    .on_press(answer(Decision::Session)),
            );
        }
        Some(
            container(
                column![
                    text("MCP terminal input request").size(20),
                    text(format!("Client-reported name · {}", pending.client)),
                    text(format!("Target · {}", pending.label)),
                    text(action),
                    text(if pending.submits {
                        "This input can submit a command."
                    } else {
                        "Review the terminal input before allowing it."
                    }),
                    scrollable(
                        text(&pending.preview)
                            .font(Font::MONOSPACE)
                            .wrapping(text::Wrapping::WordOrGlyph)
                    )
                    .height(Length::Fill),
                    buttons,
                ]
                .spacing(10)
                .padding(16),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        )
    }
}

pub(super) struct Delivery {
    owner: window::Id,
    tab: u64,
    reply: kortty_control::Reply,
    value: Value,
    binding: Option<Grant>,
    allow_session: bool,
    guard: crate::session::McpGuard,
}

impl Drop for Delivery {
    fn drop(&mut self) {
        self.guard.cancel();
    }
}

fn input_failure(error: crate::session::InputError) -> Fault {
    match error {
        crate::session::InputError::Busy => {
            Fault::new(ErrorCode::Busy, "The terminal input queue is full")
        }
        crate::session::InputError::Closed => {
            Fault::new(ErrorCode::NotConnected, "The terminal pane disconnected")
        }
        crate::session::InputError::TooLarge => Fault::new(
            ErrorCode::TooLarge,
            "MCP input exceeds the terminal byte limit",
        ),
    }
}

fn blocked(reason: &'static str) -> Fault {
    Fault::new(
        ErrorCode::McpWriteBlocked,
        "The terminal state does not allow this MCP write",
    )
    .with("reason", json!(reason))
}

fn denied(reason: &'static str) -> Fault {
    Fault::new(ErrorCode::McpWriteDenied, "The MCP write was denied").with("reason", json!(reason))
}

fn hidden(ch: char) -> bool {
    ch.is_control()
        || matches!(ch,
        '\u{ad}' | '\u{34f}' | '\u{61c}' | '\u{115f}' | '\u{1160}' | '\u{17b4}' | '\u{17b5}'
        | '\u{180b}'..='\u{180f}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
        | '\u{2060}'..='\u{206f}' | '\u{3164}' | '\u{fe00}'..='\u{fe0f}' | '\u{feff}'
        | '\u{ffa0}' | '\u{fff0}'..='\u{fffb}' | '\u{1bca0}'..='\u{1bca3}'
        | '\u{1d173}'..='\u{1d17a}' | '\u{e0000}'..='\u{e0fff}')
}

fn visible(bytes: &[u8]) -> String {
    let mut display = String::new();
    for part in bytes.utf8_chunks() {
        for ch in part.valid().chars() {
            match ch {
                '\n' | '\r' => display.push_str(if ch == '\n' { "\\n\n" } else { "\\r\n" }),
                '\\' => display.push_str("\\\\"),
                ch if hidden(ch) => display.extend(ch.escape_unicode()),
                _ => display.push(ch),
            }
        }
        for byte in part.invalid() {
            use std::fmt::Write as _;
            let _ = write!(display, "\\x{byte:02x}");
        }
    }
    display
}

fn mask_failure() -> Fault {
    Fault::new(
        ErrorCode::Unavailable,
        "The terminal output could not be safely masked",
    )
}

fn mask_value(redactor: &Redactor, value: &mut Value, count: &mut usize) -> Result<(), Fault> {
    match value {
        Value::String(text) => {
            let (masked, replacements) =
                mask::prompt_counted(redactor, text).map_err(|_| mask_failure())?;
            *text = masked;
            *count += replacements;
        }
        Value::Array(values) => {
            for value in values {
                mask_value(redactor, value, count)?;
            }
        }
        Value::Object(values) => {
            for (name, value) in values {
                if !matches!(
                    name.as_str(),
                    "pane_id" | "tab_id" | "window_id" | "instance"
                ) {
                    mask_value(redactor, value, count)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn mask_screen(
    redactor: &Redactor,
    mut screen: Screen,
    wraps: &[bool],
    partial: bool,
) -> Result<Value, Fault> {
    let mut logical = zeroize::Zeroizing::new(String::new());
    let start_row = if partial {
        wraps
            .iter()
            .position(|wrap| !wrap)
            .map_or(wraps.len(), |index| index + 1)
    } else {
        0
    };
    for (index, line) in screen.lines.iter().enumerate().skip(start_row) {
        logical.push_str(line);
        if index + 1 < screen.lines.len() && !wraps[index] {
            logical.push('\n');
        }
    }
    let (masked, mut count) =
        mask::prompt_counted(redactor, &logical).map_err(|_| mask_failure())?;
    if let Some(title) = &mut screen.osc_title {
        let (masked, replacements) =
            mask::prompt_counted(redactor, title).map_err(|_| mask_failure())?;
        *title = masked.chars().take(256).collect();
        count += replacements;
    }
    let masked_lines: Vec<_> = if *logical == masked {
        screen.lines.drain(start_row..).collect()
    } else {
        masked.split('\n').map(str::to_owned).collect()
    };
    screen.truncated |= partial || masked_lines.len() > MAX_READ_LINES;
    let mut budget = MAX_READ_CHARS;
    let mut lines = Vec::new();
    for line in masked_lines.into_iter().rev().take(MAX_READ_LINES) {
        budget = budget.saturating_sub(usize::from(!lines.is_empty()));
        let width = line.chars().count();
        if width > budget || budget == 0 {
            screen.truncated = true;
            if lines.is_empty() && budget > 0 {
                let start = line
                    .char_indices()
                    .rev()
                    .nth(budget - 1)
                    .map_or(0, |(index, _)| index);
                lines.push(line[start..].to_owned());
            }
            break;
        }
        budget -= width;
        lines.push(line);
    }
    lines.reverse();
    screen.lines = lines;
    let mut value = serde_json::to_value(screen).map_err(|_| mask_failure())?;
    value["masked_count"] = json!(count);
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(lines: &[&str]) -> Screen {
        Screen {
            pane_id: "p1".into(),
            mode: "recent".into(),
            lines: lines.iter().map(|line| (*line).into()).collect(),
            columns: 12,
            rows: 24,
            alternate_screen: false,
            osc_title: Some("test".into()),
            truncated: false,
        }
    }

    #[test]
    fn exact_mcp_preview_escapes_controls_invisibles_and_non_utf8_bytes() {
        assert_eq!(visible(b"\\r\r\n\x1b\xff"), "\\\\r\\r\n\\n\n\\u{1b}\\xff");
        assert_eq!(
            visible("abc\u{202e}déf\u{34f}".as_bytes()),
            "abc\\u{202e}déf\\u{34f}"
        );
        assert!(hidden('\t') && hidden('\u{9b}') && hidden('\u{2028}'));
        assert!(!hidden('é'));
    }

    #[test]
    fn mcp_masks_known_wrapped_secrets_and_tokens_before_capping() {
        let secrets = Secrets::default();
        secrets.add("private-local-value");
        let redactor = Redactor::for_journal(secrets.seal().unwrap(), Default::default());
        let result = mask_screen(
            &redactor,
            screen(&["private-loca", "l-value", "token=takeout"]),
            &[true, false, false],
            false,
        )
        .unwrap();
        assert_eq!(result["lines"], json!(["***", "token=***"]));
        assert_eq!(result["masked_count"], 2);

        let result = mask_screen(
            &redactor,
            screen(&["l-value", "next"]),
            &[false, false],
            true,
        )
        .unwrap();
        assert_eq!(result["lines"], json!(["next"]));
        assert_eq!(result["truncated"], true);

        let huge = "é".repeat(MAX_READ_CHARS + 10);
        let result = mask_screen(&redactor, screen(&[&huge]), &[false], false).unwrap();
        assert_eq!(
            result["lines"][0].as_str().unwrap().chars().count(),
            MAX_READ_CHARS
        );
        assert_eq!(result["truncated"], true);
    }
}
