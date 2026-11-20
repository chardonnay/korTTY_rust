//! Native activity windows and status for an exact terminal stream.

use std::cell::Cell;
use std::time::{Duration, Instant};

use iced::widget::{button, canvas, column, container, row, text};
use iced::{Alignment, Color, Element, Length, Size, Subscription, window};

use crate::horizontal_divider::{self, Grow};
use crate::session::SessionId;
use crate::terminal_agent::design::{Arrangement, History, Layout, Status};

use super::palette::{self, Palette};
use super::{Desktop, Message as AgentMessage, Phase, Run, State, Workspace, dialog_placement};

pub(in crate::workspace) mod actions;
mod activity;
pub(crate) use actions::Action;
pub(in crate::workspace::terminal_agent) mod handoff;
mod history;
pub(in crate::workspace) mod pip;
mod placement;
mod presentation;
mod runs;
mod sample;
mod status;
mod toast;

pub(crate) use activity::Change as ActivityChange;
pub(in crate::workspace) use activity::Height;
pub(super) use activity::controls as activity_controls;
use activity::fitted_activity_height;
pub(in crate::workspace) use placement::{
    DockExtents, DockPlacement, DockPreferences, Slot, compose, status_slot,
};
use presentation::Dock;
pub(crate) use presentation::Message;
pub(super) use presentation::{Presentation, Usage};
pub(in crate::workspace) use sample::{
    Action as SampleAction, Grid as SampleGrid, Sample, View as SampleView,
};

pub(in crate::workspace) const AUTO_HIDE_SECONDS: std::ops::RangeInclusive<u8> = 0..=60;
pub(in crate::workspace) const DEFAULT_AUTO_HIDE_SECONDS: u8 = 6;

#[derive(Clone, Copy)]
pub(crate) struct DockResize {
    expected: DockPreferences,
    side: Slot,
    extent: Option<f32>,
}

impl DockResize {
    fn message(self, id: window::Id, extent: Option<f32>) -> Desktop {
        Desktop::TerminalAgent(id, AgentMessage::DockResize(Self { extent, ..self }))
    }
}

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
    size: f32,
) -> Element<'a, M> {
    let entry = text(value).size(size).width(Length::Fill);
    if matches!(
        history,
        History::StepCards | History::CardStack | History::Conversation
    ) {
        container(column![text(format!("Step {}", index + 1)).size(size), entry].spacing(4))
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
        actions::run_menu(run),
        run.status_transcript()
            .filter(|(trace, _)| !trace.steps.is_empty())
            .map(|_| {
                button(if run.presentation.step_list_open {
                    "Run history"
                } else {
                    "Step details"
                })
                .on_press(AgentMessage::Design(Message::ToggleStepList))
            }),
    ]
    .spacing(5)
    .wrap()
    .into()
}

fn history_panel<'a>(
    run: &'a Run,
    chooser: Option<Element<'static, AgentMessage>>,
    palette: Option<Palette>,
) -> Element<'a, AgentMessage> {
    let (phase, _) = status::phase(run);
    let activity = history_with_steps(run, palette);
    let controls = controls(run);
    let body = history_body(activity, controls, run.design.approval);
    let muted = palette.map(|palette| palette.muted.color());
    palette::panel(
        palette,
        container(
            column![
                text(format!("{} · {phase}", run.design.history)).size(13),
                text(&run.label)
                    .size(12)
                    .style(move |_| text::Style { color: muted }),
                chooser,
                body,
            ]
            .spacing(5),
        )
        .padding(7)
        .width(Length::Fill)
        .height(Length::Fill),
    )
}

fn history_with_steps(run: &Run, palette: Option<Palette>) -> Element<'_, AgentMessage> {
    if run.presentation.step_list_open {
        status::step_list(run)
    } else {
        history::view(run, palette)
    }
}

fn history_body<'a>(
    activity: Element<'a, AgentMessage>,
    controls: Element<'a, AgentMessage>,
    approval: crate::terminal_agent::design::Approval,
) -> iced::widget::Column<'a, AgentMessage> {
    if approval == crate::terminal_agent::design::Approval::AboveActivity {
        column![controls, activity]
    } else {
        column![activity, controls]
    }
    .spacing(6)
    .height(Length::Fill)
}

fn adaptive_status(dock: &Cell<Dock>, columns: usize, rows: usize) -> (Status, Slot) {
    let chosen = dock.get().choose(columns, rows);
    dock.set(chosen);
    match chosen {
        Dock::Side => (Status::Metro, Slot::Right),
        Dock::Bar => (Status::Powerline, Slot::Bottom),
        Dock::Pill => (Status::Spotlight, Slot::OverlayTop),
    }
}

fn adaptive_timeline<'a>(
    activity: history::Activity<'a>,
    phase: &str,
    controls: Element<'a, AgentMessage>,
    palette: Option<Palette>,
) -> Element<'a, AgentMessage> {
    palette::panel(
        palette,
        container(
            column![
                text(format!("Agent · {phase}")).size(13),
                history::view_as(activity, History::Timeline, palette),
                controls,
            ]
            .spacing(5),
        )
        .padding(7)
        .height(Length::Fill)
        .width(Length::Fill),
    )
}

impl State {
    pub(super) fn agent_activity_for_stream(
        &self,
        session: SessionId,
        stream: u64,
    ) -> Option<(window::Id, &Run)> {
        if let Some(&(selected_stream, id)) = self.presented_runs.get(&session)
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
        pane_status(id, run, run.design.status, None, Instant::now(), true)
    }
}

fn pane_status(
    id: window::Id,
    run: &Run,
    status: Status,
    palette: Option<Palette>,
    now: Instant,
    reduced_motion: bool,
) -> Option<Element<'_, Desktop>> {
    (status != Status::Hidden).then(|| {
        status::view(run, status, palette, now, reduced_motion)
            .map(move |message| Desktop::TerminalAgent(id, message))
    })
}

impl Workspace {
    pub(super) fn run_uses_spotlight_status(&self, run: &Run) -> bool {
        if run.design.arrangement != Arrangement::Adaptive {
            return run.design.status == Status::Spotlight;
        }
        run.binding
            .live()
            .ok()
            .and_then(|source| self.sessions.get(&source.session))
            .is_some_and(|pane| {
                let screen = &pane.frame.screen;
                run.presentation
                    .dock
                    .get()
                    .choose(usize::from(screen.columns), usize::from(screen.rows))
                    == Dock::Pill
            })
    }

    pub(in crate::workspace) fn agent_status_subscription(&self) -> Subscription<Desktop> {
        let seconds = self.window_settings.agent_status_auto_hide_seconds();
        let reduced_motion = !self.window_settings.agent_animations();
        let now = Instant::now();
        if self.exiting {
            return Subscription::none();
        }
        let interval = self
            .terminal_agents
            .runs
            .values()
            .filter(|run| {
                !run.swarm
                    && !run.closing
                    && (run.design.status != Status::Hidden
                        || run.design.arrangement == Arrangement::Adaptive)
                    && run.presentation.status_visibility.visible(seconds, now)
            })
            .filter_map(|run| {
                let toasts = run.design.status == Status::Toasts
                    && run.presentation.status_events.ticking(now, reduced_motion);
                if toasts && !reduced_motion {
                    return Some(16);
                }
                (toasts
                    || run
                        .presentation
                        .status_visibility
                        .deadline(seconds)
                        .is_some()
                    || run.active()
                        && (matches!(run.design.status, Status::Spotlight | Status::Orbit)
                            || run.design.arrangement == Arrangement::Adaptive
                                && run.presentation.dock.get() == Dock::Pill))
                    .then_some(100)
            })
            .chain(self.spotlight_animating(now).then_some(16))
            .min();
        interval.map_or_else(Subscription::none, |millis| {
            crate::workspace::scheduler_access::desktop_clock(Duration::from_millis(millis))
                .map(|()| Desktop::AgentStatusClock)
        })
    }

    fn agent_status_target(&self, owner: window::Id) -> Option<window::Id> {
        let session = self.windows.get(&owner)?.tab()?.focused_session()?;
        let stream = self.sessions.get(&session)?.stream_generation;
        let (id, run) = self
            .terminal_agents
            .agent_activity_for_stream(session, stream)?;
        (run.design.status != Status::Hidden || run.design.arrangement == Arrangement::Adaptive)
            .then_some(id)
    }

    pub(in crate::workspace) fn agent_status_available(&self, owner: window::Id) -> bool {
        self.agent_status_target(owner).is_some()
    }

    pub(in crate::workspace) fn toggle_agent_status(&mut self, owner: window::Id) {
        if let Some(id) = self.agent_status_target(owner)
            && let Some(run) = self.terminal_agents.runs.get_mut(&id)
        {
            run.presentation.status_visibility.toggle(
                self.window_settings.agent_status_auto_hide_seconds(),
                Instant::now(),
            );
        }
    }

    pub(in crate::workspace) fn cancel_agent_dock_resizes(&mut self) {
        for run in self.terminal_agents.runs.values_mut() {
            run.presentation.dock_resize = None;
            run.presentation.activity.preview = None;
        }
    }

    pub(super) fn resize_agent_dock(
        &mut self,
        id: window::Id,
        resize: DockResize,
    ) -> Option<window::Id> {
        let dock = self.window_settings.ai_agent_panel;
        if dock != resize.expected {
            return None;
        }
        let current = self.terminal_agents.runs.get(&id).and_then(|run| {
            if run.swarm
                || run.closing
                || run.design.arrangement != Arrangement::Normal
                || dock.history_slot(run.design) != Some(resize.side)
                || !matches!(resize.side, Slot::Left | Slot::Right | Slot::Bottom)
            {
                return None;
            }
            let source = run.binding.live().ok()?;
            let (owner, pane, _) = self.agent_pane(source.session)?;
            (source.stream == pane.stream_generation
                && self
                    .terminal_agents
                    .agent_activity_for_stream(source.session, source.stream)
                    .is_some_and(|(current, _)| current == id)
                && self.owner_dialog(owner).is_none()
                && self.hosted_close_prompt(owner).is_none()
                && !self.terminal_only_fullscreen_active(owner))
            .then_some(owner)
        });
        let owner = current?;
        if resize.side == Slot::Bottom {
            return self.resize_activity_height(id, owner, resize.extent);
        }
        let preview = &mut self
            .terminal_agents
            .runs
            .get_mut(&id)
            .expect("checked run")
            .presentation
            .dock_resize;
        if let Some(width) = resize.extent {
            if width.is_finite() {
                let range = DockPreferences::WIDTH_RANGE;
                let width = width
                    .clamp(f32::from(*range.start()), f32::from(*range.end()))
                    .round() as u16;
                *preview = Some((dock, width));
            }
        } else if let Some((expected, width)) = preview.take()
            && expected == dock
            && dock.width != Some(width)
        {
            self.window_settings.ai_agent_panel.width = Some(width);
            self.cancel_agent_dock_resizes();
            return Some(owner);
        }
        None
    }

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
            (status_kind, status_position) = adaptive_status(
                &run.presentation.dock,
                (bounds.width / cell.width.max(1.0)) as usize,
                (bounds.height / cell.height.max(1.0)) as usize,
            );
            adaptive_side = status_position == Slot::Right;
        }
        let now = Instant::now();
        let seconds = self.window_settings.agent_status_auto_hide_seconds();
        let visibility = &run.presentation.status_visibility;
        let status_visible = visibility.visible(seconds, now);
        let chooser = || agents.run_picker(id, run);
        let picture_in_picture = run.design.arrangement == Arrangement::PictureInPicture;
        let dock = self.window_settings.ai_agent_panel;
        let history_position = dock.history_slot(run.design);
        let activity = &run.presentation.activity;
        let bottom_height = fitted_activity_height(
            bounds.height,
            activity.preview.unwrap_or(activity.height),
            activity.collapsed,
        );
        let side_width = run
            .presentation
            .dock_resize
            .filter(|(expected, _)| *expected == dock)
            .map_or_else(
                || dock.side_width(cell.width * 40.0),
                |(_, width)| f32::from(width),
            );
        let history = history_position.map(|side| {
            if side == Slot::Bottom {
                let header: Element<'_, AgentMessage> = activity::toggle(run).into();
                let header = header.map(move |message| Desktop::TerminalAgent(id, message));
                let mut panel = column![].spacing(3);
                if !activity.collapsed
                    && run.design.arrangement == Arrangement::Normal
                    && bottom_height >= Height::MIN
                {
                    let resize = DockResize {
                        expected: dock,
                        side,
                        extent: None,
                    };
                    let max_height = fitted_activity_height(bounds.height, f32::MAX, false);
                    panel = panel.push(
                        canvas(horizontal_divider::program(
                            bottom_height,
                            Grow::Up,
                            move |height| resize.message(id, Some(height.min(max_height))),
                            Some(resize.message(id, None)),
                        ))
                        .width(Length::Fill)
                        .height(horizontal_divider::WIDTH),
                    );
                }
                panel = panel.push(header);
                if !activity.collapsed {
                    panel = panel.push(
                        history_panel(run, chooser(), palette)
                            .map(move |message| Desktop::TerminalAgent(id, message)),
                    );
                }
                return panel.height(Length::Fill).into();
            }
            let history = history_panel(run, chooser(), palette)
                .map(move |message| Desktop::TerminalAgent(id, message));
            if run.design.arrangement != Arrangement::Normal
                || !matches!(side, Slot::Left | Slot::Right)
            {
                return history;
            }
            let left = side == Slot::Left;
            let extent = placement::fitted_side_width(
                bounds.width,
                side_width,
                left && status_visible && status_position == Slot::Right,
            );
            let resize = DockResize {
                expected: dock,
                side,
                extent: None,
            };
            let grip = canvas(horizontal_divider::program(
                extent,
                if left { Grow::Right } else { Grow::Left },
                move |width| resize.message(id, Some(width)),
                Some(resize.message(id, None)),
            ))
            .width(horizontal_divider::WIDTH)
            .height(Length::Fill);
            if left {
                row![history, grip].height(Length::Fill).into()
            } else {
                row![grip, history].height(Length::Fill).into()
            }
        });
        let status = if !status_visible || adaptive_side && run.design.history == History::Timeline
        {
            // Both roles resolve to Java d02. Keep its single history presenter.
            None
        } else if adaptive_side {
            let controls = column![chooser(), controls(run)].spacing(4).into();
            let panel = adaptive_timeline(run.into(), status::phase(run).0, controls, palette);
            Some(panel.map(move |message| Desktop::TerminalAgent(id, message)))
        } else {
            pane_status(id, run, status_kind, palette, now, reduced_motion).map(|status| {
                let picker = if history_position.is_none() && !picture_in_picture {
                    chooser().map(|picker| {
                        picker.map(move |message| Desktop::TerminalAgent(id, message))
                    })
                } else {
                    None
                };
                if let Some(picker) = picker {
                    palette::panel(
                        palette,
                        container(column![status, picker].spacing(3)).padding(2),
                    )
                } else {
                    status
                }
            })
        };
        let terminal = if run.design.arrangement == Arrangement::Adaptive {
            status::adaptive_countdown(
                terminal,
                visibility
                    .deadline(seconds)
                    .map(|deadline| (deadline, seconds)),
                now,
                reduced_motion,
                palette,
            )
        } else {
            terminal
        };
        let rail = status_kind == Status::Minimap && history_position != Some(Slot::Right);
        let terminal = compose(
            terminal,
            history,
            status,
            history_position,
            status_position,
            bounds,
            DockExtents {
                left: side_width,
                right: if rail { 84.0 } else { side_width },
                bottom: Some(bottom_height),
            },
        );
        if picture_in_picture {
            pip::decorate(agents, id, run, terminal, reduced_motion, palette)
        } else {
            terminal
        }
    }
}

#[cfg(test)]
mod dock_tests {
    use super::*;
    use crate::terminal_agent::Mode;
    use crate::workspace::terminal_agent::flow::tests::Fixture;

    #[test]
    fn activity_height_remembers_only_committed_changes_and_reopens_collapsed() {
        let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
        let id = fixture.open(Mode::Ask);
        let workspace = &mut fixture.workspace;
        let dock = DockPreferences {
            placement: Some(DockPlacement::Bottom),
            width: None,
        };
        workspace.window_settings.ai_agent_panel = dock;
        workspace.window_settings.agent_remember_activity_font = Some(true);
        let resize = DockResize {
            expected: dock,
            side: Slot::Bottom,
            extent: None,
        };
        assert!(
            workspace
                .resize_agent_dock(
                    id,
                    DockResize {
                        extent: Some(307.5),
                        ..resize
                    }
                )
                .is_none()
        );
        assert_eq!(workspace.window_settings.agent_activity_height, None);
        assert!(workspace.resize_agent_dock(id, resize).is_some());
        assert_eq!(workspace.window_settings.agent_activity_height, Some(307.5));

        workspace.change_agent_activity_panel(id, ActivityChange::KeepCollapsed(true));
        workspace.change_agent_activity_panel(id, ActivityChange::ExpandAll(true));
        let reopened = activity::Panel::load(&workspace.window_settings);
        assert!(reopened.collapsed && reopened.expand_all);
        assert_eq!(reopened.height, 307.5);
        assert_eq!(fitted_activity_height(350.0, reopened.height, false), 250.0);
        assert_eq!(fitted_activity_height(150.0, reopened.height, true), 50.0);
        workspace.change_agent_activity_panel(id, ActivityChange::ToggleCollapsed);
        assert!(
            workspace
                .resize_agent_dock(
                    id,
                    DockResize {
                        extent: Some(350.0),
                        ..resize
                    }
                )
                .is_none()
        );
        assert!(workspace.resize_agent_dock(id, resize).is_none());
        assert_eq!(workspace.window_settings.agent_activity_height, Some(307.5));
    }

    #[test]
    fn dock_drag_previews_then_commits_and_rejects_stale_layout_and_stream() {
        let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
        let run = fixture.open(Mode::Ask);
        let workspace = &mut fixture.workspace;
        let panel = DockPreferences {
            placement: Some(DockPlacement::Left),
            width: None,
        };
        workspace.window_settings.ai_agent_panel = panel;
        let resize = DockResize {
            expected: panel,
            side: Slot::Left,
            extent: None,
        };
        let move_to = |width| DockResize {
            extent: Some(width),
            ..resize
        };

        assert!(workspace.resize_agent_dock(run, move_to(430.5)).is_none());
        assert_eq!(workspace.window_settings.ai_agent_panel, panel);
        assert_eq!(
            workspace.terminal_agents.runs[&run]
                .presentation
                .dock_resize,
            Some((panel, 431))
        );
        assert!(workspace.resize_agent_dock(run, resize).is_some());
        assert_eq!(workspace.window_settings.ai_agent_panel.width, Some(431));

        assert!(workspace.resize_agent_dock(run, move_to(590.0)).is_none());
        assert!(
            workspace.terminal_agents.runs[&run]
                .presentation
                .dock_resize
                .is_none()
        );
        let updated = DockResize {
            expected: workspace.window_settings.ai_agent_panel,
            ..resize
        };
        workspace
            .sessions
            .get_mut(&fixture.session)
            .unwrap()
            .stream_generation += 1;
        assert!(
            workspace
                .resize_agent_dock(
                    run,
                    DockResize {
                        extent: Some(550.0),
                        ..updated
                    }
                )
                .is_none()
        );
        assert!(workspace.resize_agent_dock(run, updated).is_none());
        assert_eq!(workspace.window_settings.ai_agent_panel.width, Some(431));
    }
}
