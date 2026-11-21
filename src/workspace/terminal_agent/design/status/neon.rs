//! The d18 departure-board ticker and its local animation clock.

use std::f32::consts::PI;
use std::time::{Duration, Instant};

use iced::advanced::{
    Clipboard, Layout, Shell, Widget,
    graphics::geometry::Renderer as _,
    layout::{self, Limits, Node},
    renderer::{self, Renderer as _},
    widget::{Operation, Tree, tree},
};
use iced::font::Weight;
use iced::widget::{button, canvas, container, row, text, tooltip};
use iced::{
    Alignment, Border, Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Renderer,
    Size, Theme, Vector, alignment, mouse, theme, window,
};

use super::{AgentMessage, Trace, history, palette};

const HEIGHT: f32 = 28.0;
const SWAP: Duration = Duration::from_millis(320);
const FLICKER: Duration = Duration::from_millis(360);
const TICK: Duration = Duration::from_millis(16);

#[derive(Clone, Debug, Eq, PartialEq)]
struct Label {
    text: String,
    review: bool,
}

struct Board {
    label: Label,
    run_started: Option<Instant>,
    active: bool,
    paused: bool,
    reduced_motion: bool,
}

#[derive(Default)]
struct Motion {
    run_started: Option<Instant>,
    shown_at: Option<Instant>,
    current: Option<Label>,
    outgoing: Option<Label>,
    changed_at: Option<Instant>,
}

impl Motion {
    fn sync(&mut self, board: &Board, now: Instant) {
        if self.run_started != board.run_started || self.shown_at.is_none() {
            *self = Self {
                run_started: board.run_started,
                shown_at: Some(now),
                ..Self::default()
            };
        }
        if self.current.as_ref() != Some(&board.label) {
            self.outgoing = self.current.replace(board.label.clone());
            self.changed_at = self.outgoing.as_ref().map(|_| now);
        }
        if board.reduced_motion
            || self
                .changed_at
                .is_some_and(|at| now.saturating_duration_since(at) >= SWAP)
        {
            self.outgoing = None;
            self.changed_at = None;
        }
    }

    fn animating(&self, board: &Board, now: Instant) -> bool {
        !board.reduced_motion
            && ((board.active && !board.paused)
                || self.changed_at.is_some()
                || self
                    .shown_at
                    .is_some_and(|at| now.saturating_duration_since(at) < FLICKER))
    }
}

fn flicker(age: Duration) -> f32 {
    const LEVELS: [(u128, f32); 6] = [
        (0, 0.0),
        (90, 1.0),
        (140, 0.25),
        (210, 1.0),
        (280, 0.55),
        (360, 1.0),
    ];
    let now = age.as_millis();
    for segment in LEVELS.windows(2) {
        let [(start, from), (end, to)] = segment else {
            continue;
        };
        if now <= *end {
            return from + (to - from) * ((now - start) as f32 / (end - start) as f32);
        }
    }
    1.0
}

fn lamp(age: Duration, board: &Board) -> f32 {
    if !board.active {
        0.4
    } else if board.paused || board.reduced_motion {
        1.0
    } else {
        let phase = (age.as_millis() % 1200) as f32 / 600.0;
        0.65 + 0.35 * (phase * PI).cos()
    }
}

fn travel(age: Duration) -> f32 {
    let t = (age.as_secs_f32() / SWAP.as_secs_f32()).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn board_color(theme: &Theme) -> Color {
    let base = theme.palette().background;
    if theme::palette::is_dark(base) {
        Color::from_rgb8(0x03, 0x07, 0x0c)
    } else {
        base
    }
}

fn glow_color(theme: &Theme) -> Color {
    let colors = theme.palette();
    if theme::palette::is_dark(colors.background) {
        let c = colors.primary;
        Color::from_rgb(c.r * 0.55 + 0.45, c.g * 0.55 + 0.45, c.b * 0.55 + 0.45)
    } else {
        let c = colors.primary;
        Color::from_rgb(c.r * 0.72, c.g * 0.72, c.b * 0.72)
    }
}

fn paint_label(frame: &mut canvas::Frame, label: &str, x: f32, y: f32, color: Color) {
    frame.fill_text(canvas::Text {
        content: label.into(),
        position: Point::new(x, y),
        align_y: alignment::Vertical::Center,
        color,
        size: Pixels(12.0),
        font: Font {
            weight: Weight::Bold,
            ..Font::MONOSPACE
        },
        ..canvas::Text::default()
    });
}

impl Widget<AgentMessage, Theme, Renderer> for Board {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Motion>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Motion::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fixed(HEIGHT))
    }

    fn layout(&mut self, _: &mut Tree, _: &Renderer, limits: &Limits) -> Node {
        layout::atomic(limits, Length::Fill, Length::Fixed(HEIGHT))
    }

    fn operate(
        &mut self,
        _: &mut Tree,
        layout: Layout<'_>,
        _: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.text(None, layout.bounds(), &self.label.text);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        _: Layout<'_>,
        _: mouse::Cursor,
        _: &Renderer,
        _: &mut dyn Clipboard,
        shell: &mut Shell<'_, AgentMessage>,
        _: &Rectangle,
    ) {
        let Event::Window(window::Event::RedrawRequested(now)) = event else {
            return;
        };
        let motion = tree.state.downcast_mut::<Motion>();
        motion.sync(self, *now);
        if motion.animating(self, *now) {
            shell.request_redraw_at(*now + TICK);
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _: &renderer::Style,
        layout: Layout<'_>,
        _: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let Some(visible) = bounds.intersection(viewport) else {
            return;
        };
        let motion = tree.state.downcast_ref::<Motion>();
        let now = Instant::now();
        let elapsed = motion
            .shown_at
            .map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
        let alpha = if self.reduced_motion || motion.shown_at.is_none() {
            1.0
        } else {
            flicker(elapsed)
        };
        let foreground = glow_color(theme);
        let amber = if theme::palette::is_dark(theme.palette().background) {
            Color::from_rgb8(0xff, 0xc1, 0x64)
        } else {
            Color::from_rgb8(0x87, 0x45, 0x00)
        };
        let lamp_color = if self.label.review { amber } else { foreground };
        let core = lamp_color.scale_alpha(alpha * lamp(elapsed, self));
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let center = Point::new(10.0, HEIGHT / 2.0);
        for (radius, opacity) in [(8.0, 0.12), (6.0, 0.22), (4.0, 1.0)] {
            frame.fill(
                &canvas::Path::circle(center, radius),
                core.scale_alpha(opacity),
            );
        }
        paint_label(
            &mut frame,
            "AGENT",
            22.0,
            HEIGHT / 2.0,
            foreground.scale_alpha(alpha),
        );
        let x = 70.0;
        if bounds.width > x {
            let color = |label: &Label, opacity| {
                (if label.review { amber } else { foreground }).scale_alpha(alpha * opacity)
            };
            let current = &self.label;
            if !self.reduced_motion
                && let (Some(old), Some(at)) = (&motion.outgoing, motion.changed_at)
                && motion.current.as_ref() == Some(current)
            {
                let progress = travel(now.saturating_duration_since(at));
                paint_label(
                    &mut frame,
                    &old.text,
                    x,
                    HEIGHT * (0.5 - progress),
                    color(old, 1.0 - progress),
                );
                paint_label(
                    &mut frame,
                    &current.text,
                    x,
                    HEIGHT * (1.5 - progress),
                    color(current, 1.0),
                );
            } else {
                paint_label(
                    &mut frame,
                    &current.text,
                    x,
                    HEIGHT / 2.0,
                    color(current, 1.0),
                );
            }
        }
        renderer.with_layer(visible, |renderer| {
            renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
                renderer.draw_geometry(frame.into_geometry());
            });
        });
    }
}

pub(super) fn view<'a>(
    activity: history::Activity<'a>,
    phase: &str,
    trace: &Trace<'a>,
    palette: Option<palette::Palette>,
    has_archive: bool,
) -> Element<'a, AgentMessage> {
    let value = if trace.active || trace.waiting || trace.answer.is_empty() {
        trace.current_label(phase)
    } else {
        history::summary(trace.answer)
    };
    let prefix = match trace.tone {
        super::Tone::Stopped => "× ",
        super::Tone::Finished => "✓ ",
        _ if trace.waiting => "",
        _ => "▸ ",
    };
    let board = Board {
        label: Label {
            text: format!("{prefix}{value}").to_uppercase(),
            review: trace.waiting,
        },
        run_started: trace.started_at,
        active: trace.active,
        paused: trace.paused,
        reduced_motion: trace.reduced_motion,
    };
    let (done, total) = trace.progress();
    let elapsed = trace.elapsed_label();
    let mut ticker = row![
        Element::new(board),
        text(format!("{done}/{total} · {elapsed}"))
            .size(11)
            .font(Font::MONOSPACE),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    if let Some(usage) = super::usage_label(activity, palette) {
        ticker = ticker.push(usage);
    }
    if let Some(run) = trace.run {
        if trace.waiting {
            ticker = ticker.push(
                button("Review…")
                    .style(button::text)
                    .padding(3)
                    .on_press(AgentMessage::Focus),
            );
        }
        let pause = if trace.paused { "Resume" } else { "Pause" };
        for (label, title, message) in [
            (
                if trace.paused { "▶" } else { "Ⅱ" },
                pause,
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
                Some(super::super::actions::hide_status(run)),
            ),
        ] {
            ticker = ticker.push(tooltip(
                button(label)
                    .style(button::text)
                    .padding(3)
                    .on_press_maybe(message),
                text(title).size(12),
                tooltip::Position::Top,
            ));
        }
        if let Some(menu) = super::super::actions::run_menu(run, has_archive) {
            ticker = ticker.push(menu);
        }
        ticker = ticker.push(
            button(if activity.presentation.drawer {
                "Close details"
            } else {
                "Details"
            })
            .padding(3)
            .style(button::text)
            .on_press(AgentMessage::Design(super::Message::ToggleDrawer)),
        );
    }
    palette::scope(
        palette,
        container(ticker)
            .padding([0, 6])
            .width(Length::Fill)
            .style(|theme: &Theme| container::Style {
                background: Some(board_color(theme).into()),
                border: Border {
                    width: 1.0,
                    color: theme.palette().primary.scale_alpha(0.45),
                    ..Border::default()
                },
                ..container::Style::default()
            })
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neon_roll_and_pulse_stop_in_reduced_motion() {
        let now = Instant::now();
        let mut board = Board {
            label: Label {
                text: "FIRST".into(),
                review: false,
            },
            run_started: Some(now),
            active: false,
            paused: false,
            reduced_motion: false,
        };
        let mut motion = Motion::default();
        motion.sync(&board, now);
        let next = now + FLICKER;
        assert!(!motion.animating(&board, next));
        board.label = Label {
            text: "REVIEW".into(),
            review: true,
        };
        motion.sync(&board, next);
        motion.sync(&board, next + Duration::from_millis(30));
        assert_eq!(motion.outgoing.as_ref().unwrap().text, "FIRST");
        assert_eq!(motion.changed_at, Some(next));
        assert!(motion.animating(&board, next));
        assert!((0.0..1.0).contains(&travel(SWAP / 2)));
        assert_eq!(travel(SWAP), 1.0);

        board.active = true;
        assert!((lamp(Duration::from_millis(600), &board) - 0.3).abs() < 0.001);
        board.reduced_motion = true;
        motion.sync(&board, next);
        assert!(motion.outgoing.is_none());
        assert!(!motion.animating(&board, next));
        assert_eq!(lamp(Duration::from_millis(600), &board), 1.0);
    }
}
