use iced::advanced::widget::{Operation, Tree, operation::Focusable, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::Id;
use iced::{
    Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector, mouse, touch, window,
};

use super::super::WorkspaceMessage;
use super::super::toolbar_item_context::{self, Command, Event as ItemEvent, Target};

pub(in crate::workspace::chrome) fn item_surface<'a>(
    content: Element<'a, WorkspaceMessage>,
    owner: window::Id,
    target: Target,
    focus: Option<Id>,
) -> Element<'a, WorkspaceMessage> {
    Element::new(ItemSurface {
        content,
        owner,
        target,
        focus,
    })
}

struct ItemSurface<'a> {
    content: Element<'a, WorkspaceMessage>,
    owner: window::Id,
    target: Target,
    focus: Option<Id>,
}

struct State {
    target: Target,
    focused: bool,
    press: Option<Point>,
}

impl State {
    fn new(target: Target) -> Self {
        Self {
            target,
            focused: false,
            press: None,
        }
    }
}

impl Focusable for State {
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

impl Widget<WorkspaceMessage, Theme, Renderer> for ItemSurface<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(State::new(self.target.clone()))
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }
    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<State>();
        if state.target != self.target {
            *state = State::new(self.target.clone());
        }
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
        operation.focusable(
            self.focus.as_ref(),
            layout.bounds(),
            tree.state.downcast_mut::<State>(),
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
        shell: &mut Shell<'_, WorkspaceMessage>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let hit = cursor
            .position_in(layout.bounds())
            .filter(|_| cursor.is_over(*viewport));
        let action = match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                state.focused = hit.is_some();
                state.press = hit;
                None
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                state.focused = hit.is_some();
                state.press = None;
                hit.map(|point| ItemEvent::Open(self.target.clone(), Some(point)))
            }
            Event::Mouse(mouse::Event::CursorMoved { .. })
            | Event::Touch(touch::Event::FingerMoved { .. }) => {
                if !is_click(state.press, hit) {
                    state.press = None;
                }
                None
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. }) => is_click(state.press.take(), hit)
                .then(|| ItemEvent::Open(self.target.clone(), None)),
            Event::Window(window::Event::Unfocused) => {
                state.focused = false;
                state.press = None;
                None
            }
            Event::Mouse(mouse::Event::CursorLeft)
            | Event::Touch(touch::Event::FingerLost { .. }) => {
                state.press = None;
                None
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                repeat,
                ..
            }) if state.focused => {
                let action = match key {
                    Key::Named(Named::Delete | Named::Backspace) => {
                        Some(ItemEvent::Shortcut(self.target.clone(), Command::Remove))
                    }
                    Key::Named(Named::ArrowLeft | Named::ArrowRight) if modifiers.alt() => {
                        Some(ItemEvent::Shortcut(
                            self.target.clone(),
                            Command::Step(*key == Key::Named(Named::ArrowRight)),
                        ))
                    }
                    Key::Named(Named::Enter | Named::Space) => {
                        Some(ItemEvent::Open(self.target.clone(), None))
                    }
                    _ => None,
                };
                if *repeat && action.is_some() {
                    shell.capture_event();
                    return;
                }
                action
            }
            _ => None,
        };
        if let Some(action) = action {
            shell.publish(toolbar_item_context::message(self.owner, action));
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

fn is_click(press: Option<Point>, current: Option<Point>) -> bool {
    press
        .zip(current)
        .is_some_and(|(press, current)| press.distance(current) <= 4.0)
}
