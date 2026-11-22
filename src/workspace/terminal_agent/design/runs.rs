//! Selection among the runs attached to a single terminal stream.

use std::fmt;

use iced::widget::pick_list;
use iced::{Element, Task, window};

use crate::terminal_agent::design::{Arrangement, Status};

use super::super::finished_runs::FinishedRun;
use super::super::{Desktop, LiveBinding, Message, Phase, Run, State, Workspace};
use super::{history, placement};

#[derive(Clone, Eq, PartialEq)]
pub(super) struct Choice {
    pub(super) id: window::Id,
    pub(super) label: String,
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

fn eligible(run: &Run, source: LiveBinding) -> bool {
    !run.closing
        && !run.swarm
        && run.phase != Phase::Setup
        && run.binding.live().ok() == Some(source)
}

fn label(run: &Run) -> String {
    let state = if run.awaiting_input() {
        "Review"
    } else if run.paused || run.phase == Phase::Paused {
        "Paused"
    } else if run.phase.finished() {
        "Finished"
    } else {
        "Active"
    };
    let prompt = run
        .status_transcript()
        .map_or("Agent run", |(trace, _)| &trace.goal);
    format!("{state} · {}", history::summary(prompt))
}

fn finished_label(run: &FinishedRun) -> String {
    format!("Finished · {}", history::summary(run.goal()))
}

impl State {
    pub(super) fn active_metro_runs(&self, run: &Run) -> usize {
        let Ok(source) = run.binding.live() else {
            return 0;
        };
        let history_panel =
            |run: &Run| placement::history_slot(run.design.history).map(|_| run.design.history);
        let panel = history_panel(run);
        self.runs
            .values()
            .filter(|candidate| {
                eligible(candidate, source)
                    && candidate.has_terminal_presentation()
                    && candidate.active()
                    && candidate.status_transcript().is_some()
                    && candidate.design.arrangement != Arrangement::Adaptive
                    && candidate.design.status == Status::Metro
                    && history_panel(candidate) == panel
            })
            .count()
    }

    fn finished_choices(&self, source: LiveBinding) -> impl Iterator<Item = Choice> {
        self.finished_runs
            .iter()
            .filter(move |entry| entry.source == source)
            .map(|entry| Choice {
                id: entry.id,
                label: finished_label(entry),
            })
    }

    pub(super) fn finished_run_picker(&self, run: &Run) -> Option<Element<'static, Message>> {
        let choices: Vec<_> = self.finished_choices(run.binding.live().ok()?).collect();
        let epoch = run.epoch;
        (!choices.is_empty()).then(|| {
            pick_list(choices, None::<Choice>, move |choice: Choice| {
                Message::SelectPresentedRun(epoch, choice.id)
            })
            .placeholder("Earlier finished runs")
            .text_size(12)
            .into()
        })
    }

    fn run_choices(&self, source: LiveBinding) -> Vec<Choice> {
        let mut runs: Vec<_> = self
            .runs
            .iter()
            .filter(|(_, run)| eligible(run, source))
            .collect();
        runs.sort_unstable_by_key(|(id, run)| std::cmp::Reverse((run.started_order, **id)));
        runs.into_iter()
            .map(|(&id, run)| Choice {
                id,
                label: label(run),
            })
            .collect()
    }

    pub(super) fn history_choices(&self, id: window::Id, run: &Run) -> Vec<Choice> {
        let Ok(source) = run.binding.live() else {
            return Vec::new();
        };
        if self
            .agent_activity_for_stream(source.session, source.stream)
            .is_none_or(|(shown, _)| shown != id)
        {
            return Vec::new();
        }
        self.run_choices(source)
            .into_iter()
            .filter(|choice| choice.id != id)
            .chain(self.finished_choices(source))
            .collect()
    }

    pub(super) fn run_picker(
        &self,
        id: window::Id,
        run: &Run,
    ) -> Option<Element<'static, Message>> {
        let source = run.binding.live().ok()?;
        let mut choices = self.run_choices(source);
        choices.extend(self.finished_choices(source));
        let selected = choices.iter().find(|choice| choice.id == id)?.clone();
        let epoch = run.epoch;
        (choices.len() > 1).then(|| {
            pick_list(choices, Some(selected), move |choice: Choice| {
                Message::SelectPresentedRun(epoch, choice.id)
            })
            .text_size(12)
            .into()
        })
    }
}

impl Workspace {
    pub(in crate::workspace) fn alternate_agent_runs(
        &self,
        session: crate::session::SessionId,
        stream: u64,
    ) -> Vec<(window::Id, u64, window::Id)> {
        let Some((current, run)) = self
            .terminal_agents
            .agent_activity_for_stream(session, stream)
            .filter(|(_, run)| self.current_run_pane(run).is_ok())
        else {
            return Vec::new();
        };
        let Ok(source) = run.binding.live() else {
            return Vec::new();
        };
        self.terminal_agents
            .run_choices(source)
            .into_iter()
            .filter(|choice| choice.id != current)
            .map(|choice| (current, run.epoch, choice.id))
            .collect()
    }

    pub(in crate::workspace) fn agent_run_choice_label(&self, id: window::Id) -> String {
        self.terminal_agents
            .runs
            .get(&id)
            .map(label)
            .or_else(|| {
                self.terminal_agents
                    .finished_runs
                    .get(id)
                    .map(finished_label)
            })
            .unwrap_or_else(|| "Agent run".to_owned())
    }

    pub(in crate::workspace) fn finished_agent_run_choices(
        &self,
        session: crate::session::SessionId,
        stream: u64,
    ) -> Vec<window::Id> {
        self.terminal_agents
            .finished_runs
            .iter()
            .filter(|entry| {
                entry.source.session == session
                    && entry.source.stream == stream
                    && self.current_agent_pane(entry.source, entry.mode).is_ok()
            })
            .map(|entry| entry.id)
            .collect()
    }

    pub(in crate::workspace::terminal_agent) fn select_presented_agent_run(
        &mut self,
        current: window::Id,
        epoch: u64,
        selected: window::Id,
    ) -> Task<Desktop> {
        let Some(run) = self.terminal_agents.runs.get(&current) else {
            return Task::none();
        };
        let Ok(source) = run.binding.live() else {
            return Task::none();
        };
        let Ok((owner, _)) = self.current_run_pane(run) else {
            return Task::none();
        };
        if run.epoch != epoch
            || current == selected
            || self
                .terminal_agents
                .agent_activity_for_stream(source.session, source.stream)
                .is_none_or(|(shown, _)| shown != current)
        {
            return Task::none();
        }
        if self.terminal_agents.finished_runs.get(selected).is_some() {
            return self.reopen_finished_agent_run(owner, source.session, source.stream, selected);
        }
        if self
            .terminal_agents
            .runs
            .get(&selected)
            .is_none_or(|run| !eligible(run, source) || self.current_run_pane(run).is_err())
        {
            return Task::none();
        }
        if run.has_terminal_presentation()
            && !self.terminal_agents.runs[&selected].has_terminal_presentation()
        {
            return self.focus_agent_surface(selected, Some(owner));
        }
        let run = self
            .terminal_agents
            .runs
            .get_mut(&selected)
            .expect("checked run");
        if run.uses_hud()
            && run
                .presentation
                .status_opacity(
                    Status::Hud,
                    self.window_settings.agent_status_auto_hide_seconds(),
                    std::time::Instant::now(),
                    !self.window_settings.agent_animations(),
                )
                .is_none()
        {
            run.presentation.dismiss_hud();
        }
        run.presentation.status_visibility.show();
        run.presentation.set_dock_hidden(false);
        run.presentation.pip.show();
        self.terminal_agents
            .presented_runs
            .insert(source.session, (source.stream, selected));
        self.reconcile_agent_pip()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::terminal_agent::design::History;
    use crate::terminal_agent::{Action, Decision, Mode};
    use crate::workspace::terminal_agent::Binding;
    use crate::workspace::terminal_agent::flow::tests::Fixture;

    #[test]
    fn metro_count_uses_unfinished_runs_with_the_same_stream_and_history_panel() {
        let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
        let current = fixture.open(Mode::Ask);
        let peer = fixture.open(Mode::Ask);
        for id in [current, peer] {
            let run = fixture.run_mut(id);
            run.goal = "Review logs".into();
            run.next(Phase::Thinking, "");
        }
        let count = |fixture: &Fixture| {
            let agents = &fixture.workspace.terminal_agents;
            agents.active_metro_runs(&agents.runs[&current])
        };
        assert_eq!(count(&fixture), 2);
        fixture.run_mut(peer).next(Phase::Paused, "");
        fixture.run_mut(peer).design.history = History::Focus;
        assert_eq!(count(&fixture), 2);
        fixture.run_mut(peer).design.history = History::Timeline;
        assert_eq!(count(&fixture), 1);
        fixture.run_mut(peer).design.history = History::InlineBlocks;
        fixture.run_mut(peer).design.status = Status::Neon;
        assert_eq!(count(&fixture), 1);
        fixture.run_mut(peer).design.status = Status::Metro;
        let own_stream = fixture.run_mut(peer).binding.live().unwrap();
        fixture.run_mut(peer).binding = Binding::Live(LiveBinding {
            stream: own_stream.stream + 1,
            ..own_stream
        });
        assert_eq!(count(&fixture), 1);
        fixture.run_mut(peer).binding = Binding::Live(own_stream);
        fixture.run_mut(peer).next(Phase::Finished, "");
        assert_eq!(count(&fixture), 1);
    }

    #[test]
    fn earlier_run_selection_reveals_history_and_yields_to_new_input() {
        let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
        let old = fixture.open(Mode::Run);
        let current = fixture.open(Mode::Run);
        let unstarted = fixture.open(Mode::Run);
        let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
        let prior = fixture.run_mut(old);
        prior.goal = "Review previous logs".into();
        prior.next(Phase::Thinking, "");
        prior.next(Phase::Finished, "");
        prior
            .presentation
            .status_visibility
            .toggle(0, std::time::Instant::now(), false);
        let active = fixture.run_mut(current);
        active.goal = "Review active logs".into();
        let epoch = active.next(Phase::Thinking, "");
        assert_eq!(
            fixture
                .workspace
                .alternate_agent_runs(fixture.session, stream),
            [(current, epoch, old)]
        );
        drop(fixture.send(current, Message::SelectPresentedRun(epoch, unstarted)));
        assert!(fixture.workspace.terminal_agents.presented_runs.is_empty());
        drop(fixture.send(current, Message::SelectPresentedRun(epoch, old)));
        assert_eq!(
            fixture.workspace.terminal_agents.presented_runs[&fixture.session],
            (stream, old)
        );
        assert!(
            fixture
                .run_mut(old)
                .presentation
                .status_visibility
                .opacity(0, std::time::Instant::now(), false)
                .is_some()
        );
        let decision = Decision {
            summary: "Choose log".into(),
            message: "Which log should I read?".into(),
            action: Action::Input {
                question: "Which log?".into(),
                answers: vec!["Application".into()],
            },
        };
        drop(fixture.send(
            current,
            Message::Decision(epoch, Arc::new(Ok(decision).into())),
        ));
        assert!(fixture.run_mut(current).phase == Phase::Question);
        drop(fixture.send(current, Message::SelectPresentedRun(epoch, old)));
        assert!(fixture.workspace.terminal_agents.presented_runs.is_empty());
        assert_eq!(
            fixture
                .workspace
                .terminal_agents
                .agent_activity_for_stream(fixture.session, stream)
                .unwrap()
                .0,
            current
        );
    }
}
