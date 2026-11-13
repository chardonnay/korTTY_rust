//! Dismiss an open AI Manager picker before Escape reaches its form.

use std::borrow::Borrow;
use std::cell::Cell;

use iced::advanced::overlay::{self, Overlay};
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, renderer};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::PickList;
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, mouse};

pub(in crate::ai_manager) trait DismissOnEscape<'a, Message: 'a> {
    fn dismiss_on_escape(self) -> Element<'a, Message>;
}

impl<'a, T, L, V, Message> DismissOnEscape<'a, Message> for PickList<'a, T, L, V, Message>
where
    T: ToString + PartialEq + Clone + 'a,
    L: Borrow<[T]> + 'a,
    V: Borrow<T> + 'a,
    Message: Clone + 'a,
{
    fn dismiss_on_escape(self) -> Element<'a, Message> {
        escape_dismissible(self)
    }
}

pub(super) fn escape_dismissible<'a, Message: Clone + 'a>(
    picker: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    Element::new(Picker {
        widget: picker.into(),
    })
}

struct Picker<'a, Message> {
    widget: Element<'a, Message>,
}

#[derive(Default)]
struct PickerState {
    dismiss: Cell<bool>,
}

impl<Message: Clone> Widget<Message, Theme, Renderer> for Picker<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<PickerState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(PickerState::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(self.widget.as_widget())]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.widget));
    }

    fn size(&self) -> Size<Length> {
        self.widget.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.widget.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        if tree
            .state
            .downcast_ref::<PickerState>()
            .dismiss
            .replace(false)
        {
            // Iced has no close operation for PickList. Rebuild only this widget's local state.
            tree.children[0] = Tree::new(self.widget.as_widget());
        }
        self.widget
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.widget
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
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
        self.widget.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
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
        self.widget.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.widget.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        let state = tree.state.downcast_ref::<PickerState>();
        let menu = self.widget.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )?;
        Some(overlay::Element::new(Box::new(Menu { menu, state })))
    }
}

struct Menu<'a, Message> {
    menu: overlay::Element<'a, Message, Theme, Renderer>,
    state: &'a PickerState,
}

impl<Message> Overlay<Message, Theme, Renderer> for Menu<'_, Message> {
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        self.menu.as_overlay_mut().layout(renderer, bounds)
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.menu
            .as_overlay()
            .draw(renderer, theme, style, layout, cursor);
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.menu
            .as_overlay_mut()
            .operate(layout, renderer, operation);
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        if matches!(
            event,
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(Named::Escape),
                ..
            })
        ) {
            self.state.dismiss.set(true);
            shell.invalidate_layout();
            shell.request_redraw();
            shell.capture_event();
        } else {
            self.menu
                .as_overlay_mut()
                .update(event, layout, cursor, renderer, clipboard, shell);
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.menu
            .as_overlay()
            .mouse_interaction(layout, cursor, renderer)
    }

    fn overlay<'a>(
        &'a mut self,
        layout: Layout<'a>,
        renderer: &Renderer,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        self.menu.as_overlay_mut().overlay(layout, renderer)
    }

    fn index(&self) -> f32 {
        self.menu.as_overlay().index()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_manager::profiles::{ConnectionMode, Message};
    use iced::advanced::clipboard;
    use iced::keyboard::{Location, Modifiers, key};
    use iced::{Font, Point, event};
    use iced_runtime::{UserInterface, user_interface::Cache};

    #[test]
    fn wizard_picker_consumes_escape_only_while_its_menu_is_open() {
        let view = escape_dismissible(
            iced::widget::pick_list(
                ConnectionMode::ALL,
                Some(ConnectionMode::External),
                Message::ConnectionMode,
            )
            .width(230),
        );
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut ui = UserInterface::build(
            view,
            Size::new(320.0, 240.0),
            Cache::default(),
            &mut renderer,
        );
        let mut send = |events: &[Event], point| {
            let mut messages = Vec::new();
            let (_, statuses) = ui.update(
                events,
                mouse::Cursor::Available(point),
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            (statuses, messages)
        };
        let press = |key: Key, modifiers| {
            Event::Keyboard(keyboard::Event::KeyPressed {
                modified_key: key.clone(),
                key,
                physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                location: Location::Standard,
                modifiers,
                text: None,
                repeat: false,
            })
        };
        let button = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let escape = press(Key::Named(Named::Escape), Modifiers::empty());
        let trigger = Point::new(10.0, 10.0);
        assert_eq!(
            send(std::slice::from_ref(&button), trigger).0,
            [event::Status::Captured]
        );
        assert_eq!(
            send(
                &[press(Key::Character("w".into()), Modifiers::COMMAND)],
                trigger
            )
            .0,
            [event::Status::Ignored]
        );
        let (status, messages) = send(std::slice::from_ref(&escape), trigger);
        assert_eq!(status, [event::Status::Captured]);
        assert!(messages.is_empty());
        assert_eq!(send(&[escape], trigger).0, [event::Status::Ignored]);

        assert_eq!(
            send(std::slice::from_ref(&button), trigger).0,
            [event::Status::Captured]
        );
        let choice = Point::new(10.0, 75.0);
        let (_, messages) = send(
            &[
                Event::Mouse(mouse::Event::CursorMoved { position: choice }),
                button,
            ],
            choice,
        );
        assert!(matches!(
            messages.as_slice(),
            [Message::ConnectionMode(ConnectionMode::Managed)]
        ));
    }
}
