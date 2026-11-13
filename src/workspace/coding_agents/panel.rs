use std::collections::HashMap;
use std::time::{Duration, Instant};

use iced::widget::text_editor;
use iced::{Task, window};
use zeroize::Zeroizing;

use crate::session::{
    CodingAgentInput, CodingAgentKey, CodingAgentState, CodingAgentStatus, InputError, SessionId,
    Status,
};
use crate::workspace::{Message as Desktop, Workspace, coding_agent_dock::Placement};

use super::{Entry, FocusTarget, PanelMessage, explain::Explanation};

const MAX_PROMPT_BYTES: usize = 64 * 1024;
const NOTICE_LIFETIME: Duration = Duration::from_secs(5);

/// A rendered action is bound to the raw agent state and terminal evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputTarget {
    pub(super) target: FocusTarget,
    pub(super) expected: CodingAgentStatus,
    pub(super) revision: u64,
}

impl InputTarget {
    pub(in crate::workspace) fn new(
        target: FocusTarget,
        expected: CodingAgentStatus,
        revision: u64,
    ) -> Self {
        Self {
            target,
            expected,
            revision,
        }
    }
}

#[derive(Default)]
pub(in crate::workspace) struct PanelState {
    pub(super) prompt: text_editor::Content,
    pub(super) revision: u64,
    selection: Option<FocusTarget>,
    pub(super) roster_selection: Option<FocusTarget>,
    pending: Option<Pending>,
    notice: Option<(Instant, String)>,
    pub(super) explanations: HashMap<SessionId, Explanation>,
}

struct Pending {
    ticket: u64,
    name: String,
    destination: Destination,
}

#[derive(Clone, Copy)]
enum Destination {
    Dock { prompt_revision: Option<u64> },
    Dashboard,
}

impl PanelState {
    pub(in crate::workspace) fn needs_tick(&self) -> bool {
        self.notice.is_some() || !self.explanations.is_empty()
    }

    pub(in crate::workspace) fn expire_notice(&mut self) {
        if self
            .notice
            .as_ref()
            .is_some_and(|(until, _)| Instant::now() >= *until)
        {
            self.notice = None;
        }
    }

    pub(super) fn notice(&self) -> Option<&str> {
        self.notice.as_ref().map(|(_, message)| message.as_str())
    }

    pub(super) fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn has_text(&self) -> bool {
        self.prompt.lines().any(|line| !line.text.trim().is_empty())
    }

    pub(super) fn select_roster_entry(&mut self, target: FocusTarget) {
        self.roster_selection = Some(target);
        if !self.has_text() && self.selection != Some(target) {
            self.selection = Some(target);
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub(super) fn selected<'a>(&self, entries: &'a [Entry]) -> Option<&'a Entry> {
        self.selection
            .and_then(|target| entries.iter().find(|entry| entry.target == target))
            .or_else(|| {
                // Match Java's default choice while the draft is empty. A draft
                // already typed for a removed agent needs an explicit new target.
                if self.has_text() {
                    None
                } else {
                    entries.first()
                }
            })
    }

    fn edit(&mut self, target: Option<FocusTarget>, action: text_editor::Action) {
        if !action.is_edit() {
            self.prompt.perform(action);
            return;
        }
        if matches!(&action, text_editor::Action::Edit(text_editor::Edit::Paste(value)) if value.len() > MAX_PROMPT_BYTES)
        {
            self.announce("The prompt exceeds the terminal input limit.");
            return;
        }
        let previous = Zeroizing::new(self.prompt.text());
        self.prompt.perform(action);
        let next = Zeroizing::new(self.prompt.text());
        if next.len() > MAX_PROMPT_BYTES {
            self.prompt = text_editor::Content::with_text(&previous);
            self.announce("The prompt exceeds the terminal input limit.");
        } else if *previous != *next {
            if previous.trim().is_empty() {
                self.selection = target;
            }
            self.revision = self.revision.wrapping_add(1);
        }
    }

    fn announce(&mut self, message: impl Into<String>) {
        self.notice = Some((Instant::now() + NOTICE_LIFETIME, message.into()));
    }
}

impl Workspace {
    pub(super) fn update_coding_agent_input(
        &mut self,
        owner: window::Id,
        message: PanelMessage,
    ) -> Task<Desktop> {
        if let PanelMessage::InputWritten(ticket, result) = message {
            self.finish_agent_input(owner, ticket, result);
            return Task::none();
        }
        if self.coding_agent_panel_placement(owner) == Placement::Hidden {
            return Task::none();
        }
        match message {
            PanelMessage::ChooseTarget(target) => {
                if self
                    .coding_agent_entries()
                    .iter()
                    .any(|entry| entry.target == target)
                    && let Some(panel) = self.agent_panel_mut(owner)
                {
                    panel.selection = Some(target);
                    panel.roster_selection = Some(target);
                    panel.revision = panel.revision.wrapping_add(1);
                }
            }
            PanelMessage::EditPrompt(target, action) => {
                if let Some(panel) = self.agent_panel_mut(owner) {
                    panel.edit(target, action);
                }
            }
            PanelMessage::SubmitPrompt(Some(target), revision) => {
                let Some(panel) = self.agent_panel_mut(owner) else {
                    return Task::none();
                };
                if panel.busy() || !panel.has_text() {
                    return Task::none();
                }
                if panel.revision != revision || panel.selection != Some(target.target) {
                    panel.announce("The prompt or target changed. Choose Send again.");
                    return Task::none();
                }
                if target.expected.state == CodingAgentState::Blocked {
                    panel.announce("Answer the agent's decision before sending a prompt.");
                    return Task::none();
                }
                let prompt = Zeroizing::new(panel.prompt.text());
                return self.send_agent_input(
                    owner,
                    target,
                    CodingAgentInput::Prompt(prompt),
                    Destination::Dock {
                        prompt_revision: Some(revision),
                    },
                );
            }
            PanelMessage::QuickKey(target, key) => {
                return self.send_agent_input(
                    owner,
                    target,
                    CodingAgentInput::Key(key),
                    Destination::Dock {
                        prompt_revision: None,
                    },
                );
            }
            _ => {}
        }
        Task::none()
    }

    fn agent_panel_mut(&mut self, owner: window::Id) -> Option<&mut PanelState> {
        Some(&mut self.windows.get_mut(&owner)?.coding_agent_dock.panel)
    }

    pub(in crate::workspace) fn send_dashboard_coding_agent_key(
        &mut self,
        owner: window::Id,
        target: InputTarget,
        key: CodingAgentKey,
    ) -> Task<Desktop> {
        if target.expected.state != CodingAgentState::Blocked
            || !matches!(
                key,
                CodingAgentKey::Enter | CodingAgentKey::Escape | CodingAgentKey::Interrupt
            )
            || self.terminal_only_fullscreen_active(owner)
            || !self.palette_can_open(owner)
            || !self.windows.get(&owner).is_some_and(|window| {
                window.dashboard.visible() && window.pane_location(target.target.session).is_some()
            })
        {
            return Task::none();
        }
        self.send_agent_input(
            owner,
            target,
            CodingAgentInput::Key(key),
            Destination::Dashboard,
        )
    }

    fn send_agent_input(
        &mut self,
        owner: window::Id,
        target: InputTarget,
        input: CodingAgentInput,
        destination: Destination,
    ) -> Task<Desktop> {
        if self.agent_panel_mut(owner).is_none_or(|panel| panel.busy()) {
            if matches!(destination, Destination::Dashboard) {
                self.notice(owner, "Another coding agent input is still pending.".into());
            }
            return Task::none();
        }
        let result = self
            .sessions
            .get(&target.target.session)
            .filter(|pane| {
                let frame = pane.session.frame();
                self.window_settings.coding_agent_detection_enabled
                    && pane.stream_generation == target.target.stream_generation
                    && frame.status == Status::Running
                    && frame.coding_agent() == Some(target.expected)
            })
            .ok_or("The coding agent or terminal changed. Choose the action again.")
            .and_then(|pane| {
                let receive = pane
                    .session
                    .send_coding_agent_input(target.expected, target.revision, input)
                    .map_err(input_error)?;
                let name = pane
                    .coding_agent
                    .alias
                    .as_deref()
                    .unwrap_or(target.expected.name);
                Ok((receive, name.to_owned()))
            });
        match result {
            Ok((receive, name)) => {
                let ticket = self.allocate();
                let Some(panel) = self.agent_panel_mut(owner) else {
                    return Task::none();
                };
                panel.pending = Some(Pending {
                    ticket,
                    name,
                    destination,
                });
                Task::perform(
                    async move {
                        receive
                            .recv()
                            .await
                            .unwrap_or(Err("The terminal pane disconnected."))
                    },
                    move |result| {
                        Desktop::CodingAgentPanel(owner, PanelMessage::InputWritten(ticket, result))
                    },
                )
            }
            Err(error) => {
                self.agent_input_error(owner, destination, error);
                Task::none()
            }
        }
    }

    fn finish_agent_input(
        &mut self,
        owner: window::Id,
        ticket: u64,
        result: Result<usize, &'static str>,
    ) {
        let Some(panel) = self.agent_panel_mut(owner) else {
            return;
        };
        if !panel
            .pending
            .as_ref()
            .is_some_and(|pending| pending.ticket == ticket)
        {
            return;
        }
        let pending = panel.pending.take().expect("matching input ticket");
        match result {
            Ok(bytes) => {
                let prompt = match pending.destination {
                    Destination::Dock { prompt_revision } => {
                        if prompt_revision == Some(panel.revision) {
                            panel.prompt = text_editor::Content::new();
                            panel.revision = panel.revision.wrapping_add(1);
                        }
                        panel.announce(format!("Sent to {}", pending.name));
                        prompt_revision.is_some()
                    }
                    Destination::Dashboard => false,
                };
                self.app_logging
                    .record(crate::app_logging::Event::CodingAgentInputSent { prompt, bytes });
            }
            Err(error) => self.agent_input_error(owner, pending.destination, error),
        }
    }

    fn agent_input_error(
        &mut self,
        owner: window::Id,
        destination: Destination,
        error: &'static str,
    ) {
        match destination {
            Destination::Dashboard => self.notice(owner, error.into()),
            Destination::Dock { .. } => {
                if let Some(panel) = self.agent_panel_mut(owner) {
                    panel.announce(error);
                }
            }
        }
    }
}

fn input_error(error: InputError) -> &'static str {
    match error {
        InputError::Busy => "The terminal input queue is busy.",
        InputError::Closed => "The terminal pane disconnected.",
        InputError::TooLarge => "The prompt exceeds the terminal input limit.",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::session::SessionId;

    use super::*;

    fn entry(session: u64) -> Entry {
        let target = FocusTarget {
            session: SessionId(session),
            stream_generation: 1,
            instance: 1,
        };
        let status = CodingAgentStatus {
            id: "gemini-cli",
            name: "Gemini CLI",
            state: CodingAgentState::Idle,
            since: Instant::now(),
            instance: 1,
        };
        Entry {
            target,
            status,
            input: InputTarget {
                target,
                expected: status,
                revision: 1,
            },
            details: None,
            alias: None,
            window: 1,
            tab: session,
            title: format!("Tab {session}"),
            title_sort: format!("tab {session}"),
            pane: 1,
            directory: None,
        }
    }

    #[test]
    fn coding_agent_explanation_is_scoped_to_one_agent_and_expires() {
        use crate::session::CodingAgentDetails;

        let mut entry = entry(3);
        entry.alias = Some("Build Agent".into());
        entry.details = Some(Arc::new(CodingAgentDetails {
            rule: None,
            pid: 42,
            command: "gemini".into(),
            started_at: None,
            privacy_generation: 0,
        }));
        let now = Instant::now();
        let mut panel = PanelState::default();
        panel
            .explanations
            .insert(entry.target.session, Explanation::new(&entry, now).unwrap());
        let content = panel.explanation(entry.target, 0, now).unwrap().text();
        assert!(content.contains("Rule: none (fallback state)"));
        assert!(content.contains("Process: pid 42 gemini"));
        assert!(content.contains("Alias: Build Agent\nPane: 3/3"));
        let replacement = FocusTarget {
            instance: 2,
            ..entry.target
        };
        assert!(panel.explanation(replacement, 0, now).is_none());
        assert!(panel.explanation(entry.target, 1, now).is_none());
        assert!(
            panel
                .explanation(entry.target, 0, now + Duration::from_secs(60))
                .is_none()
        );
        panel.clear_explanations();
        assert!(panel.explanation(entry.target, 0, now).is_none());
    }

    #[test]
    fn coding_agent_prompt_draft_never_falls_back_to_a_different_agent() {
        let first = entry(1);
        let second = entry(2);
        let mut panel = PanelState::default();
        assert_eq!(
            panel.selected(&[first]).unwrap().target.session,
            SessionId(1)
        );
        let target = entry(1).target;
        let edit = PanelMessage::EditPrompt(
            Some(target),
            text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new("Private draft".into()))),
        );
        assert!(!format!("{edit:?}").contains("Private draft"));
        if let PanelMessage::EditPrompt(target, action) = edit {
            panel.edit(target, action);
        }
        assert!(panel.has_text());
        assert_eq!(panel.revision, 1);
        assert!(panel.selected(std::slice::from_ref(&second)).is_none());
        panel.selection = Some(second.target);
        assert_eq!(
            panel.selected(&[second]).unwrap().target.session,
            SessionId(2)
        );
    }
}
