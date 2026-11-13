use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use iced::Task;
use kortty_control::{AGENT_WAIT_DEFAULT_MS, ErrorCode, Fault, Method, Params, Request};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::session::{CodingAgentInput, CodingAgentState, CodingAgentStatus, SessionId, Status};
use crate::terminal_logging::Redactor;

use super::{Address, Desktop, Message, Workspace, input, pane_id, tab_id, window_id};

#[derive(Default)]
pub(super) struct AgentControl {
    writes: HashMap<u64, Write>,
    waits: Vec<Wait>,
    observed: HashMap<SessionId, Value>,
}

impl AgentControl {
    pub(super) fn needs_tick(&self) -> bool {
        !self.waits.is_empty() || !self.writes.is_empty()
    }
}

struct Write {
    request: Request,
    session: SessionId,
    generation: u64,
    expected: CodingAgentStatus,
    keys: Vec<String>,
}

struct Wait {
    request: Request,
    session: SessionId,
    generation: u64,
    agent: u64,
    previous: CodingAgentState,
    until: String,
    started: Instant,
    deadline: Instant,
    base: Value,
}

fn state(state: CodingAgentState) -> &'static str {
    match state {
        CodingAgentState::Blocked => "blocked",
        CodingAgentState::Done => "done",
        CodingAgentState::Working => "working",
        CodingAgentState::Idle => "idle",
        CodingAgentState::Unknown => "unknown",
    }
}

fn until(value: &str) -> Result<String, Fault> {
    let value = value.trim().to_ascii_lowercase();
    let value = if value.is_empty() {
        "any".to_owned()
    } else {
        value
    };
    matches!(
        value.as_str(),
        "blocked" | "done" | "idle" | "working" | "any"
    )
    .then_some(value)
    .ok_or_else(|| Fault::param("until"))
}

fn missing() -> Fault {
    Fault::new(
        ErrorCode::AgentNotFound,
        "No coding agent is registered in this pane",
    )
}

impl Workspace {
    pub(super) fn control_agent_totals(&self, tab: Option<&super::Tab>) -> Value {
        let mut totals = json!({"total":0,"blocked":0,"working":0,"done":0,"idle":0,"unknown":0,"most_urgent":null});
        if !self.window_settings.coding_agent_detection_enabled {
            return totals;
        }
        let mut urgent = None;
        for (id, pane) in &self.sessions {
            if tab.is_some_and(|tab| !tab.grid.iter().any(|(_, session)| session == id)) {
                continue;
            }
            if !self
                .windows
                .values()
                .any(|window| window.pane_location(*id).is_some())
            {
                continue;
            }
            let frame = pane.session.frame();
            let Some(status) = pane.coding_agent.status.filter(|status| {
                frame.status == Status::Running
                    && frame
                        .coding_agent()
                        .is_some_and(|raw| raw.instance == status.instance)
            }) else {
                continue;
            };
            totals["total"] = json!(totals["total"].as_u64().unwrap_or(0) + 1);
            totals[state(status.state)] =
                json!(totals[state(status.state)].as_u64().unwrap_or(0) + 1);
            if urgent.is_none_or(|current: CodingAgentStatus| {
                (status.state != CodingAgentState::Blocked, status.since)
                    < (current.state != CodingAgentState::Blocked, current.since)
            }) {
                urgent = Some(status);
            }
        }
        if let Some(status) = urgent {
            totals["most_urgent"] = json!(state(status.state));
        }
        totals
    }

    pub(super) fn control_agent_start(&mut self, request: &Request) -> Result<Value, Fault> {
        if !self.window_settings.coding_agent_detection_enabled {
            return Err(Fault::new(
                ErrorCode::Unavailable,
                "Coding agent detection is disabled",
            ));
        }
        let address = self.control_resolve(request.params.required("pane")?, &request.instance)?;
        self.control_allow_mutation(address.window)?;
        let aliases = self.control_shortcut_aliases();
        let pane = self
            .sessions
            .get_mut(&address.session)
            .ok_or_else(missing)?;
        if pane.stream_generation != request.params.number("stream_generation", u64::MAX)? {
            return Err(Fault::new(ErrorCode::Stale, "The terminal was replaced"));
        }
        let frame = pane.session.frame();
        if frame.status != Status::Running {
            return Err(Fault::new(
                ErrorCode::NotConnected,
                "The terminal is disconnected",
            ));
        }
        if pane.session.local_shell_pid().is_none()
            || frame.coding_agent().is_some()
            || frame.password_prompt
        {
            return Err(Fault::new(
                ErrorCode::Busy,
                "The pane is not an available local shell",
            ));
        }
        let parts = request
            .params
            .strings("command", 128)?
            .ok_or_else(|| Fault::param("command"))?;
        // Match Java's explicit command composition. Newlines and NUL were rejected by the transport.
        let line = parts.join(" ");
        let mut params = Params::default();
        params.insert("command", json!(line));
        input::write(
            pane,
            address.session,
            Method::Run,
            &params,
            aliases.as_ref(),
        )
    }
    fn control_agent_status(&self, address: Address) -> Result<CodingAgentStatus, Fault> {
        if !self.window_settings.coding_agent_detection_enabled {
            return Err(missing());
        }
        let pane = self.sessions.get(&address.session).ok_or_else(missing)?;
        let frame = pane.session.frame();
        let status = pane.coding_agent.status.ok_or_else(missing)?;
        if frame.status != Status::Running
            || frame
                .coding_agent()
                .is_none_or(|raw| raw.instance != status.instance)
        {
            return Err(missing());
        }
        Ok(status)
    }

    pub(super) fn control_agent_info(&self, address: Address, instance: &str) -> Value {
        let window = &self.windows[&address.window];
        let tab = window.tabs[address.tab]
            .terminal()
            .expect("resolved terminal tab");
        let pane = &self.sessions[&address.session];
        let frame = pane.session.frame();
        let status = self.control_agent_status(address).ok();
        let details = status.and(frame.coding_agent_details());
        let mask = |text: &str| -> Option<String> {
            if tab.incognito || frame.password_prompt {
                return None;
            }
            let secrets = pane.session.journal_redaction_secrets()?;
            let redactor =
                Redactor::for_journal(secrets, self.clipboard.policy.terminal_logging.clone());
            let text: String = text
                .chars()
                .filter(|&ch| crate::session_journal::is_display_character(ch))
                .take(4096)
                .collect();
            redactor
                .journal_text(&text, Instant::now() + Duration::from_millis(100))
                .map(|text| text.to_string())
        };
        let elapsed = status
            .map(|status| status.since.elapsed())
            .unwrap_or_default();
        let since = SystemTime::now()
            .checked_sub(elapsed)
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|time| time.as_millis());
        let alias = status
            .and(pane.coding_agent.alias.as_deref())
            .and_then(mask);
        json!({"pane_id":pane_id(address.session),"window_id":window_id(window),"tab_id":tab_id(instance,tab.id),
            "detected":status.is_some(),"state":status.map(|status| state(status.state)).unwrap_or("unknown"),
            "kind":status.map(|status| status.id),"display_name":alias.as_deref().or_else(||status.map(|status| status.name)),
            "alias":alias,
            "matched_rule_id":details.and_then(|details| details.rule.as_ref()).map(|rule|rule.id.as_ref()),
            "evidence":details.and_then(|details|details.rule.as_ref()).and_then(|rule|mask(&rule.evidence)),
            "pid":details.map(|details|i64::from(details.pid)).unwrap_or(-1),"command":details.and_then(|details|mask(&details.command)),
            "state_since_millis":since.unwrap_or(0),"seconds_in_state":elapsed.as_secs(),
            "done_until_seen":status.is_some_and(|status|status.state==CodingAgentState::Done && frame.coding_agent().is_some_and(|raw|raw.state!=CodingAgentState::Done)),
            "agent_instance":status.map(|status|status.instance),"stream_generation":pane.stream_generation})
    }

    pub(super) fn control_agents(&mut self, request: &Request) -> Result<Value, Fault> {
        let params = &request.params;
        if request.method == Method::Agents {
            let wanted_state = params.string("state")?;
            if wanted_state.is_some_and(|value| {
                !matches!(value, "blocked" | "done" | "working" | "idle" | "unknown")
            }) {
                return Err(Fault::param("state"));
            }
            let wanted_kind = params.string("kind")?;
            let wanted_tab = params.string("tab")?;
            let mut found_tab = wanted_tab.is_none();
            let mut agents = Vec::new();
            let totals = self.control_agent_totals(None);
            for (id, window) in self.control_windows(params.string("window")?)? {
                for (index, tab) in super::indexed_terminal_tabs(window) {
                    if request.reply.mcp_client().is_some() && tab.incognito {
                        continue;
                    }
                    if wanted_tab.is_some_and(|wanted| wanted != tab_id(&request.instance, tab.id))
                    {
                        continue;
                    }
                    found_tab = true;
                    for (pane, session) in super::ordered_panes(tab) {
                        let address = Address {
                            window: id,
                            tab: index,
                            pane,
                            session,
                        };
                        let Ok(status) = self.control_agent_status(address) else {
                            continue;
                        };
                        if wanted_state.is_none_or(|wanted| wanted == state(status.state))
                            && wanted_kind.is_none_or(|wanted| wanted == status.id)
                        {
                            agents.push(self.control_agent_info(address, &request.instance));
                        }
                    }
                }
            }
            if !found_tab {
                return Err(Fault::new(ErrorCode::TabNotFound, "Tab is not open"));
            }
            agents.sort_by(|a, b| {
                (b["state"] == "blocked")
                    .cmp(&(a["state"] == "blocked"))
                    .then_with(|| {
                        a["state_since_millis"]
                            .as_u64()
                            .cmp(&b["state_since_millis"].as_u64())
                    })
                    .then_with(|| a["pane_id"].as_str().cmp(&b["pane_id"].as_str()))
            });
            return Ok(json!({"agents":agents,"totals":totals}));
        }
        let address = self.control_resolve(params.required("pane")?, &request.instance)?;
        let status = self.control_agent_status(address)?;
        if request.method == Method::AgentRename {
            self.control_allow_mutation(address.window)?;
            let alias = params.string("alias")?.unwrap_or("").trim();
            if alias.chars().count() > 256 || alias.chars().any(char::is_control) {
                return Err(Fault::param("alias"));
            }
            self.sessions
                .get_mut(&address.session)
                .expect("resolved pane")
                .coding_agent
                .alias = (!alias.is_empty()).then(|| alias.to_owned());
            self.control_agent_updated(address.session);
        }
        let info = self.control_agent_info(address, &request.instance);
        if request.method == Method::AgentExplain {
            return Ok(
                json!({"explain":format!("{} is {}\nRule: {}\nEvidence: {}\nProcess: {} {}\nSeconds in state: {}",status.name,state(status.state),info["matched_rule_id"].as_str().unwrap_or("none (fallback)"),info["evidence"].as_str().unwrap_or("redacted"),info["pid"],info["command"].as_str().unwrap_or("redacted"),info["seconds_in_state"])}),
            );
        }
        Ok(json!({"agent":info}))
    }

    pub(super) fn control_agent_write(&mut self, request: &Request) -> Task<Desktop> {
        let result = (|| {
            if self.control.agents.writes.len() >= 8 {
                return Err(Fault::new(ErrorCode::Busy, "Too many pending agent inputs"));
            }
            if let Some(value) = request.params.string("wait_until")? {
                until(value)?;
            }
            let address =
                self.control_resolve(request.params.required("pane")?, &request.instance)?;
            self.control_allow_mutation(address.window)?;
            self.control_agent_status(address)?;
            let pane = &self.sessions[&address.session];
            let frame = pane.session.frame();
            let expected = frame.coding_agent().ok_or_else(missing)?;
            if expected.instance != request.params.number("agent_instance", expected.instance)?
                || pane.stream_generation
                    != request
                        .params
                        .number("stream_generation", pane.stream_generation)?
            {
                return Err(Fault::new(
                    ErrorCode::Stale,
                    "The agent or terminal changed",
                ));
            }
            let (input, keys) = if request.method == Method::AgentPrompt {
                let text = request.params.required("text")?;
                if text.trim().is_empty() {
                    return Err(Fault::new(ErrorCode::EmptyInput, "Nothing to send"));
                }
                if text.len() > 64 * 1024 {
                    return Err(Fault::new(
                        ErrorCode::TooLarge,
                        "The prompt exceeds the input limit",
                    ));
                }
                if expected.state == CodingAgentState::Blocked {
                    return Err(Fault::new(
                        ErrorCode::AgentBlocked,
                        "Answer the agent decision before sending a prompt",
                    ));
                }
                if let Some(aliases) = self.control_shortcut_aliases() {
                    let normalized = Zeroizing::new(crate::input::sanitize_paste(text));
                    let bracketed =
                        frame.bracketed_paste && normalized.trim_end_matches('\n').contains('\n');
                    if !bracketed && input::matching_shortcut(&aliases, &normalized).is_some() {
                        return Err(Fault::new(
                            ErrorCode::HostShortcutConflict,
                            "The first line starts with korTTY's own AI shortcut command and would not reach the agent",
                        ));
                    }
                }
                (
                    CodingAgentInput::Prompt(Zeroizing::new(text.to_owned())),
                    Vec::new(),
                )
            } else {
                let (bytes, keys, decision, submits) = input::agent_keys(&frame, &request.params)?;
                (
                    CodingAgentInput::Keys {
                        bytes: Zeroizing::new(bytes),
                        decision,
                        submits,
                    },
                    keys,
                )
            };
            let receive = pane
                .session
                .send_coding_agent_input(expected, frame.search_revision, input)
                .map_err(|_| {
                    Fault::new(
                        ErrorCode::WriteFailed,
                        "The agent input could not be queued",
                    )
                })?;
            Ok((
                receive,
                address.session,
                pane.stream_generation,
                expected,
                keys,
            ))
        })();
        let (receive, session, generation, expected, keys) = match result {
            Ok(value) => value,
            Err(error) => {
                request.reply.send(Err(error));
                return Task::none();
            }
        };
        let ticket = self.allocate();
        self.control.agents.writes.insert(
            ticket,
            Write {
                request: request.clone(),
                session,
                generation,
                expected,
                keys,
            },
        );
        Task::perform(
            async move {
                receive
                    .recv()
                    .await
                    .unwrap_or(Err("The terminal disconnected"))
            },
            move |result| Desktop::Control(Message::AgentWritten(ticket, result)),
        )
    }

    pub(super) fn control_agent_written(
        &mut self,
        ticket: u64,
        result: Result<usize, &'static str>,
    ) -> Task<Desktop> {
        let Some(write) = self.control.agents.writes.remove(&ticket) else {
            return Task::none();
        };
        let notice = if result.is_ok() {
            let owner = self
                .windows
                .iter()
                .find(|(_, window)| window.pane_location(write.session).is_some())
                .map(|(&id, _)| id);
            owner.map_or_else(Task::none, |owner| {
                self.control_first_mutation(
                    owner,
                    "A local Control API client sent coding-agent input.",
                )
            })
        } else {
            Task::none()
        };
        let result = (|| {
            let pane = self.sessions.get(&write.session).ok_or_else(missing)?;
            if pane.stream_generation != write.generation
                || pane
                    .session
                    .frame()
                    .coding_agent()
                    .is_none_or(|agent| agent.instance != write.expected.instance)
            {
                return Err(Fault::new(
                    ErrorCode::Stale,
                    "The agent or terminal changed",
                ));
            }
            let count = result.map_err(|message| Fault::new(ErrorCode::WriteFailed, message))?;
            Ok(
                json!({"pane_id":pane_id(write.session),"bytes_written":count,"submitted":write.request.method==Method::AgentPrompt || write.keys.iter().any(|key|key=="enter"),"keys":write.keys}),
            )
        })();
        match result {
            Ok(base)
                if write
                    .request
                    .params
                    .string("wait_until")
                    .ok()
                    .flatten()
                    .is_some() =>
            {
                let mut stable = write.request.clone();
                stable.params.insert("pane", json!(pane_id(write.session)));
                stable
                    .params
                    .insert("agent_instance", json!(write.expected.instance));
                stable
                    .params
                    .insert("stream_generation", json!(write.generation));
                if let Err(error) = self.control_agent_wait(&stable, Some(base)) {
                    write.request.reply.send(Err(error));
                }
            }
            result => write.request.reply.send(result),
        }
        notice
    }

    pub(super) fn control_agent_wait(
        &mut self,
        request: &Request,
        base: Option<Value>,
    ) -> Result<(), Fault> {
        if self.control.agents.waits.len() >= 8 {
            return Err(Fault::new(ErrorCode::Busy, "Too many agent waits"));
        }
        let wanted = until(request.params.required(if base.is_some() {
            "wait_until"
        } else {
            "until"
        })?)?;
        let address = self.control_resolve(request.params.required("pane")?, &request.instance)?;
        let status = self.control_agent_status(address)?;
        let generation = self.sessions[&address.session].stream_generation;
        if status.instance != request.params.number("agent_instance", status.instance)?
            || generation != request.params.number("stream_generation", generation)?
        {
            return Err(Fault::new(
                ErrorCode::Stale,
                "The agent or terminal changed",
            ));
        }
        let started = Instant::now();
        let deadline = started
            + Duration::from_millis(
                request
                    .params
                    .number("timeout_ms", AGENT_WAIT_DEFAULT_MS)?
                    .clamp(1, 600_000),
            );
        self.control.agents.waits.push(Wait {
            request: request.clone(),
            session: address.session,
            generation,
            agent: status.instance,
            previous: status.state,
            until: wanted,
            started,
            deadline,
            base: base.unwrap_or_else(|| json!({})),
        });
        self.control_agent_tick();
        Ok(())
    }

    pub(super) fn control_agent_tick(&mut self) {
        let waits = std::mem::take(&mut self.control.agents.waits);
        for mut wait in waits {
            if !wait.request.reply.active() {
                continue;
            }
            let result = (|| {
                if !self.clipboard.policy.control_allowed() {
                    return Err(Fault::new(
                        ErrorCode::Policy,
                        "Managed policy blocks local control",
                    ));
                }
                let address =
                    self.control_resolve(&pane_id(wait.session), &wait.request.instance)?;
                let status = self.control_agent_status(address)?;
                if self.sessions[&wait.session].stream_generation != wait.generation
                    || status.instance != wait.agent
                {
                    return Err(Fault::new(
                        ErrorCode::Stale,
                        "The agent or terminal changed",
                    ));
                }
                if wait.until == state(status.state)
                    || wait.until == "any" && status.state != wait.previous
                {
                    wait.base["state"] = json!(state(status.state));
                    wait.base["pane_id"] = json!(pane_id(wait.session));
                    wait.base["previous_state"] = json!(state(wait.previous));
                    wait.base["waited_millis"] = json!(wait.started.elapsed().as_millis());
                    wait.base["agent"] = self.control_agent_info(address, &wait.request.instance);
                    return Ok(true);
                }
                if Instant::now() >= wait.deadline {
                    return Err(Fault::new(
                        ErrorCode::Timeout,
                        "Timed out waiting for the coding agent",
                    ));
                }
                Ok(false)
            })();
            match result {
                Ok(true) => wait.request.reply.send(Ok(wait.base)),
                Ok(false) => self.control.agents.waits.push(wait),
                Err(error) => wait.request.reply.send(Err(error)),
            }
        }
    }

    pub(in crate::workspace) fn control_agent_updated(&mut self, session: SessionId) {
        let Some(server) = self.control.server.as_ref() else {
            return;
        };
        if !self.clipboard.policy.control_allowed() {
            return;
        }
        let events = server.events();
        // Event payloads are bounded and never retain raw terminal evidence.
        let instance = server.instance().to_owned();
        let instance = instance.as_str();
        let next = self
            .control_resolve(&pane_id(session), instance)
            .ok()
            .filter(|address| self.control_agent_status(*address).is_ok())
            .map(|address| self.control_agent_info(address, instance));
        let previous = self.control.agents.observed.remove(&session);
        let public = |next: &Value| {
            let mut public = next.clone();
            public["evidence"] = Value::Null;
            public
        };
        match (previous, next) {
            (None, Some(next)) => {
                events.publish(
                    "agent.added",
                    json!({"pane_id":pane_id(session),"agent":public(&next)}),
                );
                self.control.agents.observed.insert(session, next);
            }
            (Some(previous), Some(next)) => {
                let kind = if previous["agent_instance"] != next["agent_instance"] {
                    events.publish("agent.removed", removed_event(session, &previous));
                    Some("agent.added")
                } else if previous["alias"] != next["alias"] {
                    Some("agent.alias_changed")
                } else if previous["state"] != next["state"] {
                    Some(if previous["state"] == "done" && next["state"] == "idle" {
                        "agent.seen"
                    } else {
                        "agent.state_changed"
                    })
                } else {
                    None
                };
                if let Some(kind) = kind {
                    events.publish(kind,json!({"pane_id":pane_id(session),"previous_state":previous["state"],"agent":public(&next)}));
                }
                if previous["evidence"] != next["evidence"]
                    || previous["matched_rule_id"] != next["matched_rule_id"]
                {
                    events.publish("agent.evidence",json!({"pane_id":pane_id(session),"previous_state":previous["state"],"agent":next}));
                }
                self.control.agents.observed.insert(session, next);
            }
            (Some(previous), None) => {
                events.publish("agent.removed", removed_event(session, &previous))
            }
            _ => {}
        }
        self.control_agent_tick();
    }

    pub(in crate::workspace) fn control_agent_removed(&mut self, session: SessionId) {
        if let Some(previous) = self.control.agents.observed.remove(&session)
            && let Some(server) = self.control.server.as_ref()
        {
            server
                .events()
                .publish("agent.removed", removed_event(session, &previous));
        }
        self.control_agent_tick();
    }
}

fn removed_event(session: SessionId, previous: &Value) -> Value {
    json!({"pane_id":pane_id(session),"tab_id":previous["tab_id"],"window_id":previous["window_id"],"previous_state":previous["state"]})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_accept_only_the_documented_states() {
        for value in ["blocked", "done", "idle", "working", "any"] {
            assert_eq!(until(value).unwrap(), value);
        }
        assert_eq!(until(" DONE ").unwrap(), "done");
        assert_eq!(until("").unwrap(), "any");
        for value in ["unknown", "removed"] {
            assert!(until(value).is_err());
        }
    }

    #[test]
    fn empty_workspace_agent_totals_keep_the_complete_shape() {
        let workspace = Workspace::default();
        let totals = workspace.control_agent_totals(None);
        for key in ["total", "blocked", "working", "done", "idle", "unknown"] {
            assert_eq!(totals[key], 0);
        }
        assert!(totals["most_urgent"].is_null());
    }
}
