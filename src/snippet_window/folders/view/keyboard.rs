//! Focus and navigation confined to the visible folder rows.

use iced::advanced::widget::operation::scrollable::{AbsoluteOffset, Scrollable};
use iced::advanced::widget::{Id, Operation, Tree, operation::Focusable, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::keyboard::{self, Key, key::Named};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, mouse, window};

use super::{Message, Selection, WindowMessage, wrap};

#[derive(Clone, Copy)]
pub(super) struct Row {
    selection: Selection,
    parent: Option<Selection>,
    expanded: Option<bool>,
}

impl Row {
    pub(super) fn new(
        selection: Selection,
        parent: Option<Selection>,
        expanded: Option<bool>,
    ) -> Self {
        Self {
            selection,
            parent,
            expanded,
        }
    }
}

pub(super) fn tree<'a>(
    content: Element<'a, WindowMessage>,
    rows: Vec<Row>,
    selection: Selection,
    menu_open: bool,
) -> Element<'a, WindowMessage> {
    Element::new(FocusSurface {
        content,
        rows,
        selection,
        menu_open,
    })
}

enum Command {
    Select(usize),
    Action(Message),
}

fn command(rows: &[Row], selection: Selection, key: Named, repeat: bool) -> Option<Command> {
    let index = rows.iter().position(|row| row.selection == selection)?;
    let row = rows[index];
    match key {
        Named::ArrowUp => index.checked_sub(1).map(Command::Select),
        Named::ArrowDown => (index + 1 < rows.len()).then_some(Command::Select(index + 1)),
        Named::ArrowLeft => {
            if let (Some(id), Some(true)) = (selection.folder(), row.expanded) {
                return Some(Command::Action(Message::Expanded(id, false)));
            }
            let parent = row.parent?;
            rows.iter()
                .position(|row| row.selection == parent)
                .map(Command::Select)
        }
        Named::ArrowRight => {
            if let (Some(id), Some(false)) = (selection.folder(), row.expanded) {
                return Some(Command::Action(Message::Expanded(id, true)));
            }
            rows.get(index + 1)
                .filter(|child| child.parent == Some(selection))
                .map(|_| Command::Select(index + 1))
        }
        Named::F2 if !repeat => selection
            .folder()
            .map(|id| Command::Action(Message::Rename(id))),
        Named::Delete | Named::Backspace if !repeat => selection
            .folder()
            .map(|id| Command::Action(Message::Delete(id))),
        _ => None,
    }
}

struct FocusState {
    focused: bool,
    selection: Selection,
}

impl Focusable for FocusState {
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn focus(&mut self) {
        self.focused = true;
    }
    fn unfocus(&mut self) {
        self.focused = false;
    }
}

struct FocusSurface<'a> {
    content: Element<'a, WindowMessage>,
    rows: Vec<Row>,
    selection: Selection,
    menu_open: bool,
}

impl Widget<WindowMessage, Theme, Renderer> for FocusSurface<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<FocusState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(FocusState {
            focused: false,
            selection: self.selection,
        })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.state.downcast_mut::<FocusState>().selection = self.selection;
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
        operation.focusable(
            None,
            layout.bounds(),
            tree.state.downcast_mut::<FocusState>(),
        );
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
        shell: &mut Shell<'_, WindowMessage>,
        viewport: &Rectangle,
    ) {
        let focus = tree.state.downcast_mut::<FocusState>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left | mouse::Button::Right,
            )) => {
                focus.focused = cursor.is_over(layout.bounds()) && cursor.is_over(*viewport);
            }
            Event::Window(window::Event::Unfocused) => focus.unfocus(),
            _ => {}
        }
        if focus.focused
            && !self.menu_open
            && !shell.is_event_captured()
            && let Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(key),
                modifiers,
                repeat,
                ..
            }) = event
            && modifiers.is_empty()
            && matches!(
                key,
                Named::ArrowUp
                    | Named::ArrowDown
                    | Named::ArrowLeft
                    | Named::ArrowRight
                    | Named::F2
                    | Named::Delete
                    | Named::Backspace
                    | Named::Enter
            )
        {
            match command(&self.rows, focus.selection, *key, *repeat) {
                Some(Command::Select(index)) => {
                    focus.selection = self.rows[index].selection;
                    if let Some(row) = layout
                        .children()
                        .next()
                        .and_then(|rows| rows.children().nth(index))
                    {
                        self.content.as_widget_mut().operate(
                            &mut tree.children[0],
                            layout,
                            renderer,
                            &mut Reveal(row.bounds()),
                        );
                    }
                    shell.publish(wrap(Message::Select(focus.selection)));
                }
                Some(Command::Action(message)) => shell.publish(wrap(message)),
                None => {}
            }
            // At a boundary, the key still belongs to the tree, not the snippet list.
            shell.capture_event();
            return;
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
    ) -> Option<overlay::Element<'a, WindowMessage, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

struct Reveal(Rectangle);

impl Operation for Reveal {
    fn traverse(&mut self, _operate: &mut dyn FnMut(&mut dyn Operation)) {}

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        bounds: Rectangle,
        content: Rectangle,
        translation: Vector,
        state: &mut dyn Scrollable,
    ) {
        let top = self.0.y - content.y;
        let bottom = top + self.0.height;
        let offset = if top < translation.y {
            top
        } else if bottom > translation.y + bounds.height {
            bottom - bounds.height
        } else {
            return;
        };
        state.scroll_to(AbsoluteOffset {
            x: None,
            y: Some(offset.clamp(0.0, (content.height - bounds.height).max(0.0))),
        });
    }
}

#[cfg(test)]
mod tests {
    use iced::advanced::clipboard;
    use iced::widget::{column, scrollable, space};
    use iced::{Font, Pixels, Point};

    use crate::snippets::{Library, tests::Profile};

    use super::*;

    #[test]
    fn folder_tree_navigation_follows_only_visible_nodes_and_their_parents() {
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let parent = library.create_folder(None, "Parent").unwrap();
        let child = library.create_folder(Some(parent), "Child").unwrap();
        let peer = library.create_folder(None, "Peer").unwrap();
        let branch = Selection::Folder(parent);
        let leaf = Selection::Folder(child);
        let mut rows = vec![
            Row::new(Selection::All, None, None),
            Row::new(Selection::TopLevel, Some(Selection::All), None),
            Row::new(branch, Some(Selection::All), Some(false)),
            Row::new(Selection::Folder(peer), Some(Selection::All), None),
        ];
        assert!(matches!(
            command(&rows, branch, Named::ArrowDown, false),
            Some(Command::Select(3))
        ));
        assert!(
            matches!(command(&rows, branch, Named::ArrowRight, false), Some(Command::Action(Message::Expanded(id, true))) if id == parent)
        );
        rows[2].expanded = Some(true);
        rows.insert(3, Row::new(leaf, Some(branch), None));
        assert!(matches!(
            command(&rows, branch, Named::ArrowRight, false),
            Some(Command::Select(3))
        ));
        assert!(matches!(
            command(&rows, leaf, Named::ArrowLeft, false),
            Some(Command::Select(2))
        ));
        assert!(
            matches!(command(&rows, branch, Named::ArrowLeft, false), Some(Command::Action(Message::Expanded(id, false))) if id == parent)
        );
        assert!(
            matches!(command(&rows, leaf, Named::F2, false), Some(Command::Action(Message::Rename(id))) if id == child)
        );
        for key in [Named::Delete, Named::Backspace] {
            assert!(
                matches!(command(&rows, leaf, key, false), Some(Command::Action(Message::Delete(id))) if id == child)
            );
            assert!(command(&rows, leaf, key, true).is_none());
            assert!(command(&rows, Selection::All, key, false).is_none());
        }
    }

    fn press(key: Named, modifiers: keyboard::Modifiers) -> Event {
        // Test double for a keyboard event delivered by the window system.
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: Key::Named(key),
            modified_key: Key::Named(key),
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::ArrowDown),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        })
    }

    #[test]
    fn folder_tree_widget_captures_keys_only_while_it_has_focus() {
        let renderer = Renderer::new(Font::DEFAULT, Pixels(13.0));
        let mut widget = tree(
            scrollable(column![
                space::vertical().height(22),
                space::vertical().height(22)
            ])
            .height(Length::Fill)
            .width(Length::Fill)
            .into(),
            vec![
                Row::new(Selection::All, None, None),
                Row::new(Selection::TopLevel, Some(Selection::All), None),
            ],
            Selection::All,
            false,
        );
        let mut state = Tree::new(widget.as_widget());
        let node = widget.as_widget_mut().layout(
            &mut state,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(200.0, 38.0)),
        );
        let inside = mouse::Cursor::Available(Point::new(20.0, 15.0));
        let outside = mouse::Cursor::Available(Point::new(250.0, 20.0));
        let mouse_press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let down = press(Named::ArrowDown, keyboard::Modifiers::empty());
        let mut dispatch = |event: &Event, cursor| {
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            widget.as_widget_mut().update(
                &mut state,
                event,
                Layout::new(&node),
                cursor,
                &renderer,
                &mut clipboard::Null,
                &mut shell,
                &node.bounds(),
            );
            let captured = shell.is_event_captured();
            (messages, captured)
        };
        assert!(!dispatch(&down, inside).1);
        let _ = dispatch(&mouse_press, inside);
        let (messages, captured) = dispatch(&down, inside);
        assert!(captured);
        assert!(matches!(
            messages.as_slice(),
            [WindowMessage::Folders(Message::Select(Selection::TopLevel))]
        ));
        assert!(matches!(dispatch(&down, inside), (messages, true) if messages.is_empty()));
        assert!(!dispatch(&press(Named::ArrowUp, keyboard::Modifiers::CTRL), inside).1);
        assert!(
            matches!(dispatch(&press(Named::Enter, keyboard::Modifiers::empty()), inside), (messages, true) if messages.is_empty())
        );
        let _ = dispatch(&mouse_press, outside);
        assert!(!dispatch(&down, inside).1);
    }
}
