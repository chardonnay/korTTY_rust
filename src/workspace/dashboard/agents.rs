use std::sync::Arc;
use std::time::{Instant, SystemTime};

use jiff::{Timestamp, tz::TimeZone};

use crate::session::{CodingAgentDetails, CodingAgentState, CodingAgentStatus, SessionId, Status};

use super::super::{Workspace, coding_agents};
use super::Action;

#[derive(Clone)]
pub(super) struct Agent {
    pub(super) target: coding_agents::FocusTarget,
    pub(super) state: CodingAgentState,
    since: Instant,
    session: SessionId,
    id: &'static str,
    name: &'static str,
    alias: Option<String>,
    raw: CodingAgentStatus,
    revision: u64,
    details: Option<Arc<CodingAgentDetails>>,
}

impl Agent {
    pub(super) fn actions(&self) -> impl Iterator<Item = Action> + use<> {
        let keys = (self.state == CodingAgentState::Blocked
            && self.raw.state == CodingAgentState::Blocked)
            .then(|| {
                let target = coding_agents::InputTarget::new(self.target, self.raw, self.revision);
                [
                    Action::SendAgentEnter(target),
                    Action::SendAgentEscape(target),
                    Action::InterruptAgent(target),
                ]
            });
        std::iter::once(Action::ShowCodingAgent(self.target)).chain(keys.into_iter().flatten())
    }

    pub(super) fn urgency(&self) -> (u8, Instant, u64) {
        use CodingAgentState::{Blocked, Done, Idle, Unknown, Working};

        let rank = match self.state {
            Blocked => 0,
            Done => 1,
            Working => 2,
            Idle => 3,
            Unknown => 4,
        };
        (rank, self.since, self.session.0)
    }

    pub(super) fn chip(&self, now: Instant) -> String {
        let short_name = match self.id {
            "antigravity" => "agy",
            "claude-code" => "claude",
            "cursor-agent" => "cursor",
            "gemini-cli" => "gemini",
            "minimax-code" => "mcode",
            "qwen-code" => "qwen",
            id => id,
        };
        let name = self.alias.as_deref().unwrap_or(short_name);
        let mut label = format!("{} ", glyph(self.state));
        let mut characters = name.chars();
        label.extend(characters.by_ref().take(22));
        if characters.next().is_some() {
            label.push('…');
        }
        if timed(self.state) {
            label.push(' ');
            label.push_str(&coding_agents::duration_text(
                now.saturating_duration_since(self.since).as_secs(),
            ));
        }
        label
    }

    pub(super) fn tooltip(&self) -> String {
        let name = self.alias.as_deref().unwrap_or(self.name);
        let state = coding_agents::state_label(self.state);
        let now = SystemTime::now();
        let since = now.checked_sub(self.since.elapsed()).unwrap_or(now);
        let mut tooltip = format!("{name} · {state}");
        if let Ok(since) = Timestamp::try_from(since) {
            let since = since.to_zoned(TimeZone::system());
            tooltip.push_str(&format!(" since {}", since.strftime("%H:%M")));
        }
        if let Some(details) = &self.details {
            let state = format!("{:?}", self.raw.state).to_uppercase();
            let name = self.name;
            if let Some(rule) = &details.rule {
                tooltip.push_str(&format!("\n{name} is {state} (rule {}", rule.id));
                if !rule.evidence.is_empty() {
                    tooltip.push_str(&format!(": \"{}\"", rule.evidence));
                }
                tooltip.push(')');
            } else {
                tooltip.push_str(&format!("\n{name} is {state} (no rule matched, fallback)"));
            }
        }
        tooltip
    }
}

fn glyph(state: CodingAgentState) -> &'static str {
    match state {
        CodingAgentState::Blocked => "✋",
        CodingAgentState::Working => "⚡",
        CodingAgentState::Done => "✓",
        CodingAgentState::Idle | CodingAgentState::Unknown => "·",
    }
}

pub(super) fn timed(state: CodingAgentState) -> bool {
    matches!(
        state,
        CodingAgentState::Blocked | CodingAgentState::Working | CodingAgentState::Done
    )
}

#[derive(Clone, Copy, Default)]
pub(super) struct Rollup {
    blocked: usize,
    working: usize,
    done: usize,
}

impl Rollup {
    pub(super) fn count(&mut self, state: CodingAgentState) {
        match state {
            CodingAgentState::Blocked => self.blocked += 1,
            CodingAgentState::Working => self.working += 1,
            CodingAgentState::Done => self.done += 1,
            CodingAgentState::Idle | CodingAgentState::Unknown => {}
        }
    }

    pub(super) fn merge(&mut self, other: Self) {
        self.blocked += other.blocked;
        self.working += other.working;
        self.done += other.done;
    }

    pub(super) fn chip(self) -> Option<(CodingAgentState, String)> {
        let mut groups = [
            (CodingAgentState::Blocked, self.blocked),
            (CodingAgentState::Working, self.working),
            (CodingAgentState::Done, self.done),
        ]
        .into_iter()
        .filter(|&(_, count)| count > 0);
        let (first, count) = groups.next()?;
        let mut label = format!("{} {count}", glyph(first));
        let mut state = first;
        for (next, count) in groups {
            label.push_str(&format!(" · {} {count}", glyph(next)));
            if state == CodingAgentState::Working && next == CodingAgentState::Done {
                state = next;
            }
        }
        Some((state, label))
    }
}

impl Workspace {
    pub(in crate::workspace) fn dashboard_agent_clock_needed(&self) -> bool {
        self.windows.iter().any(|(&owner, window)| {
            window.dashboard.visible()
                && !self.terminal_only_fullscreen_active(owner)
                && window.terminal_tabs().any(|tab| {
                    tab.grid.iter().any(|(_, session)| {
                        self.coding_agent_status(*session)
                            .is_some_and(|agent| timed(agent.state))
                    })
                })
        })
    }

    pub(super) fn dashboard_agent(&self, session: SessionId) -> Option<Agent> {
        let status = self.coding_agent_status(session)?;
        let pane = self.sessions.get(&session)?;
        let frame = pane.session.frame();
        if frame.status != Status::Running {
            return None;
        }
        let raw = frame
            .coding_agent()
            .filter(|raw| raw.instance == status.instance)?;
        Some(Agent {
            target: coding_agents::FocusTarget::new(
                session,
                pane.stream_generation,
                status.instance,
            ),
            state: status.state,
            since: status.since,
            session,
            id: status.id,
            name: status.name,
            alias: pane.coding_agent.alias.clone(),
            raw,
            revision: frame.search_revision,
            details: frame.coding_agent_details().cloned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_keys_follow_show_for_the_exact_blocked_frame() {
        let session = SessionId(4);
        let target = coding_agents::FocusTarget::new(session, 7, 12);
        let since = Instant::now();
        let status = CodingAgentStatus {
            id: "gemini-cli",
            name: "Gemini CLI",
            state: CodingAgentState::Blocked,
            since,
            instance: 12,
        };
        let mut agent = Agent {
            target,
            state: status.state,
            since,
            session,
            id: status.id,
            name: status.name,
            alias: None,
            raw: status,
            revision: 9,
            details: None,
        };
        let input = coding_agents::InputTarget::new(target, status, 9);
        assert_eq!(
            agent.actions().collect::<Vec<_>>(),
            [
                Action::ShowCodingAgent(target),
                Action::SendAgentEnter(input),
                Action::SendAgentEscape(input),
                Action::InterruptAgent(input),
            ]
        );
        let context = super::super::Context {
            target: super::super::Target::tab(2),
            selected: 0,
            input: Some(input),
        };
        assert!(context.accepts(Action::SendAgentEnter(input)));
        assert!(context.accepts(Action::ShowCodingAgent(target)));
        let later = coding_agents::InputTarget::new(target, status, 10);
        assert!(!context.accepts(Action::SendAgentEnter(later)));
        let other = coding_agents::FocusTarget::new(SessionId(5), 8, 12);
        let replacement = coding_agents::InputTarget::new(other, status, 9);
        assert!(!context.accepts(Action::SendAgentEnter(replacement)));
        agent.raw.state = CodingAgentState::Working;
        assert_eq!(
            agent.actions().collect::<Vec<_>>(),
            [Action::ShowCodingAgent(target)]
        );
        agent.raw = status;
        agent.state = CodingAgentState::Working;
        assert_eq!(
            agent.actions().collect::<Vec<_>>(),
            [Action::ShowCodingAgent(target)]
        );
    }
}
