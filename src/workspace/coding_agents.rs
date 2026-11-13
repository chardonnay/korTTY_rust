use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use iced::{Task, window};

use crate::session::{
    CodingAgentDetails, CodingAgentKey, CodingAgentState, CodingAgentStatus, SessionId, Status,
};

use super::{Message as Desktop, Window, Workspace, control_access};

pub(super) mod aliases;
mod explain;
pub(super) mod notifications;
mod panel;
mod roster_scroll;
mod view;
pub use panel::InputTarget;
pub(super) use panel::PanelState;
pub(super) use view::{duration_text, state_label};

#[derive(Default)]
pub(super) struct PaneState {
    last_observed: Option<CodingAgentStatus>,
    pub(super) status: Option<CodingAgentStatus>,
    pub(super) alias: Option<String>,
}

impl PaneState {
    pub(super) fn observe(
        &mut self,
        observed: Option<CodingAgentStatus>,
        is_seen: impl FnOnce() -> bool,
    ) {
        use CodingAgentState::{Blocked, Done, Idle, Unknown, Working};

        let Some(mut next) = observed else {
            self.clear_detection();
            return;
        };
        if self.last_observed.is_some_and(|last| {
            (last.id, last.instance, last.state, last.since)
                == (next.id, next.instance, next.state, next.since)
        }) {
            return;
        }
        self.last_observed = observed;
        let previous = self
            .status
            .filter(|status| status.id == next.id && status.instance == next.instance);
        let seen = is_seen();
        next.state = match next.state {
            Blocked | Done | Working => next.state,
            Idle | Unknown
                if !seen && previous.is_some_and(|old| matches!(old.state, Done | Working)) =>
            {
                Done
            }
            Idle | Unknown => next.state,
        };
        if let Some(previous) = previous.filter(|previous| previous.state == next.state) {
            next.since = previous.since;
        }
        self.status = Some(next);
    }

    pub(super) fn clear_detection(&mut self) {
        self.last_observed = None;
        self.status = None;
    }

    pub(super) fn acknowledge(&mut self) {
        if let Some(status) = &mut self.status
            && status.state == CodingAgentState::Done
        {
            status.state = CodingAgentState::Idle;
            status.since = Instant::now();
        }
    }
}

pub(super) fn seen_pane(windows: &HashMap<window::Id, Window>) -> Option<SessionId> {
    windows
        .values()
        .find(|window| window.focused)
        .and_then(Window::tab)
        .and_then(super::Tab::focused_session)
}

/// An action target survives a tab transfer but never a reconnect or agent replacement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FocusTarget {
    session: SessionId,
    stream_generation: u64,
    instance: u64,
}

impl FocusTarget {
    pub(super) fn new(session: SessionId, stream_generation: u64, instance: u64) -> Self {
        Self {
            session,
            stream_generation,
            instance,
        }
    }
}

#[derive(Clone)]
pub enum PanelMessage {
    Focus(FocusTarget),
    NextBlocked,
    Close,
    QuickKey(InputTarget, CodingAgentKey),
    ChooseTarget(FocusTarget),
    EditPrompt(Option<FocusTarget>, iced::widget::text_editor::Action),
    SubmitPrompt(Option<InputTarget>, u64),
    InputWritten(u64, Result<usize, &'static str>),
    Explain(FocusTarget),
    ExplainSelect(FocusTarget, iced::widget::text_editor::Action),
    ExplainCopy(FocusTarget),
    Rename(FocusTarget),
}

impl std::fmt::Debug for PanelMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Coding Agents panel message")
    }
}

struct Entry {
    target: FocusTarget,
    status: CodingAgentStatus,
    details: Option<Arc<CodingAgentDetails>>,
    input: InputTarget,
    alias: Option<String>,
    window: u64,
    tab: u64,
    title: String,
    title_sort: String,
    pane: usize,
    directory: Option<String>,
}

impl Entry {
    fn name(&self) -> &str {
        self.alias.as_deref().unwrap_or(self.status.name)
    }

    fn location(&self) -> String {
        let title = if self.title.is_empty() {
            "Terminal"
        } else {
            &self.title
        };
        let mut location = format!("Window {} › {title} › Pane {}", self.window, self.pane);
        if let Some(directory) = &self.directory {
            location.push_str(" · ");
            location.push_str(directory);
        }
        location
    }
}

fn order(entries: &mut [Entry]) {
    let blocked = |entry: &Entry| entry.status.state == CodingAgentState::Blocked;
    entries.sort_unstable_by(|a, b| {
        blocked(b)
            .cmp(&blocked(a))
            .then_with(|| a.status.since.cmp(&b.status.since))
            .then_with(|| a.window.cmp(&b.window))
            .then_with(|| a.title_sort.cmp(&b.title_sort))
            .then_with(|| a.pane.cmp(&b.pane))
            .then_with(|| a.tab.cmp(&b.tab))
            .then_with(|| a.target.session.0.cmp(&b.target.session.0))
    });
}

fn next_target(
    entries: &[Entry],
    current: Option<SessionId>,
    fallback: bool,
) -> Option<FocusTarget> {
    let start = current
        .and_then(|id| entries.iter().position(|entry| entry.target.session == id))
        .map_or(0, |position| position + 1);
    entries[start..]
        .iter()
        .chain(&entries[..start])
        .find(|entry| entry.status.state == CodingAgentState::Blocked)
        .or_else(|| if fallback { entries.first() } else { None })
        .map(|entry| entry.target)
}

fn label(input: &str, limit: usize) -> String {
    input
        .chars()
        .filter(|&character| {
            !character.is_control() && crate::session_journal::is_display_character(character)
        })
        .take(limit)
        .collect::<String>()
        .trim()
        .to_owned()
}

impl Workspace {
    fn coding_agent_entries(&self) -> Vec<Entry> {
        let mut entries = Vec::new();
        if !self.window_settings.coding_agent_detection_enabled {
            return entries;
        }
        for window in self.windows.values() {
            for tab in window.tabs.iter().filter_map(super::TabEntry::terminal) {
                let mut title = None;
                for (index, (_, session)) in
                    control_access::ordered_panes(tab).into_iter().enumerate()
                {
                    let Some(pane) = self.sessions.get(&session) else {
                        continue;
                    };
                    let Some(status) = pane
                        .coding_agent
                        .status
                        .filter(|_| pane.frame.status == Status::Running)
                    else {
                        continue;
                    };
                    let Some(expected) = pane
                        .frame
                        .coding_agent()
                        .filter(|agent| agent.instance == status.instance)
                    else {
                        continue;
                    };
                    let title = title.get_or_insert_with(|| label(&self.tab_names(tab).0, 100));
                    let directory = pane
                        .frame
                        .shell_activity()
                        .directory
                        .as_ref()
                        .map(|directory| label(&directory.path, 120))
                        .filter(|path| !path.is_empty());
                    let target = FocusTarget {
                        session,
                        stream_generation: pane.stream_generation,
                        instance: status.instance,
                    };
                    entries.push(Entry {
                        target,
                        status,
                        details: pane.frame.coding_agent_details().cloned(),
                        input: InputTarget {
                            target,
                            expected,
                            revision: pane.frame.search_revision,
                        },
                        alias: pane.coding_agent.alias.clone(),
                        window: window.number,
                        tab: tab.id,
                        title: title.clone(),
                        title_sort: title.to_lowercase(),
                        pane: index + 1,
                        directory,
                    });
                }
            }
        }
        order(&mut entries);
        entries
    }

    pub(super) fn update_coding_agent_panel(
        &mut self,
        owner: window::Id,
        message: PanelMessage,
    ) -> Task<Desktop> {
        match message {
            PanelMessage::Focus(target) => self.focus_coding_agent(target),
            PanelMessage::NextBlocked => self.navigate_coding_agents(owner, false),
            PanelMessage::Close => self.hide_coding_agent_panel(owner),
            PanelMessage::Explain(target) => {
                self.toggle_coding_agent_explanation(owner, target);
                Task::none()
            }
            PanelMessage::ExplainSelect(target, action) => {
                self.select_coding_agent_explanation(owner, target, action);
                Task::none()
            }
            PanelMessage::ExplainCopy(target) => self.copy_coding_agent_explanation(owner, target),
            PanelMessage::Rename(target) => self.show_coding_agent_alias_dialog(owner, target),
            message => self.update_coding_agent_input(owner, message),
        }
    }

    pub(super) fn focus_next_blocked_coding_agent(&mut self, owner: window::Id) -> Task<Desktop> {
        self.navigate_coding_agents(owner, true)
    }

    pub(super) fn show_coding_agent_panel_for(
        &mut self,
        owner: window::Id,
        target: FocusTarget,
    ) -> Task<Desktop> {
        use super::coding_agent_dock::Placement;

        if !self.palette_can_open(owner)
            || self.terminal_only_fullscreen_active(owner)
            || self.windows[&owner].pane_location(target.session).is_none()
            || self
                .coding_agent_status(target.session)
                .is_none_or(|status| status.instance != target.instance)
            || self.sessions.get(&target.session).is_none_or(|pane| {
                let frame = pane.session.frame();
                pane.stream_generation != target.stream_generation
                    || frame.status != Status::Running
                    || frame
                        .coding_agent()
                        .is_none_or(|status| status.instance != target.instance)
            })
        {
            return Task::none();
        }
        let opened = if self.coding_agent_panel_placement(owner) == Placement::Hidden {
            self.toggle_coding_agent_panel(owner)
        } else {
            Task::none()
        };
        self.windows
            .get_mut(&owner)
            .expect("live dashboard owner")
            .coding_agent_dock
            .panel
            .select_roster_entry(target);
        opened.chain(roster_scroll::reveal(owner, target.session))
    }

    fn navigate_coding_agents(&mut self, owner: window::Id, fallback: bool) -> Task<Desktop> {
        let entries = self.coding_agent_entries();
        let current = seen_pane(&self.windows).or_else(|| {
            self.windows
                .get(&owner)
                .and_then(Window::tab)
                .and_then(super::Tab::focused_session)
        });
        next_target(&entries, current, fallback)
            .map_or_else(Task::none, |target| self.focus_coding_agent(target))
    }

    fn focus_coding_agent(&mut self, target: FocusTarget) -> Task<Desktop> {
        self.focus_coding_agent_with(target, window::gain_focus)
    }

    fn focus_coding_agent_with(
        &mut self,
        target: FocusTarget,
        raise: impl FnOnce(window::Id) -> Task<Desktop>,
    ) -> Task<Desktop> {
        if !self.window_settings.coding_agent_detection_enabled
            || self.sessions.get(&target.session).is_none_or(|pane| {
                pane.stream_generation != target.stream_generation
                    || pane
                        .session
                        .frame()
                        .coding_agent()
                        .is_none_or(|agent| agent.instance != target.instance)
            })
        {
            return Task::none();
        }
        let Some((owner, tab_id, pane)) = self.windows.iter().find_map(|(&owner, window)| {
            let (index, pane) = window.pane_location(target.session)?;
            Some((owner, window.tabs.get(index)?.id(), pane))
        }) else {
            return Task::none();
        };
        if let Some((dialog, focus)) = self.owner_dialog(owner) {
            return Task::batch([raise(dialog), Task::done(focus)]);
        }
        self.select_tab(owner, tab_id, false);
        if self.windows[&owner].active_tab_id() != Some(tab_id) {
            return Task::none();
        }
        let Some(tab) = self.active_tab_mut(owner) else {
            return Task::none();
        };
        if tab
            .grid
            .maximized()
            .is_some_and(|maximized| maximized != pane)
        {
            tab.grid.restore();
        }
        tab.focused = pane;
        tab.remote_files.release_keyboard();
        self.session_restore.changed(false);
        raise(owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_agent_completion_waits_until_the_pane_is_seen() {
        use CodingAgentState::{Blocked, Done, Idle, Unknown, Working};

        let start = Instant::now();
        let status = |instance, state, elapsed| {
            Some(CodingAgentStatus {
                id: "codex",
                name: "Codex",
                state,
                since: start + std::time::Duration::from_secs(elapsed),
                instance,
            })
        };
        let mut pane = PaneState::default();
        pane.observe(status(1, Working, 0), || false);
        pane.observe(status(1, Idle, 1), || false);
        assert_eq!(pane.status.unwrap().state, Done);
        pane.observe(status(1, Unknown, 2), || false);
        assert_eq!(pane.status.unwrap().state, Done);
        pane.acknowledge();
        pane.observe(status(1, Unknown, 2), || false);
        assert_eq!(pane.status.unwrap().state, Idle);

        pane.observe(status(1, Blocked, 3), || false);
        pane.observe(status(1, Idle, 4), || false);
        assert_eq!(pane.status.unwrap().state, Idle);
        pane.observe(status(1, Working, 5), || false);
        pane.observe(status(2, Idle, 6), || false);
        assert_eq!(pane.status.unwrap().state, Idle);
        pane.observe(status(2, Done, 7), || true);
        assert_eq!(pane.status.unwrap().state, Done);
        pane.acknowledge();
        assert_eq!(pane.status.unwrap().state, Idle);
        pane.alias = Some("Build agent".into());
        pane.observe(None, || false);
        assert!(pane.status.is_none());
        assert_eq!(pane.alias.as_deref(), Some("Build agent"));
    }

    #[test]
    fn coding_agent_roster_orders_oldest_waits_and_cycles_blocked_panes() {
        use CodingAgentState::{Blocked, Done, Working};

        let start = Instant::now();
        let entry = |session, state, seconds, title: &str, pane| {
            let target = FocusTarget {
                session: SessionId(session),
                stream_generation: 1,
                instance: 2,
            };
            let status = CodingAgentStatus {
                id: "codex",
                name: "Codex",
                state,
                since: start + std::time::Duration::from_secs(seconds),
                instance: 2,
            };
            Entry {
                target,
                status,
                details: None,
                input: InputTarget {
                    target,
                    expected: status,
                    revision: 1,
                },
                alias: None,
                window: 1,
                tab: 1,
                title: title.into(),
                title_sort: title.to_lowercase(),
                pane,
                directory: None,
            }
        };
        let mut rows = [
            entry(1, Working, 0, "Terminal", 1),
            entry(2, Blocked, 4, "Zebra", 1),
            entry(3, Blocked, 3, "Terminal", 2),
            entry(4, Blocked, 4, "alpha", 2),
            entry(5, Blocked, 4, "ALPHA", 1),
            entry(6, Done, 2, "Terminal", 1),
        ];
        order(&mut rows);
        assert_eq!(
            rows.each_ref().map(|entry| entry.target.session.0),
            [3, 5, 4, 2, 1, 6]
        );
        let next = |current, fallback| {
            next_target(&rows, Some(SessionId(current)), fallback).map(|target| target.session.0)
        };
        assert_eq!(next(3, false), Some(5));
        assert_eq!(next(2, false), Some(3));
        assert_eq!(next(1, false), Some(3));
        assert_eq!(next(99, true), Some(3));
        assert_eq!(next_target(&rows[4..], None, false), None);
        assert_eq!(next_target(&rows[4..], None, true), Some(rows[4].target));
        assert_eq!(next_target(&[], None, true), None);
    }
}
