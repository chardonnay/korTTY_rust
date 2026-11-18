//! Keyboard commands scoped to an SFTP pane or file listing.

use iced::advanced::widget::{Operation, Tree, operation, operation::Focusable, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::keyboard::key::Named;
use iced::keyboard::{self, Key, Modifiers};
use iced::widget::Id;
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, mouse, window};

use super::{Change, Message, Side};

pub(super) fn wrap(
    content: Element<'_, Message>,
    side: Side,
    enabled: bool,
    can_rename: bool,
    can_delete: bool,
) -> Element<'_, Message> {
    Element::new(Surface {
        content,
        binding: Binding::Listing {
            side,
            can_rename,
            can_delete,
        },
        enabled,
    })
}

pub(super) fn pane(content: Element<'_, Message>, path: Id) -> Element<'_, Message> {
    Element::new(Surface {
        content,
        binding: Binding::Path(path),
        enabled: true,
    })
}

enum Binding {
    Listing {
        side: Side,
        can_rename: bool,
        can_delete: bool,
    },
    Path(Id),
}

fn listing_command(
    side: Side,
    can_rename: bool,
    can_delete: bool,
    key: &Key,
    modifiers: Modifiers,
) -> Option<Message> {
    let operation = match key.as_ref() {
        Key::Character(".") if modifiers == (Modifiers::COMMAND | Modifiers::SHIFT) => {
            return Some(Message::Hidden(side));
        }
        Key::Named(Named::F2) if can_rename && !modifiers.command() && !modifiers.alt() => {
            Change::Rename
        }
        Key::Named(Named::Delete) if can_delete => Change::Delete,
        Key::Named(Named::Backspace) if can_delete && modifiers.command() => Change::Delete,
        _ => return None,
    };
    Some(Message::Change(side, operation))
}

struct Surface<'a> {
    content: Element<'a, Message>,
    binding: Binding,
    enabled: bool,
}

#[derive(Default)]
struct Focus(bool);

impl Focusable for Focus {
    fn is_focused(&self) -> bool {
        self.0
    }

    fn focus(&mut self) {
        self.0 = true;
    }

    fn unfocus(&mut self) {
        self.0 = false;
    }
}

struct PaneFocus<'a> {
    path: &'a Id,
    child_focused: bool,
    path_focused: bool,
}

impl Operation for PaneFocus<'_> {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn focusable(&mut self, id: Option<&Id>, _: Rectangle, focus: &mut dyn Focusable) {
        if focus.is_focused() {
            self.child_focused = true;
            self.path_focused |= id == Some(self.path);
        }
    }
}

impl Widget<Message, Theme, Renderer> for Surface<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Focus>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Focus::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        if !self.enabled {
            tree.state.downcast_mut::<Focus>().unfocus();
        }
        tree.diff_children(&[&self.content]);
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

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if self.enabled {
            operation.focusable(None, layout.bounds(), tree.state.downcast_mut::<Focus>());
        }
        self.content
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
        let focus = tree.state.downcast_mut::<Focus>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left | mouse::Button::Right,
            )) => {
                focus.0 =
                    self.enabled && cursor.is_over(layout.bounds()) && cursor.is_over(*viewport);
            }
            Event::Window(window::Event::Unfocused) => focus.unfocus(),
            _ => {}
        }
        if self.enabled
            && !shell.is_event_captured()
            && let Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                repeat,
                ..
            }) = event
        {
            match &self.binding {
                Binding::Listing {
                    side,
                    can_rename,
                    can_delete,
                } if focus.is_focused() => {
                    if let Some(message) =
                        listing_command(*side, *can_rename, *can_delete, key, *modifiers)
                    {
                        if !repeat {
                            shell.publish(message);
                        }
                        shell.capture_event();
                        return;
                    }
                }
                Binding::Path(path)
                    if matches!(key.as_ref(), Key::Character("l" | "L"))
                        && *modifiers == Modifiers::COMMAND =>
                {
                    let child = self.content.as_widget_mut();
                    let child_tree = &mut tree.children[0];
                    let mut current = PaneFocus {
                        path,
                        child_focused: false,
                        path_focused: false,
                    };
                    child.operate(child_tree, layout, renderer, &mut current);
                    if focus.is_focused() || current.child_focused {
                        if !repeat && !current.path_focused {
                            focus.unfocus();
                            child.operate(
                                child_tree,
                                layout,
                                renderer,
                                &mut operation::focusable::focus(path.clone()),
                            );
                            child.operate(
                                child_tree,
                                layout,
                                renderer,
                                &mut operation::text_input::select_all(path.clone()),
                            );
                            shell.request_redraw();
                        }
                        shell.capture_event();
                        return;
                    }
                }
                _ => {}
            }
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

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use iced::advanced::clipboard;
    use iced::keyboard::{Location, key};
    use iced::widget::{column, row, space, text_input};
    use iced::{Font, Point, event};
    use iced_runtime::{UserInterface, user_interface::Cache};

    use super::*;

    fn shortcut(modifiers: Modifiers, repeat: bool) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: Key::Character(".".into()),
            modified_key: Key::Character(">".into()),
            physical_key: key::Physical::Code(key::Code::Period),
            location: Location::Standard,
            modifiers,
            text: None,
            repeat,
        })
    }

    #[test]
    fn path_shortcut_selects_only_the_active_panes_path() {
        let local = Id::unique();
        let remote = Id::unique();
        let filter = Id::unique();
        let content = row![
            pane(
                column![
                    text_input("Path", "/local")
                        .id(local.clone())
                        .on_input(|text| Message::Path(Side::Local, text)),
                    wrap(
                        space::Space::new().height(65).into(),
                        Side::Local,
                        true,
                        false,
                        false
                    ),
                ]
                .width(120)
                .into(),
                local,
            ),
            pane(
                column![
                    text_input("Path", "/remote")
                        .id(remote.clone())
                        .on_input(|text| Message::Path(Side::Remote, text)),
                    text_input("Filter", "")
                        .id(filter.clone())
                        .on_input(|text| Message::Filter(Side::Remote, text)),
                ]
                .width(120)
                .into(),
                remote,
            ),
        ];
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut interface = UserInterface::build(
            content,
            Size::new(240.0, 100.0),
            Cache::default(),
            &mut renderer,
        );
        let press = |character: &str, modifiers, text| {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Character(character.into()),
                modified_key: Key::Character(character.into()),
                physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                location: Location::Standard,
                modifiers,
                text,
                repeat: false,
            })
        };
        let key = press("l", Modifiers::COMMAND, None);
        let replacement = press("x", Modifiers::empty(), Some("x".into()));
        let click = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let mut messages = Vec::new();
        interface.update(
            &[click],
            mouse::Cursor::Available(Point::new(50.0, 60.0)),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        for side in [Side::Local, Side::Remote] {
            if side == Side::Remote {
                interface.operate(&renderer, &mut operation::focusable::focus(filter.clone()));
            }
            messages.clear();
            let (_, statuses) = interface.update(
                &[key.clone(), replacement.clone()],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            assert_eq!(statuses, [event::Status::Captured; 2]);
            assert!(matches!(messages.as_slice(), [Message::Path(actual, text)]
                if *actual == side && text == "x"));
        }
    }

    #[test]
    fn file_shortcuts_route_to_the_focused_listing_only() {
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let size = Size::new(250.0, 100.0);
        let content = row![
            wrap(
                space::Space::new().width(100).height(100).into(),
                Side::Local,
                true,
                true,
                true
            ),
            wrap(
                space::Space::new().width(100).height(100).into(),
                Side::Remote,
                true,
                true,
                true
            ),
        ];
        let mut interface = UserInterface::build(content, size, Cache::default(), &mut renderer);
        let mut send = |event: Event, cursor| {
            let mut messages = Vec::new();
            let (_, statuses) = interface.update(
                &[event],
                cursor,
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            (messages, statuses[0])
        };
        let click = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let keys = Modifiers::COMMAND | Modifiers::SHIFT;
        let key = shortcut(keys, false);
        assert!(send(key.clone(), mouse::Cursor::Unavailable).0.is_empty());
        for (x, expected) in [(50.0, Side::Local), (150.0, Side::Remote)] {
            let cursor = mouse::Cursor::Available(Point::new(x, 50.0));
            assert!(send(click.clone(), cursor).0.is_empty());
            let (messages, status) = send(key.clone(), mouse::Cursor::Unavailable);
            assert_eq!(status, event::Status::Captured);
            assert!(matches!(messages.as_slice(), [Message::Hidden(side)] if *side == expected));
            let (messages, status) = send(shortcut(keys, true), mouse::Cursor::Unavailable);
            assert_eq!(status, event::Status::Captured);
            assert!(messages.is_empty());
            for (key, modifiers, expected_operation) in [
                (Named::F2, Modifiers::empty(), Change::Rename),
                (Named::Delete, Modifiers::empty(), Change::Delete),
                (Named::Backspace, Modifiers::COMMAND, Change::Delete),
            ] {
                let key = Key::Named(key);
                let (messages, status) = send(
                    Event::Keyboard(keyboard::Event::KeyPressed {
                        key: key.clone(),
                        modified_key: key,
                        physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                        location: Location::Standard,
                        modifiers,
                        text: None,
                        repeat: false,
                    }),
                    mouse::Cursor::Unavailable,
                );
                assert_eq!(status, event::Status::Captured);
                assert!(
                    matches!(messages.as_slice(), [Message::Change(side, operation)]
                    if *side == expected && *operation == expected_operation)
                );
            }
            for wrong in [Modifiers::SHIFT, keys | Modifiers::ALT] {
                let (messages, status) = send(shortcut(wrong, false), mouse::Cursor::Unavailable);
                assert_eq!(status, event::Status::Ignored);
                assert!(messages.is_empty());
            }
        }
        send(
            click.clone(),
            mouse::Cursor::Available(Point::new(225.0, 50.0)),
        );
        assert!(send(key.clone(), mouse::Cursor::Unavailable).0.is_empty());
        send(click, mouse::Cursor::Available(Point::new(50.0, 50.0)));
        send(
            Event::Window(window::Event::Unfocused),
            mouse::Cursor::Unavailable,
        );
        assert!(send(key, mouse::Cursor::Unavailable).0.is_empty());
    }

    #[test]
    fn file_mutation_shortcuts_require_an_available_selection() {
        for key in [Named::Delete, Named::F2] {
            assert!(
                listing_command(
                    Side::Local,
                    false,
                    false,
                    &Key::Named(key),
                    Modifiers::empty()
                )
                .is_none()
            );
        }
        assert!(
            listing_command(
                Side::Local,
                true,
                true,
                &Key::Named(Named::Backspace),
                Modifiers::empty(),
            )
            .is_none()
        );
        assert!(
            listing_command(
                Side::Local,
                true,
                true,
                &Key::Named(Named::F2),
                Modifiers::ALT
            )
            .is_none()
        );
        assert!(matches!(
            listing_command(
                Side::Local,
                false,
                true,
                &Key::Named(Named::Delete),
                Modifiers::empty()
            ),
            Some(Message::Change(Side::Local, Change::Delete))
        ));
    }
}
