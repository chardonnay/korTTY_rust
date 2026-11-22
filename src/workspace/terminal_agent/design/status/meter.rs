use std::f32::consts::{FRAC_PI_2, TAU};
use std::time::{Duration, Instant};

use iced::widget::{canvas, container, stack, text};
use iced::{Element, Length, Point, Radians, Rectangle, Renderer, Theme, mouse};

use super::{Palette, palette};

#[derive(Clone, Copy)]
pub(in crate::workspace::terminal_agent::design) enum Tone {
    Active,
    Finished,
    Stopped,
}

pub(super) struct Ring {
    pub(super) fraction: f32,
    pub(super) tone: Tone,
}

impl<Message> canvas::Program<Message> for Ring {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let center = Point::new(bounds.width * 0.5, bounds.height * 0.5);
        let half = center.x.min(center.y);
        let radius = half - if half > 20.0 { 7.0 } else { 3.0 };
        if radius <= 0.0 {
            return Vec::new();
        }
        self.paint(&mut frame, center, radius, 2.5, theme);
        vec![frame.into_geometry()]
    }
}

impl Ring {
    pub(super) fn paint(
        &self,
        frame: &mut canvas::Frame,
        center: Point,
        radius: f32,
        width: f32,
        theme: &Theme,
    ) {
        let colors = theme.palette();
        let color = match self.tone {
            Tone::Active => colors.primary,
            Tone::Finished => colors.success,
            Tone::Stopped => colors.danger,
        };
        let stroke = canvas::Stroke::default()
            .with_width(width)
            .with_line_cap(canvas::LineCap::Round);
        let track = canvas::Path::circle(center, radius);
        frame.stroke(&track, stroke.with_color(colors.text.scale_alpha(0.24)));
        if self.fraction > 0.0 {
            let progress = if self.fraction >= 1.0 {
                track
            } else {
                canvas::Path::new(|path| {
                    path.arc(canvas::path::Arc {
                        center,
                        radius,
                        start_angle: Radians(-FRAC_PI_2),
                        end_angle: Radians(-FRAC_PI_2 + self.fraction * TAU),
                    });
                })
            };
            frame.stroke(&progress, stroke.with_color(color));
        }
    }
}

pub(super) fn graphic<'a, M: 'a>(
    size: f32,
    ring: impl canvas::Program<M> + 'a,
    label: Element<'a, M>,
) -> Element<'a, M> {
    stack![
        canvas(ring).width(size).height(size),
        container(label).center(Length::Fill),
    ]
    .width(size)
    .height(size)
    .into()
}

pub(in crate::workspace::terminal_agent::design) fn adaptive_countdown<'a, M: 'a>(
    terminal: Element<'a, M>,
    timer: Option<(Instant, u8)>,
    now: Instant,
    reduced_motion: bool,
    palette: Option<Palette>,
) -> Element<'a, M> {
    let Some((remaining, fraction)) =
        timer.and_then(|(deadline, seconds)| countdown(deadline, seconds, now, reduced_motion))
    else {
        return terminal;
    };
    let label = text(remaining).size(11).into();
    let ring = graphic(
        28.0,
        Ring {
            fraction,
            tone: Tone::Active,
        },
        label,
    );
    let ring = palette::panel(palette, container(ring).padding(1));
    stack![
        terminal,
        container(ring)
            .align_right(Length::Fill)
            .align_bottom(Length::Fill)
            .padding(8),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn countdown(
    deadline: Instant,
    seconds: u8,
    now: Instant,
    reduced_motion: bool,
) -> Option<(u8, f32)> {
    let remaining = deadline
        .checked_duration_since(now)?
        .min(Duration::from_secs(seconds.into()));
    if remaining.is_zero() {
        return None;
    }
    let seconds_left = remaining.as_secs_f32().ceil() as u8;
    let remaining = if reduced_motion {
        seconds_left.into()
    } else {
        remaining.as_secs_f32()
    };
    Some((seconds_left, remaining / f32::from(seconds)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_steps_in_reduced_motion_and_stops_at_the_status_deadline() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(2500);
        assert_eq!(countdown(deadline, 5, now, false), Some((3, 0.5)));
        assert_eq!(countdown(deadline, 5, now, true), Some((3, 0.6)));
        assert_eq!(countdown(deadline, 5, deadline, false), None);
        assert_eq!(countdown(deadline, 0, now, false), None);
    }
}
