//! Actions on the displayed run and its retained, masked steps.

use std::fmt;
use std::sync::Arc;

use iced::widget::pick_list;
use iced::{Element, Task, window};

use crate::clipboard_policy::ClipboardPolicy;
use crate::session::SessionId;
use crate::terminal_agent::design::inline::Step;
use crate::terminal_agent::design::{Layout, Preset};

use super::super::finished_runs::FinishedRun;
use super::super::{
    Desktop, LiveBinding, Message, Mode, PaneSession, Phase, Run, RunPresentation, Workspace,
    activity_export, mask, workflow,
};

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
    pub(super) fn label(self) -> &'static str {
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
    enabled: bool,
}

impl Action {
    pub(in crate::workspace) fn label(self) -> &'static str {
        self.item.label()
    }

    pub(in crate::workspace) fn enabled(&self) -> bool {
        self.enabled
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
    if run.can_chat_handoff() {
        choices.push(Item::Chat);
    }
    if run.has_history_dock() {
        choices.push(if run.presentation.dock_hidden {
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
            .option_enabled(Action::enabled)
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
            enabled: item != Item::Chat || run.chat_handoff_enabled && !run.chat_handoff_pending,
        })
        .collect()
}

pub(super) fn step_items(step: &Step, private: bool) -> Vec<Item> {
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
    if !private {
        choices.push(Item::Snippet);
    }
    choices
}

pub(super) fn step_actions(run: &Run, step: &Step) -> Vec<Action> {
    step_items(step, run.binding.private())
        .into_iter()
        .map(|item| Action {
            epoch: run.epoch,
            step: Some(step.id),
            item,
            enabled: true,
        })
        .collect()
}

pub(super) fn copy_log_detail(run: &Run, revision: u64) -> Message {
    Message::DesignAction(Action {
        epoch: run.epoch,
        step: None,
        item: Item::CopyDetail(revision),
        enabled: true,
    })
}

pub(super) fn hide_status(run: &Run) -> Message {
    Message::DesignAction(Action {
        epoch: run.epoch,
        step: None,
        item: Item::HideStatus,
        enabled: true,
    })
}

enum LastRun<'a> {
    Live(window::Id, &'a Run),
    Retained(&'a FinishedRun),
}

fn shows_in_terminal(layout: Layout, surface: RunPresentation, mode: Mode) -> bool {
    layout != Preset::Classic.layout() && (surface != RunPresentation::ChatTab || mode == Mode::Ask)
}

impl Workspace {
    fn last_agent_run(&self, session: SessionId, stream: u64) -> Option<LastRun<'_>> {
        let (_, pane, tab) = self.agent_pane(session)?;
        if pane.stream_generation != stream {
            return None;
        }
        let source = LiveBinding {
            session,
            stream,
            private: tab.incognito,
        };
        let live = self.terminal_agents.runs.iter().filter_map(|(&id, run)| {
            (!run.closing
                && !run.swarm
                && run.started_order != 0
                && run.binding.live().ok() == Some(source)
                && run.status_transcript().is_some()
                && shows_in_terminal(run.design, run.surface, run.mode))
            .then_some(((run.started_order, id), LastRun::Live(id, run)))
        });
        let retained = self.terminal_agents.finished_runs.iter().filter_map(|run| {
            (run.source == source && shows_in_terminal(run.design, run.surface, run.mode))
                .then_some(((run.order, run.id), LastRun::Retained(run)))
        });
        live.chain(retained)
            .max_by_key(|(order, _)| *order)
            .map(|(_, run)| run)
    }

    fn live_context_run_actions(&self, session: SessionId, run: &Run) -> Vec<Action> {
        let mut choices = run_actions(run, self.terminal_agents.has_archived_activity(run));
        if self.current_run_pane(run).is_err() {
            let same_scope = self.agent_pane(session).is_some_and(|(_, _, tab)| {
                run.binding
                    .live()
                    .is_ok_and(|source| source.private == tab.incognito)
            });
            if self.ai_chat_enabled() || !same_scope {
                return Vec::new();
            }
            // Keep the finished run's disabled Chat entry when all AI is off.
            choices.retain_mut(|action| {
                action.enabled = false;
                action.item == Item::Chat
            });
        }
        choices
    }

    #[cfg(test)]
    pub(in crate::workspace) fn terminal_agent_design_actions(
        &self,
        session: SessionId,
        stream: u64,
    ) -> Vec<(window::Id, Action)> {
        self.terminal_agents
            .agent_activity_for_stream(session, stream)
            .into_iter()
            .flat_map(|(id, run)| {
                self.live_context_run_actions(session, run)
                    .into_iter()
                    .map(move |action| (id, action))
            })
            .collect()
    }

    fn retained_context_run_actions(&self, run: &FinishedRun) -> Vec<Action> {
        let chat = run.chat_handoff_id();
        let chat_enabled = self.ai_chat_enabled();
        let chat_available = chat_enabled
            && chat.is_some_and(|id| !self.terminal_agents.pending_chat_handoffs.contains_key(id));
        let allowed = self.current_agent_pane(run.source, run.mode).is_ok();
        let can_export = run.can_export();
        let items = [
            (Item::Workflow, run.can_generate_workflow()),
            (Item::ExportRun, can_export),
            (
                Item::ExportAll,
                can_export || self.terminal_agents.activity.contains(run.source),
            ),
            (Item::Repeat, true),
            (Item::CopyAnswer, !run.answer().is_empty()),
            (Item::Chat, chat.is_some()),
        ];
        items
            .into_iter()
            .filter(|&(item, available)| {
                available && (allowed || item == Item::Chat && !chat_enabled)
            })
            .map(|(item, _)| Action {
                epoch: 0,
                step: None,
                item,
                enabled: allowed && (item != Item::Chat || chat_available),
            })
            .collect()
    }

    pub(in crate::workspace) fn last_agent_run_actions(
        &self,
        session: SessionId,
        stream: u64,
    ) -> Vec<(window::Id, Action)> {
        let (id, choices) = match self.last_agent_run(session, stream) {
            Some(LastRun::Live(id, run)) => (id, self.live_context_run_actions(session, run)),
            Some(LastRun::Retained(run)) => (run.id, self.retained_context_run_actions(run)),
            None => return Vec::new(),
        };
        choices.into_iter().map(|action| (id, action)).collect()
    }

    pub(in crate::workspace) fn invoke_last_agent_run_action(
        &mut self,
        owner: window::Id,
        session: SessionId,
        id: window::Id,
        mut action: Action,
    ) -> Task<Desktop> {
        let Some((pane_owner, pane, _)) = self.agent_pane(session) else {
            return Task::none();
        };
        let stream = pane.stream_generation;
        if owner != pane_owner
            || !action.enabled()
            || !self
                .last_agent_run_actions(session, stream)
                .contains(&(id, action))
        {
            return Task::none();
        }
        let Some(run) = self.terminal_agents.finished_runs.get(id) else {
            return self.perform_agent_run_action(id, action);
        };
        let policy = ClipboardPolicy::load();
        if !self.agent_run_action_allowed(&policy, run.mode, pane) {
            return Task::none();
        }
        if action.item == Item::CopyAnswer {
            return self.copy_retained_agent_answer(run, pane, &policy);
        }
        let (id, opened) = match self.restore_finished_agent_run(owner, session, stream, id) {
            Ok(restored) => restored,
            Err(task) => return task,
        };
        action.epoch = self.terminal_agents.runs[&id].epoch;
        opened.chain(self.perform_agent_run_action(id, action))
    }

    pub(in crate::workspace::terminal_agent) fn agent_design_action(
        &mut self,
        id: window::Id,
        action: Action,
    ) -> Task<Desktop> {
        let presented = self
            .terminal_agents
            .runs
            .get(&id)
            .and_then(|run| run.binding.live().ok())
            .and_then(|source| {
                self.terminal_agents
                    .agent_activity_for_stream(source.session, source.stream)
            })
            .is_some_and(|(current, _)| current == id);
        if !presented {
            return Task::none();
        }
        self.perform_agent_run_action(id, action)
    }

    fn perform_agent_run_action(&mut self, id: window::Id, action: Action) -> Task<Desktop> {
        if !action.enabled() {
            return Task::none();
        }
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
        let policy = ClipboardPolicy::load();
        if !self.agent_run_action_allowed(&policy, run.mode, pane) {
            return Task::none();
        }
        let finished = run.phase.finished();
        match (action.step, action.item) {
            (None, Item::Workflow) if run.can_generate_workflow() => self.update_terminal_agent(
                id,
                Message::Workflow(workflow::Message::Open(action.epoch)),
            ),
            (None, Item::Chat) if self.ai_chat_enabled() => {
                self.update_terminal_agent(id, Message::ContinueInChat(action.epoch))
            }
            (None, Item::HideHistory | Item::ShowHistory) if run.has_history_dock() => {
                if let Some(run) = self.terminal_agents.runs.get_mut(&id) {
                    run.presentation
                        .set_dock_hidden(action.item == Item::HideHistory);
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
                let prepared = self.prepare_agent_step_action(
                    step,
                    item,
                    source.private,
                    pane,
                    &policy,
                    Some(&run.redactor),
                );
                self.publish_agent_step_action(owner, prepared)
            }
            _ => Task::none(),
        }
    }

    pub(super) fn agent_run_action_allowed(
        &self,
        policy: &ClipboardPolicy,
        mode: Mode,
        pane: &PaneSession,
    ) -> bool {
        super::super::policy::agent_mode(policy, mode).is_ok()
            && pane
                .ssh
                .as_ref()
                .is_none_or(|route| route.workflow_route_allowed(policy))
    }

    fn agent_clipboard_write_allowed(&self, policy: &ClipboardPolicy) -> bool {
        policy.write_allowed && (!policy.internal || self.clipboard.policy.internal)
    }

    fn copy_retained_agent_answer(
        &self,
        run: &FinishedRun,
        pane: &PaneSession,
        policy: &ClipboardPolicy,
    ) -> Task<Desktop> {
        if !self.agent_clipboard_write_allowed(policy) {
            return Task::none();
        }
        self.agent_source_redactor(pane)
            .and_then(|redactor| mask::prompt(&redactor, run.answer()))
            .map_or_else(|_| Task::none(), |text| self.write_clipboard(text))
    }

    fn copy_agent_design_text(
        &self,
        run: &Run,
        policy: &ClipboardPolicy,
        text: &str,
        source: Option<&str>,
    ) -> Task<Desktop> {
        if text.trim().is_empty() || !self.agent_clipboard_write_allowed(policy) {
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

    pub(super) fn prepare_agent_step_action(
        &self,
        step: &Step,
        item: Item,
        private: bool,
        pane: &PaneSession,
        policy: &ClipboardPolicy,
        original: Option<&crate::terminal_logging::Redactor>,
    ) -> Option<PreparedStepAction> {
        if item == Item::Snippet {
            if private {
                return None;
            }
        } else if !self.agent_clipboard_write_allowed(policy) {
            return None;
        }
        let mut text = step_text(step, item);
        if text.trim().is_empty() {
            return None;
        }
        if let Some(original) = original {
            text = mask::prompt(original, &text).ok()?;
        }
        let redactor = self.agent_source_redactor(pane).ok()?;
        let text = mask::prompt(&redactor, &text).ok()?;
        Some(if item == Item::Snippet {
            PreparedStepAction::Snippet(text)
        } else {
            PreparedStepAction::Copy(text)
        })
    }

    pub(super) fn publish_agent_step_action(
        &mut self,
        owner: window::Id,
        prepared: Option<PreparedStepAction>,
    ) -> Task<Desktop> {
        match prepared {
            Some(PreparedStepAction::Snippet(text)) => {
                self.open_agent_step_snippet(owner, Arc::from(text))
            }
            Some(PreparedStepAction::Copy(text)) => self.write_clipboard(text),
            None => Task::none(),
        }
    }
}

pub(super) enum PreparedStepAction {
    Copy(String),
    Snippet(String),
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

    fn finish(fixture: &mut Fixture, order: u64, answer: &str) -> window::Id {
        let id = fixture.open(Mode::Run);
        let run = fixture.run_mut(id);
        run.started_order = order;
        run.goal = format!("Fixture run {order}");
        run.next(Phase::Thinking, "Working");
        run.inline_decision(
            &super::super::super::engine::Action::Done,
            "Finished",
            answer,
        );
        run.next(Phase::Finished, "Finished");
        id
    }

    fn last_copy(workspace: &Workspace, session: SessionId, stream: u64) -> (window::Id, Action) {
        workspace
            .last_agent_run_actions(session, stream)
            .into_iter()
            .find(|(_, action)| action.item == Item::CopyAnswer)
            .unwrap()
    }

    #[test]
    fn terminal_context_uses_latest_live_or_retained_run_without_changing_the_display() {
        let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
        let session = fixture.session;
        let (owner, pane, _) = fixture.workspace.agent_pane(session).unwrap();
        let stream = pane.stream_generation;
        let earlier = finish(&mut fixture, 1, "Earlier answer");
        let latest = finish(&mut fixture, 2, "Latest answer");
        let classic = finish(&mut fixture, 3, "Classic answer");
        fixture.run_mut(classic).design = Preset::Classic.layout();
        fixture
            .workspace
            .terminal_agents
            .presented_runs
            .insert(session, (stream, earlier));
        fixture.workspace.clipboard.policy.internal = true;

        let (id, initial) = last_copy(&fixture.workspace, session, stream);
        assert_eq!(id, latest);
        drop(
            fixture
                .workspace
                .invoke_last_agent_run_action(owner, session, id, initial),
        );
        assert_eq!(
            fixture.workspace.clipboard.buffer.get().as_deref(),
            Some("Latest answer")
        );
        let displayed = Action {
            epoch: fixture.run_mut(earlier).epoch,
            ..initial
        };
        drop(fixture.send(earlier, Message::DesignAction(displayed)));
        assert_eq!(
            fixture.workspace.clipboard.buffer.get().as_deref(),
            Some("Earlier answer")
        );

        drop(fixture.send(latest, Message::Close));
        let (id, retained) = last_copy(&fixture.workspace, session, stream);
        assert_eq!(id, latest);
        drop(
            fixture
                .workspace
                .invoke_last_agent_run_action(owner, session, id, retained),
        );
        assert_eq!(
            fixture.workspace.clipboard.buffer.get().as_deref(),
            Some("Latest answer")
        );
        assert!(!fixture.workspace.terminal_agents.runs.contains_key(&id));
        assert!(
            fixture
                .workspace
                .terminal_agents
                .finished_runs
                .get(id)
                .is_some()
        );
        assert_eq!(
            fixture.workspace.terminal_agents.presented_runs[&session],
            (stream, earlier)
        );
    }

    #[test]
    fn terminal_context_rejects_a_queued_action_after_a_new_trace_or_stream_change() {
        let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
        let session = fixture.session;
        let (owner, pane, _) = fixture.workspace.agent_pane(session).unwrap();
        let stream = pane.stream_generation;
        let earlier = finish(&mut fixture, 1, "Earlier answer");
        let (_, old_action) = last_copy(&fixture.workspace, session, stream);
        let next = fixture.open(Mode::Run);
        let run = fixture.run_mut(next);
        run.goal = "New trace".into();
        run.started_order = 2;
        run.next(Phase::Thinking, "Working");
        fixture.workspace.clipboard.policy.internal = true;
        fixture.workspace.clipboard.buffer.set("unchanged".into());

        drop(
            fixture
                .workspace
                .invoke_last_agent_run_action(owner, session, earlier, old_action),
        );
        assert_eq!(
            fixture.workspace.clipboard.buffer.get().as_deref(),
            Some("unchanged")
        );
        let run = fixture.run_mut(next);
        run.inline_decision(
            &super::super::super::engine::Action::Done,
            "Finished",
            "New answer",
        );
        run.next(Phase::Finished, "Finished");
        let (id, action) = last_copy(&fixture.workspace, session, stream);
        assert_eq!(id, next);
        fixture
            .workspace
            .sessions
            .get_mut(&session)
            .unwrap()
            .stream_generation += 1;
        drop(
            fixture
                .workspace
                .invoke_last_agent_run_action(owner, session, id, action),
        );
        assert!(
            fixture
                .workspace
                .last_agent_run_actions(session, stream + 1)
                .is_empty()
        );
        assert_eq!(
            fixture.workspace.clipboard.buffer.get().as_deref(),
            Some("unchanged")
        );
    }

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
            enabled: true,
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
