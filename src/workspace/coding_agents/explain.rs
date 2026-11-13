use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use iced::widget::text_editor;
use iced::{Task, window};
use jiff::Timestamp;
use zeroize::Zeroizing;

use crate::app_logging::Event;
use crate::session::{CodingAgentDetails, CodingAgentState, Status};
use crate::workspace::{Message as Desktop, PaneSession, Workspace, coding_agent_dock::Placement};

use super::{Entry, FocusTarget, PanelState, view::duration_text};

const LIFETIME: Duration = Duration::from_secs(60);

pub(super) struct Explanation {
    target: FocusTarget,
    privacy_generation: u64,
    deadline: Instant,
    content: text_editor::Content,
}

impl Explanation {
    pub(super) fn new(entry: &Entry, now: Instant) -> Option<Self> {
        let details = entry.details.as_ref()?;
        let detection = entry.input.expected.state;
        let observed = state_name(detection);
        let mut text = Zeroizing::new(format!("{} is {observed}", entry.status.name));
        if let Some(rule) = &details.rule {
            let _ = write!(text, " (rule {}", rule.id);
            if !rule.evidence.is_empty() {
                let _ = write!(text, ": \"{}\"", rule.evidence);
            }
            let _ = writeln!(text, ")\nRule: {}", rule.id);
        } else {
            text.push_str(" (no rule matched, fallback)\nRule: none (fallback state)\n");
        }
        let elapsed = now.saturating_duration_since(entry.status.since);
        let state = state_name(entry.status.state);
        let _ = write!(text, "State: {state}");
        if entry.status.state == CodingAgentState::Done && detection != CodingAgentState::Done {
            text.push_str(" (done until seen)");
        }
        let _ = write!(text, " for {}", duration_text(elapsed.as_secs()));
        if let Some(since) = SystemTime::now().checked_sub(elapsed).and_then(timestamp) {
            let _ = write!(text, ", since {since}");
        }
        let _ = write!(text, "\nProcess: pid {} {}", details.pid, details.command);
        if let Some(started) = details.started_at.and_then(timestamp) {
            let _ = write!(text, " (started {started})");
        }
        if let Some(alias) = &entry.alias {
            let _ = write!(text, "\nAlias: {alias}");
        }
        let _ = write!(text, "\nPane: {}/{}", entry.tab, entry.target.session.0);
        Some(Self {
            target: entry.target,
            privacy_generation: details.privacy_generation,
            deadline: now + LIFETIME,
            content: text_editor::Content::with_text(&text),
        })
    }

    fn is_open(&self, target: FocusTarget, privacy_generation: u64, now: Instant) -> bool {
        self.target == target
            && self.privacy_generation == privacy_generation
            && self.deadline > now
    }
}

fn timestamp(time: SystemTime) -> Option<Timestamp> {
    Timestamp::try_from(time).ok()
}

fn state_name(state: CodingAgentState) -> &'static str {
    match state {
        CodingAgentState::Blocked => "BLOCKED",
        CodingAgentState::Done => "DONE",
        CodingAgentState::Idle => "IDLE",
        CodingAgentState::Unknown => "UNKNOWN",
        CodingAgentState::Working => "WORKING",
    }
}

fn pane_details(pane: &PaneSession, target: FocusTarget) -> Option<Arc<CodingAgentDetails>> {
    let frame = pane.session.frame();
    if pane.stream_generation != target.stream_generation
        || frame.status != Status::Running
        || frame
            .coding_agent()
            .is_none_or(|status| status.instance != target.instance)
    {
        return None;
    }
    frame.coding_agent_details().cloned()
}

impl PanelState {
    pub(super) fn explanation(
        &self,
        target: FocusTarget,
        privacy_generation: u64,
        now: Instant,
    ) -> Option<&text_editor::Content> {
        self.explanations
            .get(&target.session)
            .filter(|explanation| explanation.is_open(target, privacy_generation, now))
            .map(|explanation| &explanation.content)
    }

    pub(in crate::workspace) fn clear_explanations(&mut self) {
        self.explanations.clear();
    }
}

impl Workspace {
    pub(super) fn toggle_coding_agent_explanation(
        &mut self,
        owner: window::Id,
        target: FocusTarget,
    ) {
        let Some(details) = self.visible_explanation_details(owner, target) else {
            return;
        };
        let now = Instant::now();
        let panel = &mut self
            .windows
            .get_mut(&owner)
            .expect("visible owner")
            .coding_agent_dock
            .panel;
        if panel
            .explanation(target, details.privacy_generation, now)
            .is_some()
        {
            panel.explanations.remove(&target.session);
            return;
        }
        let Some(explanation) = self
            .coding_agent_entries()
            .into_iter()
            .find(|entry| entry.target == target)
            .and_then(|mut entry| {
                entry.details = Some(details);
                Explanation::new(&entry, now)
            })
        else {
            return;
        };
        self.windows
            .get_mut(&owner)
            .expect("visible owner")
            .coding_agent_dock
            .panel
            .explanations
            .insert(target.session, explanation);
        self.app_logging.record(Event::CodingAgentExplanationOpened);
    }

    pub(super) fn select_coding_agent_explanation(
        &mut self,
        owner: window::Id,
        target: FocusTarget,
        action: text_editor::Action,
    ) {
        if action.is_edit() {
            return;
        }
        let Some(details) = self.visible_explanation_details(owner, target) else {
            return;
        };
        if let Some(explanation) = self
            .windows
            .get_mut(&owner)
            .and_then(|window| {
                window
                    .coding_agent_dock
                    .panel
                    .explanations
                    .get_mut(&target.session)
            })
            .filter(|explanation| {
                explanation.is_open(target, details.privacy_generation, Instant::now())
            })
        {
            explanation.content.perform(action);
        }
    }

    pub(super) fn copy_coding_agent_explanation(
        &self,
        owner: window::Id,
        target: FocusTarget,
    ) -> Task<Desktop> {
        let Some(details) = self.visible_explanation_details(owner, target) else {
            return Task::none();
        };
        self.windows
            .get(&owner)
            .and_then(|window| {
                window.coding_agent_dock.panel.explanation(
                    target,
                    details.privacy_generation,
                    Instant::now(),
                )
            })
            .and_then(text_editor::Content::selection)
            .map_or_else(Task::none, |value| self.write_clipboard(value))
    }

    fn visible_explanation_details(
        &self,
        owner: window::Id,
        target: FocusTarget,
    ) -> Option<Arc<CodingAgentDetails>> {
        if !self.window_settings.coding_agent_detection_enabled
            || self.coding_agent_panel_placement(owner) == Placement::Hidden
            || self.terminal_only_fullscreen_active(owner)
        {
            return None;
        }
        pane_details(self.sessions.get(&target.session)?, target)
    }

    pub(in crate::workspace) fn expire_coding_agent_panel_transients(&mut self) {
        let now = Instant::now();
        let detection_enabled = self.window_settings.coding_agent_detection_enabled;
        for window in self.windows.values_mut() {
            let panel = &mut window.coding_agent_dock.panel;
            panel.expire_notice();
            panel.explanations.retain(|session, explanation| {
                detection_enabled
                    && explanation.deadline > now
                    && self
                        .sessions
                        .get(session)
                        .and_then(|pane| pane_details(pane, explanation.target))
                        .is_some_and(|details| {
                            details.privacy_generation == explanation.privacy_generation
                        })
            });
        }
    }
}
