//! Paint native agent annotations without giving them terminal cells or input authority.

use std::ops::Range;
use std::time::{Duration, Instant};

use iced::font::Weight;
use iced::widget::canvas::{self, gradient};
use iced::{Color, Font, Pixels, Point, Rectangle, Renderer, Size, Theme, mouse};

use crate::session::Frame;
use crate::terminal_agent::design::inline::{Line, Role};
use crate::terminal_theme::Appearance;
use crate::workspace::Message;

use super::{CanvasFrame, Geometry, INSET, Metrics, TerminalView, default_background, row_cache};

const TRACK: Duration = Duration::from_millis(66);
const FADE: Duration = Duration::from_millis(320);
const GROW: Duration = Duration::from_millis(380);
const LINGER: Duration = Duration::from_millis(900);

pub(super) fn paint_line(
    frame: &mut CanvasFrame,
    view: &row_cache::View<'_>,
    row: u16,
    line: &Line,
    (font, font_size): (Font, u8),
) {
    let screen = &view.frame.screen;
    let foreground = if screen.reverse_video {
        view.appearance.background.color()
    } else {
        view.appearance.foreground.color()
    };
    let background = default_background(screen, &view.appearance);
    let opacity = 1.0 - view.dim_at(row);
    let mut column = 0;
    for segment in &line.segments {
        let color = match segment.role {
            Role::Accent | Role::Key => accent(background),
            Role::Border => mix(foreground, accent(background), 0.6),
            Role::Title | Role::Text => foreground,
            Role::Muted => mix(foreground, background, 0.35),
            Role::Ok => paired(background, 0x3fb68b, 0x1f8a5f),
            Role::Warn => paired(background, 0xe5a84b, 0x986212),
            Role::Error => paired(background, 0xff817d, 0xb42324),
            Role::Think => paired(background, 0xa48cff, 0x6b46c1),
        };
        if !segment.text.trim().is_empty() {
            frame.fill_text(canvas::Text {
                content: segment.text.clone(),
                position: view.metrics.point(row, column),
                size: Pixels(f32::from(font_size)),
                color: Color {
                    a: opacity,
                    ..color
                },
                font: Font {
                    weight: if matches!(segment.role, Role::Title | Role::Key) {
                        Weight::Bold
                    } else {
                        font.weight
                    },
                    ..font
                },
                ..canvas::Text::default()
            });
        }
        let width: usize = segment
            .text
            .chars()
            .filter_map(kortty_terminal::character_cell_width)
            .sum();
        column = column.saturating_add(width as u16);
    }
}

fn paired(background: Color, dark: u32, light: u32) -> Color {
    let luma = 0.299 * background.r + 0.587 * background.g + 0.114 * background.b;
    super::Rgb::from_hex(if luma < 0.5 { dark } else { light }).color()
}

fn accent(background: Color) -> Color {
    paired(background, 0x64b2ff, 0x1769aa)
}

fn mix(from: Color, to: Color, weight: f32) -> Color {
    Color::from_rgb(
        from.r + (to.r - from.r) * weight,
        from.g + (to.g - from.g) * weight,
        from.b + (to.b - from.b) * weight,
    )
}

pub(super) fn next_redraw(view: &TerminalView<'_>, now: Instant) -> Option<Instant> {
    if view.reduced_motion {
        return None;
    }
    let inline = view.frame.inline.as_deref()?;
    let age = inline.focus_age(now)?;
    let wait = if inline.focus_finished() {
        if age < LINGER {
            LINGER - age
        } else {
            TRACK.min((LINGER + FADE).saturating_sub(age))
        }
    } else {
        TRACK
    };
    (!wait.is_zero()).then_some(now + wait)
}

pub(super) struct Focus {
    rows: Range<u16>,
    bright_end: u16,
    opacity: f32,
    growth: f32,
    sweep: Option<f32>,
}

pub(super) fn focus(frame: &Frame, reduced_motion: bool, now: Instant) -> Option<Focus> {
    let inline = frame.inline.as_deref()?;
    let rows = inline.focus()?;
    let age = inline.focus_age(now)?;
    let finished = inline.focus_finished();
    let (opacity, growth, sweep) = if finished {
        if reduced_motion {
            return None;
        }
        (1.0 - progress(age.saturating_sub(LINGER), FADE), 1.0, None)
    } else if reduced_motion {
        (1.0, 1.0, Some(0.5))
    } else {
        let phase = (age.as_secs_f32() / TRACK.as_secs_f32() * 0.035) % 1.4;
        (progress(age, FADE), progress(age, GROW), Some(phase - 0.2))
    };
    let bright_end = rows.end.max(inline.focus_bright_end());
    (opacity > 0.0).then_some(Focus {
        rows,
        bright_end,
        opacity,
        growth,
        sweep,
    })
}

impl Focus {
    pub(super) fn dim_at(&self, row: u16) -> f32 {
        if (self.rows.start..self.bright_end).contains(&row) {
            0.0
        } else {
            self.dimming()
        }
    }

    fn dimming(&self) -> f32 {
        0.58 * self.opacity
    }

    pub(super) fn draw(
        &self,
        terminal: &Frame,
        appearance: Appearance,
        renderer: &Renderer,
        size: Size,
        cell: Metrics,
    ) -> Geometry {
        let y = |row| cell.point(row, 0).y.min(size.height);
        let top = y(self.rows.start);
        let bottom = y(self.rows.end);
        let bright_bottom = y(self.bright_end);
        let background = default_background(&terminal.screen, &appearance);
        let dim = Color {
            a: self.dimming(),
            ..background
        };
        let accent = Color {
            a: self.opacity,
            ..accent(background)
        };
        let mut frame = CanvasFrame::new(renderer, size);
        if top > 0.0 {
            frame.fill_rectangle(Point::ORIGIN, Size::new(size.width, top), dim);
        }
        if bright_bottom < size.height {
            frame.fill_rectangle(
                Point::new(0.0, bright_bottom),
                Size::new(size.width, size.height - bright_bottom),
                dim,
            );
        }
        let height = ((bottom - top - 2.0) * self.growth).max(0.0);
        if height > 0.0 {
            frame.fill_rectangle(
                Point::new(INSET, top + 1.0),
                Size::new(7.0, height),
                Color {
                    a: self.opacity * 0.18,
                    ..accent
                },
            );
            frame.fill_rectangle(
                Point::new(INSET + 2.0, top + 1.0),
                Size::new(3.0, height),
                accent,
            );
        }
        if let Some(sweep) = self.sweep.filter(|_| bottom > top) {
            let width =
                (size.width - 2.0 * INSET).min(cell.width * f32::from(terminal.screen.columns));
            let center = INSET + width * sweep;
            let radius = width * 0.18;
            let start = (center - radius).max(INSET);
            let end = (center + radius).min(INSET + width);
            if start < end {
                let y = bottom - 1.0;
                let transparent = Color { a: 0.0, ..accent };
                let fill = gradient::Linear::new(
                    Point::new(center - radius, y),
                    Point::new(center + radius, y),
                )
                .add_stop(0.0, transparent)
                .add_stop(0.5, accent)
                .add_stop(1.0, transparent);
                frame.fill_rectangle(Point::new(start, y), Size::new(end - start, 2.0), fill);
            }
        }
        frame.into_geometry()
    }
}

/// On a Kitty stack, tint the composited text and all three graphics layers together.
pub(super) struct Overlay<'a> {
    pub(super) frame: &'a Frame,
    pub(super) appearance: Appearance,
    pub(super) metrics: Metrics,
    pub(super) reduced_motion: bool,
}

impl canvas::Program<Message> for Overlay<'_> {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        focus(self.frame, self.reduced_motion, Instant::now())
            .map(|focus| {
                focus.draw(
                    self.frame,
                    self.appearance,
                    renderer,
                    bounds.size(),
                    self.metrics,
                )
            })
            .into_iter()
            .collect()
    }
}

fn progress(age: Duration, duration: Duration) -> f32 {
    (age.as_secs_f32() / duration.as_secs_f32()).min(1.0)
}
