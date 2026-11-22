//! The Orbit ring and step list use the same typed trace in the terminal and preview.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::time::{Duration, Instant};

use iced::widget::{
    button, canvas, column, container, opaque, row, scrollable, space, stack, text,
};
use iced::{
    Alignment, Element, Event, Font, Length, Padding, Point, Rectangle, Renderer, Theme, mouse,
    window,
};

use super::super::presentation::ease_both;
use super::{
    AgentMessage, Message, Palette, Status, Tone, Trace, history, meter, palette,
    status_run_context, trace_drawer, usage_label,
};
use crate::workspace::terminal_agent::design::log_table::Surface;

const SIZE: f32 = 72.0;
const RIGHT: f32 = 14.0;
const BOTTOM: f32 = 12.0;
const LIST_GAP: f32 = 5.0;
const FRAME: Duration = Duration::from_millis(16);
const FILL_TIME: Duration = Duration::from_millis(400);
const ORBIT_SECONDS: f32 = 1.6;
mod bloom;

pub(super) fn view<'a>(
    run: history::Activity<'a>,
    phase: &str,
    trace: Trace<'a>,
    palette: Option<Palette>,
    has_archive: bool,
    controls: impl Fn() -> Element<'a, AgentMessage>,
) -> Element<'a, AgentMessage> {
    let (finished, total) = trace.progress();
    let target = if matches!(trace.tone, Tone::Finished | Tone::Stopped) {
        1.0
    } else if total == 0 {
        0.0
    } else {
        finished as f32 / total as f32
    };
    let key = (trace.run.map(|run| run.started_order), trace.started_at);
    let glyph = meter::graphic(
        SIZE,
        Orbit {
            key,
            target,
            tone: trace.tone,
            visible_dots: trace.active && !trace.waiting,
            paused: trace.paused || trace.waiting,
            reduced_motion: trace.reduced_motion,
        },
        column![
            text(format!("{finished}/{total}")).size(13),
            text(trace.elapsed_label()).size(11),
        ]
        .align_x(Alignment::Center)
        .into(),
    );
    let label = palette::panel(
        palette,
        container(
            text(trace.current_label(phase))
                .font(Font::MONOSPACE)
                .size(11)
                .wrapping(text::Wrapping::None)
                .style(move |theme: &Theme| text::Style {
                    color: if trace.waiting {
                        Some(theme.palette().warning)
                    } else if matches!(trace.tone, Tone::Stopped) {
                        Some(theme.palette().danger)
                    } else {
                        None
                    },
                }),
        )
        .max_width(240)
        .padding([3, 9])
        .clip(true),
    );
    let header = status_run_context(
        button(row![label, glyph].spacing(8).align_y(Alignment::Center))
            .padding(0)
            .style(button::text)
            .on_press(AgentMessage::Design(Message::ToggleDrawer))
            .into(),
        run,
        Status::Orbit,
    );
    let steps = {
        let hidden = trace.steps.len().saturating_sub(12);
        let recent = Trace {
            steps: &trace.steps[hidden..],
            ..trace
        };
        palette::panel(
            palette,
            container(
                column![
                    (hidden > 0).then(|| text(format!("+{hidden} earlier steps")).size(11)),
                    trace_drawer(
                        &recent,
                        run.presentation.selected_step,
                        124.into(),
                        Surface::StatusStepList,
                    ),
                    controls(),
                ]
                .spacing(5),
            )
            .padding(5)
            .max_width(320),
        )
    };
    let actions = trace
        .run
        .and_then(|run| super::super::actions::run_menu(run, has_archive));
    let footer = column![usage_label(run, palette), actions]
        .spacing(5)
        .align_x(Alignment::End);
    let details = Element::new(bloom::Bloom {
        steps: opaque(scrollable(steps).height(Length::Shrink)),
        footer: opaque(footer),
        key,
        open: run.presentation.drawer_open(),
        reduced_motion: trace.reduced_motion,
    });
    palette::scope(
        palette,
        stack![
            space().width(Length::Fill).height(Length::Fill),
            details,
            anchor(opaque(header), BOTTOM),
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
    )
}

fn anchor(content: Element<'_, AgentMessage>, bottom: f32) -> Element<'_, AgentMessage> {
    container(content)
        .align_right(Length::Fill)
        .align_bottom(Length::Fill)
        .padding(Padding {
            top: 0.0,
            right: RIGHT,
            bottom,
            left: 0.0,
        })
        .into()
}

type Key = (Option<u64>, Option<Instant>);

struct Orbit {
    key: Key,
    target: f32,
    tone: Tone,
    visible_dots: bool,
    paused: bool,
    reduced_motion: bool,
}

#[derive(Default)]
struct Motion {
    key: Option<Key>,
    target: f32,
    fill: Option<(f32, Instant)>,
    rotation: Duration,
    rotating_since: Option<Instant>,
}

impl Motion {
    fn sync(&mut self, orbit: &Orbit, now: Instant) -> bool {
        if self.key != Some(orbit.key) {
            *self = Self {
                key: Some(orbit.key),
                ..Self::default()
            };
        }
        if orbit.reduced_motion {
            self.fill = None;
            self.rotation = Duration::ZERO;
            self.rotating_since = None;
        } else {
            if self.target != orbit.target {
                self.fill = Some((self.fraction(now), now));
            }
            if orbit.visible_dots && !orbit.paused {
                self.rotating_since.get_or_insert(now);
            } else if let Some(since) = self.rotating_since.take() {
                self.rotation += now.saturating_duration_since(since);
            }
        }
        self.target = orbit.target;
        if self
            .fill
            .is_some_and(|(_, since)| now.saturating_duration_since(since) >= FILL_TIME)
        {
            self.fill = None;
        }
        self.fill.is_some() || self.rotating_since.is_some()
    }

    fn fraction(&self, now: Instant) -> f32 {
        self.fill.map_or(self.target, |(origin, since)| {
            origin
                + (self.target - origin)
                    * ease_both(now.saturating_duration_since(since), FILL_TIME)
        })
    }

    fn angle(&self, now: Instant) -> f32 {
        let elapsed = self.rotation
            + self
                .rotating_since
                .map_or(Duration::ZERO, |since| now.saturating_duration_since(since));
        (elapsed.as_secs_f32() % ORBIT_SECONDS) / ORBIT_SECONDS * TAU - FRAC_PI_2
    }
}

impl canvas::Program<AgentMessage> for Orbit {
    type State = Motion;

    fn update(
        &self,
        state: &mut Motion,
        event: &Event,
        _: Rectangle,
        _: mouse::Cursor,
    ) -> Option<canvas::Action<AgentMessage>> {
        let Event::Window(window::Event::RedrawRequested(now)) = event else {
            return None;
        };
        state
            .sync(self, *now)
            .then(|| canvas::Action::request_redraw_at(*now + FRAME))
    }

    fn draw(
        &self,
        state: &Motion,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let half = bounds.width.min(bounds.height) / 2.0;
        if half <= 5.0 {
            return Vec::new();
        }
        let now = Instant::now();
        let center = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let radius = half - 5.0;
        let colors = theme.palette();
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let face = canvas::Path::circle(center, half - 0.5);
        frame.fill(&face, colors.background.scale_alpha(0.95));
        frame.stroke(
            &face,
            canvas::Stroke::default().with_color(colors.text.scale_alpha(0.24)),
        );
        let attached = state.key == Some(self.key);
        meter::Ring {
            fraction: if self.reduced_motion {
                self.target
            } else if attached {
                state.fraction(now)
            } else {
                0.0
            },
            tone: self.tone,
        }
        .paint(&mut frame, center, radius, 4.0, theme);
        if self.visible_dots {
            let angle = if attached && !self.reduced_motion {
                state.angle(now)
            } else {
                -FRAC_PI_2
            };
            for index in 0..3 {
                let index = index as f32;
                let (sin, cos) = (angle - (index * 22.0).to_radians()).sin_cos();
                let point = Point::new(
                    center.x + (radius + 0.5) * cos,
                    center.y + (radius + 0.5) * sin,
                );
                frame.fill(
                    &canvas::Path::circle(point, 2.4 - index * 0.5),
                    colors.primary.scale_alpha(1.0 - index * 0.28),
                );
            }
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_eases_from_the_current_ring_and_respects_reduced_motion() {
        let start = Instant::now();
        let mut orbit = Orbit {
            key: (Some(1), Some(start)),
            target: 0.5,
            tone: Tone::Active,
            visible_dots: false,
            paused: false,
            reduced_motion: false,
        };
        let mut motion = Motion::default();
        assert!(motion.sync(&orbit, start));
        assert_eq!(motion.fraction(start), 0.0);
        let middle = start + FILL_TIME / 2;
        let intermediate = motion.fraction(middle);
        assert!((0.0..0.5).contains(&intermediate));
        orbit.target = 1.0;
        assert!(motion.sync(&orbit, middle));
        assert_eq!(motion.fraction(middle), intermediate);
        assert!(!motion.sync(&orbit, middle + FILL_TIME));
        assert_eq!(motion.fraction(middle + FILL_TIME), 1.0);

        orbit.key = (Some(2), Some(start));
        orbit.target = 0.25;
        assert!(motion.sync(&orbit, middle));
        assert_eq!(motion.fraction(middle), 0.0);
        orbit.reduced_motion = true;
        assert!(!motion.sync(&orbit, middle));
        assert_eq!(motion.fraction(middle), 0.25);
    }

    #[test]
    fn dots_hold_their_position_through_a_pause_and_resume_from_it() {
        let start = Instant::now();
        let mut orbit = Orbit {
            key: (Some(1), Some(start)),
            target: 0.0,
            tone: Tone::Active,
            visible_dots: true,
            paused: false,
            reduced_motion: false,
        };
        let mut motion = Motion::default();
        assert!(motion.sync(&orbit, start));
        let pause = start + Duration::from_millis(400);
        let angle = motion.angle(pause);
        orbit.paused = true;
        assert!(!motion.sync(&orbit, pause));
        let resume = pause + Duration::from_secs(2);
        assert_eq!(motion.angle(resume), angle);
        orbit.paused = false;
        assert!(motion.sync(&orbit, resume));
        assert_eq!(motion.angle(resume), angle);
        assert_ne!(motion.angle(resume + FRAME), angle);
        orbit.reduced_motion = true;
        assert!(!motion.sync(&orbit, resume + FRAME));
        assert_eq!(motion.angle(resume + FRAME), -FRAC_PI_2);
    }
}
