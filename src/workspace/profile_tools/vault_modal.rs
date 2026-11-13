use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, mouse, touch, window,
};

use crate::vault_window;

use super::Message;

pub(in crate::workspace) fn view(
    content: Element<'_, Message>,
    modal: Option<window::Id>,
) -> Element<'_, Message> {
    gate(
        content,
        modal.map(|id| Message::Vault(id, vault_window::Message::FocusUnlock)),
    )
}

pub(in crate::workspace) fn gate(
    content: Element<'_, Message>,
    modal: Option<Message>,
) -> Element<'_, Message> {
    if modal.is_none() {
        return content;
    }
    Element::new(InputGate { content, modal })
}

/// Keep the background's widget state while a separate native window owns input.
struct InputGate<'a> {
    content: Element<'a, Message>,
    modal: Option<Message>,
}

impl Widget<Message, Theme, Renderer> for InputGate<'_> {
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
        if let Some(modal) = &self.modal
            && !matches!(event, Event::Window(_))
        {
            if matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(_))
                    | Event::Touch(touch::Event::FingerPressed { .. })
            ) {
                shell.publish(modal.clone());
            }
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
        let cursor = if self.modal.is_some() {
            mouse::Cursor::Unavailable
        } else {
            cursor
        };
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
        if self.modal.is_some() {
            mouse::Interaction::None
        } else {
            self.content
                .as_widget()
                .mouse_interaction(tree, layout, cursor, viewport, renderer)
        }
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        if self.modal.is_some() {
            return None;
        }
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}

#[cfg(test)]
mod tests {
    use iced::advanced::clipboard;
    use iced::advanced::widget::operation::focusable;
    use iced::keyboard::{self, Key, Location, Modifiers, key};
    use iced::{Font, Pixels};

    use super::*;

    #[test]
    fn unlock_vault_modal_blocks_a_focused_background_input_without_resetting_it() {
        let renderer = Renderer::new(Font::DEFAULT, Pixels(16.0));
        let input = iced::widget::Id::unique();
        let field = || -> Element<'static, Message> {
            iced::widget::text_input("", "")
                .id(input.clone())
                .on_input(|_| Message::NewWindow)
                .into()
        };
        let prompt = window::Id::unique();
        let mut gate = view(field(), Some(prompt));
        let mut tree = Tree::new(gate.as_widget());
        let limits = layout::Limits::new(Size::ZERO, Size::new(220.0, 50.0));
        let node = gate.as_widget_mut().layout(&mut tree, &renderer, &limits);
        gate.as_widget_mut().operate(
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut focusable::focus(input.clone()),
        );
        // Test double for a platform-delivered keyboard event.
        let key = Event::Keyboard(keyboard::Event::KeyPressed {
            key: Key::Character("a".into()),
            modified_key: Key::Character("a".into()),
            physical_key: key::Physical::Code(key::Code::KeyA),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
            text: Some("a".into()),
            repeat: false,
        });
        let dispatch = |gate: &mut Element<'_, Message>, tree: &mut Tree, node: &layout::Node| {
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            gate.as_widget_mut().update(
                tree,
                &key,
                Layout::new(node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut clipboard::Null,
                &mut shell,
                &node.bounds(),
            );
            assert!(shell.is_event_captured());
            messages
        };
        assert!(dispatch(&mut gate, &mut tree, &node).is_empty());

        let mut gate = view(field(), None);
        tree.diff(gate.as_widget());
        let node = gate.as_widget_mut().layout(&mut tree, &renderer, &limits);
        assert!(matches!(
            dispatch(&mut gate, &mut tree, &node).as_slice(),
            [Message::NewWindow]
        ));
    }
}
