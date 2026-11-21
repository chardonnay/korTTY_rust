//! The d09 history renders the masked activity as a companion terminal pane.

use std::time::{Duration, Instant};

use iced::widget::{button, column, container, rich_text, row, scrollable, span, text};
use iced::{Color, Element, Font, Length};

use crate::terminal_agent::design::inline::{
    Kind, Prompt, State, Step, StepState, Transcript, clean,
};
use crate::terminal_view::agent_cursor::Key;

use super::{AgentMessage, Palette, Presentation, history::Activity, log_table, status};

mod review;
pub(super) use review::Review;
pub(crate) use review::ReviewAction;

const CHAR_MS: u64 = 24;
const CURSOR_HALF_CYCLE_MS: u64 = 530;
const CURSOR_FRAME_MS: u64 = 32;

fn typing_age(presentation: &Presentation, now: Instant, reduced_motion: bool) -> Option<Duration> {
    if reduced_motion {
        return None;
    }
    presentation
        .finish_time()
        .map(|finished| now.saturating_duration_since(finished))
}

fn revealed(answer: &str, age: Option<Duration>) -> &str {
    age.and_then(|age| {
        answer
            .char_indices()
            .nth((age.as_millis() / u128::from(CHAR_MS)) as usize)
            .map(|(end, _)| &answer[..end])
    })
    .unwrap_or(answer)
}

pub(super) fn refresh_interval(
    transcript: &Transcript,
    presentation: &Presentation,
    now: Instant,
    reduced_motion: bool,
) -> Option<u64> {
    if reduced_motion {
        return None;
    }
    let answer = transcript.final_answer.trim();
    if revealed(answer, typing_age(presentation, now, false)).len() < answer.len() {
        Some(CHAR_MS)
    } else {
        Some(CURSOR_FRAME_MS)
    }
}

fn cursor_opacity(age: Duration, active: bool) -> f32 {
    if !active {
        return 1.0;
    }
    let cycle = (age.as_millis() % u128::from(2 * CURSOR_HALF_CYCLE_MS)) as f32;
    (cycle / CURSOR_HALF_CYCLE_MS as f32 - 1.0).abs()
}

fn step_marker(step: &Step) -> &'static str {
    match (step.state, step.kind) {
        (StepState::Failed, _) | (_, Kind::Error) => "✕",
        (StepState::Cancelled, _) => "–",
        (StepState::Running, Kind::Question) => "?",
        (StepState::Running, _) => "›",
        (StepState::Completed, Kind::Action) => "✓",
        (StepState::Completed, Kind::Thinking) => "◇",
        (StepState::Completed, _) => "·",
    }
}

pub(super) fn view<'a>(
    activity: Activity<'a>,
    palette: Option<Palette>,
    reduced_motion: bool,
) -> Element<'a, AgentMessage> {
    let mut lines = column![].spacing(5).padding([5, 8]).width(Length::Fill);
    let Some(transcript) = activity.transcript else {
        return lines.into();
    };
    let now = Instant::now();
    let size = activity.activity_font;
    let live = activity.live_run;
    let accent = palette.map(|palette| palette.accent.color());
    let muted = palette.map(|palette| palette.muted.color());
    let phase = live.map_or_else(
        || match transcript.state {
            State::Running if transcript.prompt.is_some() => "Review the command",
            State::Running => "Working",
            State::Paused => "Paused",
            State::Finished => "Completed",
            State::Failed => "Failed",
            State::Cancelled => "Cancelled",
        },
        |run| status::phase(run).0,
    );
    lines = lines.push(
        text(format!("» {}", clean(&transcript.goal, false)))
            .font(Font::MONOSPACE)
            .size(size)
            .style(move |_| text::Style { color: accent }),
    );
    for (index, step) in transcript.steps.iter().enumerate() {
        let result = step
            .exit_code
            .map(|code| format!("Exit {code}"))
            .or_else(|| {
                step.elapsed_at(now)
                    .map(|elapsed| format!("{:.1} s", elapsed as f64 / 1000.0))
            });
        let label = clean(step.command.as_deref().unwrap_or(&step.title), false);
        let content: Element<'_, AgentMessage> = row![
            text(step_marker(step))
                .font(Font::MONOSPACE)
                .size(size)
                .style(move |_| text::Style { color: accent }),
            text(log_table::kind(step))
                .font(Font::MONOSPACE)
                .size(size)
                .style(move |_| text::Style { color: muted }),
            text(label)
                .font(Font::MONOSPACE)
                .size(size)
                .width(Length::Fill),
            result.map(|result| text(result)
                .font(Font::MONOSPACE)
                .size(size)
                .style(move |_| text::Style { color: muted })),
        ]
        .spacing(6)
        .into();
        let entry = if let Some(run) = live {
            log_table::step_context(content, run, step, activity.surface.activity_entry(index))
        } else {
            content
        };
        lines = lines.push(entry);
    }

    let prompt = if let Some(run) = live {
        run.masked_command_review()
    } else {
        transcript.prompt.clone()
    };
    let active = live.map_or(!transcript.state.finished(), super::Run::active);
    let answer = if active {
        ""
    } else {
        transcript.final_answer.trim()
    };
    let age = typing_age(activity.presentation, now, reduced_motion);
    let shown = revealed(answer, age);
    let blink = activity
        .presentation
        .tmux_started
        .or(activity.presentation.finish_time())
        .map_or(Duration::ZERO, |started| {
            now.saturating_duration_since(started)
        });
    let opacity = cursor_opacity(blink, !reduced_motion);
    let cursor = accent
        .unwrap_or(Color::from_rgb8(0x47, 0x97, 0xe1))
        .scale_alpha(opacity);
    lines = lines.push(
        rich_text([span::<(), _>(shown), span("█").color(cursor)])
            .font(Font::MONOSPACE)
            .size(size)
            .width(Length::Fill),
    );

    // Keep the review at the scroll anchor so the cursor cannot crowd out its first row.
    if let Some(Prompt::Approval {
        message, commands, ..
    }) = prompt
    {
        let once = |label, action| {
            button(text(label).font(Font::MONOSPACE).size(size)).on_press_maybe(action)
        };
        let controls: Element<'_, AgentMessage> = if let Some(review) = activity.tmux_review {
            let mut buttons =
                row![once("[1] Allow once", review.action(Key::One, false))].spacing(6);
            if review.remaining() {
                buttons = buttons
                    .push(once(
                        "[2] Allow remaining ordinary commands",
                        review.action(Key::Two, false),
                    ))
                    .push(once("[3] Cancel", review.action(Key::Three, false)));
            } else {
                buttons = buttons.push(once("[2] Cancel", review.action(Key::Two, false)));
            }
            buttons.wrap().into()
        } else if live.is_none() {
            row![
                once("[1] Allow once", Some(AgentMessage::Approve(0))),
                once("[2] Cancel", Some(AgentMessage::Cancel)),
            ]
            .spacing(6)
            .wrap()
            .into()
        } else {
            button("Review in Activity…")
                .on_press(AgentMessage::Focus)
                .into()
        };
        let card = column![
            text(format!("? {}", clean(&message, false)))
                .font(Font::MONOSPACE)
                .size(size),
            text(commands.join("\n")).font(Font::MONOSPACE).size(size),
            controls,
        ]
        .spacing(5)
        .width(Length::Fill);
        if let Some(review) = activity.tmux_review {
            lines = lines.push(review.guard(card.into()));
        } else {
            lines = lines.push(card);
        }
    }
    let clock = jiff::Zoned::now().strftime("%H:%M").to_string();
    let footer = container(
        text(format!("0:shell 1:agent*  {phase} │ {clock}"))
            .font(Font::MONOSPACE)
            .size(size),
    )
    .padding([3, 8])
    .width(Length::Fill)
    .style(|theme: &iced::Theme| {
        let color = theme.extended_palette().primary.strong;
        container::Style {
            background: Some(color.color.into()),
            text_color: Some(color.text),
            ..container::Style::default()
        }
    });
    column![
        scrollable(lines).anchor_bottom().height(Length::Fill),
        footer
    ]
    .height(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_reveals_unicode_answer_then_keeps_blinking_when_motion_is_enabled() {
        let now = Instant::now();
        let mut presentation = Presentation::default();
        presentation.finished(now);
        let transcript = Transcript {
            final_answer: "✓界".into(),
            ..Transcript::default()
        };
        let rendered = |ms, reduced| {
            let time = now + Duration::from_millis(ms);
            revealed(
                &transcript.final_answer,
                typing_age(&presentation, time, reduced),
            )
        };
        assert_eq!(rendered(0, false), "");
        assert_eq!(rendered(24, false), "✓");
        assert_eq!(rendered(48, false), "✓界");
        assert_eq!(rendered(0, true), "✓界");
        assert_eq!(
            refresh_interval(&transcript, &presentation, now, false),
            Some(24)
        );
        assert_eq!(
            refresh_interval(&transcript, &presentation, now, true),
            None
        );
        assert_eq!(
            refresh_interval(
                &transcript,
                &presentation,
                now + Duration::from_millis(48),
                false
            ),
            Some(CURSOR_FRAME_MS)
        );
        assert_eq!(cursor_opacity(Duration::from_millis(530), true), 0.0);
        assert_eq!(cursor_opacity(Duration::from_millis(530), false), 1.0);
    }
}
