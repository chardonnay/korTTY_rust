//! Native slider with a complete pointer-seek lifecycle.

use std::time::Duration;

use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, renderer};
use iced::widget::slider;
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, keyboard, touch, window};

use super::Message;

pub(super) fn view(position: Duration, duration: Duration) -> Element<'static, Message> {
    let position = position.as_secs_f64();
    let duration = duration.as_secs_f64();
    Element::new(Timeline {
        position,
        duration,
        content: slider(0.0..=duration, position, Message::Scrub)
            .step(0.001)
            .into(),
    })
}

struct Timeline {
    position: f64,
    duration: f64,
    content: Element<'static, Message>,
}

#[derive(Default)]
struct State {
    modifiers: keyboard::Modifiers,
    dragging: Option<Pointer>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Pointer {
    Mouse,
    Finger(touch::Finger),
}

fn pointer(event: &Event) -> Option<Pointer> {
    match event {
        Event::Mouse(
            mouse::Event::ButtonPressed(mouse::Button::Left)
            | mouse::Event::ButtonReleased(mouse::Button::Left)
            | mouse::Event::CursorMoved { .. }
            | mouse::Event::WheelScrolled { .. },
        ) => Some(Pointer::Mouse),
        Event::Touch(
            touch::Event::FingerPressed { id, .. }
            | touch::Event::FingerMoved { id, .. }
            | touch::Event::FingerLifted { id, .. }
            | touch::Event::FingerLost { id, .. },
        ) => Some(Pointer::Finger(*id)),
        _ => None,
    }
}

impl Widget<Message, Theme, Renderer> for Timeline {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(self.content.as_widget())]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.children[0].diff(self.content.as_widget());
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if matches!(event, Event::Keyboard(keyboard::Event::KeyPressed { .. }))
            && shell.is_event_captured()
        {
            return;
        }
        let state = tree.state.downcast_mut::<State>();
        let pointer = pointer(event);
        if let (Some(active), Some(incoming)) = (state.dragging, pointer)
            && active != incoming
        {
            return;
        }
        let over = cursor.is_over(layout.bounds());
        let mut released = false;
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.modifiers = *modifiers
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. })
                if over && !state.modifiers.command() && state.dragging.is_none() =>
            {
                state.dragging = pointer;
                // The slider only sends on_change when the value moves. Pause even when the
                // user initially presses the handle without moving it.
                shell.publish(Message::ScrubStarted);
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. } | touch::Event::FingerLost { .. }) => {
                released = state.dragging.take().is_some();
            }
            Event::Window(window::Event::Unfocused) => {
                if state.dragging.is_some() {
                    shell.publish(Message::ScrubFinished);
                }
                *state = State::default();
                tree.children[0] = Tree::new(self.content.as_widget());
                return;
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. })
                if over && modifiers.is_empty() =>
            {
                if let Some(at) = arrow_position(key, self.position, self.duration) {
                    shell.publish(Message::Scrub(at));
                    shell.capture_event();
                    return;
                }
            }
            _ => {}
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if released {
            shell.publish(Message::ScrubFinished);
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
}

fn arrow_position(key: &keyboard::Key, position: f64, duration: f64) -> Option<f64> {
    use keyboard::{Key, key::Named};

    let step = (duration / 20.0).clamp(1.0, 30.0);
    match key {
        Key::Named(Named::ArrowLeft | Named::ArrowDown) => Some((position - step).max(0.0)),
        Key::Named(Named::ArrowRight | Named::ArrowUp) => Some((position + step).min(duration)),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
