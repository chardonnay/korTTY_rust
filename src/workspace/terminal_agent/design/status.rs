//! Status modules expose existing run actions without creating approvals.

use std::time::{Duration, Instant};

use iced::widget::{Row, button, column, container, progress_bar, row, scrollable, text, tooltip};
use iced::{Alignment, Element, Font, Length, window};

use crate::terminal_agent::design::Status;
use crate::terminal_agent::design::inline::{
    self, Kind, Role, State, Step, StepState, Transcript, compact_tokens,
};

use super::super::{Message as AgentMessage, Phase};
use super::{
    Message, Run, history,
    log_table::{self, Surface, status_run_context, step_context},
    palette::{self, Palette},
    presentation::Notice,
    toast,
};

pub(super) mod hud;
mod meter;
mod metro;
pub(super) mod minimap;
mod neon;
mod orbit;
pub(super) mod powerline;
pub(super) mod top_bar;
use meter::Tone;
pub(super) use meter::adaptive_countdown;

const STEP_SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const STEP_FRAME_MS: u64 = 100;

#[derive(Clone, Copy)]
pub(super) struct Trace<'a> {
    run: Option<&'a Run>,
    started_at: Option<Instant>,
    steps: &'a [Step],
    answer: &'a str,
    elapsed: Duration,
    animation_age: Duration,
    active: bool,
    active_runs: usize,
    waiting: bool,
    paused: bool,
    tone: Tone,
    reduced_motion: bool,
    opacity: f32,
    now: Instant,
}

impl<'a> Trace<'a> {
    pub(super) fn live(run: &'a Run, now: Instant, reduced_motion: bool) -> Self {
        let (transcript, elapsed, started_at) =
            run.status_transcript()
                .map_or((None, Duration::ZERO, None), |(transcript, start)| {
                    let end = run
                        .presentation
                        .status_visibility
                        .finished_at
                        .unwrap_or(now);
                    (
                        Some(transcript),
                        end.saturating_duration_since(start),
                        Some(start),
                    )
                });
        Self {
            run: Some(run),
            started_at,
            steps: transcript.map_or(&[], |trace| trace.steps.as_slice()),
            answer: transcript.map_or("", |trace| trace.final_answer.as_str()),
            elapsed,
            animation_age: elapsed,
            active: run.active() && !run.cancel.requested(),
            active_runs: 0,
            waiting: run.awaiting_input(),
            paused: run.paused || run.phase == Phase::Paused,
            tone: match run.phase {
                Phase::Finished => Tone::Finished,
                Phase::Failed | Phase::Blocked | Phase::Cancelled => Tone::Stopped,
                _ => Tone::Active,
            },
            reduced_motion,
            opacity: 1.0,
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
            started_at: Some(start),
            steps: &transcript.steps,
            answer: &transcript.final_answer,
            elapsed: Duration::from_millis(transcript.elapsed_ms),
            animation_age: now.saturating_duration_since(start),
            active: !transcript.state.finished(),
            active_runs: 0,
            waiting: transcript.prompt.is_some(),
            paused: transcript.state == State::Paused,
            tone: match transcript.state {
                State::Finished => Tone::Finished,
                State::Failed | State::Cancelled => Tone::Stopped,
                _ => Tone::Active,
            },
            reduced_motion,
            opacity: 1.0,
            now,
        }
    }

    pub(super) fn opacified(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
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

    fn spinner(&self) -> Option<&'static str> {
        (self.active && !self.paused && !self.reduced_motion).then(|| {
            let frame = self.animation_age.as_millis() / u128::from(STEP_FRAME_MS);
            STEP_SPINNER[(frame % STEP_SPINNER.len() as u128) as usize]
        })
    }
}

pub(super) fn timeline_header<'a>(
    activity: history::Activity<'a>,
    trace: Trace<'a>,
    palette: Option<Palette>,
) -> Element<'a, AgentMessage> {
    let colors = palette.unwrap_or_else(Palette::default_status);
    let accent = colors.accent.color();
    let muted = colors.muted.color();
    let ink = colors.text.color();
    let icon = move |glyph, label, action| {
        tooltip(
            button(text(glyph).size(11).center())
                .height(21)
                .padding([1, 5])
                .style(move |_, state| {
                    let hovered =
                        matches!(state, button::Status::Hovered | button::Status::Pressed);
                    button::Style {
                        background: hovered.then(|| accent.scale_alpha(0.18).into()),
                        text_color: if hovered { ink } else { muted },
                        border: iced::Border {
                            radius: 4.0.into(),
                            ..iced::Border::default()
                        },
                        ..button::Style::default()
                    }
                })
                .on_press_maybe(action),
            text(label).size(12),
            tooltip::Position::Bottom,
        )
    };
    let hide = trace.run.map_or(
        AgentMessage::Design(Message::HideTimelineSample),
        super::actions::hide_history,
    );
    let usage = usage_label(activity, Some(colors)).or_else(|| {
        activity
            .transcript
            .and_then(|transcript| transcript.tokens)
            .map(|tokens| {
                let label = format!("{} ·", compact_tokens(tokens));
                text(label)
                    .size(11)
                    .style(move |_| text::Style { color: Some(muted) })
                    .into()
            })
    });
    column![
        row![
            container(
                text("◆ AI agent")
                    .size(13)
                    .font(Font {
                        weight: iced::font::Weight::Bold,
                        ..Font::DEFAULT
                    })
                    .wrapping(text::Wrapping::None)
                    .style(move |_| text::Style { color: Some(ink) })
            )
            .width(Length::Fill)
            .clip(true),
            icon(
                if trace.paused { "▶" } else { "Ⅱ" },
                if trace.paused { "Resume" } else { "Pause" },
                (trace.active && !trace.waiting).then_some(AgentMessage::Pause)
            ),
            icon(
                "■",
                "Cancel run",
                trace.active.then_some(AgentMessage::Cancel)
            ),
            log_table::timeline_icon_menu(activity, hide.clone(), |action| {
                icon("⋯", "Run actions", Some(action)).into()
            }),
            icon("✕", "Hide timeline", Some(hide)),
        ]
        .align_y(Alignment::Center)
        .spacing(4),
        row![
            usage,
            text(trace.elapsed_label())
                .size(11)
                .style(move |_| text::Style { color: Some(muted) })
        ]
        .spacing(6),
        container(
            text(history::summary(activity.goal))
                .size(12)
                .wrapping(text::Wrapping::WordOrGlyph)
        )
        .max_height(36)
        .clip(true),
    ]
    .spacing(4)
    .width(Length::Fill)
    .into()
}

pub(super) fn step_interval(steps: &[Step], active: bool, still: bool) -> Option<u64> {
    (active
        && steps.iter().any(|step| {
            step.state == StepState::Running && matches!(step.kind, Kind::Action | Kind::Thinking)
        }))
    .then_some(if still { 1000 } else { STEP_FRAME_MS })
}

pub(super) fn drawer_uses_typed_list(status: Status) -> bool {
    matches!(
        status,
        Status::Metro | Status::Spotlight | Status::Toasts | Status::Orbit
    )
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
        Phase::TriageChoice => ("Choose an approach", 1),
        Phase::SudoPreflight => ("Review sudo authentication", 1),
        Phase::SudoPassword => ("Waiting for sudo password", 1),
        Phase::PlanQuestions | Phase::PlanOptions | Phase::PlanReady | Phase::PlanLibrary => {
            ("Planning review", 1)
        }
        Phase::PlanImplementationReview => ("Review implementation plan", 1),
        Phase::PlanHandedOff => ("Implementation opened", 3),
        Phase::FinalAdvice => ("Preparing final response", 2),
        Phase::Finished => ("Completed", 3),
        Phase::Failed => ("Failed", 3),
        Phase::Blocked => ("Blocked", 3),
        Phase::Cancelled => ("Cancelled", 3),
        _ => ("Working", 2),
    }
}

pub(super) fn view<'a>(
    run: &'a Run,
    status: Status,
    palette: Option<Palette>,
    (now, reduced_motion, opacity): (Instant, bool, f32),
    owner: (&'a super::State, window::Id),
) -> Element<'a, AgentMessage> {
    let (agents, _) = owner;
    let has_archive = agents.has_archived_activity(run);
    let mut trace = Trace::live(run, now, reduced_motion).opacified(opacity);
    if status == Status::Metro {
        trace.active_runs = agents.active_metro_runs(run);
    }
    render(
        history::Activity {
            owner: Some(owner),
            ..run.into()
        },
        phase(run),
        trace,
        status,
        palette,
        has_archive,
        move || super::controls(run, has_archive),
    )
}

pub(super) fn render<'a>(
    run: history::Activity<'a>,
    (phase, stage): (&'static str, usize),
    trace: Trace<'a>,
    status: Status,
    palette: Option<Palette>,
    has_archive: bool,
    controls: impl Fn() -> Element<'a, AgentMessage> + 'a,
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
    let run_context = |trigger| status_run_context(trigger, run, status);
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
        Status::Metro => metro::view(run, phase, &trace, palette),
        Status::Minimap => return minimap::view(run, trace, phase, palette),
        Status::Powerline => {
            return powerline::view(run, phase, trace, palette, has_archive, controls);
        }
        Status::TopBar => return top_bar::view(run, phase, trace, palette, controls),
        Status::Neon => neon::view(run, phase, &trace, palette, has_archive),
        Status::Hud => return hud::view(run, phase, &trace, palette, has_archive, controls),
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
            let badge = button(
                text(format!("Agent · {phase} · {total} steps"))
                    .size(12)
                    .style(heading_style),
            )
            .style(button::secondary)
            .on_press(toggle());
            let mut cards = column![run_context(badge.into())].spacing(5);
            let limit = if trace.waiting { 3 } else { 4 };
            let mut recent: Vec<_> = run
                .presentation
                .status_events
                .recent(trace.now, trace.reduced_motion)
                .rev()
                .take(limit)
                .collect();
            if let Some(id) = run.presentation.log_table.open_step_menu(Surface::Toast)
                && !recent
                    .iter()
                    .any(|(notice, _)| matches!(notice, Notice::Step(step) if *step == id))
            {
                recent.truncate(limit - 1);
                recent.push((Notice::Step(id), toast::review_frame(Duration::MAX, true)));
            }
            for (notice, frame) in recent.into_iter().rev() {
                let (title, step) = match notice {
                    Notice::Step(id) => {
                        let Some(step) = trace.step(id) else { continue };
                        let title =
                            history::summary(step.command.as_deref().unwrap_or(&step.title));
                        (
                            button(text(title).size(12))
                                .on_press(AgentMessage::Design(Message::SelectStep(id))),
                            Some(step),
                        )
                    }
                    Notice::Answer => (
                        button(text(history::summary(trace.answer)).size(12)).on_press(toggle()),
                        None,
                    ),
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
                let card = toast::view(
                    column![title, timer].width(Length::Fill).max_width(320),
                    frame,
                );
                cards = cards.push(if let Some((run, step)) = trace.run.zip(step) {
                    step_context(card, run, step, Surface::Toast)
                } else {
                    card
                });
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
        Status::Orbit => return orbit::view(run, phase, trace, palette, has_archive, controls),
    };
    let header = if matches!(status, Status::Pill | Status::Spotlight) {
        run_context(header)
    } else {
        header
    };
    let drawer = run.presentation.drawer_open();
    let has_own_controls = matches!(status, Status::Metro | Status::Neon);
    if has_own_controls && !drawer {
        if status == Status::Metro && trace.run.is_none() && trace.waiting {
            return column![header, controls()].into();
        }
        return header;
    }
    let simple = matches!(status, Status::Steps | Status::Pill);
    let mut panel = if has_own_controls {
        column![header]
    } else {
        let action_menu = trace
            .run
            .and_then(|run| super::actions::run_menu(run, has_archive));
        column![header, usage_label(run, palette), action_menu]
    }
    .spacing(5);
    if drawer && !simple {
        if drawer_uses_typed_list(status) {
            panel = panel.push(trace_drawer(
                &trace,
                run.presentation.selected_step,
                124.into(),
                Surface::StatusStepList,
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

pub(super) fn step_list(
    run: &Run,
    surface: Surface,
    reduced_motion: bool,
) -> Element<'_, AgentMessage> {
    trace_drawer(
        &Trace::live(run, Instant::now(), reduced_motion),
        run.presentation.selected_step,
        Length::Fill,
        surface,
    )
}

pub(super) fn step_metadata(step: &Step, now: Instant) -> Option<String> {
    if step.state == StepState::Running && matches!(step.kind, Kind::Action | Kind::Thinking) {
        return Some(step.elapsed_at(now).map_or_else(
            || "Running".into(),
            |elapsed| format!("Running {} s", elapsed / 1000),
        ));
    }
    match step.kind {
        Kind::Action => Some(step.exit_code.map_or_else(
            || super::log_table::state_label(step).into(),
            |code| match step.output.len() {
                0 => format!("Exit {code}"),
                1 => format!("Exit {code} · 1 line"),
                lines => format!("Exit {code} · {lines} lines"),
            },
        )),
        Kind::Thinking => step.tokens.map(compact_tokens),
        _ => None,
    }
}

fn trace_drawer<'a>(
    trace: &Trace<'a>,
    selected_id: Option<u64>,
    height: Length,
    surface: Surface,
) -> Element<'a, AgentMessage> {
    let selected = selected_id
        .and_then(|id| trace.steps.iter().find(|step| step.id == id))
        .or_else(|| trace.steps.last());
    let mut rows = column![].spacing(3);
    for step in trace.steps {
        let symbol = inline::marker(
            step,
            trace.spinner().unwrap_or(if step.kind == Kind::Thinking {
                "◇"
            } else {
                "›"
            }),
        );
        let marker_style =
            |role| move |theme: &iced::Theme| super::plan_execution::marker_style(theme, role);
        let title = step.command.as_deref().unwrap_or(&step.title);
        let label = text(history::summary(title))
            .font(if step.command.is_some() {
                Font::MONOSPACE
            } else {
                Font::DEFAULT
            })
            .size(12)
            .wrapping(text::Wrapping::None);
        let mut contents = row![
            text(symbol.text)
                .size(12)
                .width(14)
                .style(marker_style(symbol.role)),
            container(label).width(Length::Fill).clip(true),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .clip(true);
        if let Some(metadata) = step_metadata(step, trace.now) {
            contents = contents.push(
                container(
                    text(metadata)
                        .size(11)
                        .wrapping(text::Wrapping::None)
                        .style(marker_style(Role::Muted)),
                )
                .max_width(148)
                .clip(true),
            );
        }
        let chosen = selected.is_some_and(|selected| selected.id == step.id);
        let row = button(contents)
            .width(Length::Fill)
            .style(if chosen {
                button::secondary
            } else {
                button::text
            })
            .on_press(AgentMessage::Design(Message::SelectStep(step.id)))
            .into();
        rows = rows.push(if let Some(run) = trace.run {
            step_context(row, run, step, surface)
        } else {
            row
        });
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

fn bar_controls<'a>(
    trace: &Trace<'a>,
    has_archive: bool,
    drawer: bool,
    compact: bool,
    toggle: Message,
) -> Option<Row<'a, AgentMessage>> {
    let run = trace.run?;
    let details = || {
        button(if drawer { "Close details" } else { "Details" })
            .padding(3)
            .style(button::text)
            .on_press(AgentMessage::Design(toggle))
    };
    let mut controls = row![].spacing(6).align_y(Alignment::Center);
    if trace.waiting {
        controls = controls.push(
            button("Review…")
                .style(button::text)
                .padding(3)
                .on_press(AgentMessage::Focus),
        );
    }
    if compact {
        controls = controls.push(details());
    }
    for (label, title, message) in [
        (
            if trace.paused { "▶" } else { "Ⅱ" },
            if trace.paused { "Resume" } else { "Pause" },
            (trace.active && !trace.waiting).then_some(AgentMessage::Pause),
        ),
        (
            "■",
            "Cancel run",
            trace.active.then_some(AgentMessage::Cancel),
        ),
        (
            "×",
            "Hide Agent status",
            Some(super::actions::hide_status(run)),
        ),
    ] {
        if compact && message.is_none() {
            continue;
        }
        controls = controls.push(tooltip(
            button(label)
                .style(button::text)
                .padding(3)
                .on_press_maybe(message),
            text(title).size(12),
            tooltip::Position::Top,
        ));
    }
    if let Some(menu) = super::actions::run_menu(run, has_archive) {
        controls = controls.push(menu);
    }
    if !compact {
        controls = controls.push(details());
    }
    Some(controls)
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

    #[test]
    fn step_rows_use_measured_results_elapsed_time_and_reported_tokens() {
        let now = Instant::now();
        let mut step = Step {
            kind: Kind::Action,
            started_at: Some(now),
            state: StepState::Running,
            ..Step::default()
        };
        let later = now + Duration::from_millis(3_250);
        assert_eq!(step_metadata(&step, later).as_deref(), Some("Running 3 s"));
        assert_eq!(
            step_interval(std::slice::from_ref(&step), true, false),
            Some(100)
        );
        assert_eq!(
            step_interval(std::slice::from_ref(&step), true, true),
            Some(1000)
        );
        step.state = StepState::Failed;
        step.exit_code = Some(7);
        step.output = vec!["One".into(), "Two".into()];
        assert_eq!(
            step_metadata(&step, later).as_deref(),
            Some("Exit 7 · 2 lines")
        );
        assert_eq!(
            step_interval(std::slice::from_ref(&step), true, false),
            None
        );
        step.kind = Kind::Thinking;
        step.exit_code = None;
        step.state = StepState::Completed;
        assert_eq!(step_metadata(&step, later), None);
        step.tokens = Some(0);
        assert_eq!(step_metadata(&step, later).as_deref(), Some("0 tokens"));
        step.kind = Kind::Question;
        step.state = StepState::Running;
        assert_eq!(step_metadata(&step, later), None);
    }
}
