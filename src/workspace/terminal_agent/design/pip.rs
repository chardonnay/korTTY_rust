//! Java d19: adopt the history without taking space from the terminal or its status.

use iced::widget::{button, column, container, row, space, text, tooltip};
use iced::{Element, Length, Size, Task, window};

use crate::session::SessionId;
use crate::terminal_agent::design::{Arrangement, History};

use super::super::{Message as AgentMessage, Run, State, Workspace};
use super::{
    Desktop, dialog_placement, history,
    palette::{self, Palette},
};

mod surface;

pub(crate) use surface::Corner;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Message {
    Corner(Corner),
    Fold,
    Hide,
    Show,
    Detach,
    Select(window::Id),
}

#[derive(Default)]
pub(in crate::workspace::terminal_agent) struct Panel {
    pub(in crate::workspace::terminal_agent) detached: Option<window::Id>,
    corner: Corner,
    folded: bool,
    hidden: bool,
}

pub(super) fn matches(run: &Run, session: SessionId, stream: u64) -> bool {
    !run.swarm
        && !run.closing
        && run
            .binding
            .live()
            .is_ok_and(|source| source.session == session && source.stream == stream)
}

fn action(message: Message) -> AgentMessage {
    AgentMessage::Pip(message)
}

fn size(run: &Run) -> Size {
    let adopted = !matches!(run.design.history, History::Transcript | History::StepCards);
    Size::new(
        if adopted { 460.0 } else { 300.0 },
        if run.presentation.pip.folded {
            36.0
        } else if adopted {
            320.0
        } else {
            240.0
        },
    )
}

fn body<'a>(
    state: &'a State,
    id: window::Id,
    run: &'a Run,
    palette: Option<Palette>,
) -> Element<'a, AgentMessage> {
    let panel = &run.presentation.pip;
    let title = row![
        container(text("⠿ Agent history").size(13)).width(Length::Fill),
        button(if panel.detached.is_some() {
            "Attach"
        } else {
            "Detach"
        })
        .on_press(action(Message::Detach))
        .padding([3, 5]),
        button(if panel.folded { "+" } else { "−" })
            .on_press(action(Message::Fold))
            .padding([3, 7]),
        button("×").on_press(action(Message::Hide)).padding([3, 7]),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center)
    .height(32);
    let mut content = column![title].spacing(4);
    if !panel.folded {
        let source = run
            .binding
            .live()
            .expect("PiP only presents a live binding");
        let mut runs: Vec<_> = state
            .runs
            .iter()
            .filter(|(_, other)| matches(other, source.session, source.stream))
            .collect();
        runs.sort_unstable_by_key(|(id, run)| (run.started_order, **id));
        if runs.len() > 1 {
            let mut tabs = row![].spacing(3);
            for (index, (&other_id, other)) in runs.into_iter().enumerate() {
                let marker = if other.awaiting_input() {
                    "?"
                } else if other.active() {
                    "●"
                } else {
                    "✓"
                };
                tabs = tabs.push(tooltip(
                    button(text(format!("{marker} {}", index + 1)).size(12))
                        .on_press(action(Message::Select(other_id)))
                        .style(if other_id == id {
                            button::primary
                        } else {
                            button::secondary
                        }),
                    text(history::summary(&other.goal)),
                    tooltip::Position::Bottom,
                ));
            }
            content = content.push(tabs.wrap());
        }
        // There is only one presenter for the selected history. Status stays outside PiP.
        content = content
            .push(history::view(run, palette))
            .push(super::controls(run));
    }
    palette::panel(
        palette,
        container(content)
            .id("agent-picture-in-picture")
            .padding(2)
            .width(Length::Fill)
            .height(Length::Fill),
    )
}

pub(super) fn decorate<'a>(
    state: &'a State,
    id: window::Id,
    run: &'a Run,
    terminal: Element<'a, Desktop>,
    reduced_motion: bool,
    palette: Option<Palette>,
) -> Element<'a, Desktop> {
    let panel = &run.presentation.pip;
    if panel.detached.is_some() {
        return terminal;
    }
    let view: Element<'a, AgentMessage> = if panel.hidden {
        palette::scope(
            palette,
            button("Show agent history")
                .on_press(action(Message::Show))
                .into(),
        )
    } else {
        body(state, id, run, palette)
    };
    surface::view(
        terminal,
        view.map(move |message| Desktop::TerminalAgent(id, message)),
        id,
        panel.corner,
        if panel.hidden {
            Size::new(180.0, 36.0)
        } else {
            size(run)
        },
        surface::Options {
            draggable: !panel.hidden,
            reduced_motion,
        },
        move |corner| Desktop::TerminalAgent(id, action(Message::Corner(corner))),
    )
}

pub(in crate::workspace::terminal_agent) fn detached_view(
    state: &State,
    run_id: window::Id,
    palette: Option<Palette>,
) -> Element<'_, AgentMessage> {
    let run = &state.runs[&run_id];
    body(state, run_id, run, palette)
        .map(move |message| AgentMessage::PipOwned(run_id, Box::new(message)))
}

impl State {
    pub(in crate::workspace::terminal_agent) fn pip_run_for_window(
        &self,
        detached: window::Id,
    ) -> Option<window::Id> {
        self.runs
            .iter()
            .find_map(|(&id, run)| (run.presentation.pip.detached == Some(detached)).then_some(id))
    }
}

impl Workspace {
    pub(in crate::workspace::terminal_agent) fn agent_pip_action(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        let Some(run) = self.terminal_agents.runs.get(&id) else {
            return Task::none();
        };
        let Ok(source) = run.binding.live() else {
            return Task::none();
        };
        let (session, stream) = (source.session, source.stream);
        let Some(owner) = self
            .agent_pane(session)
            .and_then(|(owner, pane, _)| (pane.stream_generation == stream).then_some(owner))
        else {
            return Task::none();
        };
        if !matches(run, session, stream) || run.design.arrangement != Arrangement::PictureInPicture
        {
            return Task::none();
        }
        if matches!(message, Message::Show) {
            let mut closed = Vec::new();
            for (&other_id, other) in &mut self.terminal_agents.runs {
                if other_id != id
                    && matches(other, session, stream)
                    && let Some(detached) = other.presentation.pip.detached.take()
                {
                    closed.push(detached);
                }
            }
            let panel = &mut self
                .terminal_agents
                .runs
                .get_mut(&id)
                .unwrap()
                .presentation
                .pip;
            panel.hidden = false;
            let detached = panel.detached;
            self.terminal_agents
                .pip_selected
                .retain(|_, (_, selected)| self.terminal_agents.runs.contains_key(selected));
            self.terminal_agents
                .pip_selected
                .insert(session, (stream, id));
            let focus = detached.map_or_else(Task::none, |window| {
                window::minimize(window, false).chain(window::gain_focus(window))
            });
            if closed.is_empty() {
                return focus;
            }
            return Task::batch(
                std::iter::once(focus).chain(
                    closed
                        .into_iter()
                        .map(|window| self.close_placed_dialog(window)),
                ),
            );
        }
        if self
            .terminal_agents
            .agent_activity_for_stream(session, stream)
            .is_none_or(|(selected, _)| selected != id)
        {
            return Task::none();
        }
        if let Message::Select(selected) = message {
            if !self
                .terminal_agents
                .runs
                .get(&selected)
                .is_some_and(|other| matches(other, session, stream))
            {
                return Task::none();
            }
            self.terminal_agents
                .pip_selected
                .retain(|_, (_, selected)| self.terminal_agents.runs.contains_key(selected));
            self.terminal_agents
                .pip_selected
                .insert(session, (stream, selected));
            self.terminal_agents
                .runs
                .get_mut(&selected)
                .unwrap()
                .presentation
                .pip
                .hidden = false;
            return self.reconcile_agent_pip();
        }
        let run = self.terminal_agents.runs.get_mut(&id).unwrap();
        let panel = &mut run.presentation.pip;
        match message {
            Message::Corner(corner) if panel.detached.is_none() => panel.corner = corner,
            Message::Fold => {
                panel.folded = !panel.folded;
                if let Some(detached) = panel.detached {
                    return window::resize(detached, size(run));
                }
            }
            Message::Hide => {
                panel.hidden = true;
                if let Some(detached) = panel.detached.take() {
                    return self.close_placed_dialog(detached);
                }
            }
            Message::Detach => {
                if let Some(detached) = panel.detached.take() {
                    panel.corner = Corner::BottomRight;
                    return self.close_placed_dialog(detached);
                }
                let settings = window::Settings {
                    size: size(run),
                    min_size: Some(Size::new(240.0, 36.0)),
                    exit_on_close_request: false,
                    ..window::Settings::default()
                };
                let (detached, task) = self.open_modeless_placed_dialog(
                    owner,
                    dialog_placement::Kind::TerminalAgentPictureInPicture,
                    settings,
                );
                self.terminal_agents
                    .runs
                    .get_mut(&id)
                    .unwrap()
                    .presentation
                    .pip
                    .detached = Some(detached);
                return task;
            }
            _ => {}
        }
        Task::none()
    }

    pub(in crate::workspace::terminal_agent) fn close_agent_pip(
        &mut self,
        detached: window::Id,
    ) -> Option<Task<Desktop>> {
        let id = self.terminal_agents.pip_run_for_window(detached)?;
        let panel = &mut self.terminal_agents.runs.get_mut(&id)?.presentation.pip;
        panel.detached = None;
        panel.corner = Corner::BottomRight;
        Some(self.close_placed_dialog(detached))
    }

    pub(in crate::workspace::terminal_agent) fn reconcile_agent_pip(&mut self) -> Task<Desktop> {
        let stale: Vec<_> = self
            .terminal_agents
            .pip_selected
            .iter()
            .filter_map(|(&session, &(stream, id))| {
                let live = self
                    .terminal_agents
                    .runs
                    .get(&id)
                    .is_some_and(|run| matches(run, session, stream))
                    && self
                        .agent_pane(session)
                        .is_some_and(|(_, pane, _)| pane.stream_generation == stream);
                (!live).then_some(session)
            })
            .collect();
        for session in stale {
            self.terminal_agents.pip_selected.remove(&session);
        }
        let detached: Vec<_> = self
            .terminal_agents
            .runs
            .iter()
            .filter_map(|(&id, run)| run.presentation.pip.detached.map(|window| (id, window)))
            .collect();
        let mut tasks = Vec::new();
        for (id, window) in detached {
            let next = self
                .terminal_agents
                .runs
                .get(&id)
                .and_then(|run| run.binding.live().ok())
                .filter(|source| {
                    self.agent_pane(source.session)
                        .is_some_and(|(_, pane, _)| pane.stream_generation == source.stream)
                })
                .and_then(|source| {
                    self.terminal_agents
                        .agent_activity_for_stream(source.session, source.stream)
                })
                .filter(|(_, run)| run.design.arrangement == Arrangement::PictureInPicture)
                .map(|(id, _)| id);
            if next == Some(id) {
                continue;
            }
            if let Some(next) = next
                && self.terminal_agents.runs[&next]
                    .presentation
                    .pip
                    .detached
                    .is_none()
            {
                self.terminal_agents
                    .runs
                    .get_mut(&id)
                    .unwrap()
                    .presentation
                    .pip
                    .detached = None;
                let run = self.terminal_agents.runs.get_mut(&next).unwrap();
                run.presentation.pip.detached = Some(window);
                run.presentation.pip.hidden = false;
                tasks.push(window::resize(window, size(run)));
            } else if let Some(task) = self.close_agent_pip(window) {
                tasks.push(task);
            }
        }
        if tasks.is_empty() {
            Task::none()
        } else {
            Task::batch(tasks)
        }
    }
}

/// A static preview uses the same floating surface and dimensions, without actions.
pub(in crate::workspace) fn preview<'a, M: 'a>(
    terminal: Element<'a, M>,
    history: Element<'a, M>,
) -> Element<'a, M> {
    surface::preview(
        terminal,
        container(column![
            text("Agent history").size(13),
            history,
            space::vertical()
        ])
        .padding(6)
        .style(container::bordered_box)
        .into(),
    )
}
