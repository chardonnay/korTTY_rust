//! Native activity windows and status for an exact terminal stream.

use iced::widget::{button, column, container, row, text};
use iced::{Alignment, Color, Element, Length, Size, window};

use crate::session::SessionId;
use crate::terminal_agent::design::{Arrangement, History, Layout, Status};

use super::palette::{self, Palette};
use super::{Desktop, Message as AgentMessage, Phase, Run, State, Workspace, dialog_placement};

pub(in crate::workspace::terminal_agent) mod handoff;
mod history;
pub(in crate::workspace) mod pip;
mod placement;
mod presentation;
mod status;

pub(in crate::workspace) use placement::{Slot, compose, history_slot, status_slot};
use presentation::Dock;
pub(crate) use presentation::Message;
pub(super) use presentation::Presentation;

pub(super) fn window(layout: Layout) -> (dialog_placement::Kind, window::Settings) {
    let (kind, size, minimum, level) = if layout.arrangement == Arrangement::Floating {
        (
            dialog_placement::Kind::TerminalAgentFloating,
            Size::new(560.0, 470.0),
            Size::new(500.0, 340.0),
            window::Level::AlwaysOnTop,
        )
    } else {
        (
            dialog_placement::Kind::TerminalAgent,
            Size::new(820.0, 620.0),
            Size::new(530.0, 340.0),
            window::Level::Normal,
        )
    };
    (
        kind,
        window::Settings {
            size,
            min_size: Some(minimum),
            level,
            exit_on_close_request: false,
            ..window::Settings::default()
        },
    )
}

pub(in crate::workspace) fn activity_entry<'a, M: 'a>(
    history: History,
    index: usize,
    value: &'a str,
) -> Element<'a, M> {
    let entry = text(value).width(Length::Fill);
    if matches!(
        history,
        History::StepCards | History::CardStack | History::Conversation
    ) {
        container(column![text(format!("Step {}", index + 1)).size(13), entry].spacing(4))
            .style(container::bordered_box)
            .padding(8)
            .width(Length::Fill)
            .into()
    } else if history == History::Tmux {
        entry.font(iced::Font::MONOSPACE).into()
    } else {
        entry.into()
    }
}

pub(in crate::workspace) fn status_indicator<'a, M: 'a>(
    status: Status,
    phase: &str,
    stage: usize,
) -> Option<Element<'a, M>> {
    status_indicator_with_colors(status, phase, stage, None, None)
}

fn status_indicator_with_colors<'a, M: 'a>(
    status: Status,
    phase: &str,
    stage: usize,
    label_color: Option<Color>,
    accent: Option<Color>,
) -> Option<Element<'a, M>> {
    let label = text(format!("Agent · {phase}"))
        .size(12)
        .style(move |_| text::Style { color: label_color });
    match status {
        Status::Hidden => None,
        Status::Pill | Status::Spotlight | Status::Hud | Status::Toasts | Status::Orbit => Some(
            container(label)
                .style(container::rounded_box)
                .padding([4, 8])
                .into(),
        ),
        _ => {
            let mut steps = row![].spacing(4).align_y(Alignment::Center);
            for index in 0..4 {
                let active = index == stage;
                steps = steps.push(text(if active { "●" } else { "○" }).size(11).style(
                    move |_| text::Style {
                        color: if active { accent } else { None },
                    },
                ));
            }
            Some(
                row![steps, label]
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .into(),
            )
        }
    }
}

pub(super) fn history_view(run: &Run, palette: Option<Palette>) -> Element<'_, AgentMessage> {
    history::view(run, palette)
}

/// Review remains in the owned activity window, where the exact destination,
/// command, password mode and epoch are reviewed together.
fn controls(run: &Run) -> Element<'_, AgentMessage> {
    let active = run.active() && !run.cancel.requested();
    row![
        button(match run.phase {
            Phase::ModelReview => run
                .model_review
                .as_ref()
                .map_or("Review request…", |review| review.title()),
            Phase::ProbeApproval | Phase::Approval if run.reviewed_command().is_some() => {
                "Review and approve…"
            }
            Phase::SudoPassword => "Review and approve…",
            _ => "Activity window",
        })
        .on_press(AgentMessage::Focus),
        active
            .then(|| button(if run.paused { "Resume" } else { "Pause" })
                .on_press(AgentMessage::Pause)),
        active.then(|| button("Cancel").on_press(AgentMessage::Cancel)),
        handoff::compact(run),
    ]
    .spacing(5)
    .wrap()
    .into()
}

fn history_panel(run: &Run, palette: Option<Palette>) -> Element<'_, AgentMessage> {
    let (phase, _) = status::phase(run);
    let activity = history::view(run, palette);
    let controls = controls(run);
    let body = if run.design.approval == crate::terminal_agent::design::Approval::AboveActivity {
        column![controls, activity]
    } else {
        column![activity, controls]
    };
    let muted = palette.map(|palette| palette.muted.color());
    palette::panel(
        palette,
        container(
            column![
                text(format!("{} · {phase}", run.design.history)).size(13),
                text(&run.label)
                    .size(12)
                    .style(move |_| text::Style { color: muted }),
                body.spacing(6).height(Length::Fill),
            ]
            .spacing(5),
        )
        .padding(7)
        .width(Length::Fill)
        .height(Length::Fill),
    )
}

impl State {
    pub(super) fn agent_activity_for_stream(
        &self,
        session: SessionId,
        stream: u64,
    ) -> Option<(window::Id, &Run)> {
        if let Some(&(selected_stream, id)) = self.pip_selected.get(&session)
            && selected_stream == stream
            && let Some(run) = self.runs.get(&id)
            && pip::matches(run, session, stream)
        {
            return Some((id, run));
        }
        self.runs
            .iter()
            .filter(|(_, run)| {
                !run.swarm
                    && !run.closing
                    && run
                        .binding
                        .live()
                        .is_ok_and(|source| source.session == session && source.stream == stream)
            })
            .max_by(|(left_id, left), (right_id, right)| {
                let priority = |run: &Run| {
                    if run.awaiting_input() {
                        4
                    } else if run.active()
                        && !run.paused
                        && run.phase != Phase::Paused
                        && !run.cancel.requested()
                    {
                        3
                    } else if run.active() {
                        2
                    } else if run.phase.finished() {
                        1
                    } else {
                        0
                    }
                };
                priority(left).cmp(&priority(right)).then_with(|| {
                    let order =
                        (left.started_order, **left_id).cmp(&(right.started_order, **right_id));
                    if left.awaiting_input() {
                        order.reverse()
                    } else {
                        order
                    }
                })
            })
            .map(|(&id, run)| (id, run))
    }

    #[cfg(test)]
    pub(in crate::workspace) fn pane_agent_status(
        &self,
        session: SessionId,
        stream: u64,
    ) -> Option<Element<'_, Desktop>> {
        let (id, run) = self.agent_activity_for_stream(session, stream)?;
        pane_status(id, run, run.design.status, None)
    }
}

fn pane_status(
    id: window::Id,
    run: &Run,
    status: Status,
    palette: Option<Palette>,
) -> Option<Element<'_, Desktop>> {
    (status != Status::Hidden).then(|| {
        status::view(run, status, palette).map(move |message| Desktop::TerminalAgent(id, message))
    })
}

impl Workspace {
    pub(in crate::workspace) fn decorate_agent_terminal<'a>(
        &'a self,
        session: SessionId,
        stream: u64,
        terminal: Element<'a, Desktop>,
        bounds: Size,
        cell: Size,
        reduced_motion: bool,
    ) -> Element<'a, Desktop> {
        let agents = &self.terminal_agents;
        let Some((id, run)) = agents.agent_activity_for_stream(session, stream) else {
            return terminal;
        };
        let palette = self.agent_panel_palette(self.pane_theme_id(session));
        let mut status_kind = run.design.status;
        let mut status_position = status_slot(status_kind);
        let mut adaptive_side = false;
        if run.design.arrangement == Arrangement::Adaptive {
            let dock = run.presentation.dock.get().choose(
                (bounds.width / cell.width.max(1.0)) as usize,
                (bounds.height / cell.height.max(1.0)) as usize,
            );
            run.presentation.dock.set(dock);
            adaptive_side = dock == Dock::Side;
            (status_kind, status_position) = match dock {
                Dock::Side => (Status::Metro, Slot::Right),
                Dock::Bar => (Status::Powerline, Slot::Bottom),
                Dock::Pill => (Status::Spotlight, Slot::OverlayTop),
            };
        }
        let picture_in_picture = run.design.arrangement == Arrangement::PictureInPicture;
        let history_position = (!picture_in_picture)
            .then(|| history_slot(run.design.history))
            .flatten();
        let history = history_position.map(|_| {
            history_panel(run, palette).map(move |message| Desktop::TerminalAgent(id, message))
        });
        let status = if adaptive_side && run.design.history == History::Timeline {
            // Both roles resolve to Java d02. Keep its single history presenter.
            None
        } else if adaptive_side {
            let panel = palette::panel(
                palette,
                container(
                    column![
                        text(format!("Agent · {}", status::phase(run).0)).size(13),
                        history::view_as(run, History::Timeline, palette),
                        controls(run),
                    ]
                    .spacing(5),
                )
                .padding(7)
                .height(Length::Fill)
                .width(Length::Fill),
            );
            Some(panel.map(move |message| Desktop::TerminalAgent(id, message)))
        } else {
            pane_status(id, run, status_kind, palette)
        };
        let rail = status_kind == Status::Minimap && history_position != Some(Slot::Right);
        let terminal = compose(
            terminal,
            history,
            status,
            history_position,
            status_position,
            bounds,
            if rail { 84.0 } else { cell.width * 40.0 },
        );
        if picture_in_picture {
            pip::decorate(agents, id, run, terminal, reduced_motion, palette)
        } else {
            terminal
        }
    }
}
