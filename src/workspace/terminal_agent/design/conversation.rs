//! Conversation history and follow-up runs on the original terminal stream.

use iced::widget::{button, row, text_input};
use iced::{Element, Task, window};

use crate::clipboard_policy::ClipboardPolicy;
use crate::terminal_agent::design::{History, inline::Transcript};
use crate::terminal_agent::shortcut::shortcut_character;
use crate::terminal_logging::Redactor;

use super::super::finished_runs::FinishedRun;
use super::super::{
    Desktop, MAX_GOAL, MAX_RUNS, Message, Mode, Run, RunPresentation, State, Workspace,
};
use super::history::Activity;
use super::{
    actions,
    log_table::{self, Row, Surface},
};

const EARLIER_TURNS: usize = 5;

enum EarlierTurn<'a> {
    Live(&'a Run, &'a Transcript),
    Retained(&'a FinishedRun),
}

impl<'a> EarlierTurn<'a> {
    fn order(&self) -> u64 {
        match self {
            Self::Live(run, _) => run.started_order,
            Self::Retained(run) => run.order,
        }
    }

    fn transcript(&self) -> &'a Transcript {
        match self {
            Self::Live(_, trace) => trace,
            Self::Retained(run) => run.transcript(),
        }
    }

    fn mode_and_redactor(&self) -> (Mode, Option<&Redactor>) {
        match self {
            Self::Live(run, _) => (run.mode, Some(&run.redactor)),
            Self::Retained(run) => (run.mode, None),
        }
    }
}

impl Run {
    fn shows_conversation(&self) -> bool {
        self.design.history == History::Conversation
            && self.has_terminal_presentation()
            && !self.swarm
            && !self.closing
            && self.status_transcript().is_some()
    }

    pub(super) fn conversation_pending(&self) -> bool {
        self.active()
            && !self.awaiting_input()
            && !self.paused
            && !self.cancel.requested()
            && !self.closing
    }
}

impl State {
    fn can_compose(&self, id: window::Id, run: &Run) -> bool {
        run.phase.finished()
            && run.shows_conversation()
            && self.runs.len() < MAX_RUNS
            && run.binding.live().is_ok_and(|source| {
                self.agent_activity_for_stream(source.session, source.stream)
                    .is_some_and(|(shown, _)| shown == id)
                    && self.check_live_run_slot(source, false).is_ok()
            })
            && !self.runs.values().any(|next| {
                next.conversation_parent == Some(id) && !next.closing && !next.phase.finished()
            })
    }

    fn earlier_conversation<'a>(&'a self, current: &Run) -> Vec<EarlierTurn<'a>> {
        let Ok(source) = current.binding.live() else {
            return Vec::new();
        };
        let live = self.runs.values().filter_map(|run| {
            let (trace, _) = run.status_transcript()?;
            (run.started_order != 0
                && run.started_order < current.started_order
                && run.phase.finished()
                && run.shows_conversation()
                && run.binding.live().ok() == Some(source))
            .then_some(EarlierTurn::Live(run, trace))
        });
        let retained = self
            .finished_runs
            .iter()
            .filter(|run| {
                run.source == source
                    && run.order < current.started_order
                    && (run.surface != RunPresentation::ChatTab || run.mode == Mode::Ask)
                    && run.design.history == History::Conversation
            })
            .map(EarlierTurn::Retained);
        let mut turns: Vec<_> = live.chain(retained).collect();
        turns.sort_unstable_by_key(EarlierTurn::order);
        turns.drain(..turns.len().saturating_sub(EARLIER_TURNS));
        turns
    }
}

mod view;
pub(super) use view::{entrance_interval, refresh_interval, thread};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct OutputKey {
    panel: u64,
    turn: u64,
    step: u64,
}

impl Workspace {
    pub(in crate::workspace) fn revalidate_agent_conversations(&mut self) {
        let availability: Vec<_> = self
            .terminal_agents
            .runs
            .iter()
            .filter(|(_, run)| run.design.history == History::Conversation)
            .map(|(&id, run)| {
                let enabled = run.phase.finished()
                    && run.shows_conversation()
                    && run
                        .binding
                        .live()
                        .is_ok_and(|source| self.current_agent_pane(source, Mode::Run).is_ok());
                (id, enabled)
            })
            .collect();
        for (id, enabled) in availability {
            if let Some(run) = self.terminal_agents.runs.get_mut(&id) {
                run.conversation_enabled = enabled;
            }
        }
    }

    pub(in crate::workspace::terminal_agent) fn agent_conversation_action(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        let (Message::ConversationPrompt(epoch, _) | Message::ContinueConversation(epoch)) =
            &message
        else {
            return Task::none();
        };
        let Some(run) = self
            .terminal_agents
            .runs
            .get(&id)
            .filter(|run| run.epoch == *epoch && self.terminal_agents.can_compose(id, run))
        else {
            return Task::none();
        };
        let Ok(source) = run.binding.live() else {
            return Task::none();
        };
        let Ok((owner, _)) = self.current_agent_pane(source, Mode::Run) else {
            return Task::none();
        };
        if let Message::ConversationPrompt(_, prompt) = message {
            if prompt.len() <= MAX_GOAL
                && prompt.chars().all(shortcut_character)
                && let Some(run) = self.terminal_agents.runs.get_mut(&id)
            {
                run.conversation_prompt = prompt;
            }
            return Task::none();
        }
        let prompt = run.conversation_prompt.trim();
        if prompt.is_empty() {
            return Task::none();
        }
        let mut next = match self.build_agent(source.session, Mode::Run, None, None, false) {
            Ok(next) => next,
            Err(error) => return self.open_agent_result(owner, Err(error)),
        };
        next.goal = prompt.to_owned();
        next.conversation_parent = Some(id);
        let direct = !next.profile_pending
            && self
                .ai_configuration()
                .is_some_and(|config| !config.show_agent_setup());
        if let Some(run) = self.terminal_agents.runs.get_mut(&id) {
            run.conversation_prompt.clear();
        }
        self.open_agent_result(owner, Ok((next, direct)))
    }

    pub(in crate::workspace::terminal_agent) fn toggle_conversation_output(
        &mut self,
        id: window::Id,
        key: OutputKey,
    ) -> Task<Desktop> {
        let state = &self.terminal_agents;
        let Some(run) = state.runs.get(&id).filter(|run| {
            run.shows_conversation()
                && run.started_order == key.panel
                && run.binding.live().is_ok_and(|source| {
                    self.agent_pane(source.session)
                        .is_some_and(|(_, pane, tab)| {
                            pane.stream_generation == source.stream
                                && tab.incognito == source.private
                        })
                })
        }) else {
            return Task::none();
        };
        let Some((current, _)) = run.status_transcript() else {
            return Task::none();
        };
        let available: std::collections::HashSet<_> = state
            .earlier_conversation(run)
            .into_iter()
            .map(|turn| (turn.order(), turn.transcript()))
            .chain(std::iter::once((run.started_order, current)))
            .flat_map(|(order, trace)| {
                trace
                    .steps
                    .iter()
                    .filter(|step| step.command.is_some() && !step.output.is_empty())
                    .map(move |step| OutputKey {
                        panel: key.panel,
                        turn: order,
                        step: step.id,
                    })
            })
            .collect();
        if available.contains(&key)
            && let Some(run) = self.terminal_agents.runs.get_mut(&id)
        {
            run.presentation
                .conversation_output
                .retain(|opened| available.contains(opened));
            run.presentation
                .update(super::Message::ConversationOutput(key), run.activity.len());
        }
        Task::none()
    }

    pub(in crate::workspace::terminal_agent) fn earlier_conversation_step_action(
        &mut self,
        id: window::Id,
        key: OutputKey,
        surface: Surface,
        item: actions::Item,
    ) -> Task<Desktop> {
        let state = &self.terminal_agents;
        let Some(run) = state.runs.get(&id).filter(|run| {
            run.shows_conversation()
                && run.started_order == key.panel
                && matches!(surface, Surface::EarlierConversation(_, order) if order == key.turn)
                && run
                    .presentation
                    .log_table
                    .has_menu(surface, Row::Step(key.step))
        }) else {
            return Task::none();
        };
        let prepared = (|| {
            let source = run.binding.live().ok()?;
            self.current_run_pane(run).ok()?;
            let (shown, _) = state.agent_activity_for_stream(source.session, source.stream)?;
            if shown != id {
                return None;
            }
            let turn = state
                .earlier_conversation(run)
                .into_iter()
                .find(|turn| turn.order() == key.turn)?;
            let step = turn
                .transcript()
                .steps
                .iter()
                .find(|step| step.id == key.step)?;
            if !actions::step_items(step, source.private).contains(&item) {
                return None;
            }
            let (mode, original) = turn.mode_and_redactor();
            let (owner, pane) = self.current_agent_pane(source, mode).ok()?;
            let policy = ClipboardPolicy::load();
            if !self.agent_run_action_allowed(&policy, mode, pane) {
                return None;
            }
            self.prepare_agent_step_action(step, item, source.private, pane, &policy, original)
                .map(|action| (owner, action))
        })();
        if let Some(run) = self.terminal_agents.runs.get_mut(&id) {
            run.presentation
                .log_table
                .update(log_table::Message::CloseMenu);
        }
        prepared.map_or_else(Task::none, |(owner, action)| {
            self.publish_agent_step_action(owner, Some(action))
        })
    }
}

pub(super) fn prompt(activity: Activity<'_>) -> Option<Element<'_, Message>> {
    let (state, id) = activity.owner?;
    let run = activity.live_run?;
    if activity.surface == Surface::Activity || !run.shows_conversation() {
        return None;
    }
    let enabled = run.conversation_enabled && state.can_compose(id, run);
    let submit = Message::ContinueConversation(run.epoch);
    Some(
        row![
            text_input("Ask a follow-up…", &run.conversation_prompt)
                .on_input_maybe(
                    enabled.then_some(move |prompt| Message::ConversationPrompt(run.epoch, prompt))
                )
                .on_submit_maybe(enabled.then_some(submit.clone()))
                .size(13),
            button("Send").on_press_maybe(
                (enabled && !run.conversation_prompt.trim().is_empty()).then_some(submit)
            ),
        ]
        .spacing(6)
        .into(),
    )
}

#[cfg(test)]
mod tests;
