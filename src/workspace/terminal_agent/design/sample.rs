//! Renders example activity through the same presenters as a terminal run.

use std::collections::VecDeque;
use std::time::{Instant, SystemTime};

use iced::widget::{Id, button, column, container, row, text};
use iced::{Element, Length, Size, Task, window};

use crate::terminal_agent::design::{Arrangement, History, Layout, Status, inline};

use super::{
    AgentMessage, Desktop, DockExtents, DockPreferences, Message, Presentation, SampleLogCommand,
    SampleLogKey, Slot, adaptive_status, adaptive_timeline, compose, history, history_body,
    log_table, palette, pip, placement, status, status_slot, tmux,
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Action {
    Detail(Message),
    PictureInPicture(pip::Message),
    Grid(Grid),
    Approve,
    Pause,
    Cancel,
    Ignore,
}

impl From<AgentMessage> for Action {
    fn from(message: AgentMessage) -> Self {
        match message {
            AgentMessage::Design(action) => Self::Detail(action),
            AgentMessage::Pip(action) => Self::PictureInPicture(action),
            AgentMessage::Approve(_) => Self::Approve,
            AgentMessage::Pause => Self::Pause,
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
    transcript: inline::Transcript,
    started: Instant,
    presentation: Presentation,
    pub(in crate::workspace) grid: Grid,
}

impl Default for Sample {
    fn default() -> Self {
        Self {
            id: window::Id::unique(),
            entries: VecDeque::new(),
            transcript: inline::Transcript::default(),
            started: Instant::now(),
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
    pub(in crate::workspace) status_opacity: Option<f32>,
    pub(in crate::workspace) reduced_motion: bool,
    pub(in crate::workspace) countdown: Option<(Instant, u8)>,
}

impl Sample {
    pub(in crate::workspace) fn log_keyboard<'a>(
        &'a self,
        content: Element<'a, Desktop>,
        is_pointer_action: fn(&Desktop) -> bool,
        message: impl Fn(SampleLogCommand) -> Desktop + 'a,
    ) -> Element<'a, Desktop> {
        log_table::keyboard::preview(
            content,
            &self.presentation.log_table,
            self.id,
            is_pointer_action,
            message,
        )
    }

    pub(in crate::workspace) fn navigate_log(
        &mut self,
        key: SampleLogKey,
        page: Id,
        reduced_motion: bool,
    ) -> Task<Desktop> {
        let clock = log_table::DurationClock {
            at: self.presentation.finish_time().unwrap_or_else(Instant::now),
            reduced_motion,
        };
        let table = &mut self.presentation.log_table;
        let surface = log_table::Surface::Activity;
        let navigation = table.navigate(&self.transcript, clock, surface, key, Some(0));
        if key == SampleLogKey::Open {
            navigation.chain(crate::workspace::coding_agents::roster_scroll::reveal_row(
                page,
                table.detail_id(surface),
            ))
        } else {
            navigation
        }
    }

    pub(in crate::workspace) fn reset(&mut self) {
        *self = Self {
            grid: self.grid,
            ..Self::default()
        };
    }

    pub(in crate::workspace) fn sync(&mut self, transcript: &inline::Transcript, layout: Layout) {
        let now = Instant::now();
        let clock = SystemTime::now();
        let prior_prompt = self.transcript.prompt.is_some();
        let mut next = transcript.clone();
        for step in &mut next.steps {
            let previous = self
                .transcript
                .steps
                .iter()
                .find(|previous| previous.id == step.id);
            step.recorded_at = previous
                .and_then(|previous| previous.recorded_at)
                .or(Some(clock));
            step.started_at = previous
                .and_then(|previous| previous.started_at)
                .or_else(|| (step.state == inline::StepState::Running).then_some(now));
            if step.state != inline::StepState::Running && step.elapsed_ms.is_none() {
                step.elapsed_ms = previous.and_then(|previous| previous.elapsed_at(now));
            }
        }
        if !next.final_answer.is_empty() {
            next.final_answer_at = self.transcript.final_answer_at.or(Some(clock));
        }
        if next.state.finished() && !self.transcript.state.finished() {
            self.presentation.finished(now);
        }
        self.transcript = next;
        self.presentation.sync_status(
            transcript,
            transcript.state.finished(),
            transcript.prompt.is_some(),
            now,
        );
        self.presentation
            .log_table
            .set_approval(0, transcript.prompt.clone(), now);
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
        if transcript.prompt.is_some() && !prior_prompt {
            if self.status_position(layout).0 == Status::Metro {
                self.presentation.review_started = Some(now);
            } else {
                self.presentation.review(super::super::Phase::Approval);
            }
        } else if transcript.prompt.is_none() {
            self.presentation.review_started = None;
        }
    }

    pub(in crate::workspace) fn has_live_toasts(&self, now: Instant, reduced_motion: bool) -> bool {
        self.presentation.status_events.ticking(now, reduced_motion)
    }

    pub(in crate::workspace) fn status_refresh_interval(
        &self,
        layout: Layout,
        playing: bool,
        now: Instant,
        reduced_motion: bool,
    ) -> Option<u64> {
        match self.status_position(layout).0 {
            Status::Hud => status::hud::refresh_interval(
                &self.presentation,
                playing && !self.transcript.state.finished(),
                self.transcript.state == inline::State::Paused,
                now,
                reduced_motion,
            ),
            Status::TopBar => status::top_bar::refresh_interval(
                &self.presentation,
                playing && !self.transcript.state.finished(),
                self.transcript.state == inline::State::Paused,
                now,
                reduced_motion,
            ),
            Status::Powerline => status::powerline::refresh_interval(
                &self.presentation,
                playing && !self.transcript.state.finished(),
                self.transcript.state == inline::State::Paused,
                now,
                reduced_motion,
            ),
            Status::Minimap => self
                .presentation
                .minimap
                .animating(
                    &self.transcript,
                    playing && self.transcript.state == inline::State::Running,
                    now,
                    reduced_motion,
                )
                .then_some(16),
            _ => None,
        }
    }

    pub(in crate::workspace) fn history_refresh_interval(
        &self,
        layout: Layout,
        now: Instant,
        reduced_motion: bool,
    ) -> Option<u64> {
        if layout.arrangement == Arrangement::PictureInPicture
            && !self.presentation.pip.shows_history()
        {
            return None;
        }
        match layout.history {
            History::CardStack => super::card_stack::refresh_interval(
                &self.transcript,
                &self.presentation,
                !self.transcript.state.finished(),
                now,
                reduced_motion,
            ),
            History::Timeline => super::timeline::refresh_interval(
                &self.transcript,
                &self.presentation,
                now,
                self.transcript.state == inline::State::Running,
                reduced_motion,
            ),
            History::PlanExecution => super::plan_execution::refresh_interval(
                &self.transcript,
                &self.presentation,
                now,
                !self.transcript.state.finished(),
                self.transcript.state == inline::State::Running,
                reduced_motion,
            ),
            History::LogTable => self.presentation.log_table.refresh_interval(
                &self.transcript,
                now,
                !self.transcript.state.finished(),
                self.transcript.state == inline::State::Running,
                reduced_motion,
            ),
            History::Tmux => {
                tmux::refresh_interval(&self.transcript, &self.presentation, now, reduced_motion)
            }
            History::Conversation => super::conversation::entrance_interval(
                &self.transcript,
                Some(self.started),
                self.presentation.review_started,
                now,
                reduced_motion,
            )
            .or_else(|| {
                super::conversation::refresh_interval(
                    &self.transcript,
                    self.transcript.state == inline::State::Running
                        && self.transcript.prompt.is_none(),
                    reduced_motion,
                )
            }),
            _ => None,
        }
    }

    pub(in crate::workspace) fn has_spotlight_preview(&self, now: Instant, layout: Layout) -> bool {
        self.status_position(layout).0 == Status::Spotlight
            && super::super::spotlight::preview_frame(now.saturating_duration_since(self.started))
                .is_some()
    }

    fn status_position(&self, layout: Layout) -> (Status, Slot) {
        if layout.arrangement == Arrangement::Adaptive {
            let (columns, rows) = self.grid.dimensions();
            adaptive_status(&self.presentation.dock, columns, rows)
        } else {
            (layout.status, status_slot(layout.status))
        }
    }

    pub(in crate::workspace) fn interact(&mut self, action: Action) {
        match action {
            Action::Detail(action) => {
                let len = match action {
                    super::Message::BrowseCards(_) | super::Message::ScrollCards { .. } => {
                        super::card_stack::entry_count(
                            Some(&self.transcript),
                            self.entries.len(),
                            !self.transcript.state.finished(),
                        )
                    }
                    _ => self.entries.len(),
                };
                self.presentation.update(action, len);
            }
            Action::PictureInPicture(action) => self.presentation.pip.update_sample(action),
            Action::Grid(grid) => self.grid = grid,
            Action::Approve | Action::Pause | Action::Cancel | Action::Ignore => {}
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
        let now = Instant::now();
        let fonts = super::super::font::Sizes::default();
        let activity = history::Activity {
            activity: &self.entries,
            goal: view.goal,
            plan: None,
            activity_font: fonts.activity,
            plan_font: fonts.plan,
            presentation: &self.presentation,
            transcript: Some(&self.transcript),
            started: Some(self.started),
            live_run: None,
            surface: super::log_table::Surface::Activity,
            owner: None,
            panel_review: None,
        };
        let (status_kind, position) = self.status_position(layout);
        let approval = view
            .approval
            .filter(|_| !super::command_review::handles(layout.history));
        let controls = move || -> Element<'a, AgentMessage> {
            if let Some(command) = approval {
                let choices = row![
                    button("Allow once").on_press(AgentMessage::Approve(0)),
                    button("Cancel").on_press(AgentMessage::Cancel),
                ]
                .spacing(5);
                if status_kind == Status::Metro {
                    return container(
                        row![
                            container(
                                text(command)
                                    .font(iced::Font::MONOSPACE)
                                    .size(12)
                                    .wrapping(text::Wrapping::None)
                            )
                            .width(Length::Fill)
                            .clip(true),
                            choices,
                        ]
                        .spacing(7)
                        .align_y(iced::Alignment::Center),
                    )
                    .padding([3, 8])
                    .width(Length::Fill)
                    .style(container::bordered_box)
                    .into();
                }
                column![
                    text(command).font(iced::Font::MONOSPACE).size(12),
                    choices.wrap(),
                ]
                .spacing(4)
                .into()
            } else {
                column![].into()
            }
        };
        let history_panel = || -> Element<'a, AgentMessage> {
            let plan = layout.history == History::PlanExecution;
            let timeline = layout.history == History::Timeline
                && layout.arrangement != Arrangement::PictureInPicture;
            let palette = ((timeline || plan)
                && layout.arrangement != Arrangement::PictureInPicture)
                .then(palette::Palette::default_status);
            let header = if timeline {
                Some(status::timeline_header(
                    activity,
                    status::Trace::sample(&self.transcript, self.started, now, view.reduced_motion),
                    palette,
                ))
            } else if plan {
                None
            } else {
                Some(
                    text(format!("{} · {}", layout.history, view.phase.0))
                        .size(13)
                        .into(),
                )
            };
            let panel = container(
                column![
                    header,
                    history_body(
                        history::view_as(activity, layout.history, palette, view.reduced_motion),
                        controls(),
                        layout.approval,
                    ),
                ]
                .spacing(if palette.is_some() { 4 } else { 5 }),
            )
            .padding(if timeline {
                super::timeline::PANEL_PADDING
            } else {
                iced::Padding::from(7)
            })
            .width(Length::Fill)
            .height(Length::Fill);
            if palette.is_some() {
                palette::panel(palette, panel)
            } else {
                panel.style(container::bordered_box).into()
            }
        };
        let status_opacity = view.status_opacity.filter(|_| {
            !matches!(status_kind, Status::TopBar | Status::Metro)
                || self
                    .presentation
                    .status_opacity(status_kind, 0, now, view.reduced_motion)
                    .is_some()
        });
        if status_kind == Status::Powerline && view.status_opacity.is_none() {
            self.presentation.dismiss_powerline_drawer();
        }
        if status_kind == Status::Orbit && view.status_opacity.is_none() {
            self.presentation.dismiss_drawer();
        }
        if status_kind == Status::Hud && view.status_opacity.is_none() {
            self.presentation.dismiss_hud();
        } else if status_kind != Status::Hud {
            self.presentation.status_visibility.hud_entrance.hide();
        }
        let history_position = view
            .dock
            .history_slot(layout)
            .filter(|_| !self.presentation.dock_hidden);
        let adaptive_side = layout.arrangement == Arrangement::Adaptive && position == Slot::Right;
        let status = status_opacity.and_then(|opacity| {
            if status_kind == Status::Hidden || adaptive_side && layout.history == History::Timeline
            {
                None
            } else if adaptive_side {
                Some(adaptive_timeline(activity, view.phase.0, controls(), None).map(forward))
            } else {
                Some(
                    status::render(
                        activity,
                        view.phase,
                        status::Trace::sample(
                            &self.transcript,
                            self.started,
                            now,
                            view.reduced_motion,
                        )
                        .opacified(opacity),
                        status_kind,
                        None,
                        false,
                        controls,
                    )
                    .map(forward),
                )
            }
            .map(|status| {
                if status_kind == Status::Hud {
                    status
                } else {
                    super::fade::view(status, opacity)
                }
            })
        });
        let side_width = view.dock.side_width(placement::history_width(
            layout.history,
            bounds.width,
            260.0,
        ));
        let terminal = if layout.arrangement == Arrangement::Adaptive {
            status::adaptive_countdown(terminal, view.countdown, now, view.reduced_motion, None)
        } else {
            terminal
        };
        let terminal = compose(
            terminal,
            history_position.map(|_| history_panel().map(forward)),
            status,
            history_position,
            placement::status_relative_to_history(position, layout.history, history_position),
            bounds,
            DockExtents {
                left: side_width,
                right: side_width,
                bottom: match (history_position, layout.history) {
                    (Some(Slot::Bottom), History::PlanExecution) => {
                        Some(placement::plan_sample_height(bounds.height))
                    }
                    (Some(Slot::Bottom), History::LogTable)
                        if log_table::has_detail(activity, &self.transcript) =>
                    {
                        Some(bounds.height * 0.65)
                    }
                    _ => Some((bounds.height * 0.4).min(260.0)),
                },
            },
        );
        let terminal = if status_kind == Status::Spotlight
            && !view.reduced_motion
            && !self.transcript.state.finished()
            && let Some(frame) =
                super::super::spotlight::preview_frame(now.saturating_duration_since(self.started))
        {
            super::super::spotlight::preview(terminal, frame, view.goal)
        } else {
            terminal
        };
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
        } else if history_position.is_none()
            && !self.presentation.dock_hidden
            && layout.history.inline_style().is_none()
        {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metro_preview_waits_for_an_explicit_steps_action() {
        let transcript = inline::Transcript {
            prompt: Some(inline::Prompt::Approval {
                message: "Sample approval".into(),
                commands: Vec::new(),
                allow_always: false,
                keyboard: false,
            }),
            ..inline::Transcript::default()
        };
        let mut sample = Sample::default();
        sample.sync(
            &transcript,
            Layout {
                status: Status::Metro,
                ..Layout::default()
            },
        );
        assert!(sample.presentation.review_started.is_some());
        assert!(!sample.presentation.drawer_open());

        sample.interact(Action::Detail(Message::ToggleDrawer));
        assert!(sample.presentation.drawer_open());
        sample.interact(Action::Detail(Message::HideStatus));
        assert!(!sample.presentation.drawer_open());
        assert!(
            sample
                .presentation
                .status_opacity(Status::Metro, 0, Instant::now(), true)
                .is_none()
        );

        sample.reset();
        sample.sync(
            &transcript,
            Layout {
                status: Status::TopBar,
                ..Layout::default()
            },
        );
        assert!(sample.presentation.drawer_open());
    }
}
