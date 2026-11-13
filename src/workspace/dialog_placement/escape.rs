//! Dialog Escape routing that preserves Iced overlay precedence.

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::keyboard::{self, Key, key::Named};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, mouse};

pub(in crate::workspace) fn on_escape<'a, Message: Clone + 'a>(
    content: Element<'a, Message>,
    action: Message,
) -> Element<'a, Message> {
    Element::new(Escape { content, action })
}

/// Handle Escape before a focused text input consumes it to blur.
/// Iced dispatches to the forwarded overlays before updating the base view.
struct Escape<'a, Message> {
    content: Element<'a, Message>,
    action: Message,
}

impl<Message: Clone> Widget<Message, Theme, Renderer> for Escape<'_, Message> {
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
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
        if !shell.is_event_captured()
            && matches!(
                event,
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: Key::Named(Named::Escape),
                    ..
                })
            )
        {
            shell.publish(self.action.clone());
            shell.capture_event();
            return;
        }
        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
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
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}
