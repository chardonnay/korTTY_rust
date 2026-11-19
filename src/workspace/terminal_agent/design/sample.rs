//! Renders example activity through the same presenters as a terminal run.

use std::collections::VecDeque;

use iced::widget::{button, column, container, row, text};
use iced::{Element, Length, Size, window};

use crate::terminal_agent::design::{Arrangement, History, Layout, Status, inline};

use super::{
    AgentMessage, Desktop, DockExtents, DockPreferences, Message, Presentation, Slot,
    adaptive_status, adaptive_timeline, compose, history, history_body, pip, status, status_slot,
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Action {
    Detail(Message),
    PictureInPicture(pip::Message),
    Grid(Grid),
    Approve,
    Cancel,
    Ignore,
}

impl From<AgentMessage> for Action {
    fn from(message: AgentMessage) -> Self {
        match message {
            AgentMessage::Design(action) => Self::Detail(action),
            AgentMessage::Pip(action) => Self::PictureInPicture(action),
            AgentMessage::Approve(_) => Self::Approve,
            AgentMessage::Cancel => Self::Cancel,
            _ => Self::Ignore,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Grid {
    Narrow,
    #[default]
    Medium,
    Wide,
}

impl Grid {
    pub(crate) const ALL: [Self; 3] = [Self::Narrow, Self::Medium, Self::Wide];

    fn dimensions(self) -> (usize, usize) {
        match self {
            Self::Narrow => (60, 14),
            Self::Medium => (100, 24),
            Self::Wide => (160, 36),
        }
    }
}

impl std::fmt::Display for Grid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (columns, rows) = self.dimensions();
        write!(formatter, "{columns} columns × {rows} rows")
    }
}

pub(in crate::workspace) struct Sample {
    id: window::Id,
    entries: VecDeque<String>,
    presentation: Presentation,
    pub(in crate::workspace) grid: Grid,
}

impl Default for Sample {
    fn default() -> Self {
        Self {
            id: window::Id::unique(),
            entries: VecDeque::new(),
            presentation: Presentation::default(),
            grid: Grid::default(),
        }
    }
}

pub(in crate::workspace) struct View {
    pub(in crate::workspace) layout: Layout,
    pub(in crate::workspace) dock: DockPreferences,
    pub(in crate::workspace) phase: (&'static str, usize),
    pub(in crate::workspace) goal: &'static str,
    pub(in crate::workspace) approval: Option<&'static str>,
    pub(in crate::workspace) status_visible: bool,
    pub(in crate::workspace) reduced_motion: bool,
}

impl Sample {
    pub(in crate::workspace) fn reset(&mut self) {
        *self = Self {
            grid: self.grid,
            ..Self::default()
        };
    }

    pub(in crate::workspace) fn sync(&mut self, transcript: &inline::Transcript) {
        self.entries = transcript
            .steps
            .iter()
            .map(|step| {
                let mut entry = step.title.clone();
                for line in std::iter::once(&step.detail)
                    .chain(step.command.iter())
                    .chain(&step.output)
                    .filter(|line| !line.is_empty())
                {
                    entry.push('\n');
                    entry.push_str(line);
                }
                entry
            })
            .collect();
        if !transcript.final_answer.is_empty() {
            self.entries.push_back(transcript.final_answer.clone());
        }
        self.presentation.activity.expand_all = transcript.expanded;
        if transcript.prompt.is_some() {
            self.presentation.review(super::super::Phase::Approval);
        }
    }

    pub(in crate::workspace) fn interact(&mut self, action: Action) {
        match action {
            Action::Detail(action) => self.presentation.update(action, self.entries.len()),
            Action::PictureInPicture(action) => self.presentation.pip.update_sample(action),
            Action::Grid(grid) => self.grid = grid,
            Action::Approve | Action::Cancel | Action::Ignore => {}
        }
    }

    pub(in crate::workspace) fn view<'a>(
        &'a self,
        terminal: Element<'a, Desktop>,
        view: &View,
        bounds: Size,
        map: impl Fn(Action) -> Desktop + Copy + 'a,
    ) -> Element<'a, Desktop> {
        let forward = move |message: AgentMessage| map(message.into());
        let layout = view.layout;
        let fonts = super::super::font::Sizes::default();
        let activity = history::Activity {
            activity: &self.entries,
            goal: view.goal,
            plan: (layout.history == History::PlanExecution)
                .then_some("Inspect disk usage, review a cleanup, and summarize the result."),
            activity_font: fonts.activity,
            plan_font: fonts.plan,
            presentation: &self.presentation,
        };
        let controls = || -> Element<'a, AgentMessage> {
            if let Some(command) = view.approval {
                column![
                    text(command).font(iced::Font::MONOSPACE).size(12),
                    row![
                        button("Allow once").on_press(AgentMessage::Approve(0)),
                        button("Cancel").on_press(AgentMessage::Cancel),
                    ]
                    .spacing(5)
                    .wrap(),
                ]
                .spacing(4)
                .into()
            } else {
                column![].into()
            }
        };
        let history_panel = || -> Element<'a, AgentMessage> {
            container(
                column![
                    text(format!("{} · {}", layout.history, view.phase.0)).size(13),
                    history_body(
                        history::view_as(activity, layout.history, None),
                        controls(),
                        layout.approval,
                    ),
                ]
                .spacing(5),
            )
            .padding(7)
            .style(container::bordered_box)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        };
        let (status_kind, position) = if layout.arrangement == Arrangement::Adaptive {
            let (columns, rows) = self.grid.dimensions();
            adaptive_status(&self.presentation.dock, columns, rows)
        } else {
            (layout.status, status_slot(layout.status))
        };
        let history_position = view.dock.history_slot(layout);
        let adaptive_side = layout.arrangement == Arrangement::Adaptive && position == Slot::Right;
        let status = if !view.status_visible
            || status_kind == Status::Hidden
            || adaptive_side && layout.history == History::Timeline
        {
            None
        } else if adaptive_side {
            Some(adaptive_timeline(activity, view.phase.0, controls(), None).map(forward))
        } else {
            Some(
                status::render(activity, view.phase, false, status_kind, None, controls)
                    .map(forward),
            )
        };
        let side_width = view.dock.side_width(260.0);
        let terminal = compose(
            terminal,
            history_position.map(|_| history_panel().map(forward)),
            status,
            history_position,
            position,
            bounds,
            DockExtents {
                left: side_width,
                right: if status_kind == Status::Minimap && history_position != Some(Slot::Right) {
                    84.0
                } else {
                    side_width
                },
                bottom: None,
            },
        );
        if layout.arrangement == Arrangement::PictureInPicture {
            pip::sample(
                terminal,
                history_panel(),
                layout.history,
                &self.presentation.pip,
                self.id,
                view.reduced_motion,
                forward,
            )
        } else if history_position.is_none() && layout.history.inline_style().is_none() {
            column![
                terminal,
                container(history_panel().map(forward)).height(148)
            ]
            .spacing(4)
            .height(Length::Fill)
            .into()
        } else {
            terminal
        }
    }
}
