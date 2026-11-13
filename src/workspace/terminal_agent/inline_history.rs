//! Typed, masked activity for inline history and reviewed Chat follow-up.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use iced::{Task, keyboard, window};

use crate::session::{
    Frame, SessionId,
    inline_history::{Interaction, Key, Update},
};
use crate::terminal_agent::design::inline::{Kind, Prompt, State, Step, StepState, Transcript};
use crate::terminal_agent::design::{Arrangement, Status};
use crate::terminal_view::agent_cursor::{Binding, Key as CursorKey};

use super::{Desktop, Message, Phase, Run, Workspace, engine, mask};

const MAX_STEPS: usize = 24;
static NEXT: AtomicU64 = AtomicU64::new(1);

mod follow_up;
mod usage;

pub(super) struct FollowUpTrace {
    pub(super) goal: String,
    pub(super) answer: String,
    pub(super) steps: Vec<crate::workspace::terminal_ai::agent_follow_up::Step>,
}

pub(super) struct History {
    pub(super) id: u64,
    started: Instant,
    next_clock: Instant,
    transcript: Transcript,
    next_step: u64,
    model_steps: Option<(u64, [Option<u64>; 2])>,
    command_started: Option<Instant>,
}

impl History {
    fn new(goal: String) -> Self {
        let now = Instant::now();
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            started: now,
            next_clock: now + Duration::from_secs(1),
            transcript: Transcript {
                goal,
                ..Transcript::default()
            },
            next_step: 1,
            model_steps: None,
            command_started: None,
        }
    }

    fn step(&mut self, step: Step) {
        if self.transcript.steps.len() == MAX_STEPS {
            self.transcript.steps.remove(0);
        }
        self.transcript.steps.push(step);
    }

    fn event(&mut self, kind: Kind, title: String, detail: String, command: Option<String>) {
        let active = command.is_some();
        let id = self.next_step;
        self.next_step += 1;
        self.step(Step {
            id,
            kind,
            title,
            detail,
            command,
            output: Vec::new(),
            exit_code: None,
            elapsed_ms: 0,
            tokens: None,
            state: if active {
                StepState::Running
            } else {
                StepState::Completed
            },
        });
        if active {
            self.command_started = Some(Instant::now());
        }
    }

    fn result(&mut self, status: engine::Status, stdout: &str, stderr: &str) {
        let Some(started) = self.command_started.take() else {
            return;
        };
        let Some(step) = self
            .transcript
            .steps
            .iter_mut()
            .rev()
            .find(|step| step.state == StepState::Running)
        else {
            return;
        };
        step.elapsed_ms = started.elapsed().as_millis() as u64;
        step.state = match status {
            engine::Status::Exited(0) => StepState::Completed,
            engine::Status::Cancelled => StepState::Cancelled,
            _ => StepState::Failed,
        };
        if let engine::Status::Exited(code) = status {
            step.exit_code = Some(code);
        }
        // Keep the tail of bounded command output. Every string has already
        // passed the run redactor.
        step.output = stdout
            .lines()
            .chain(stderr.lines())
            .rev()
            .take(8)
            .map(|line| line.chars().take(320).collect())
            .collect();
        step.output.reverse();
        if !step.output.is_empty() {
            step.detail.push('\n');
            step.detail.push_str(&step.output.join("\n"));
        }
    }
}

impl Run {
    pub(super) fn agent_history_elapsed_ms(&self) -> Option<u64> {
        u64::try_from(self.inline.as_ref()?.started.elapsed().as_millis()).ok()
    }

    pub(super) fn initialize_inline(&mut self) {
        if self.inline.is_none()
            && !self.swarm
            && self.binding.live().is_ok()
            && !matches!(
                self.phase,
                Phase::Setup | Phase::PlanLibrary | Phase::PlanStorage
            )
            && !self.goal.trim().is_empty()
            && let Ok(goal) = mask::prompt(&self.redactor, &self.goal)
        {
            self.inline = Some(History::new(goal));
        }
    }

    pub(super) fn inline_command(&mut self, command: &str) {
        if let Some(history) = &mut self.inline {
            let title = command
                .lines()
                .next()
                .unwrap_or("Approved command")
                .chars()
                .take(120)
                .collect();
            history.event(Kind::Action, title, command.into(), Some(command.into()));
        }
    }

    pub(super) fn inline_decision(
        &mut self,
        decision: &engine::Action,
        summary: &str,
        detail: &str,
    ) {
        if let Some(history) = &mut self.inline {
            match decision {
                engine::Action::Done | engine::Action::Blocked => {
                    history.transcript.final_answer = detail.into();
                }
                engine::Action::Input { .. } => {
                    history.event(Kind::Question, summary.into(), detail.into(), None);
                }
                _ => history.event(Kind::Message, summary.into(), detail.into(), None),
            }
        }
    }

    pub(super) fn inline_result(&mut self, status: engine::Status, stdout: &str, stderr: &str) {
        if let Some(history) = &mut self.inline {
            history.result(status, stdout, stderr);
        }
    }

    pub(super) fn toggle_inline_details(&mut self) {
        if let Some(history) = &mut self.inline {
            history.transcript.expanded = !history.transcript.expanded;
        }
    }

    pub(super) fn inline_update(&self, animations: bool) -> Option<Update> {
        let history = self.inline.as_ref()?;
        let style = self.design.history.inline_style()?;
        let mut transcript = history.transcript.clone();
        transcript.tokens = self.presentation.model_usage.total();
        transcript.elapsed_ms = history.started.elapsed().as_millis() as u64;
        transcript.state = match self.phase {
            _ if self.cancel.requested() => State::Cancelled,
            Phase::Finished | Phase::PlanReady => State::Finished,
            Phase::Cancelled => State::Cancelled,
            Phase::Failed | Phase::Blocked => State::Failed,
            Phase::Paused => State::Paused,
            _ if self.paused => State::Paused,
            _ => State::Running,
        };
        if transcript.state == State::Finished
            && transcript.final_answer.is_empty()
            && let Some(plan) = &self.plan
        {
            transcript.final_answer = mask::prompt(&self.redactor, plan).unwrap_or_default();
        }
        if matches!(
            transcript.state,
            State::Finished | State::Failed | State::Cancelled
        ) && transcript.final_answer.is_empty()
        {
            transcript.final_answer =
                mask::prompt(&self.redactor, &self.notice).unwrap_or_default();
        }
        if self.phase == Phase::ModelReview
            && let Some(review) = &self.model_review
        {
            transcript.steps.push(Step {
                id: history.next_step,
                kind: Kind::Question,
                title: review.title().into(),
                detail: "Open the Activity window to review the masked request.".into(),
                state: StepState::Running,
                ..Step::default()
            });
        } else if self.phase.deciding() && !self.presentation.model_usage.repairing(self.epoch) {
            transcript.steps.push(Step {
                id: history.next_step,
                kind: Kind::Thinking,
                title: if self.phase == Phase::FinalAdvice {
                    "Preparing the final AI response"
                } else {
                    "Waiting for the AI decision"
                }
                .into(),
                detail: String::new(),
                command: None,
                output: Vec::new(),
                exit_code: None,
                elapsed_ms: 0,
                tokens: None,
                state: StepState::Running,
            });
        }
        transcript.prompt = match self.phase {
            Phase::SudoPassword => Some(Prompt::Password {
                summary: "Open the Activity window to provide the password.".into(),
            }),
            Phase::ProbeApproval | Phase::Approval => {
                self.reviewed_command().map(|(purpose, command)| {
                    if self.reviewed_password_sudo().is_some() {
                        Prompt::Password {
                            summary: "Review the sudo request in the Activity window.".into(),
                        }
                    } else {
                        let destination = self.target.as_ref().map_or_else(
                            || purpose.into(),
                            |target| format!("{purpose} · {}", target.directory()),
                        );
                        let message =
                            mask::prompt(&self.redactor, &destination).unwrap_or_else(|_| {
                                "Review the destination in the Activity window.".into()
                            });
                        let masked_command =
                            mask::prompt(&self.redactor, command).unwrap_or_else(|_| {
                                "Review the command in the Activity window.".into()
                            });
                        let keyboard = !self.reviewing_sudo()
                            && message == destination
                            && masked_command == command;
                        Prompt::Approval {
                            message,
                            commands: vec![masked_command],
                            allow_always: false,
                            keyboard,
                        }
                    }
                })
            }
            _ => None,
        };
        Some(Update {
            id: history.id,
            epoch: self.epoch,
            style,
            transcript,
            animations,
        })
    }
}

impl Workspace {
    pub(super) fn publish_agent_inline_history(&self, run: &Run) {
        let Some(update) = run.inline_update(self.window_settings.appearance.animations) else {
            return;
        };
        let Ok(source) = run.binding.live() else {
            return;
        };
        if let Some((_, pane, tab)) = self.agent_pane(source.session)
            && pane.stream_generation == source.stream
            && tab.incognito == source.private
        {
            pane.session.publish_agent_history(update);
        }
    }

    pub(super) fn tick_agent_inline_history(&mut self) {
        let now = Instant::now();
        let due: Vec<_> = self
            .terminal_agents
            .runs
            .iter_mut()
            .filter_map(|(&id, run)| {
                if !run.active()
                    || run.cancel.requested()
                    || run.design.history.inline_style().is_none()
                {
                    return None;
                }
                let history = run.inline.as_mut()?;
                if now < history.next_clock {
                    return None;
                }
                history.next_clock = now + Duration::from_secs(1);
                Some(id)
            })
            .collect();
        for id in due {
            self.publish_agent_inline_history(&self.terminal_agents.runs[&id]);
        }
    }

    pub(in crate::workspace) fn agent_inline_interaction(
        &self,
        owner: window::Id,
        session: SessionId,
        frame: &Frame,
    ) -> Option<Interaction> {
        let pane = self.agent_cursor_pane(owner, session)?;
        let view = frame.inline.as_ref()?;
        if !frame.at_live_viewport()
            || frame.password_prompt
            || frame.coding_agent_visible()
            || frame.approval_prompt_visible()
            || frame.coding_agent().is_some()
        {
            return None;
        }
        let (run_id, run) = self.terminal_agents.runs.iter().find(|(_, run)| {
            run.active()
                && !run.closing
                && !run.cancel.requested()
                && run.design.history.inline_style().is_some()
                && run.binding.live().is_ok_and(|source| {
                    source.session == session && source.stream == pane.stream_generation
                })
                && run
                    .inline
                    .as_ref()
                    .is_some_and(|history| view.visible(history.id, run.epoch))
        })?;
        let (current_owner, _) = self.current_run_pane(run).ok()?;
        if current_owner != owner {
            return None;
        }
        let target_current = match &run.target {
            Some(super::RunTarget::Attached(engine::Target::Ssh { opener, .. })) => {
                !opener.is_closed()
                    && opener.verified_host_key().is_some()
                    && pane
                        .session
                        .exec_opener()
                        .is_some_and(|current| current.same_connection(opener))
            }
            _ => true,
        };
        let unobstructed = !matches!(
            run.design.arrangement,
            Arrangement::PictureInPicture | Arrangement::Adaptive
        ) && !matches!(
            run.design.status,
            Status::TopBar | Status::Hud | Status::Spotlight | Status::Toasts | Status::Orbit
        ) && pane.effect.is_none();
        let approval = !run.paused
            && !run.reviewing_sudo()
            && target_current
            && unobstructed
            && matches!(run.phase, Phase::ProbeApproval | Phase::Approval)
            && run
                .inline
                .as_ref()
                .is_some_and(|history| view.approval_visible(history.id, run.epoch));
        Some(Interaction {
            binding: Binding {
                owner,
                session,
                stream: pane.stream_generation,
                run: *run_id,
                epoch: run.epoch,
                screen_revision: frame.search_revision,
                input_revision: pane.session.mcp_input_revision(),
            },
            approval,
        })
    }

    pub(in crate::workspace) fn agent_inline_repeated(
        &self,
        key: &keyboard::Key,
        modifiers: keyboard::Modifiers,
    ) -> bool {
        self.terminal_agents.inline_details_held
            && Key::from_event(key, modifiers, false) == Some(Key::Details)
    }

    pub(in crate::workspace) fn agent_inline_fresh(
        &mut self,
        key: &keyboard::Key,
        modifiers: keyboard::Modifiers,
    ) {
        if Key::from_event(key, modifiers, false) == Some(Key::Details) {
            self.terminal_agents.inline_details_held = false;
        }
    }

    pub(in crate::workspace) fn update_agent_inline(
        &mut self,
        binding: Binding,
        key: Key,
    ) -> Task<Desktop> {
        match key {
            Key::Details => self.terminal_agents.inline_details_held = true,
            Key::Dismiss => self.terminal_agents.cursor_keys |= CursorKey::Escape.mask(),
            Key::Approve => self.terminal_agents.cursor_keys |= CursorKey::One.mask(),
            Key::Reject => self.terminal_agents.cursor_keys |= CursorKey::Two.mask(),
        }
        let Some(frame) = self
            .sessions
            .get(&binding.session)
            .map(|pane| pane.session.frame())
        else {
            return Task::none();
        };
        let Some(interaction) = self
            .agent_inline_interaction(binding.owner, binding.session, &frame)
            .filter(|interaction| interaction.binding == binding)
        else {
            return Task::none();
        };
        let message = match key {
            Key::Details => Message::InlineDetails,
            Key::Dismiss => Message::Cancel,
            Key::Approve if interaction.approval => Message::Approve(binding.epoch),
            Key::Reject if interaction.approval => Message::Cancel,
            _ => return Task::none(),
        };
        self.update_terminal_agent(binding.run, message)
    }
}
