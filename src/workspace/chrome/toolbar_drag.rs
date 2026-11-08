//! Resolve toolbar drag edges against their ordered pointer events.

use iced::advanced::layout;
use iced::advanced::overlay;
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, renderer};
use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, mouse, touch, window,
};

use super::WorkspaceMessage;

mod item;
pub(super) use item::item_surface;

pub(super) fn surface<'a>(
    content: Element<'a, WorkspaceMessage>,
    cancel: WorkspaceMessage,
) -> Element<'a, WorkspaceMessage> {
    Element::new(Surface { content, cancel })
}

struct Surface<'a> {
    content: Element<'a, WorkspaceMessage>,
    cancel: WorkspaceMessage,
}

#[derive(Default)]
struct Pointer {
    last: Option<mouse::Cursor>,
}

impl Pointer {
    fn event_cursor(&mut self, event: &Event, cursor: mouse::Cursor) -> mouse::Cursor {
        let ordered = match event {
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let current = mouse::Cursor::Available(*position);
                self.last = Some(current);
                Some(current)
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                self.last = Some(mouse::Cursor::Unavailable);
                self.last
            }
            Event::Mouse(mouse::Event::CursorEntered) | Event::Window(window::Event::Unfocused) => {
                self.last = None;
                None
            }
            Event::Mouse(_) => self.last,
            Event::Touch(
                touch::Event::FingerPressed { position, .. }
                | touch::Event::FingerMoved { position, .. }
                | touch::Event::FingerLifted { position, .. }
                | touch::Event::FingerLost { position, .. },
            ) => Some(mouse::Cursor::Available(*position)),
            _ => None,
        };
        // An Iced overlay can hide the base cursor even when a move was observed.
        if cursor.position().is_none() {
            return cursor;
        }
        ordered.unwrap_or(cursor)
    }
}

impl Widget<WorkspaceMessage, Theme, Renderer> for Surface<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Pointer>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Pointer::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.content]);
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
        shell: &mut Shell<'_, WorkspaceMessage>,
        viewport: &Rectangle,
    ) {
        // Iced's Winit backend passes the batch's final cursor to every event.
        // Keep the earlier CursorMoved payloads for accurate press/release hit tests.
        let cursor = tree
            .state
            .downcast_mut::<Pointer>()
            .event_cursor(event, cursor);
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
        // A valid target publishes Drop first. Every other release still clears
        // the source so a later unrelated click cannot move a toolbar entry.
        if matches!(
            event,
            Event::Mouse(
                mouse::Event::ButtonReleased(mouse::Button::Left) | mouse::Event::CursorLeft
            ) | Event::Touch(touch::Event::FingerLifted { .. } | touch::Event::FingerLost { .. })
                | Event::Window(window::Event::Unfocused)
        ) {
            shell.publish(self.cancel.clone());
        }
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
    ) -> Option<overlay::Element<'a, WorkspaceMessage, Theme, Renderer>> {
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
    use iced::widget::{mouse_area, row, space};
    use iced::{Font, Pixels, Point};

    use super::*;
    use crate::workspace::chrome::toolbar_settings::{Edit, Group, Location};
    use crate::workspace::chrome::{Message, ToolbarMessage, wrap};

    #[test]
    fn target_drop_precedes_cleanup_and_releasing_elsewhere_only_cleans_up() {
        let owner = window::Id::unique();
        let edit = |edit| wrap(Message::Toolbar(ToolbarMessage::Edit(owner, edit)));
        let target = mouse_area(space::horizontal().width(40).height(30))
            .on_release(edit(Edit::Drop(Group::Tools, None)));
        let mut toolbar = surface(target.into(), edit(Edit::CancelDrag));
        let renderer = Renderer::new(Font::DEFAULT, Pixels(14.0));
        let mut tree = Tree::new(&toolbar);
        let viewport = Rectangle::with_size(Size::new(200.0, 100.0));
        let bounds = toolbar.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, viewport.size()),
        );
        let event = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        let mut release_at = |x, y| {
            let mut messages = Vec::new();
            toolbar.as_widget_mut().update(
                &mut tree,
                &event,
                Layout::new(&bounds),
                mouse::Cursor::Available(Point::new(x, y)),
                &renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
                &viewport,
            );
            messages
        };

        assert!(matches!(
            release_at(10.0, 10.0).as_slice(),
            [
                WorkspaceMessage::Chrome(Message::Toolbar(ToolbarMessage::Edit(
                    _,
                    Edit::Drop(Group::Tools, None)
                ))),
                WorkspaceMessage::Chrome(Message::Toolbar(ToolbarMessage::Edit(
                    _,
                    Edit::CancelDrag
                ))),
            ]
        ));
        assert!(matches!(
            release_at(130.0, 70.0).as_slice(),
            [WorkspaceMessage::Chrome(Message::Toolbar(
                ToolbarMessage::Edit(_, Edit::CancelDrag)
            ))],
        ));
    }

    #[test]
    fn a_later_move_in_each_iced_batch_cannot_change_the_pressed_or_released_item() {
        let owner = window::Id::unique();
        let edit = |edit| wrap(Message::Toolbar(ToolbarMessage::Edit(owner, edit)));
        let source = |group| {
            mouse_area(space::horizontal().width(40).height(30))
                .on_press(edit(Edit::Pick(Location { group, index: 0 })))
        };
        let destination = mouse_area(space::horizontal().width(40).height(30))
            .on_release(edit(Edit::Drop(Group::Tools, None)));
        let content = row![source(Group::Terminal), source(Group::Tools), destination];
        let mut toolbar = surface(content.into(), edit(Edit::CancelDrag));
        let renderer = Renderer::new(Font::DEFAULT, Pixels(14.0));
        let mut tree = Tree::new(&toolbar);
        let viewport = Rectangle::with_size(Size::new(200.0, 100.0));
        let bounds = toolbar.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, viewport.size()),
        );
        let moved = |x| {
            Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(x, 10.0),
            })
        };
        let final_cursor = mouse::Cursor::Available(Point::new(50.0, 10.0));
        let mut messages = Vec::new();
        for batch in [
            [
                moved(10.0),
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                moved(50.0),
            ],
            [
                moved(90.0),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                moved(50.0),
            ],
        ] {
            for event in batch {
                toolbar.as_widget_mut().update(
                    &mut tree,
                    &event,
                    Layout::new(&bounds),
                    final_cursor,
                    &renderer,
                    &mut clipboard::Null,
                    &mut Shell::new(&mut messages),
                    &viewport,
                );
            }
        }

        assert!(matches!(
            messages.as_slice(),
            [
                WorkspaceMessage::Chrome(Message::Toolbar(ToolbarMessage::Edit(
                    _,
                    Edit::Pick(Location {
                        group: Group::Terminal,
                        index: 0
                    })
                ))),
                WorkspaceMessage::Chrome(Message::Toolbar(ToolbarMessage::Edit(
                    _,
                    Edit::Drop(Group::Tools, None)
                ))),
                WorkspaceMessage::Chrome(Message::Toolbar(ToolbarMessage::Edit(
                    _,
                    Edit::CancelDrag
                ))),
            ],
        ));
    }
}
