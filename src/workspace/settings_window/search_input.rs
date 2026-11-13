use iced::widget::TextInput;
use iced::{Element, window};

pub(super) use crate::settings_search::scroll_to_row;

use super::{Desktop, Message};

pub(super) fn view<'a>(input: TextInput<'a, Desktop>, id: window::Id) -> Element<'a, Desktop> {
    crate::settings_search::input(
        input,
        Desktop::Settings(Message::SearchStep(id, -1)),
        Desktop::Settings(Message::SearchStep(id, 1)),
        Some(Desktop::Settings(Message::Escape(id))),
        None,
    )
}
#[cfg(test)]
mod tests {
    use iced::advanced::{
        Layout, Shell, clipboard, layout,
        widget::{Tree, operation::focusable},
    };
    use iced::keyboard::{self, Key, Location, Modifiers, key, key::Named};
    use iced::{Event, Font, Renderer, Size, mouse, widget};

    use super::*;

    #[test]
    fn focused_search_field_owns_navigation_escape_and_submit() {
        let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let id = window::Id::unique();
        let field_id = widget::Id::unique();
        let mut input = view(
            widget::text_input("Search settings…", "quick select")
                .id(field_id.clone())
                .on_input(move |query| Desktop::Settings(Message::Search(id, query)))
                .on_submit(Desktop::Settings(Message::SearchChoose(id))),
            id,
        );
        let mut tree = Tree::new(input.as_widget());
        let node = input.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(300.0, 60.0)),
        );
        let dispatch = |input: &mut Element<'_, Desktop>, tree: &mut Tree, named| {
            let event = Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(named),
                modified_key: Key::Named(named),
                physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                location: Location::Standard,
                modifiers: Modifiers::empty(),
                text: None,
                repeat: false,
            });
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            input.as_widget_mut().update(
                tree,
                &event,
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut clipboard::Null,
                &mut shell,
                &node.bounds(),
            );
            (shell.is_event_captured(), messages)
        };
        assert!(
            matches!(dispatch(&mut input, &mut tree, Named::Escape), (false, messages) if messages.is_empty())
        );
        input.as_widget_mut().operate(
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut focusable::focus(field_id),
        );
        for (key, direction) in [(Named::ArrowDown, 1), (Named::ArrowUp, -1)] {
            let (captured, messages) = dispatch(&mut input, &mut tree, key);
            assert!(captured);
            assert!(
                matches!(messages.as_slice(), [Desktop::Settings(Message::SearchStep(target, step))] if *target == id && *step == direction)
            );
        }
        let (captured, messages) = dispatch(&mut input, &mut tree, Named::Escape);
        assert!(captured);
        assert!(
            matches!(messages.as_slice(), [Desktop::Settings(Message::Escape(target))] if *target == id)
        );
        let (captured, messages) = dispatch(&mut input, &mut tree, Named::Enter);
        assert!(captured);
        assert!(
            matches!(messages.as_slice(), [Desktop::Settings(Message::SearchChoose(target))] if *target == id)
        );
    }
}
