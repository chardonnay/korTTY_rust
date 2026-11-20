//! Route keys to the focused, visible table before the terminal sees them.

use std::cell::Cell;

use iced::advanced::widget::{Operation, Tree, operation, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::keyboard::{self, Modifiers, key::Named};
use iced::widget::Id;
use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, mouse, touch, window,
};

use super::{AgentMessage, Key, Message, Surface, event, presentation};
use crate::workspace::terminal_agent::{Desktop, State};

pub(super) const PANE_FOCUS_KEY: Named = Named::F6;
pub(super) const PANE_FOCUS_MODIFIERS: Modifiers = Modifiers::CTRL.union(Modifiers::SHIFT);
pub(super) const MENU_KEY: Named = Named::F10;
pub(super) const MENU_MODIFIERS: Modifiers = Modifiers::SHIFT;

pub(super) enum EntryKey {
    Pane,
    Detached,
}

pub(super) fn wrap<'a>(
    content: Element<'a, Desktop>,
    agents: &'a State,
    window: window::Id,
    entry: Option<(window::Id, EntryKey)>,
    shift_f10_available: bool,
) -> Element<'a, Desktop> {
    Element::new(Scope {
        content,
        target: Live {
            agents,
            window,
            entry,
            shift_f10_available,
        },
    })
}

pub(in crate::workspace) enum PreviewCommand {
    Navigate(Key),
    Tab(bool),
}

pub(in crate::workspace::terminal_agent::design) fn preview<'a>(
    content: Element<'a, Desktop>,
    table: &'a super::State,
    sample: window::Id,
    is_pointer_action: fn(&Desktop) -> bool,
    message: impl Fn(PreviewCommand) -> Desktop + 'a,
) -> Element<'a, Desktop> {
    Element::new(Scope {
        content,
        target: Preview {
            table,
            sample,
            is_pointer_action,
            message,
        },
    })
}

struct Scope<'a, T> {
    content: Element<'a, Desktop>,
    target: T,
}

trait Target {
    type Route: Copy + Eq + 'static;

    fn pointer_route(&self, message: &Desktop) -> Option<Self::Route>;
    fn table(&self, route: Self::Route) -> Option<&super::State>;
    fn navigate(&self, route: Self::Route, key: Key) -> Desktop;

    fn menu_shortcut(&self, _: Named, _: Modifiers) -> bool {
        false
    }

    fn open_menu(&self, _: Self::Route, _: Vector) -> Option<Desktop> {
        None
    }

    fn menu_open(&self, _: Self::Route) -> bool {
        false
    }

    fn focus_node(&self) -> Option<(Self::Route, &super::State)> {
        None
    }

    fn tab(&self, _: bool) -> Option<Desktop> {
        None
    }

    fn focus_entry(&self, _: Named, _: Modifiers, _: Option<Self::Route>) -> Option<Self::Route> {
        None
    }
}

struct Preview<'a, Message> {
    table: &'a super::State,
    sample: window::Id,
    is_pointer_action: fn(&Desktop) -> bool,
    message: Message,
}

impl<Message: Fn(PreviewCommand) -> Desktop> Target for Preview<'_, Message> {
    type Route = window::Id;

    fn pointer_route(&self, message: &Desktop) -> Option<Self::Route> {
        (self.is_pointer_action)(message).then_some(self.sample)
    }

    fn table(&self, route: Self::Route) -> Option<&super::State> {
        (route == self.sample).then_some(self.table)
    }

    fn navigate(&self, _: Self::Route, key: Key) -> Desktop {
        (self.message)(PreviewCommand::Navigate(key))
    }

    fn focus_node(&self) -> Option<(Self::Route, &super::State)> {
        Some((self.sample, self.table))
    }

    fn tab(&self, backwards: bool) -> Option<Desktop> {
        Some((self.message)(PreviewCommand::Tab(backwards)))
    }
}

struct Live<'a> {
    agents: &'a State,
    window: window::Id,
    entry: Option<(window::Id, EntryKey)>,
    shift_f10_available: bool,
}

impl Target for Live<'_> {
    type Route = Route;

    fn pointer_route(&self, message: &Desktop) -> Option<Self::Route> {
        Route::from_pointer(message)
    }

    fn table(&self, route: Self::Route) -> Option<&super::State> {
        self.agents
            .runs
            .get(&route.run)
            .filter(|run| !run.closing)
            .map(|run| &run.presentation.log_table)
    }

    fn navigate(&self, route: Self::Route, key: Key) -> Desktop {
        route.message(Message::Navigate(key))
    }

    fn menu_shortcut(&self, name: Named, modifiers: Modifiers) -> bool {
        (name == Named::ContextMenu && modifiers.is_empty())
            || (self.shift_f10_available && name == MENU_KEY && modifiers == MENU_MODIFIERS)
    }

    fn open_menu(&self, route: Route, offset: Vector) -> Option<Desktop> {
        Some(route.message(Message::KeyboardMenu(route.surface(self.window), offset.x)))
    }

    fn menu_open(&self, route: Route) -> bool {
        self.table(route)
            .and_then(|table| table.menu)
            .is_some_and(|menu| menu.surface == route.surface(self.window))
    }

    fn focus_entry(
        &self,
        name: Named,
        modifiers: Modifiers,
        focused: Option<Route>,
    ) -> Option<Route> {
        let (run, key) = self.entry.as_ref()?;
        match key {
            EntryKey::Pane if name == PANE_FOCUS_KEY && modifiers == PANE_FOCUS_MODIFIERS => {
                Some(Route {
                    window: *run,
                    run: *run,
                })
            }
            EntryKey::Detached if matches!(navigation(name, modifiers), Some(Key::Leave)) => {
                let route = Route {
                    window: self.window,
                    run: *run,
                };
                (focused != Some(route)).then_some(route)
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
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
            AgentMessage::Design(presentation::Message::LogTable(message)) if message.takes_focus()
        )
        .then_some(Self {
            window: *window,
            run,
        })
    }

    fn surface(self, window: window::Id) -> Surface {
        if self.window != self.run {
            Surface::Detached
        } else if window == self.run {
            Surface::Activity
        } else {
            Surface::Pane
        }
    }

    fn message(self, message: Message) -> Desktop {
        let action = event(message);
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

struct Focus<R> {
    route: Option<R>,
    held: Vec<Named>,
}

impl<R> Default for Focus<R> {
    fn default() -> Self {
        Self {
            route: None,
            held: Vec::new(),
        }
    }
}

impl<R> Focus<R> {
    fn hold(&mut self, name: Named) {
        if !self.held.contains(&name) {
            self.held.push(name);
        }
    }
}

struct FocusNode<'a, R>(&'a mut Option<R>, R);

impl<R: Copy + Eq> operation::Focusable for FocusNode<'_, R> {
    fn is_focused(&self) -> bool {
        *self.0 == Some(self.1)
    }

    fn focus(&mut self) {
        *self.0 = Some(self.1);
    }

    fn unfocus(&mut self) {
        *self.0 = None;
    }
}

struct Visible<'a> {
    viewport: &'a Id,
    bounds: Option<(Rectangle, Vector)>,
}

impl Visible<'_> {
    fn find(
        viewport: &Id,
        child: &mut dyn Widget<Desktop, Theme, Renderer>,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
    ) -> Option<(Rectangle, Vector)> {
        let mut visible = Visible {
            viewport,
            bounds: None,
        };
        child.operate(tree, layout, renderer, &mut visible);
        visible.bounds
    }
}

impl Operation for Visible<'_> {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
        if self.bounds.is_none() {
            visit(self);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        _: Rectangle,
        offset: Vector,
        _: &mut dyn operation::Scrollable,
    ) {
        if id == Some(self.viewport) && bounds.width > 0.0 && bounds.height > 0.0 {
            self.bounds = Some((bounds, offset));
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

impl<T: Target> Widget<Desktop, Theme, Renderer> for Scope<'_, T> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Focus<T::Route>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Focus::<T::Route>::default())
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
        let child = self.content.as_widget_mut();
        let child_tree = &mut tree.children[0];
        if let Some((route, table)) = self.target.focus_node()
            && let Some((bounds, _)) =
                Visible::find(&table.viewport, child, child_tree, layout, renderer)
        {
            let focus = tree.state.downcast_mut::<Focus<T::Route>>();
            operation.focusable(
                Some(&table.viewport),
                bounds,
                &mut FocusNode(&mut focus.route, route),
            );
        }
        child.operate(child_tree, layout, renderer, operation);
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
        let focus = tree.state.downcast_mut::<Focus<T::Route>>();
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
                if let Some(route) = self.target.focus_entry(*name, *modifiers, focus.route)
                    && let Some(table) = self.target.table(route)
                    && Visible::find(&table.viewport, child, child_tree, layout, renderer).is_some()
                {
                    focus.hold(*name);
                    shell.capture_event();
                    if !repeat {
                        focus.route = Some(route);
                        shell.publish(self.target.navigate(route, Key::Focus));
                    }
                    return;
                }
                if self.target.menu_shortcut(*name, *modifiers)
                    && let Some(route) = focus.route
                {
                    let visible = self.target.table(route).and_then(|table| {
                        Visible::find(&table.viewport, child, child_tree, layout, renderer)
                    });
                    if let Some((_, offset)) = visible {
                        focus.hold(*name);
                        shell.capture_event();
                        if !repeat && let Some(message) = self.target.open_menu(route, offset) {
                            shell.publish(message);
                        }
                        return;
                    }
                    focus.route = None;
                }
                let key = navigation(*name, *modifiers);
                if matches!(key, Some(Key::Leave))
                    && let Some(action) = self.target.tab(modifiers.shift())
                {
                    focus.hold(*name);
                    shell.capture_event();
                    if !repeat {
                        shell.publish(action);
                    }
                    return;
                }
                if let Some(key) = key
                    && let Some(route) = focus.route
                {
                    focus.hold(*name);
                    shell.capture_event();
                    let Some(table) = self.target.table(route) else {
                        focus.route = None;
                        return;
                    };
                    if Visible::find(&table.viewport, child, child_tree, layout, renderer).is_none()
                        || matches!(key, Key::Leave)
                        || matches!(key, Key::Close) && table.detail.is_none()
                    {
                        focus.route = None;
                    } else if !repeat || matches!(key, Key::Previous | Key::Next) {
                        shell.publish(self.target.navigate(route, key));
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
                if let Some(route) = self.target.pointer_route(&message) {
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
        let focus = tree.state.downcast_mut::<Focus<T::Route>>();
        let menu_open = focus
            .route
            .is_some_and(|route| self.target.menu_open(route));
        let content = self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )?;
        if menu_open {
            Some(overlay::Element::new(Box::new(MenuOverlay {
                content,
                focus,
            })))
        } else {
            Some(content)
        }
    }
}

/// Keep key releases paired with presses consumed by the context menu, even
/// after an action dismisses the overlay and the terminal becomes reachable.
struct MenuOverlay<'a, R> {
    content: overlay::Element<'a, Desktop, Theme, Renderer>,
    focus: &'a mut Focus<R>,
}

impl<R> overlay::Overlay<Desktop, Theme, Renderer> for MenuOverlay<'_, R> {
    fn index(&self) -> f32 {
        self.content.as_overlay().index()
    }

    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        self.content.as_overlay_mut().layout(renderer, bounds)
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.content
            .as_overlay()
            .draw(renderer, theme, style, layout, cursor);
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.content
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
        shell: &mut Shell<'_, Desktop>,
    ) {
        self.content
            .as_overlay_mut()
            .update(event, layout, cursor, renderer, clipboard, shell);
        match event {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(name),
                ..
            }) if shell.is_event_captured() => self.focus.hold(*name),
            Event::Keyboard(keyboard::Event::KeyReleased {
                key: keyboard::Key::Named(name),
                ..
            }) if self.focus.held.contains(name) => {
                self.focus.held.retain(|held| held != name);
                shell.capture_event();
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_overlay()
            .mouse_interaction(layout, cursor, renderer)
    }
}
