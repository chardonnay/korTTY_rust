//! Actions on the displayed run and its retained, masked steps.

use std::fmt;
use std::sync::Arc;

use iced::widget::pick_list;
use iced::{Element, Task, window};

use crate::clipboard_policy::ClipboardPolicy;
use crate::terminal_agent::design::inline::Step;

use super::super::{Desktop, Message, Phase, Run, Workspace, activity_export, mask, workflow};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Item {
    Workflow,
    ExportRun,
    ExportAll,
    Repeat,
    CopyAnswer,
    Chat,
    HideHistory,
    ShowHistory,
    HideStatus,
    CopyCommand,
    CopyOutput,
    CopyEntry,
    CopyDetail(u64),
    Snippet,
}

impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl Item {
    fn label(self) -> &'static str {
        match self {
            Self::Workflow => "Generate workflow script…",
            Self::ExportRun => "Export this run…",
            Self::ExportAll => "Export all finished runs…",
            Self::Repeat => "Set up this run again…",
            Self::CopyAnswer => "Copy answer",
            Self::Chat => "Continue in AI Chat",
            Self::HideHistory => "Hide Agent history",
            Self::ShowHistory => "Show Agent history",
            Self::HideStatus => "Hide Agent status",
            Self::CopyCommand => "Copy command",
            Self::CopyOutput => "Copy output",
            Self::CopyEntry => "Copy entry",
            Self::CopyDetail(_) => "Copy selected",
            Self::Snippet => "Open in Snippet Editor…",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Action {
    epoch: u64,
    step: Option<u64>,
    item: Item,
}

impl Action {
    pub(in crate::workspace) fn label(self) -> &'static str {
        self.item.label()
    }
}

fn answer(run: &Run) -> Option<&str> {
    let (trace, _) = run.status_transcript()?;
    let answer = if run.phase == Phase::PlanReady {
        run.plan.as_deref()?
    } else {
        trace.final_answer.as_str()
    };
    let answer = answer.trim();
    (!answer.is_empty()).then_some(answer)
}

fn run_items(run: &Run, has_archive: bool) -> Vec<Item> {
    let mut choices = Vec::new();
    if run.can_generate_workflow() {
        choices.push(Item::Workflow);
    }
    if run.can_export_activity(activity_export::Scope::This, has_archive) {
        choices.push(Item::ExportRun);
    }
    if run.can_export_activity(activity_export::Scope::All, has_archive) {
        choices.push(Item::ExportAll);
    }
    if run.phase.finished() && run.profile().is_some() {
        choices.push(Item::Repeat);
    }
    if (run.phase.finished() || run.phase == Phase::PlanReady) && answer(run).is_some() {
        choices.push(Item::CopyAnswer);
    }
    if run.chat_handoff_enabled && run.can_chat_handoff() {
        choices.push(Item::Chat);
    }
    if run.has_tmux_dock() {
        choices.push(if run.presentation.tmux_hidden {
            Item::ShowHistory
        } else {
            Item::HideHistory
        });
    }
    choices
}

pub(super) fn run_menu(run: &Run, has_archive: bool) -> Option<Element<'static, Message>> {
    let choices = run_actions(run, has_archive);
    (!choices.is_empty()).then(|| {
        pick_list(choices, None::<Action>, Message::DesignAction)
            .placeholder("Run actions")
            .text_size(12)
            .into()
    })
}

pub(super) fn step_menu(run: &Run, step: &Step) -> Element<'static, Message> {
    pick_list(
        step_actions(run, step),
        None::<Action>,
        Message::DesignAction,
    )
    .placeholder("Step actions")
    .text_size(12)
    .into()
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.item.fmt(f)
    }
}

pub(super) fn run_actions(run: &Run, has_archive: bool) -> Vec<Action> {
    run_items(run, has_archive)
        .into_iter()
        .map(|item| Action {
            epoch: run.epoch,
            step: None,
            item,
        })
        .collect()
}

pub(super) fn step_actions(run: &Run, step: &Step) -> Vec<Action> {
    let mut choices = Vec::new();
    if step
        .command
        .as_ref()
        .is_some_and(|command| !command.trim().is_empty())
    {
        choices.push(Item::CopyCommand);
    }
    if !step.output.is_empty() {
        choices.push(Item::CopyOutput);
    }
    choices.push(Item::CopyEntry);
    if !run.binding.private() {
        choices.push(Item::Snippet);
    }
    choices
        .into_iter()
        .map(|item| Action {
            epoch: run.epoch,
            step: Some(step.id),
            item,
        })
        .collect()
}

pub(super) fn copy_log_detail(run: &Run, revision: u64) -> Message {
    Message::DesignAction(Action {
        epoch: run.epoch,
        step: None,
        item: Item::CopyDetail(revision),
    })
}

pub(super) fn hide_status(run: &Run) -> Message {
    Message::DesignAction(Action {
        epoch: run.epoch,
        step: None,
        item: Item::HideStatus,
    })
}

impl Workspace {
    pub(in crate::workspace) fn terminal_agent_design_actions(
        &self,
        session: crate::session::SessionId,
        stream: u64,
    ) -> Vec<(window::Id, Action)> {
        let Some((id, run)) = self
            .terminal_agents
            .agent_activity_for_stream(session, stream)
        else {
            return Vec::new();
        };
        if self.current_run_pane(run).is_err() {
            return Vec::new();
        }
        run_actions(run, self.terminal_agents.has_archived_activity(run))
            .into_iter()
            .map(|action| (id, action))
            .collect()
    }

    pub(in crate::workspace::terminal_agent) fn agent_design_action(
        &mut self,
        id: window::Id,
        action: Action,
    ) -> Task<Desktop> {
        let Some(run) = self
            .terminal_agents
            .runs
            .get(&id)
            .filter(|run| !run.closing && !run.swarm && run.epoch == action.epoch)
        else {
            return Task::none();
        };
        let (Ok(source), Ok((owner, pane))) = (run.binding.live(), self.current_run_pane(run))
        else {
            return Task::none();
        };
        if self
            .terminal_agents
            .agent_activity_for_stream(source.session, source.stream)
            .is_none_or(|(current, _)| current != id)
        {
            return Task::none();
        }
        let policy = ClipboardPolicy::load();
        if super::super::policy::agent_mode(&policy, run.mode).is_err()
            || pane
                .ssh
                .as_ref()
                .is_some_and(|route| !route.workflow_route_allowed(&policy))
        {
            return Task::none();
        }
        let finished = run.phase.finished();
        match (action.step, action.item) {
            (None, Item::Workflow) if run.can_generate_workflow() => self.update_terminal_agent(
                id,
                Message::Workflow(workflow::Message::Open(action.epoch)),
            ),
            (None, Item::Chat) => {
                self.update_terminal_agent(id, Message::ContinueInChat(action.epoch))
            }
            (None, Item::HideHistory | Item::ShowHistory) if run.has_tmux_dock() => {
                if let Some(run) = self.terminal_agents.runs.get_mut(&id) {
                    run.presentation
                        .set_tmux_hidden(action.item == Item::HideHistory);
                }
                Task::none()
            }
            (None, Item::HideStatus)
                if matches!(
                    run.design.status,
                    crate::terminal_agent::design::Status::Metro
                        | crate::terminal_agent::design::Status::Neon
                ) =>
            {
                if let Some(run) = self.terminal_agents.runs.get_mut(&id) {
                    run.presentation.status_visibility.hide();
                }
                Task::none()
            }
            (None, Item::ExportRun | Item::ExportAll) if self.agent_export_pane(run).is_ok() => {
                let scope = if action.item == Item::ExportAll {
                    activity_export::Scope::All
                } else {
                    activity_export::Scope::This
                };
                self.update_terminal_agent(
                    id,
                    Message::ActivityExport(activity_export::Message::OpenScope(scope)),
                )
                .chain(self.update_terminal_agent(id, Message::Focus))
            }
            (None, Item::Repeat)
                if finished
                    && self
                        .terminal_agents
                        .check_live_run_slot(source, false)
                        .is_ok() =>
            {
                let result = (|| {
                    let profile = run
                        .profile()
                        .ok_or("The original Agent profile is unavailable.")?;
                    let mut next =
                        self.build_agent(source.session, run.mode, None, Some(profile.id), false)?;
                    if !matches!(&next.purpose, super::super::Purpose::Agent(current) if current == profile)
                    {
                        return Err(
                            "The original Agent profile changed. Choose a current profile.".into(),
                        );
                    }
                    next.profile_pending = false;
                    next.goal = mask::prompt(&next.redactor, &run.goal)?;
                    next.confirm_every_command = run.confirm_every_command;
                    Ok((next, false))
                })();
                self.open_agent_result(owner, result)
            }
            (None, Item::CopyAnswer) if finished || run.phase == Phase::PlanReady => {
                let Some(answer) = answer(run) else {
                    return Task::none();
                };
                self.copy_agent_design_text(run, &policy, answer, None)
            }
            (None, Item::CopyDetail(revision)) => {
                let Some((source, selected)) = run.log_detail_selection(revision) else {
                    return Task::none();
                };
                self.copy_agent_design_text(run, &policy, &selected, Some(source))
            }
            (
                Some(step),
                item @ (Item::CopyCommand | Item::CopyOutput | Item::CopyEntry | Item::Snippet),
            ) => {
                let Some((trace, _)) = run.status_transcript() else {
                    return Task::none();
                };
                let Some(step) = trace.steps.iter().find(|entry| entry.id == step) else {
                    return Task::none();
                };
                let text = step_text(step, item);
                if item == Item::Snippet
                    && !source.private
                    && !text.trim().is_empty()
                    && let Ok(redactor) = self.agent_source_redactor(pane)
                    && let Ok(text) = mask::prompt(&run.redactor, &text)
                        .and_then(|text| mask::prompt(&redactor, &text))
                {
                    return self.open_agent_step_snippet(owner, Arc::from(text));
                }
                if item == Item::Snippet {
                    Task::none()
                } else {
                    self.copy_agent_design_text(run, &policy, &text, None)
                }
            }
            _ => Task::none(),
        }
    }

    fn copy_agent_design_text(
        &self,
        run: &Run,
        policy: &ClipboardPolicy,
        text: &str,
        source: Option<&str>,
    ) -> Task<Desktop> {
        if text.trim().is_empty()
            || !policy.write_allowed
            || policy.internal && !self.clipboard.policy.internal
        {
            return Task::none();
        }
        self.current_run_pane(run)
            .ok()
            .and_then(|(_, pane)| self.agent_source_redactor(pane).ok())
            .and_then(|current| {
                let mask = |input| {
                    mask::prompt(&run.redactor, input)
                        .and_then(|text| mask::prompt(&current, &text))
                        .ok()
                };
                // A selection can intersect only part of a newly masked value.
                if let Some(source) = source
                    && mask(source).as_deref() != Some(source)
                {
                    return None;
                }
                mask(text)
            })
            .map_or_else(Task::none, |text| self.write_clipboard(text))
    }
}

fn step_text(step: &Step, item: Item) -> String {
    match item {
        Item::CopyCommand => step.command.clone().unwrap_or_default(),
        Item::CopyOutput => step.output.join("\n"),
        Item::Snippet
            if step
                .command
                .as_ref()
                .is_some_and(|command| !command.trim().is_empty()) =>
        {
            step.command.clone().unwrap_or_default()
        }
        _ if step.detail.trim().is_empty() || step.detail == step.title => step.title.clone(),
        _ => format!("{}\n{}", step.title, step.detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_agent::{Mode, Status};
    use crate::workspace::terminal_agent::flow::tests::Fixture;

    #[test]
    fn step_copy_is_bound_to_the_exact_step_epoch_and_stream() {
        let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
        let id = fixture.open(Mode::Run);
        let run = fixture.run_mut(id);
        run.goal = "Inspect the fixture".into();
        run.next(Phase::Thinking, "");
        run.inline_command("printf fixture");
        run.inline_result(Status::Exited(0), "fixture output", "");
        run.next(Phase::Finished, "");
        let step = &run.status_transcript().unwrap().0.steps[0];
        assert_eq!(step_text(step, Item::Snippet), "printf fixture");
        let action = Action {
            epoch: run.epoch,
            step: Some(step.id),
            item: Item::CopyOutput,
        };
        fixture.workspace.clipboard.policy.internal = true;
        drop(fixture.send(id, Message::DesignAction(action)));
        assert_eq!(
            fixture.workspace.clipboard.buffer.get().as_deref(),
            Some("fixture output")
        );
        fixture
            .workspace
            .clipboard
            .buffer
            .set("unchanged".to_owned());
        drop(fixture.send(
            id,
            Message::DesignAction(Action {
                epoch: action.epoch - 1,
                ..action
            }),
        ));
        drop(fixture.send(
            id,
            Message::DesignAction(Action {
                step: Some(999),
                ..action
            }),
        ));
        fixture
            .workspace
            .sessions
            .get_mut(&fixture.session)
            .unwrap()
            .stream_generation += 1;
        drop(fixture.send(id, Message::DesignAction(action)));
        assert_eq!(
            fixture.workspace.clipboard.buffer.get().as_deref(),
            Some("unchanged")
        );
    }
}
