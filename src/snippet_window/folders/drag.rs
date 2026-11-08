//! Pointer gestures confined to the folder tree and snippet list in one view.

use std::cell::Cell;
use std::rc::Rc;

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Renderer as _, Shell, Widget, layout, overlay, renderer};
use iced::{
    Border, Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector, mouse,
    window,
};

use crate::snippet_window::Message as WindowMessage;
use crate::snippets::{SnippetId, folders::Id};

const DRAG_DISTANCE: f32 = 6.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    Folder(Id),
    Snippet(SnippetId),
}

impl Source {
    fn can_drop(self, folder: Option<Id>) -> bool {
        !matches!(self, Self::Folder(id) if folder == Some(id))
    }
}

type Destination = Option<Id>;

pub(in crate::snippet_window) fn scope<'a>(
    build: impl FnOnce(Zone) -> Element<'a, WindowMessage>,
) -> Element<'a, WindowMessage> {
    let zone = Zone(Rc::new(Context::default()));
    Element::new(Area {
        child: build(zone.clone()),
        zone,
        hook: Hook::Scope,
    })
}

#[derive(Clone)]
pub(in crate::snippet_window) struct Zone(Rc<Context>);

impl Zone {
    pub(in crate::snippet_window) fn item<'a>(
        &self,
        child: impl Into<Element<'a, WindowMessage>>,
        source: Option<Source>,
        destination: Option<Destination>,
    ) -> Element<'a, WindowMessage> {
        Element::new(Area {
            child: child.into(),
            zone: self.clone(),
            hook: Hook::Item {
                source,
                destination,
            },
        })
    }
}

#[derive(Default)]
struct Context {
    dispatch: Cell<Dispatch>,
    active: Cell<Option<Source>>,
}

#[derive(Clone, Copy, Default)]
enum Dispatch {
    #[default]
    Idle,
    Press {
        origin: Point,
        source: Option<Source>,
    },
    Release {
        source: Source,
        target: Option<Destination>,
    },
}

struct Press {
    origin: Point,
    source: Source,
    dragging: bool,
}

#[derive(Default)]
struct Gesture {
    pointer: Option<Point>,
    press: Option<Press>,
}

impl Gesture {
    fn active(&self) -> Option<Source> {
        self.press
            .as_ref()
            .filter(|press| press.dragging)
            .map(|press| press.source)
    }

    fn start_event(
        &mut self,
        event: &Event,
        mut cursor: mouse::Cursor,
    ) -> (mouse::Cursor, Dispatch) {
        let dispatch = match event {
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                self.pointer = Some(*position);
                if let Some(press) = self.press.as_mut() {
                    press.dragging |= press.origin.distance(*position) >= DRAG_DISTANCE;
                }
                Dispatch::Idle
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                self.press = None;
                // Iced shares the latest cursor across a batch. The last move
                // delivered before the press still identifies the source row.
                if let Some(current) = cursor.position() {
                    let origin = self.pointer.unwrap_or(current);
                    cursor = mouse::Cursor::Available(origin);
                    Dispatch::Press {
                        origin,
                        source: None,
                    }
                } else {
                    Dispatch::Idle
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let active = self.active();
                self.press = None;
                if let Some(source) = active {
                    if let Some(current) = cursor.position() {
                        cursor = mouse::Cursor::Available(self.pointer.unwrap_or(current));
                    }
                    Dispatch::Release {
                        source,
                        target: None,
                    }
                } else {
                    Dispatch::Idle
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) | Event::Window(window::Event::Unfocused) => {
                *self = Self::default();
                Dispatch::Idle
            }
            _ => Dispatch::Idle,
        };
        (cursor, dispatch)
    }
}

#[derive(Clone, Copy)]
enum Hook {
    Scope,
    Item {
        source: Option<Source>,
        destination: Option<Destination>,
    },
}

impl Hook {
    fn child_tree(self, tree: &Tree) -> &Tree {
        match self {
            Self::Scope => &tree.children[0],
            Self::Item { .. } => tree,
        }
    }

    fn child_tree_mut(self, tree: &mut Tree) -> &mut Tree {
        match self {
            Self::Scope => &mut tree.children[0],
            Self::Item { .. } => tree,
        }
    }
}

struct Area<'a> {
    child: Element<'a, WindowMessage>,
    zone: Zone,
    hook: Hook,
}

impl Widget<WindowMessage, Theme, Renderer> for Area<'_> {
    fn tag(&self) -> tree::Tag {
        match self.hook {
            Hook::Scope => tree::Tag::of::<Gesture>(),
            Hook::Item { .. } => self.child.as_widget().tag(),
        }
    }

    fn state(&self) -> tree::State {
        match self.hook {
            Hook::Scope => tree::State::new(Gesture::default()),
            Hook::Item { .. } => self.child.as_widget().state(),
        }
    }

    fn children(&self) -> Vec<Tree> {
        match self.hook {
            Hook::Scope => vec![Tree::new(&self.child)],
            Hook::Item { .. } => self.child.as_widget().children(),
        }
    }

    fn diff(&self, tree: &mut Tree) {
        match self.hook {
            Hook::Scope => tree.diff_children(&[&self.child]),
            Hook::Item { .. } => self.child.as_widget().diff(tree),
        }
    }

    fn size(&self) -> Size<Length> {
        self.child.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.child.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.child
            .as_widget_mut()
            .layout(self.hook.child_tree_mut(tree), renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.child.as_widget_mut().operate(
            self.hook.child_tree_mut(tree),
            layout,
            renderer,
            operation,
        );
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        mut cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, WindowMessage>,
        viewport: &Rectangle,
    ) {
        let context = &self.zone.0;
        match self.hook {
            Hook::Scope => {
                let gesture = tree.state.downcast_mut::<Gesture>();
                let before = gesture.active();
                let (ordered_cursor, dispatch) = gesture.start_event(event, cursor);
                cursor = ordered_cursor;
                context.dispatch.set(dispatch);
                let active = gesture.active();
                if before != active
                    || active.is_some()
                        && matches!(event, Event::Mouse(mouse::Event::CursorMoved { .. }))
                {
                    shell.request_redraw();
                }
            }
            Hook::Item {
                source,
                destination,
            } => {
                let over = || {
                    cursor
                        .position_over(layout.bounds())
                        .is_some_and(|point| viewport.contains(point))
                };
                match context.dispatch.get() {
                    Dispatch::Press {
                        origin,
                        source: None,
                    } if source.is_some() && !shell.is_event_captured() && over() => {
                        context.dispatch.set(Dispatch::Press { origin, source });
                    }
                    Dispatch::Release { source, target } => {
                        // The pressed row's button can consume the release
                        // before traversal reaches the destination below it.
                        if target.is_none()
                            && let Some(folder) =
                                destination.filter(|&folder| source.can_drop(folder))
                            && over()
                        {
                            context.dispatch.set(Dispatch::Release {
                                source,
                                target: Some(folder),
                            });
                        }
                        // Allow children to reset their press state without
                        // treating a drag release as activation of a row.
                        cursor = cursor.levitate();
                    }
                    _ => {}
                }
            }
        }

        self.child.as_widget_mut().update(
            self.hook.child_tree_mut(tree),
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );

        if matches!(self.hook, Hook::Scope) {
            match context.dispatch.take() {
                Dispatch::Press {
                    origin,
                    source: Some(source),
                } => {
                    tree.state.downcast_mut::<Gesture>().press = Some(Press {
                        origin,
                        source,
                        dragging: false,
                    });
                }
                Dispatch::Release { source, target } => {
                    if let Some(folder) = target {
                        shell.publish(WindowMessage::Folders(super::Message::Drop(source, folder)));
                    }
                    shell.capture_event();
                }
                _ => {}
            }
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
        if matches!(self.hook, Hook::Scope)
            && tree.state.downcast_ref::<Gesture>().active().is_some()
            && cursor.position().is_some()
        {
            return mouse::Interaction::Grabbing;
        }
        self.child.as_widget().mouse_interaction(
            self.hook.child_tree(tree),
            layout,
            cursor,
            viewport,
            renderer,
        )
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
        let context = &self.zone.0;
        if matches!(self.hook, Hook::Scope) {
            context
                .active
                .set(tree.state.downcast_ref::<Gesture>().active());
        }
        self.child.as_widget().draw(
            self.hook.child_tree(tree),
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        if let Hook::Item {
            destination: Some(destination),
            ..
        } = self.hook
            && context
                .active
                .get()
                .is_some_and(|source| source.can_drop(destination))
            && let Some(bounds) = layout.bounds().intersection(viewport)
            && cursor.is_over(bounds)
        {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: Border {
                        color: theme.extended_palette().primary.strong.color,
                        width: 2.0,
                        ..Border::default()
                    },
                    ..renderer::Quad::default()
                },
                Color::TRANSPARENT,
            );
        }
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, WindowMessage, Theme, Renderer>> {
        self.child.as_widget_mut().overlay(
            self.hook.child_tree_mut(tree),
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pressed_source_row_does_not_block_the_destination_below_it() {
        use iced::Pixels;
        use iced::advanced::clipboard;
        use iced::widget::{button, column};

        let source = Source::Snippet(SnippetId::try_from("01".repeat(16)).unwrap());
        let mut list = scope(|zone| {
            column![
                zone.item(
                    button("Snippet")
                        .width(120)
                        .height(28)
                        .on_press(WindowMessage::New),
                    Some(source),
                    None,
                ),
                zone.item(button("Top level").width(120).height(28), None, Some(None),)
            ]
            .into()
        });
        let renderer = Renderer::new(iced::Font::DEFAULT, Pixels(16.0));
        let mut tree = Tree::new(list.as_widget());
        let limits = layout::Limits::new(Size::ZERO, Size::new(200.0, 100.0));
        let node = list.as_widget_mut().layout(&mut tree, &renderer, &limits);
        let start = Point::new(40.0, 12.0);
        let end = Point::new(40.0, 40.0);
        let mut messages = Vec::new();
        // Test doubles for a platform-delivered mouse drag.
        for (event, position) in [
            (mouse::Event::CursorMoved { position: start }, start),
            (mouse::Event::ButtonPressed(mouse::Button::Left), start),
            (mouse::Event::CursorMoved { position: end }, end),
            (mouse::Event::ButtonReleased(mouse::Button::Left), end),
        ] {
            list.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(event),
                Layout::new(&node),
                mouse::Cursor::Available(position),
                &renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
                &node.bounds(),
            );
        }
        assert!(matches!(
            messages.as_slice(),
            [WindowMessage::Folders(super::super::Message::Drop(actual, None))] if *actual == source
        ));
    }

    #[test]
    fn a_batched_drag_uses_its_press_position_and_ends_on_window_leave() {
        let mut gesture = Gesture::default();
        let start = Point::new(10.0, 20.0);
        let end = Point::new(10.0, 60.0);
        let latest = mouse::Cursor::Available(end);
        let moved = |position| Event::Mouse(mouse::Event::CursorMoved { position });
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        let source = Source::Snippet(SnippetId::try_from("01".repeat(16)).unwrap());

        gesture.start_event(&moved(start), latest);
        let (cursor, dispatch) = gesture.start_event(&press, latest);
        assert_eq!(cursor.position(), Some(start));
        let Dispatch::Press { origin, .. } = dispatch else {
            panic!("expected a press");
        };
        gesture.press = Some(Press {
            origin,
            source,
            dragging: false,
        });
        gesture.start_event(&moved(Point::new(12.0, 20.0)), latest);
        assert!(gesture.active().is_none());
        gesture.start_event(&moved(end), latest);
        assert_eq!(gesture.active(), Some(source));
        gesture.start_event(&Event::Mouse(mouse::Event::CursorLeft), latest);
        assert!(matches!(
            gesture.start_event(&release, latest).1,
            Dispatch::Idle
        ));
    }
}
