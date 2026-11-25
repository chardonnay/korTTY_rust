//! Bounded masked history for completed Activity windows.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use iced::{Task, window};

use super::{
    AgentLayout, Binding, Desktop, LiveBinding, MAX_RUNS, Mode, Phase, ProfileChoice, Purpose, Run,
    RunPresentation, RunSetup, Workspace, activity_export, design, final_evidence, font,
    inline_history, mask,
};
use crate::session::SessionId;

const PER_PANE: usize = 5;
const TOTAL: usize = 40;

pub(super) struct FinishedRun {
    pub(super) id: window::Id,
    pub(super) source: LiveBinding,
    pub(super) mode: Mode,
    pub(super) order: u64,
    phase: Phase,
    pub(super) surface: RunPresentation,
    label: String,
    goal: String,
    plan: Option<String>,
    profile: ProfileChoice,
    pub(super) design: AgentLayout,
    fonts: font::Sizes,
    history: inline_history::History,
    activity: VecDeque<String>,
    diagnostics: super::diagnostics::State,
    activity_steps: HashMap<usize, u64>,
    export: activity_export::Export,
    usage: design::Usage,
    sent_context: Option<super::sent_context::Snapshot>,
    final_evidence: final_evidence::Evidence,
    chat_handoff_id: Option<String>,
    chat_handoff_elapsed_ms: Option<u64>,
    finished: Instant,
    confirm_every_command: bool,
}

#[derive(Default)]
pub(super) struct Archive(Vec<FinishedRun>);

impl Archive {
    fn record(&mut self, id: window::Id, source: LiveBinding, run: &mut Run) {
        if run.swarm || source.private || !run.phase.finished() || run.started_order == 0 {
            return;
        }
        let (Some(profile), Some(finished)) =
            (run.profile().cloned(), run.presentation.finish_time())
        else {
            return;
        };
        let Some(mut history) = run.inline.take() else {
            return;
        };
        let Ok(redactor) = run.finished_redactor() else {
            return;
        };
        if history.finish_snapshot(&redactor).is_err() {
            return;
        }
        let Ok(label) = mask::prompt(&redactor, &run.label) else {
            return;
        };
        let Ok(activity) = run
            .activity
            .iter()
            .map(|entry| mask::prompt(&redactor, entry))
            .collect()
        else {
            return;
        };
        let Ok(diagnostics) = run.diagnostics.snapshot(&redactor) else {
            return;
        };
        self.0.push(FinishedRun {
            id,
            source,
            mode: run.mode,
            order: run.started_order,
            phase: run.phase,
            surface: run.surface,
            label,
            goal: history.status_transcript().goal.clone(),
            plan: run
                .plan
                .as_ref()
                .and_then(|plan| mask::prompt(&redactor, plan).ok()),
            profile,
            design: run.design,
            fonts: std::mem::take(&mut run.fonts),
            history,
            activity,
            diagnostics,
            activity_steps: run
                .activity_steps
                .iter()
                .map(|(&identity, &step)| (identity - run.presentation.identity(0), step))
                .collect(),
            export: run.activity_export.completed_snapshot(),
            usage: std::mem::take(&mut run.presentation.model_usage),
            sent_context: run
                .presentation
                .sent_context
                .take()
                .and_then(|snapshot| snapshot.remask(&redactor)),
            final_evidence: run
                .final_evidence
                .completed_snapshot(&redactor)
                .unwrap_or_default(),
            chat_handoff_id: run.chat_handoff_id.take(),
            chat_handoff_elapsed_ms: run.chat_handoff_elapsed_ms,
            finished,
            confirm_every_command: run.confirm_every_command,
        });
        self.0
            .sort_unstable_by_key(|entry| std::cmp::Reverse(entry.order));
        let mut count = 0;
        self.0.retain(|entry| {
            if entry.source == source {
                count += 1;
                count <= PER_PANE
            } else {
                true
            }
        });
        self.0.truncate(TOTAL);
    }

    pub(super) fn get(&self, id: window::Id) -> Option<&FinishedRun> {
        self.0.iter().find(|entry| entry.id == id)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &FinishedRun> {
        self.0.iter()
    }

    pub(super) fn remove_session(&mut self, session: SessionId) {
        self.0.retain(|entry| entry.source.session != session);
    }
}

impl Run {
    pub(super) fn finished_redactor(&self) -> Result<super::Redactor, String> {
        if let Some(learned) = &self.learned_sudo {
            self.redactor
                .with_overlay(learned)
                .map_err(|error| error.to_string())
        } else {
            Ok(self.redactor.clone())
        }
    }
}

impl FinishedRun {
    pub(super) fn goal(&self) -> &str {
        &self.goal
    }

    pub(super) fn answer(&self) -> &str {
        self.history.status_transcript().final_answer.trim()
    }

    pub(super) fn transcript(&self) -> &crate::terminal_agent::design::inline::Transcript {
        self.history.status_transcript()
    }

    pub(super) fn can_export(&self) -> bool {
        self.export.has_snapshot()
    }

    pub(super) fn can_generate_workflow(&self) -> bool {
        self.mode == Mode::Run && !self.final_evidence.is_empty()
    }

    pub(super) fn chat_handoff_id(&self) -> Option<&str> {
        self.chat_handoff_id.as_deref()
    }
}

impl Workspace {
    pub(super) fn retain_finished_agent_run(&mut self, id: window::Id, run: &mut Run) {
        if !self.exiting
            && let Ok(source) = run.binding.live()
            && self.current_agent_pane(source, run.mode).is_ok()
        {
            self.terminal_agents.finished_runs.record(id, source, run);
        }
    }

    fn finished_run_scope_current(&self, source: LiveBinding) -> bool {
        self.agent_pane(source.session)
            .is_some_and(|(_, pane, tab)| {
                pane.stream_generation == source.stream && tab.incognito == source.private
            })
    }

    pub(in crate::workspace) fn prune_finished_agent_runs(&mut self) -> Task<Desktop> {
        if !self.terminal_agents.finished_runs.0.is_empty() {
            let mut archive = std::mem::take(&mut self.terminal_agents.finished_runs);
            archive
                .0
                .retain(|entry| self.finished_run_scope_current(entry.source));
            self.terminal_agents.finished_runs = archive;
        }
        let expired: Vec<_> = self
            .terminal_agents
            .runs
            .iter()
            .filter_map(|(&id, run)| {
                (run.reopened_history
                    && !run.closing
                    && run
                        .binding
                        .live()
                        .is_ok_and(|source| !self.finished_run_scope_current(source)))
                .then_some(id)
            })
            .collect();
        Task::batch(
            expired
                .into_iter()
                .filter_map(|id| self.close_terminal_agent(id)),
        )
    }

    pub(in crate::workspace) fn reopen_finished_agent_run(
        &mut self,
        owner: window::Id,
        session: SessionId,
        stream: u64,
        id: window::Id,
    ) -> Task<Desktop> {
        let (window, opened) = match self.restore_finished_agent_run(owner, session, stream, id) {
            Ok(restored) => restored,
            Err(task) => return task,
        };
        self.terminal_agents
            .presented_runs
            .insert(session, (stream, window));
        Task::batch([opened, self.reconcile_agent_pip()])
    }

    pub(super) fn restore_finished_agent_run(
        &mut self,
        owner: window::Id,
        session: SessionId,
        stream: u64,
        id: window::Id,
    ) -> Result<(window::Id, Task<Desktop>), Task<Desktop>> {
        let archive = &self.terminal_agents.finished_runs;
        let Some(index) = archive.0.iter().position(|entry| {
            entry.id == id && entry.source.session == session && entry.source.stream == stream
        }) else {
            return Err(Task::none());
        };
        let entry = &archive.0[index];
        let Ok((pane_owner, pane)) = self.current_agent_pane(entry.source, entry.mode) else {
            return Err(Task::none());
        };
        if pane_owner != owner || self.exiting {
            return Err(Task::none());
        }
        if self.terminal_agents.runs.len() >= MAX_RUNS {
            return Err(self.open_agent_result(
                owner,
                Err("Close an agent activity window before reopening history.".into()),
            ));
        }
        let redactor = match self.agent_source_redactor(pane) {
            Ok(redactor) => redactor,
            Err(error) => return Err(self.open_agent_result(owner, Err(error))),
        };
        let entry = self.terminal_agents.finished_runs.0.remove(index);
        let mut run = Run::from_setup(RunSetup {
            binding: Binding::Live(entry.source),
            mode: entry.mode,
            label: entry.label,
            context: String::new(),
            manual_directory: None,
            profiles: Vec::new(),
            purpose: Purpose::Agent(entry.profile),
            swarm: false,
            design: entry.design,
            redactor,
        });
        run.phase = entry.phase;
        run.surface = entry.surface;
        run.reopened_history = true;
        run.started_order = entry.order;
        run.goal = entry.goal;
        run.plan = entry.plan;
        run.inline = Some(entry.history);
        run.activity = entry.activity;
        run.diagnostics = entry.diagnostics;
        run.activity_steps = entry.activity_steps;
        run.activity_export = entry.export;
        run.fonts = entry.fonts;
        run.confirm_every_command = entry.confirm_every_command;
        run.presentation.model_usage = entry.usage;
        run.presentation.sent_context = entry.sent_context;
        run.final_evidence = entry.final_evidence;
        run.chat_handoff_id = entry.chat_handoff_id;
        run.chat_handoff_enabled = self.ai_chat_enabled();
        run.chat_handoff_elapsed_ms = entry.chat_handoff_elapsed_ms;
        run.presentation.load_activity(&self.window_settings);
        run.presentation.reveal_finished(entry.finished);
        let (window, opened) = self.open_agent_surface(owner, &run);
        self.terminal_agents.runs.insert(window, run);
        Ok((window, opened))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Message, flow::tests::Fixture};
    use super::*;
    use crate::terminal_logging::Secrets;

    fn finish(fixture: &mut Fixture, order: u64, goal: &str) -> window::Id {
        let id = fixture.open(Mode::Run);
        let run = fixture.run_mut(id);
        run.started_order = order;
        run.goal = goal.into();
        run.next(Phase::Thinking, "");
        run.inline_command("printf status");
        run.inline_result(
            super::super::engine::Status::Exited(0),
            "service active",
            "",
        );
        run.next(Phase::Finished, "Complete");
        id
    }

    #[test]
    fn closed_run_reopens_with_masked_typed_history_and_a_fresh_window_identity() {
        let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
        let session = fixture.session;
        let stream = fixture.workspace.sessions[&session].stream_generation;
        let owner = fixture.workspace.agent_pane(session).unwrap().0;
        let first = finish(&mut fixture, 1, "Inspect service");
        let run = fixture.run_mut(first);
        run.context = "Transient context".into();
        let marker = "test orchard";
        let secrets = Secrets::default();
        secrets.add(marker);
        run.learned_sudo = Some(secrets.seal().unwrap());
        for _ in 0..super::super::MAX_ACTIVITY {
            run.record("Fixture note".into());
        }
        for _ in 0..30 {
            run.inline_decision(
                &super::super::engine::Action::Commands(Vec::new()),
                "Recorded fixture step",
                "",
            );
        }
        run.record(format!("Old output: {marker}"));
        run.inline_command(&format!("printf {marker}"));
        let retained = run.inline_result(super::super::engine::Status::Exited(0), marker, "");
        run.record("Retained command result".into());
        run.bind_activity_step(retained);
        assert!(run.presentation.identity(0) > 0);
        let old_history_id = run.inline.as_ref().unwrap().id;
        drop(fixture.send(first, Message::Close));
        assert!(!fixture.workspace.terminal_agents.runs.contains_key(&first));
        assert_eq!(
            fixture
                .workspace
                .finished_agent_run_choices(session, stream),
            [first]
        );
        drop(
            fixture
                .workspace
                .reopen_finished_agent_run(owner, session, stream + 1, first),
        );
        assert!(fixture.workspace.terminal_agents.runs.is_empty());

        let second = finish(&mut fixture, 2, "Inspect current service");
        let epoch = fixture.run_mut(second).epoch;
        drop(fixture.send(second, Message::SelectPresentedRun(epoch, first)));
        let (reopened, restored) = fixture
            .workspace
            .terminal_agents
            .agent_activity_for_stream(session, stream)
            .unwrap();
        assert_ne!(reopened, first);
        assert_ne!(reopened, second);
        assert!(restored.reopened_history && restored.phase == Phase::Finished);
        assert!(
            restored.commands.is_empty()
                && restored.transport.is_none()
                && restored.target.is_none()
        );
        assert!(restored.context.is_empty() && restored.learned_sudo.is_none());
        assert_ne!(restored.inline.as_ref().unwrap().id, old_history_id);
        let trace = restored.status_transcript().unwrap().0;
        assert_eq!(
            restored.activity_steps.get(&(restored.activity.len() - 1)),
            retained.as_ref()
        );
        assert_eq!(trace.goal, "Inspect service");
        assert_eq!(trace.steps.len(), 32);
        assert_eq!(trace.steps[0].output, ["service active"]);
        assert!(!format!("{trace:?}").contains(marker));
        assert!(
            restored
                .activity
                .iter()
                .all(|entry| !entry.contains(marker))
        );
        assert!(
            fixture
                .workspace
                .finished_agent_run_choices(session, stream)
                .is_empty()
        );
        drop(fixture.send(reopened, Message::StartFromShortcut));
        assert!(fixture.run_mut(reopened).phase == Phase::Finished);
        fixture.workspace.active_tab_mut(owner).unwrap().incognito = true;
        drop(fixture.workspace.prune_finished_agent_runs());
        assert!(
            !fixture
                .workspace
                .terminal_agents
                .runs
                .contains_key(&reopened)
        );
    }

    #[test]
    fn closed_history_is_bounded_and_revoked_with_the_pane_scope() {
        let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
        let session = fixture.session;
        let stream = fixture.workspace.sessions[&session].stream_generation;
        let owner = fixture.workspace.agent_pane(session).unwrap().0;
        let mut ids = Vec::new();
        for order in 1..=7 {
            let id = finish(&mut fixture, order, "Finished run");
            ids.push(id);
            drop(fixture.send(id, Message::Close));
        }
        let recent: Vec<_> = ids.iter().rev().take(5).copied().collect();
        assert_eq!(
            fixture
                .workspace
                .finished_agent_run_choices(session, stream),
            recent
        );
        drop(
            fixture
                .workspace
                .reopen_finished_agent_run(owner, session, stream, recent[0]),
        );
        let reopened = fixture
            .workspace
            .terminal_agents
            .agent_activity_for_stream(session, stream)
            .unwrap()
            .0;
        assert_ne!(reopened, recent[0]);
        drop(fixture.send(reopened, Message::Close));
        assert_eq!(
            fixture
                .workspace
                .finished_agent_run_choices(session, stream)
                .len(),
            5
        );
        fixture.workspace.active_tab_mut(owner).unwrap().incognito = true;
        drop(fixture.workspace.prune_finished_agent_runs());
        assert!(fixture.workspace.terminal_agents.finished_runs.0.is_empty());
        let private = finish(&mut fixture, 8, "Private run");
        drop(fixture.send(private, Message::Close));
        assert!(fixture.workspace.terminal_agents.finished_runs.0.is_empty());
        fixture.workspace.active_tab_mut(owner).unwrap().incognito = false;
        let fresh = finish(&mut fixture, 9, "Before reconnect");
        drop(fixture.send(fresh, Message::Close));
        fixture
            .workspace
            .terminal_agents
            .invalidate_session(session);
        assert!(fixture.workspace.terminal_agents.finished_runs.0.is_empty());
    }
}
