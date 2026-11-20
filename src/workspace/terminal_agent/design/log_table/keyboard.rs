//! Route keys to the visible table that took pointer focus before the terminal sees them.

use std::cell::Cell;

use iced::advanced::widget::{Operation, Tree, operation, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::keyboard::{self, Modifiers, key::Named};
use iced::widget::Id;
use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, mouse, touch, window,
};

use super::{AgentMessage, Key, Message, event, presentation};
use crate::workspace::terminal_agent::{Desktop, State};

pub(super) fn wrap<'a>(content: Element<'a, Desktop>, agents: &'a State) -> Element<'a, Desktop> {
    Element::new(Scope { content, agents })
}

struct Scope<'a> {
    content: Element<'a, Desktop>,
    agents: &'a State,
}

#[derive(Clone, Copy)]
struct Route {
    window: window::Id,
    run: window::Id,
}

impl Route {
    fn from_pointer(message: &Desktop) -> Option<Self> {
        let Desktop::TerminalAgent(window, message) = message else {
            return None;
        };
        let (run, action) = match message {
            AgentMessage::PipOwned(run, action) => (*run, action.as_ref()),
            _ => (*window, message),
        };
        matches!(
            action,
            AgentMessage::Design(presentation::Message::LogTable(
                Message::Select(_) | Message::Open(_) | Message::Sort(_)
            ))
        )
        .then_some(Self {
            window: *window,
            run,
        })
    }

    fn message(self, key: Key) -> Desktop {
        let action = event(Message::Navigate(key));
        Desktop::TerminalAgent(
            self.window,
            if self.window == self.run {
                action
            } else {
                AgentMessage::PipOwned(self.run, Box::new(action))
            },
        )
    }
}

#[derive(Default)]
struct Focus {
    route: Option<Route>,
    held: Vec<Named>,
}

struct Visible<'a> {
    viewport: &'a Id,
    found: bool,
}

impl Operation for Visible<'_> {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
        if !self.found {
            visit(self);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        _: Rectangle,
        _: Vector,
        _: &mut dyn operation::Scrollable,
    ) {
        if id == Some(self.viewport) && bounds.width > 0.0 && bounds.height > 0.0 {
            self.found = true;
        }
    }
}

fn navigation(name: Named, modifiers: Modifiers) -> Option<Key> {
    if name == Named::Tab && (modifiers.is_empty() || modifiers == Modifiers::SHIFT) {
        return Some(Key::Leave);
    }
    if !modifiers.is_empty() {
        return None;
    }
    match name {
        Named::ArrowUp => Some(Key::Previous),
        Named::ArrowDown => Some(Key::Next),
        Named::Home => Some(Key::First),
        Named::End => Some(Key::Last),
        Named::Enter | Named::Space => Some(Key::Open),
        Named::Escape => Some(Key::Close),
        _ => None,
    }
}

impl Widget<Desktop, Theme, Renderer> for Scope<'_> {
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
        tree.diff_children(std::slice::from_ref(&self.content));
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
        input: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Desktop>,
        viewport: &Rectangle,
    ) {
        let focus = tree.state.downcast_mut::<Focus>();
        let child = self.content.as_widget_mut();
        let child_tree = &mut tree.children[0];
        match input {
            Event::Window(window::Event::Unfocused) => *focus = Focus::default(),
            Event::Keyboard(keyboard::Event::KeyReleased {
                key: keyboard::Key::Named(name),
                ..
            }) if focus.held.contains(name) => {
                focus.held.retain(|held| held != name);
                shell.capture_event();
                return;
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(name),
                modifiers,
                repeat,
                ..
            }) if !shell.is_event_captured() => {
                if let Some(key) = navigation(*name, *modifiers)
                    && let Some(route) = focus.route
                {
                    if !focus.held.contains(name) {
                        focus.held.push(*name);
                    }
                    shell.capture_event();
                    let Some(run) = self.agents.runs.get(&route.run).filter(|run| !run.closing)
                    else {
                        focus.route = None;
                        return;
                    };
                    let table = &run.presentation.log_table;
                    let mut visible = Visible {
                        viewport: &table.viewport,
                        found: false,
                    };
                    child.operate(child_tree, layout, renderer, &mut visible);
                    if !visible.found
                        || matches!(key, Key::Leave)
                        || matches!(key, Key::Close) && table.detail.is_none()
                    {
                        focus.route = None;
                    } else if !repeat || matches!(key, Key::Previous | Key::Next) {
                        shell.publish(route.message(key));
                    }
                    return;
                }
                if focus.held.contains(name) {
                    shell.capture_event();
                    return;
                }
            }
            _ => {}
        }
        let press = matches!(
            input,
            Event::Mouse(mouse::Event::ButtonPressed(_))
                | Event::Touch(touch::Event::FingerPressed { .. })
        );
        if press {
            focus.route = None;
        }
        if press
            || matches!(
                input,
                Event::Mouse(mouse::Event::ButtonReleased(_))
                    | Event::Touch(touch::Event::FingerLifted { .. })
            )
        {
            let mut messages = Vec::new();
            let mut child_shell = Shell::new(&mut messages);
            child.update(
                child_tree,
                input,
                layout,
                cursor,
                renderer,
                clipboard,
                &mut child_shell,
                viewport,
            );
            let pointed = Cell::new(None);
            shell.merge(child_shell, |message| {
                if let Some(route) = Route::from_pointer(&message) {
                    pointed.set(Some(route));
                }
                message
            });
            if let Some(route) = pointed.get() {
                focus.route = Some(route);
            }
        } else {
            child.update(
                child_tree, input, layout, cursor, renderer, clipboard, shell, viewport,
            );
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

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Desktop, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}
