use crate::core::keyboard::{Event as KeyEvent, Modifiers};
use crate::core::mouse::{self, ScrollDelta};
use crate::core::{Event, Point, Rectangle};

use super::{HorizontalMetrics, Position};

pub(super) const RESERVED_HEIGHT: f32 = 10.0;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Viewport {
    pub offset: f32,
    minimum: f32,
    maximum: f32,
    last_width: f32,
    last_caret: Option<(Position, f32)>,
    modifiers: Modifiers,
    grabbed: Option<f32>,
}

impl Viewport {
    pub fn sync(&mut self, metrics: HorizontalMetrics, position: Position, width: f32) {
        let caret = metrics.caret_x.map(|x| (position, x));
        self.minimum = metrics.left.min(0.0);
        self.maximum = (metrics.right + 2.0 - width).max(0.0);
        if let Some((_, x)) = caret
            && (self.last_caret != caret || self.last_width != width)
        {
            self.offset = self.offset.clamp((x + 2.0 - width).min(x), x);
        }
        self.offset = self.offset.clamp(self.minimum, self.maximum);
        self.last_caret = caret;
        self.last_width = width;
        if self.minimum == self.maximum {
            self.grabbed = None;
        }
    }

    pub fn caret_x(self) -> Option<f32> {
        self.last_caret.map(|(_, x)| x)
    }

    pub fn scrollbar(self, text_bounds: Rectangle) -> Option<(Rectangle, Rectangle)> {
        let span = self.maximum - self.minimum;
        if span <= 0.0 || text_bounds.width <= 0.0 {
            return None;
        }
        let track = Rectangle {
            x: text_bounds.x,
            y: text_bounds.y + text_bounds.height + 2.0,
            width: text_bounds.width,
            height: 6.0,
        };
        let thumb_width = (track.width.powi(2) / (track.width + span))
            .max(16.0)
            .min(track.width);
        let thumb = Rectangle {
            x: track.x + (track.width - thumb_width) * (self.offset - self.minimum) / span,
            width: thumb_width,
            ..track
        };
        Some((track, thumb))
    }

    pub fn update(
        &mut self,
        event: &Event,
        cursor: mouse::Cursor,
        bounds: Rectangle,
        text_bounds: Rectangle,
    ) -> bool {
        if let Event::Keyboard(KeyEvent::ModifiersChanged(modifiers)) = event {
            self.modifiers = *modifiers;
        }
        let Some((track, thumb)) = self.scrollbar(text_bounds) else {
            return false;
        };
        match event {
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(bounds) => {
                let dx = match *delta {
                    ScrollDelta::Lines { x, y } => self.wheel_x(x, y) * 32.0,
                    ScrollDelta::Pixels { x, y } => self.wheel_x(x, y),
                };
                if dx == 0.0 {
                    return false;
                }
                self.offset = (self.offset - dx).clamp(self.minimum, self.maximum);
                true
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(point) = cursor.position().filter(|point| track.contains(*point)) else {
                    return false;
                };
                let grabbed = if thumb.contains(point) {
                    point.x - thumb.x
                } else {
                    thumb.width / 2.0
                };
                self.grabbed = Some(grabbed);
                self.drag(point, track, thumb, grabbed);
                true
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let Some(grabbed) = self.grabbed else {
                    return false;
                };
                self.drag(*position, track, thumb, grabbed);
                true
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                self.grabbed.take().is_some()
            }
            _ => false,
        }
    }

    fn wheel_x(self, x: f32, y: f32) -> f32 {
        if self.modifiers.shift() && x == 0.0 {
            y
        } else if x.abs() >= y.abs() {
            x
        } else {
            0.0
        }
    }

    fn drag(&mut self, position: Point, track: Rectangle, thumb: Rectangle, grabbed: f32) {
        let travel = track.width - thumb.width;
        if travel > 0.0 {
            let ratio = (position.x - track.x - grabbed) / travel;
            self.offset = self.minimum + ratio.clamp(0.0, 1.0) * (self.maximum - self.minimum);
        }
    }
}
