//! Status modules expose existing run actions without creating approvals.

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use iced::widget::{
    button, column, container, mouse_area, progress_bar, row, scrollable, text, tooltip,
};
use iced::{Alignment, Element, Font, Length};

use crate::terminal_agent::design::Status;
use crate::terminal_agent::design::inline::{State, Step, StepState, Transcript};

use super::super::{Message as AgentMessage, Phase};
use super::{
    Message, Run, history,
    palette::{self, Palette},
    presentation::Notice,
    toast,
};

mod meter;
pub(super) use meter::adaptive_countdown;
use meter::{Ring, Tone};

pub(super) struct Trace<'a> {
    run: Option<&'a Run>,
    steps: &'a [Step],
    answer: &'a str,
    elapsed: Duration,
    animation_age: Duration,
    active: bool,
    waiting: bool,
    paused: bool,
    tone: Tone,
    reduced_motion: bool,
    now: Instant,
}

impl<'a> Trace<'a> {
    fn live(run: &'a Run, now: Instant, reduced_motion: bool) -> Self {
        let (transcript, elapsed) =
            run.status_transcript()
                .map_or((None, Duration::ZERO), |(transcript, start)| {
                    let end = run
                        .presentation
                        .status_visibility
                        .finished_at
                        .unwrap_or(now);
                    (Some(transcript), end.saturating_duration_since(start))
                });
        Self {
            run: Some(run),
            steps: transcript.map_or(&[], |trace| trace.steps.as_slice()),
            answer: transcript.map_or("", |trace| trace.final_answer.as_str()),
            elapsed,
            animation_age: elapsed,
            active: run.active() && !run.cancel.requested(),
            waiting: run.awaiting_input(),
            paused: run.paused || run.phase == Phase::Paused,
            tone: match run.phase {
                Phase::Finished => Tone::Finished,
                Phase::Failed | Phase::Blocked | Phase::Cancelled => Tone::Stopped,
                _ => Tone::Active,
            },
            reduced_motion,
            now,
        }
    }

    pub(super) fn sample(
        transcript: &'a Transcript,
        start: Instant,
        now: Instant,
        reduced_motion: bool,
    ) -> Self {
        Self {
            run: None,
            steps: &transcript.steps,
            answer: &transcript.final_answer,
            elapsed: Duration::from_millis(transcript.elapsed_ms),
            animation_age: now.saturating_duration_since(start),
            active: !transcript.state.finished(),
            waiting: transcript.prompt.is_some(),
            paused: transcript.state == State::Paused,
            tone: match transcript.state {
                State::Finished => Tone::Finished,
                State::Failed | State::Cancelled => Tone::Stopped,
                _ => Tone::Active,
            },
            reduced_motion,
            now,
        }
    }

    fn step(&self, id: u64) -> Option<&Step> {
        self.steps.iter().find(|step| step.id == id)
    }

    fn progress(&self) -> (usize, usize) {
        let finished = self
            .steps
            .iter()
            .filter(|step| step.state != StepState::Running)
            .count();
        (finished, self.steps.len())
    }

    fn current_label(&self, phase: &str) -> String {
        if self.waiting {
            return format!("? {phase}");
        }
        if !self.active || self.paused || phase != "Working" {
            return phase.to_owned();
        }
        if let Some(step) = self
            .steps
            .iter()
            .rev()
            .find(|step| step.state == StepState::Running)
        {
            return history::summary(step.command.as_deref().unwrap_or(&step.title));
        }
        if let Some(step) = self.steps.last() {
            return history::summary(&step.title);
        }
        phase.to_owned()
    }

    fn elapsed_label(&self) -> String {
        let seconds = self.elapsed.as_secs();
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

pub(super) fn phase(run: &Run) -> (&'static str, usize) {
    if run.cancel.requested() && run.phase.working() {
        return ("Cancelling", 2);
    }
    if run.paused && !run.phase.finished() {
        return ("Paused", 2);
    }
    if run.phase.deciding() && run.presentation.model_usage.repairing(run.epoch) {
        return ("Repairing response", 2);
    }
    match run.phase {
        Phase::Setup | Phase::Directory | Phase::SavedPreflight => ("Setup", 0),
        Phase::ModelReview => (
            run.model_review
                .as_ref()
                .map_or("Review model request", |review| review.title()),
            1,
        ),
        Phase::DirectoryConfirmation => ("Confirm directory", 1),
        Phase::ReviewedDirectory { .. } => ("Checking directory", 2),
        Phase::ProbeApproval => ("Review the environment probe", 1),
        Phase::Approval => ("Review the command", 1),
        Phase::Question => ("Answer the agent", 1),
        Phase::SudoPassword => ("Waiting for sudo password", 1),
        Phase::PlanQuestions | Phase::PlanOptions | Phase::PlanReady | Phase::PlanLibrary => {
            ("Planning review", 1)
        }
        Phase::FinalAdvice => ("Preparing final response", 2),
        Phase::Finished => ("Completed", 3),
        Phase::Failed => ("Failed", 3),
        Phase::Blocked => ("Blocked", 3),
        Phase::Cancelled => ("Cancelled", 3),
        _ => ("Working", 2),
    }
}

fn station(
    run: history::Activity<'_>,
    index: usize,
    vertical: bool,
    palette: Option<Palette>,
) -> Element<'_, AgentMessage> {
    let selected = run.presentation.selection(run.activity.len()) == index;
    let marker = if selected { "●" } else { "○" };
    let color = palette.map(|palette| {
        if selected {
            palette.accent.color()
        } else {
            palette.muted.color()
        }
    });
    tooltip(
        button(
            text(if vertical {
                format!("{marker} {}", index + 1)
            } else {
                marker.to_owned()
            })
            .size(13)
            .style(move |_| text::Style { color }),
        )
        .style(button::text)
        .on_press(AgentMessage::Design(Message::Select(
            run.presentation.identity(index),
        ))),
        container(
            text(
                run.activity
                    .get(index)
                    .map_or("No activity yet.", String::as_str),
            )
            .size(12),
        )
        .padding(7)
        .max_width(420)
        .style(container::bordered_box),
        if vertical {
            tooltip::Position::Left
        } else {
            tooltip::Position::Top
        },
    )
    .into()
}

pub(super) fn view(
    run: &Run,
    status: Status,
    palette: Option<Palette>,
    now: Instant,
    reduced_motion: bool,
) -> Element<'_, AgentMessage> {
    render(
        run.into(),
        phase(run),
        Trace::live(run, now, reduced_motion),
        status,
        palette,
        || super::controls(run),
    )
}

pub(super) fn render<'a>(
    run: history::Activity<'a>,
    (phase, stage): (&'static str, usize),
    trace: Trace<'a>,
    status: Status,
    palette: Option<Palette>,
    controls: impl Fn() -> Element<'a, AgentMessage>,
) -> Element<'a, AgentMessage> {
    let accent = palette.map(|palette| palette.accent.color());
    let heading_color = if matches!(trace.tone, Tone::Stopped) {
        palette.map(|palette| palette.error.color())
    } else {
        None
    };
    let heading_style = move |_: &iced::Theme| text::Style {
        color: heading_color,
    };
    let toggle = || AgentMessage::Design(Message::ToggleDrawer);
    let header: Element<'_, AgentMessage> = match status {
        Status::Hidden => return column![].into(),
        Status::Steps | Status::Pill => row![
            super::status_indicator_with_colors(status, phase, stage, heading_color, accent),
            button("Agent activity")
                .style(button::text)
                .on_press(AgentMessage::Focus),
        ]
        .spacing(7)
        .align_y(Alignment::Center)
        .wrap()
        .into(),
        Status::Metro => {
            let mut stations = row![
                text(format!("Agent · {phase}"))
                    .size(12)
                    .style(heading_style)
            ]
            .spacing(3)
            .align_y(Alignment::Center);
            let first = run.activity.len().saturating_sub(8);
            if first > 0 {
                stations = stations.push(
                    button(text(format!("+{first}")))
                        .style(button::text)
                        .on_press(AgentMessage::Design(Message::Select(
                            run.presentation.identity(first - 1),
                        ))),
                );
            }
            for index in first..run.activity.len() {
                stations = stations.push(station(run, index, false, palette));
            }
            stations
                .push(button("Details").style(button::text).on_press(toggle()))
                .wrap()
                .into()
        }
        Status::Minimap => {
            let mut rail = column![text(phase).size(11).style(heading_style)].spacing(2);
            if let Some(usage) = usage_label(run, palette) {
                rail = rail.push(usage);
            }
            for index in 0..run.activity.len() {
                rail = rail.push(station(run, index, true, palette));
            }
            let rail = column![
                scrollable(rail).height(Length::Fill),
                button("Activity")
                    .style(button::text)
                    .on_press(AgentMessage::Focus),
            ]
            .spacing(3);
            return if palette.is_some() {
                palette::panel(palette, container(rail))
            } else {
                rail.into()
            };
        }
        Status::Powerline => row![
            container(text(" AGENT ").size(12))
                .padding(4)
                .style(container::rounded_box),
            text("❯").size(15),
            text(phase).size(12).style(heading_style),
            text("❯").size(15),
            text(format!("{} activities", run.activity.len())).size(12),
            button(if run.presentation.drawer {
                "Close details"
            } else {
                "Details"
            })
            .style(button::text)
            .on_press(toggle()),
        ]
        .spacing(5)
        .align_y(Alignment::Center)
        .wrap()
        .into(),
        Status::TopBar => row![
            text(format!("Agent · {phase}"))
                .size(13)
                .style(heading_style),
            button(if run.presentation.drawer {
                "Close drawer"
            } else {
                "Open drawer"
            })
            .style(button::text)
            .on_press(toggle()),
            button("Activity window")
                .style(button::text)
                .on_press(AgentMessage::Focus),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .wrap()
        .into(),
        Status::Neon => row![
            text(format!("[{}]", phase.to_uppercase()))
                .font(Font::MONOSPACE)
                .size(12)
                .style(heading_style),
            text(run.activity.back().map_or_else(
                || "Waiting for activity".into(),
                |entry| history::summary(entry)
            ))
            .font(Font::MONOSPACE)
            .size(12),
            button("Details").style(button::text).on_press(toggle()),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .wrap()
        .into(),
        Status::Hud => column![
            text(format!("Agent · {phase}"))
                .size(14)
                .style(heading_style),
            text(history::summary(run.goal)).size(12),
            text(
                run.activity
                    .back()
                    .map_or_else(String::new, |entry| history::summary(entry))
            )
            .size(12),
            button("Show activity")
                .style(button::text)
                .on_press(toggle()),
        ]
        .spacing(5)
        .into(),
        Status::Spotlight => {
            let (finished, total) = trace.progress();
            button(
                column![
                    text(trace.current_label(phase))
                        .size(13)
                        .style(heading_style),
                    text(format!(
                        "{finished}/{total} steps · {}",
                        trace.elapsed_label()
                    ))
                    .size(11),
                ]
                .spacing(2),
            )
            .style(button::secondary)
            .on_press(toggle())
            .into()
        }
        Status::Toasts => {
            let (_, total) = trace.progress();
            let mut cards = column![
                button(
                    text(format!("Agent · {phase} · {total} steps"))
                        .size(12)
                        .style(heading_style)
                )
                .style(button::secondary)
                .on_press(toggle()),
            ]
            .spacing(5);
            let limit = if trace.waiting { 3 } else { 4 };
            let recent: Vec<_> = run
                .presentation
                .status_events
                .recent(trace.now, trace.reduced_motion)
                .rev()
                .take(limit)
                .collect();
            for (notice, frame) in recent.into_iter().rev() {
                let title = match notice {
                    Notice::Step(id) => {
                        let Some(step) = trace.step(id) else { continue };
                        let title =
                            history::summary(step.command.as_deref().unwrap_or(&step.title));
                        button(text(title).size(12))
                            .on_press(AgentMessage::Design(Message::SelectStep(id)))
                    }
                    Notice::Answer => {
                        button(text(history::summary(trace.answer)).size(12)).on_press(toggle())
                    }
                };
                let title = title
                    .style(move |theme, status| {
                        let mut style = button::secondary(theme, status);
                        style.text_color = style.text_color.scale_alpha(frame.opacity);
                        style.border.color = style.border.color.scale_alpha(frame.opacity);
                        style.shadow.color = style.shadow.color.scale_alpha(frame.opacity);
                        style.background = style.background.map(|bg| bg.scale_alpha(frame.opacity));
                        style
                    })
                    .width(Length::Fill);
                let lifetime = toast::LIFETIME.as_secs_f32();
                let remaining = if trace.reduced_motion {
                    lifetime
                } else {
                    frame.remaining.as_secs_f32()
                };
                let timer = progress_bar(0.0..=lifetime, remaining)
                    .girth(2)
                    .style(move |theme| {
                        let mut style = progress_bar::primary(theme);
                        style.background = style.background.scale_alpha(frame.opacity);
                        style.bar = style.bar.scale_alpha(frame.opacity);
                        style.border.color = style.border.color.scale_alpha(frame.opacity);
                        style
                    });
                cards = cards.push(toast::view(
                    column![title, timer].width(Length::Fill).max_width(320),
                    frame,
                ));
            }
            if trace.waiting {
                let review = toast::review_frame(
                    run.presentation
                        .status_events
                        .review_age(trace.now)
                        .unwrap_or(Duration::MAX),
                    trace.reduced_motion,
                );
                cards = cards.push(toast::view(
                    column![text(trace.current_label(phase)).size(12), controls()].spacing(4),
                    review,
                ));
            }
            cards.into()
        }
        Status::Orbit => {
            let (finished, total) = trace.progress();
            let fraction = if matches!(trace.tone, Tone::Finished | Tone::Stopped) {
                1.0
            } else if total == 0 {
                0.0
            } else {
                finished as f32 / total as f32
            };
            let orbit = (trace.active && !trace.waiting).then(|| {
                if trace.reduced_motion || trace.paused {
                    0.0
                } else {
                    trace.animation_age.as_secs_f32() % 1.6 / 1.6 * TAU
                }
            });
            let glyph = meter::graphic(
                72.0,
                Ring {
                    fraction,
                    orbit,
                    tone: trace.tone,
                },
                column![
                    text(format!("{finished}/{total}")).size(13),
                    text(trace.elapsed_label()).size(11),
                ]
                .align_x(Alignment::Center)
                .into(),
            );
            button(
                row![
                    glyph,
                    text(trace.current_label(phase))
                        .size(12)
                        .style(heading_style)
                ]
                .spacing(5)
                .align_y(Alignment::Center),
            )
            .style(button::secondary)
            .on_press(toggle())
            .into()
        }
    };
    let drawer = run.presentation.drawer;
    let simple = matches!(status, Status::Steps | Status::Pill);
    let action_menu = trace.run.and_then(super::actions::run_menu);
    let mut panel = column![header, usage_label(run, palette), action_menu].spacing(5);
    if drawer && !simple {
        if matches!(status, Status::Spotlight | Status::Toasts | Status::Orbit) {
            panel = panel.push(trace_drawer(
                &trace,
                run.presentation.selected_step,
                124.into(),
            ));
        } else {
            let selected = run.presentation.selection(run.activity.len());
            panel = panel.push(
                scrollable(
                    text(
                        run.activity
                            .get(selected)
                            .map_or("No activity yet.", String::as_str),
                    )
                    .size(13),
                )
                .height(140),
            );
        }
        if status != Status::Toasts || !trace.waiting {
            panel = panel.push(controls());
        }
    }
    palette::panel(palette, container(panel).padding(5))
}

pub(super) fn step_list(run: &Run) -> Element<'_, AgentMessage> {
    trace_drawer(
        &Trace::live(run, Instant::now(), true),
        run.presentation.selected_step,
        Length::Fill,
    )
}

fn trace_drawer<'a>(
    trace: &Trace<'a>,
    selected_id: Option<u64>,
    height: Length,
) -> Element<'a, AgentMessage> {
    let selected = selected_id
        .and_then(|id| trace.steps.iter().find(|step| step.id == id))
        .or_else(|| trace.steps.last());
    let mut rows = column![].spacing(3);
    for step in trace.steps {
        let label = history::summary(&step.title);
        let chosen = selected.is_some_and(|selected| selected.id == step.id);
        let select = AgentMessage::Design(Message::SelectStep(step.id));
        rows = rows.push(
            mouse_area(
                button(text(label).size(12))
                    .width(Length::Fill)
                    .style(if chosen {
                        button::secondary
                    } else {
                        button::text
                    })
                    .on_press(select.clone()),
            )
            .on_right_press(select),
        );
        if chosen && !step.detail.is_empty() && step.detail != step.title {
            rows = rows.push(text(&step.detail).size(12));
        }
        if chosen && let Some(run) = trace.run {
            rows = rows.push(super::actions::step_menu(run, step));
        }
    }
    if trace.steps.is_empty() {
        rows = rows.push(text("No steps yet.").size(12));
    }
    scrollable(rows).height(height).into()
}

fn usage_label(
    run: history::Activity<'_>,
    palette: Option<Palette>,
) -> Option<Element<'_, AgentMessage>> {
    let compact = run.presentation.model_usage.compact()?;
    let label = run.presentation.model_usage.label()?;
    let muted = palette.map(|palette| palette.muted.color());
    Some(
        tooltip(
            text(compact)
                .size(11)
                .style(move |_| text::Style { color: muted }),
            container(text(label).size(12)).padding(5).max_width(420),
            tooltip::Position::Bottom,
        )
        .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_status_reports_the_running_command_and_settled_steps() {
        let now = Instant::now();
        let mut transcript = Transcript {
            elapsed_ms: 61_000,
            steps: vec![
                Step {
                    id: 1,
                    title: "Read the directory".into(),
                    state: StepState::Completed,
                    ..Step::default()
                },
                Step {
                    id: 2,
                    title: "Check the volume".into(),
                    command: Some("df -h".into()),
                    state: StepState::Running,
                    ..Step::default()
                },
            ],
            ..Transcript::default()
        };
        let trace = Trace::sample(&transcript, now, now, false);
        assert_eq!(trace.progress(), (1, 2));
        assert_eq!(trace.current_label("Working"), "df -h");
        assert_eq!(trace.elapsed_label(), "1:01");
        transcript.steps[1].state = StepState::Failed;
        transcript.state = State::Failed;
        let trace = Trace::sample(&transcript, now, now, true);
        assert_eq!(trace.progress(), (2, 2));
        assert_eq!(trace.current_label("Failed"), "Failed");
    }
}
