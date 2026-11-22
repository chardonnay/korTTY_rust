//! Java d19: adopt the history without taking space from the terminal or its status.

use iced::widget::{button, column, container, row, text, tooltip};
use iced::{Element, Length, Size, Task, window};

use crate::session::SessionId;
use crate::terminal_agent::design::{Arrangement, History};

use super::super::{LiveBinding, Message as AgentMessage, Run, State, Workspace};
use super::{
    Desktop, dialog_placement, history,
    palette::{self, Palette},
};

mod menu;
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
    Menu(menu::Message),
}

#[derive(Default)]
pub(in crate::workspace::terminal_agent) struct Panel {
    pub(in crate::workspace::terminal_agent) detached: Option<window::Id>,
    corner: Corner,
    pub(super) folded: bool,
    hidden: bool,
    menu_selection: Option<usize>,
}

impl Panel {
    pub(in crate::workspace::terminal_agent) fn shows_history(&self) -> bool {
        !self.folded && !self.hidden
    }
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

fn panel_size(history: History, folded: bool) -> Size {
    let adopted = !matches!(history, History::Transcript | History::StepCards);
    Size::new(
        if adopted { 460.0 } else { 300.0 },
        if folded {
            36.0
        } else if adopted {
            320.0
        } else {
            240.0
        },
    )
}

fn size(run: &Run) -> Size {
    panel_size(run.design.history, run.presentation.pip.folded)
}

fn title(
    panel: &Panel,
    can_detach: bool,
    run_menu: Option<Element<'static, AgentMessage>>,
) -> iced::widget::Row<'static, AgentMessage> {
    row![
        container(text("⠿ Agent history").size(13)).width(Length::Fill),
        run_menu,
        can_detach.then(|| {
            button(if panel.detached.is_some() {
                "Attach"
            } else {
                "Detach"
            })
            .on_press(action(Message::Detach))
            .padding([3, 5])
        }),
        button(if panel.folded { "+" } else { "−" })
            .on_press(action(Message::Fold))
            .padding([3, 7]),
        button("×").on_press(action(Message::Hide)).padding([3, 7]),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center)
    .height(32)
}

fn panel_view<'a>(
    content: iced::widget::Column<'a, AgentMessage>,
    palette: Option<Palette>,
) -> Element<'a, AgentMessage> {
    palette::panel(
        palette,
        container(content)
            .id("agent-picture-in-picture")
            .padding(2)
            .width(Length::Fill)
            .height(Length::Fill),
    )
}

fn body<'a>(
    state: &'a State,
    id: window::Id,
    run: &'a Run,
    palette: Option<Palette>,
    reduced_motion: bool,
    panel_review: Option<super::command_review::Review>,
) -> Element<'a, AgentMessage> {
    let panel = &run.presentation.pip;
    let mut content = column![title(panel, true, menu::view(state, run))].spacing(4);
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
        if let Some(picker) = state.finished_run_picker(run) {
            content = content.push(picker);
        }
        // There is only one presenter for the selected history. Status stays outside PiP.
        content = content
            .push(super::history_with_steps(
                run,
                palette,
                reduced_motion,
                if panel.detached.is_some() {
                    super::log_table::Surface::Detached
                } else {
                    super::log_table::Surface::Pane
                },
                (state, id),
                panel_review,
            ))
            .push(super::controls_with_menu(run, None));
    }
    panel_view(content, palette)
}

pub(super) fn decorate<'a>(
    state: &'a State,
    id: window::Id,
    run: &'a Run,
    terminal: Element<'a, Desktop>,
    reduced_motion: bool,
    palette: Option<Palette>,
    panel_review: Option<super::command_review::Review>,
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
        body(state, id, run, palette, reduced_motion, panel_review)
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
    reduced_motion: bool,
    panel_review: Option<super::command_review::Review>,
) -> Element<'_, AgentMessage> {
    let run = &state.runs[&run_id];
    body(state, run_id, run, palette, reduced_motion, panel_review)
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
        if !run.has_terminal_presentation()
            || !matches(run, session, stream)
            || run.design.arrangement != Arrangement::PictureInPicture
        {
            return Task::none();
        }
        if matches!(message, Message::Show) {
            self.terminal_agents
                .runs
                .get_mut(&id)
                .unwrap()
                .presentation
                .pip
                .show();
            self.terminal_agents
                .presented_runs
                .insert(session, (stream, id));
            let follow = self.reconcile_agent_pip();
            let focus = self.terminal_agents.runs[&id]
                .presentation
                .pip
                .detached
                .map_or_else(Task::none, |window| {
                    window::minimize(window, false).chain(window::gain_focus(window))
                });
            return follow.chain(focus);
        }
        if self
            .terminal_agents
            .agent_activity_for_stream(session, stream)
            .is_none_or(|(selected, _)| selected != id)
        {
            return Task::none();
        }
        if let Message::Menu(message) = message {
            return self.agent_pip_menu(id, message);
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
                .presented_runs
                .retain(|_, (_, selected)| self.terminal_agents.runs.contains_key(selected));
            self.terminal_agents
                .presented_runs
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
                panel.menu_selection = None;
                panel.folded = !panel.folded;
                if let Some(detached) = panel.detached {
                    return window::resize(detached, size(run));
                }
            }
            Message::Hide => {
                panel.menu_selection = None;
                panel.hidden = true;
                if let Some(detached) = panel.detached.take() {
                    return self.close_placed_dialog(detached);
                }
            }
            Message::Detach => {
                panel.menu_selection = None;
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
        panel.menu_selection = None;
        panel.corner = Corner::BottomRight;
        Some(self.close_placed_dialog(detached))
    }

    fn followed_agent_pip_run(&self, source: LiveBinding) -> Option<window::Id> {
        let (id, run) = self
            .terminal_agents
            .agent_activity_for_stream(source.session, source.stream)?;
        (!self.exiting
            && run.binding.live().ok() == Some(source)
            && run.has_terminal_presentation()
            && run.design.arrangement == Arrangement::PictureInPicture
            && self.current_run_pane(run).is_ok())
        .then_some(id)
    }

    fn move_detached_agent_pip(
        &mut self,
        detached: window::Id,
        folded: bool,
        next: Option<window::Id>,
    ) -> Task<Desktop> {
        if let Some(run) = next
            .and_then(|id| self.terminal_agents.runs.get_mut(&id))
            .filter(|run| run.presentation.pip.detached.is_none())
        {
            let panel = &mut run.presentation.pip;
            panel.detached = Some(detached);
            panel.folded = folded;
            panel.hidden = false;
            panel.menu_selection = None;
            window::resize(detached, size(run))
        } else {
            self.close_placed_dialog(detached)
        }
    }

    pub(in crate::workspace::terminal_agent) fn follow_released_agent_pip(
        &mut self,
        run: &mut Run,
    ) -> Task<Desktop> {
        let Some(detached) = run.presentation.pip.detached.take() else {
            return Task::none();
        };
        let next = run
            .binding
            .live()
            .ok()
            .and_then(|source| self.followed_agent_pip_run(source));
        self.move_detached_agent_pip(detached, run.presentation.pip.folded, next)
    }

    pub(in crate::workspace::terminal_agent) fn reconcile_agent_pip(&mut self) -> Task<Desktop> {
        let stale: Vec<_> = self
            .terminal_agents
            .presented_runs
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
            self.terminal_agents.presented_runs.remove(&session);
        }
        let updates: Vec<_> = self
            .terminal_agents
            .runs
            .iter()
            .filter_map(|(&id, run)| {
                let panel = &run.presentation.pip;
                if panel.detached.is_none() && panel.menu_selection.is_none() {
                    return None;
                }
                let next = run
                    .binding
                    .live()
                    .ok()
                    .and_then(|source| self.followed_agent_pip_run(source));
                (next != Some(id)).then_some((id, next))
            })
            .collect();
        let mut tasks = Vec::new();
        for (id, next) in updates {
            let panel = &mut self
                .terminal_agents
                .runs
                .get_mut(&id)
                .unwrap()
                .presentation
                .pip;
            panel.menu_selection = None;
            if let Some(detached) = panel.detached.take() {
                let folded = panel.folded;
                tasks.push(self.move_detached_agent_pip(detached, folded, next));
            }
        }
        if tasks.is_empty() {
            Task::none()
        } else {
            Task::batch(tasks)
        }
    }
}

pub(super) fn sample<'a>(
    terminal: Element<'a, Desktop>,
    content: Element<'a, AgentMessage>,
    history: History,
    panel: &Panel,
    id: window::Id,
    reduced_motion: bool,
    map: impl Fn(AgentMessage) -> Desktop + Copy + 'a,
) -> Element<'a, Desktop> {
    let (view, size) = if panel.hidden {
        (
            button("Show agent history")
                .on_press(action(Message::Show))
                .into(),
            Size::new(180.0, 36.0),
        )
    } else {
        (
            panel_view(
                column![
                    title(panel, false, None),
                    (!panel.folded).then_some(content)
                ]
                .spacing(4),
                None,
            ),
            panel_size(history, panel.folded),
        )
    };
    surface::view(
        terminal,
        view.map(map),
        id,
        panel.corner,
        size,
        surface::Options {
            draggable: !panel.hidden,
            reduced_motion,
        },
        move |corner| map(action(Message::Corner(corner))),
    )
}

impl Panel {
    pub(super) fn show(&mut self) {
        self.hidden = false;
    }

    pub(super) fn update_sample(&mut self, message: Message) {
        match message {
            Message::Corner(corner) => self.corner = corner,
            Message::Fold => self.folded = !self.folded,
            Message::Hide => self.hidden = true,
            Message::Show => self.show(),
            Message::Detach | Message::Select(_) | Message::Menu(_) => {}
        }
    }
}
