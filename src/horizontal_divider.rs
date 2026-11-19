//! A native canvas grip for resizing a pane along one axis.

use iced::widget::canvas;
use iced::{Event, Point, Rectangle, Renderer, Size, Theme, mouse, window};

pub(crate) const WIDTH: f32 = 8.0;

pub(crate) enum Grow {
    Left,
    Right,
    Down,
    Up,
}

impl Grow {
    fn coordinate(&self, point: Point) -> f32 {
        match self {
            Self::Left | Self::Right => point.x,
            Self::Down | Self::Up => point.y,
        }
    }
}

pub(crate) fn program<Message: Clone>(
    extent: f32,
    grow: Grow,
    resize: impl Fn(f32) -> Message,
    finish: Option<Message>,
) -> Divider<Message, impl Fn(f32) -> Message> {
    Divider {
        extent,
        grow,
        resize,
        finish,
        scope: None,
    }
}

pub(crate) struct Divider<Message, Resize> {
    extent: f32,
    grow: Grow,
    resize: Resize,
    finish: Option<Message>,
    scope: Option<u64>,
}

impl<Message, Resize> Divider<Message, Resize> {
    pub(crate) fn scoped(mut self, scope: u64) -> Self {
        self.scope = Some(scope);
        self
    }
}

struct Drag {
    start: f32,
    extent: f32,
    changed: bool,
}

#[derive(Default)]
pub(crate) struct State {
    pointer: Option<Point>,
    drag: Option<Drag>,
    scope: Option<u64>,
}

impl<Message: Clone, Resize: Fn(f32) -> Message> canvas::Program<Message>
    for Divider<Message, Resize>
{
    type State = State;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        if state.scope != self.scope {
            *state = State {
                scope: self.scope,
                ..State::default()
            };
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                // A UI event batch may already carry the cursor at a later drag
                // position. Use the last move delivered before this press.
                let point = state.pointer.unwrap_or(cursor.position()?);
                if !bounds.contains(point) {
                    return None;
                }
                state.drag = Some(Drag {
                    start: self.grow.coordinate(point),
                    extent: self.extent,
                    changed: false,
                });
                Some(canvas::Action::capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                state.pointer = Some(*position);
                let active = state.drag.as_mut()?;
                let delta = self.grow.coordinate(*position) - active.start;
                let extent = active.extent
                    + match self.grow {
                        Grow::Left | Grow::Up => -delta,
                        Grow::Right | Grow::Down => delta,
                    };
                active.changed |= delta != 0.0;
                Some(canvas::Action::publish((self.resize)(extent)).and_capture())
            }
            Event::Mouse(
                mouse::Event::ButtonReleased(mouse::Button::Left) | mouse::Event::CursorLeft,
            )
            | Event::Window(window::Event::Unfocused) => {
                if !matches!(event, Event::Mouse(mouse::Event::ButtonReleased(_))) {
                    state.pointer = None;
                }
                let active = state.drag.take()?;
                if active.changed
                    && let Some(message) = self.finish.clone()
                {
                    Some(canvas::Action::publish(message).and_capture())
                } else {
                    Some(canvas::Action::capture())
                }
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let palette = theme.extended_palette();
        let color = if (state.scope == self.scope && state.drag.is_some()) || cursor.is_over(bounds)
        {
            palette.primary.strong.color
        } else {
            palette.background.strong.color
        };
        let (origin, size) = match self.grow {
            Grow::Left | Grow::Right => (
                Point::new((bounds.width - 2.0) / 2.0, 0.0),
                Size::new(2.0, bounds.height),
            ),
            Grow::Down | Grow::Up => (
                Point::new(0.0, (bounds.height - 2.0) / 2.0),
                Size::new(bounds.width, 2.0),
            ),
        };
        frame.fill_rectangle(origin, size, color);
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if (state.scope == self.scope && state.drag.is_some()) || cursor.is_over(bounds) {
            match self.grow {
                Grow::Left | Grow::Right => mouse::Interaction::ResizingHorizontally,
                Grow::Down | Grow::Up => mouse::Interaction::ResizingVertically,
            }
        } else {
            mouse::Interaction::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canvas::Program;

    #[test]
    fn a_new_scope_cancels_a_pending_drag() {
        let original = program(460.0, Grow::Right, |width| width, Some(0.0)).scoped(1);
        let replacement = program(520.0, Grow::Right, |width| width, Some(0.0)).scoped(2);
        let mut state = State::default();
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(8.0, 260.0));
        let inside = mouse::Cursor::Available(Point::new(4.0, 125.0));
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        let drag = Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(64.0, 125.0),
        });

        assert!(
            original
                .update(&mut state, &press, bounds, inside)
                .is_some()
        );
        assert!(original.update(&mut state, &drag, bounds, inside).is_some());
        assert!(
            replacement
                .update(&mut state, &release, bounds, inside)
                .is_none()
        );
        assert!(state.drag.is_none());

        assert!(
            replacement
                .update(&mut state, &press, bounds, inside)
                .is_some()
        );
        assert!(
            replacement
                .update(&mut state, &drag, bounds, inside)
                .is_some()
        );
        assert!(
            replacement
                .update(&mut state, &release, bounds, inside)
                .is_some()
        );
    }
}
