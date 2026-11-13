//! The cursor approval belongs to the currently visible pane, run and review.

use iced::{Task, window};
use kortty_terminal::{TerminalProfile, TextEncoding};

use crate::session::{Frame, PromptState, SessionId, Status as SessionStatus, agent_takeover};
use crate::terminal_agent::design::{Approval, Arrangement, Status};
use crate::terminal_view::agent_cursor::{Binding, Key, Message, Review};

use super::{Cancel, Desktop, PaneSession, Phase, Purpose, Run, RunTarget, Target, Workspace};

pub(super) struct Pending {
    binding: Binding,
    permit: Cancel,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.permit.request();
    }
}

impl Run {
    fn cursor_command(&self) -> Option<&str> {
        if self.design.approval != Approval::Cursor
            || !matches!(self.purpose, Purpose::Agent(_))
            || self.paused
            || self.cancel.requested()
            || self.reviewing_sudo()
        {
            return None;
        }
        self.reviewed_command().map(|(_, command)| command)
    }
}

impl Workspace {
    pub(super) fn agent_cursor_pane(
        &self,
        owner: window::Id,
        session: SessionId,
    ) -> Option<&PaneSession> {
        let window = self.windows.get(&owner)?;
        let tab = window.tab()?;
        let pane = self.sessions.get(&session)?;
        (window.focused
            && !self.exiting
            && !self.tab_drag.active()
            && self.terminal_context.is_none()
            && tab.focused_session() == Some(session)
            && self.window_contains_visible_pane(owner, session)
            && !self.tab_broadcast_active(tab)
            && !self.multi_exec_member(session)
            && !pane.control_ssh_provisional
            && pane.search.is_none()
            && pane.agent_shortcut.is_none()
            && window.paste_request.is_none()
            && self.quick_select_labels(owner, session).is_none()
            && !self.palette.is_open(owner)
            && self.palette_can_open(owner))
        .then_some(pane)
    }

    pub(in crate::workspace) fn agent_cursor_review<'a>(
        &'a self,
        owner: window::Id,
        session: SessionId,
        frame: &Frame,
    ) -> Option<Review<'a>> {
        let pane = self.agent_cursor_pane(owner, session)?;
        let (id, run) = self
            .terminal_agents
            .agent_activity_for_stream(session, pane.stream_generation)?;
        let command = run.cursor_command()?;
        let (current_owner, _) = self.current_run_pane(run).ok()?;
        if current_owner != owner
            || frame.status != SessionStatus::Running
            || !frame.at_live_viewport()
            || frame.screen.alternate_screen
            || frame.password_prompt
            || frame.coding_agent_visible()
            || frame.coding_agent().is_some()
            || frame.approval_prompt_visible()
        {
            return None;
        }
        let target = run.target.as_ref()?;
        if let RunTarget::Attached(Target::Ssh { opener, .. }) = target
            && (opener.is_closed()
                || opener.verified_host_key().is_none()
                || pane
                    .session
                    .exec_opener()
                    .is_none_or(|current| !current.same_connection(opener)))
        {
            return None;
        }
        // Floating history, effects and graphics can cover the reviewed command.
        // In these combinations the Activity window keeps the approval visible.
        let inline = !matches!(
            run.design.arrangement,
            Arrangement::PictureInPicture | Arrangement::Adaptive
        ) && !matches!(
            run.design.status,
            Status::TopBar | Status::Hud | Status::Spotlight | Status::Toasts | Status::Orbit
        ) && pane.effect.is_none()
            && frame.screen.images.is_empty()
            && frame.typeahead.is_none()
            && frame.shell_activity().code_prompt.state != PromptState::Busy;
        let kind = if run.phase == Phase::ProbeApproval {
            "Agent waiting for probe approval"
        } else {
            "Agent waiting for approval"
        };
        let destination = format!("{kind} · {}", target.directory());
        let prompt = (run.phase == Phase::Approval
            && frame.bracketed_paste
            && frame.typeahead.is_none()
            && frame.screen.profile == TerminalProfile::Xterm
            && frame.screen.text_encoding() == TextEncoding::Utf8
            && !pane.session.mcp_pacing()
            && crate::terminal_agent::valid_target_label(command))
        .then(|| pane.session.agent_shortcut_prompt(frame))
        .flatten();
        Some(Review {
            binding: Binding {
                owner,
                session,
                stream: pane.stream_generation,
                run: id,
                epoch: run.epoch,
                screen_revision: frame.search_revision,
                input_revision: pane.session.mcp_input_revision(),
            },
            command,
            destination,
            inline,
            editable: prompt.is_some(),
            prompt,
        })
    }

    pub(in crate::workspace) fn agent_cursor_repeated(&self, key: &iced::keyboard::Key) -> bool {
        Key::from_key(key).is_some_and(|key| self.terminal_agents.cursor_keys & key.mask() != 0)
    }

    pub(in crate::workspace) fn agent_cursor_fresh(&mut self, key: &iced::keyboard::Key) {
        if let Some(key) = Key::from_key(key) {
            self.terminal_agents.cursor_keys &= !key.mask();
        }
    }

    pub(in crate::workspace) fn update_agent_cursor(&mut self, message: Message) -> Task<Desktop> {
        let (binding, key) = match message {
            Message::Edited(binding, result) => {
                if self
                    .terminal_agents
                    .cursor_takeover
                    .as_ref()
                    .is_some_and(|pending| pending.binding == binding)
                {
                    self.terminal_agents.cursor_takeover = None;
                    if let Err(error) = result {
                        self.notice(binding.owner, error.into());
                    }
                }
                return Task::none();
            }
            Message::Key(binding, key) => (binding, key),
        };
        // Even a review that went stale after drawing consumed its physical key.
        self.terminal_agents.cursor_keys |= key.mask();
        let Some(frame) = self
            .sessions
            .get(&binding.session)
            .map(|pane| pane.session.frame())
        else {
            return Task::none();
        };
        let Some(review) = self
            .agent_cursor_review(binding.owner, binding.session, &frame)
            .filter(|review| review.binding == binding)
        else {
            return Task::none();
        };
        if !review.inline {
            return Task::none();
        }
        match key {
            Key::Enter | Key::One => {
                self.update_terminal_agent(binding.run, super::Message::Approve(binding.epoch))
            }
            Key::Escape | Key::Two => {
                self.update_terminal_agent(binding.run, super::Message::Cancel)
            }
            Key::Tab => {
                let command = review.command.to_owned();
                if let Some(prompt) = review.prompt {
                    self.edit_agent_cursor(binding, prompt, command)
                } else {
                    self.notice(
                        binding.owner,
                        "Tab requires a verified empty shell prompt with bracketed paste.".into(),
                    );
                    Task::none()
                }
            }
        }
    }

    fn edit_agent_cursor(
        &mut self,
        binding: Binding,
        prompt: crate::session::ShortcutPrompt,
        command: String,
    ) -> Task<Desktop> {
        let run = &self.terminal_agents.runs[&binding.run];
        let pane = &self.sessions[&binding.session];
        let destination = match &run.target {
            Some(RunTarget::Attached(Target::Local { directory, .. })) => {
                agent_takeover::Destination::Local(directory.clone())
            }
            Some(RunTarget::Attached(Target::Ssh { directory, opener })) => {
                let Some(ssh) = &pane.ssh else {
                    return Task::none();
                };
                agent_takeover::Destination::Ssh {
                    opener: opener.clone(),
                    user: ssh.username().into(),
                    host: ssh.host().into(),
                    directory: directory.clone(),
                }
            }
            _ => return Task::none(),
        };
        let permit = Cancel::default();
        let Ok(request) = agent_takeover::Request::new(
            destination,
            prompt,
            binding.screen_revision,
            binding.input_revision,
            permit.clone(),
            command,
        ) else {
            return Task::none();
        };
        let cancelled = self.update_terminal_agent(binding.run, super::Message::Cancel);
        self.terminal_agents.cursor_takeover = Some(Pending { binding, permit });
        let response = self
            .sessions
            .get(&binding.session)
            .and_then(|pane| pane.session.take_over_agent_command(request).ok());
        let edit = Task::perform(
            async move {
                let response = response.ok_or("The terminal input queue is unavailable.")?;
                response
                    .recv()
                    .await
                    .map_err(|_| "The terminal closed before the command could be inserted.")?
                    .map(|_| ())
            },
            move |result| Desktop::AgentCursor(Message::Edited(binding, result)),
        );
        Task::batch([cancelled, edit])
    }

    pub(in crate::workspace) fn revalidate_agent_cursor(&mut self) {
        if let Some(pending) = &self.terminal_agents.cursor_takeover {
            let binding = pending.binding;
            let current = self
                .agent_cursor_pane(binding.owner, binding.session)
                .is_some_and(|pane| pane.stream_generation == binding.stream)
                && self
                    .terminal_agents
                    .runs
                    .get(&binding.run)
                    .is_some_and(|run| {
                        run.phase == Phase::Cancelled
                            && run.epoch == binding.epoch
                            && self.current_run_pane(run).is_ok()
                    });
            if !current {
                self.terminal_agents.cursor_takeover = None;
            }
        }
    }
}
