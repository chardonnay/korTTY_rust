//! A menu anchored to its actual header bounds, including on wrapped rows.

use iced::advanced::overlay::{self, Overlay};
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, renderer};
use iced::keyboard::{self, Key, Modifiers, key::Named};
use iced::mouse;
use iced::{Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector};

use super::{Navigation, WorkspaceMessage};

pub(in crate::workspace) fn anchor<'a>(
    trigger: Element<'a, WorkspaceMessage>,
    contents: Element<'a, WorkspaceMessage>,
    dismiss: WorkspaceMessage,
    navigate: impl Fn(Navigation) -> WorkspaceMessage + 'a,
) -> Element<'a, WorkspaceMessage> {
    Element::new(Anchor {
        trigger,
        open_context: None,
        popup: Some(AnchorContent {
            contents,
            placement: Placement::Below,
            commands: Some(Commands::menu(dismiss, navigate)),
        }),
    })
}

pub(in crate::workspace) fn at_point<'a>(
    trigger: Element<'a, WorkspaceMessage>,
    point: Point,
    contents: Element<'a, WorkspaceMessage>,
    dismiss: WorkspaceMessage,
    navigate: impl Fn(Navigation) -> WorkspaceMessage + 'a,
) -> Element<'a, WorkspaceMessage> {
    Element::new(Anchor {
        trigger,
        open_context: None,
        popup: Some(AnchorContent {
            contents,
            placement: Placement::Point(point),
            commands: Some(Commands::menu(dismiss, navigate)),
        }),
    })
}

/// Keep the trigger in the same widget tree slot as when the point menu is visible.
pub(in crate::workspace) fn without_menu(
    trigger: Element<'_, WorkspaceMessage>,
) -> Element<'_, WorkspaceMessage> {
    Element::new(Anchor {
        trigger,
        open_context: None,
        popup: None,
    })
}

pub(super) fn toolbar_item_menu<'a>(
    trigger: Element<'a, WorkspaceMessage>,
    point: Option<Point>,
    contents: Element<'a, WorkspaceMessage>,
    dismiss: WorkspaceMessage,
    navigate: impl Fn(Navigation) -> WorkspaceMessage + 'a,
) -> Element<'a, WorkspaceMessage> {
    Element::new(Anchor {
        trigger,
        open_context: None,
        popup: Some(AnchorContent {
            contents,
            placement: point.map_or(Placement::Popover { nested: false }, Placement::Context),
            commands: Some(Commands::menu(dismiss, navigate)),
        }),
    })
}

pub(in crate::workspace) fn picker<'a>(
    trigger: Element<'a, WorkspaceMessage>,
    contents: Element<'a, WorkspaceMessage>,
    dismiss: WorkspaceMessage,
    keyboard: impl Fn(&Key, Modifiers) -> Option<WorkspaceMessage> + 'a,
) -> Element<'a, WorkspaceMessage> {
    Element::new(Anchor {
        trigger,
        open_context: None,
        popup: Some(AnchorContent {
            contents,
            placement: Placement::Centered,
            commands: Some(Commands {
                dismiss,
                keyboard: Box::new(keyboard),
                forward_input: true,
            }),
        }),
    })
}

/// Anchors interactive controls to their trigger, leaving nested menus room in the window.
pub(in crate::workspace) fn popover<'a, Message: Clone + 'a>(
    trigger: Element<'a, Message>,
    contents: Option<Element<'a, Message>>,
    dismiss: Message,
) -> Element<'a, Message> {
    interactive_popover(trigger, contents, dismiss, false)
}

/// An option list can dismiss itself while letting the parent handle the same outside click.
pub(in crate::workspace) fn popover_options<'a, Message: Clone + 'a>(
    trigger: Element<'a, Message>,
    contents: Option<Element<'a, Message>>,
    dismiss: Message,
) -> Element<'a, Message> {
    interactive_popover(trigger, contents, dismiss, true)
}

/// Opens a context menu at the right-click position within its trigger.
pub(in crate::workspace) fn context_action<'a, Message: Clone + 'a>(
    trigger: Element<'a, Message>,
    open: impl Fn(Point) -> Message + 'a,
    popup: Option<(Point, Element<'a, Message>)>,
    activate: Message,
    dismiss: Message,
    move_selection: Option<[Message; 2]>,
) -> Element<'a, Message> {
    let escape = dismiss.clone();
    context_action_with_keyboard(trigger, open, popup, dismiss, move |key, _| match key {
        Key::Named(Named::Escape) => Some(escape.clone()),
        Key::Named(Named::Enter) => Some(activate.clone()),
        Key::Named(Named::ArrowUp) => move_selection.as_ref().map(|steps| steps[0].clone()),
        Key::Named(Named::ArrowDown) => move_selection.as_ref().map(|steps| steps[1].clone()),
        _ => None,
    })
}

/// Anchors a context menu that also handles its own submenu navigation.
pub(crate) fn context_action_with_keyboard<'a, Message: Clone + 'a>(
    trigger: Element<'a, Message>,
    open: impl Fn(Point) -> Message + 'a,
    popup: Option<(Point, Element<'a, Message>)>,
    dismiss: Message,
    keyboard: impl Fn(&Key, Modifiers) -> Option<Message> + 'a,
) -> Element<'a, Message> {
    Element::new(Anchor {
        trigger,
        open_context: Some(Box::new(open)),
        popup: popup.map(|(point, contents)| AnchorContent {
            contents,
            placement: Placement::Context(point),
            commands: Some(Commands {
                dismiss,
                keyboard: Box::new(keyboard),
                forward_input: false,
            }),
        }),
    })
}

fn interactive_popover<'a, Message: Clone + 'a>(
    trigger: Element<'a, Message>,
    contents: Option<Element<'a, Message>>,
    dismiss: Message,
    nested: bool,
) -> Element<'a, Message> {
    Element::new(Anchor {
        trigger,
        open_context: None,
        popup: contents.map(|contents| {
            let escape = dismiss.clone();
            AnchorContent {
                contents,
                placement: Placement::Popover { nested },
                commands: Some(Commands {
                    dismiss,
                    keyboard: Box::new(move |key, _| {
                        (key == &Key::Named(Named::Escape)).then(|| escape.clone())
                    }),
                    forward_input: true,
                }),
            }
        }),
    })
}

pub(crate) fn submenu<'a, Message: Clone + 'a>(
    trigger: Element<'a, Message>,
    contents: Element<'a, Message>,
) -> Element<'a, Message> {
    Element::new(Anchor {
        trigger,
        open_context: None,
        popup: Some(AnchorContent {
            contents,
            placement: Placement::Side,
            commands: None,
        }),
    })
}

#[derive(Clone, Copy)]
enum Placement {
    Below,
    Popover { nested: bool },
    Point(Point),
    Context(Point),
    Side,
    Centered,
}

type KeyboardHandler<'a, Message> = dyn Fn(&Key, Modifiers) -> Option<Message> + 'a;

struct Commands<'a, Message> {
    dismiss: Message,
    keyboard: Box<KeyboardHandler<'a, Message>>,
    forward_input: bool,
}

impl<'a> Commands<'a, WorkspaceMessage> {
    fn menu(
        dismiss: WorkspaceMessage,
        navigate: impl Fn(Navigation) -> WorkspaceMessage + 'a,
    ) -> Self {
        Self {
            dismiss,
            keyboard: Box::new(move |key, _| {
                Some(navigate(match key {
                    Key::Named(Named::Escape) => Navigation::Escape,
                    Key::Named(Named::ArrowUp) => Navigation::Previous,
                    Key::Named(Named::ArrowDown) => Navigation::Next,
                    Key::Named(Named::ArrowLeft) => Navigation::Left,
                    Key::Named(Named::ArrowRight) => Navigation::Right,
                    Key::Named(Named::Enter) => Navigation::Activate,
                    _ => return None,
                }))
            }),
            forward_input: false,
        }
    }
}

struct Anchor<'a, Message> {
    trigger: Element<'a, Message>,
    popup: Option<AnchorContent<'a, Message>>,
    open_context: Option<Box<dyn Fn(Point) -> Message + 'a>>,
}

struct AnchorContent<'a, Message> {
    contents: Element<'a, Message>,
    placement: Placement,
    commands: Option<Commands<'a, Message>>,
}

impl<'view, Message: Clone + 'view> Widget<Message, Theme, Renderer> for Anchor<'view, Message> {
    fn children(&self) -> Vec<Tree> {
        let mut children = vec![Tree::new(&self.trigger)];
        if let Some(popup) = &self.popup {
            children.push(Tree::new(&popup.contents));
        }
        children
    }

    fn diff(&self, tree: &mut Tree) {
        if let Some(popup) = &self.popup {
            tree.diff_children(&[&self.trigger, &popup.contents]);
        } else {
            tree.diff_children(std::slice::from_ref(&self.trigger));
        }
    }

    fn size(&self) -> Size<Length> {
        self.trigger.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.trigger.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.trigger
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
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
        self.trigger.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.trigger
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
        self.trigger.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if !shell.is_event_captured()
            && let Some(open) = &self.open_context
            && matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
            )
            && cursor.is_over(*viewport)
            && let Some(point) = cursor.position_in(layout.bounds())
        {
            shell.publish(open(point));
            shell.capture_event();
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
        if self.popup.is_some() {
            mouse::Interaction::None
        } else {
            self.trigger.as_widget().mouse_interaction(
                &tree.children[0],
                layout,
                cursor,
                viewport,
                renderer,
            )
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
        let Some(popup) = self.popup.as_mut() else {
            return self.trigger.as_widget_mut().overlay(
                &mut tree.children[0],
                layout,
                renderer,
                viewport,
                translation,
            );
        };
        let mut anchor = layout.bounds() + translation;
        if let Placement::Point(point) | Placement::Context(point) = popup.placement {
            anchor = Rectangle {
                x: anchor.x + point.x.clamp(0.0, anchor.width),
                y: anchor.y + point.y.clamp(0.0, anchor.height),
                width: 0.0,
                height: 0.0,
            };
        }
        if matches!(popup.placement, Placement::Side) && !viewport.intersects(&anchor) {
            return None;
        }
        Some(overlay::Element::new(Box::new(Popup {
            contents: &mut popup.contents,
            tree: &mut tree.children[1],
            anchor,
            placement: popup.placement,
            commands: popup.commands.as_ref(),
            window: viewport.size(),
        })))
    }
}

struct Popup<'a, 'view, Message> {
    contents: &'a mut Element<'view, Message>,
    tree: &'a mut Tree,
    anchor: Rectangle,
    placement: Placement,
    commands: Option<&'a Commands<'view, Message>>,
    window: Size,
}

impl<'view, Message: Clone + 'view> Overlay<Message, Theme, Renderer>
    for Popup<'_, 'view, Message>
{
    fn layout(&mut self, renderer: &Renderer, viewport: Size) -> layout::Node {
        self.window = viewport;
        if matches!(self.placement, Placement::Centered) {
            let inset = 40.0_f32.min(viewport.height * 0.12);
            let limits = layout::Limits::new(
                Size::ZERO,
                Size::new(viewport.width, viewport.height - inset),
            );
            let node = self
                .contents
                .as_widget_mut()
                .layout(self.tree, renderer, &limits);
            let x = ((viewport.width - node.size().width) / 2.0).max(0.0);
            return node.move_to(Point::new(x, inset));
        }
        if matches!(self.placement, Placement::Side) {
            let (right, room) = side_room(self.anchor, viewport.width);
            let limits = layout::Limits::new(Size::ZERO, Size::new(room, viewport.height));
            let node = self
                .contents
                .as_widget_mut()
                .layout(self.tree, renderer, &limits);
            let size = node.size();
            let x = if right {
                self.anchor.x + self.anchor.width
            } else {
                self.anchor.x - size.width
            };
            let y = self
                .anchor
                .y
                .clamp(0.0, (viewport.height - size.height).max(0.0));
            return node.move_to(Point::new(x.max(0.0), y));
        }
        let gap = if matches!(self.placement, Placement::Popover { .. }) {
            4.0
        } else {
            0.0
        };
        let below = viewport.height - self.anchor.y - self.anchor.height - gap;
        let above = self.anchor.y - gap;
        let limits = layout::Limits::new(Size::ZERO, Size::new(viewport.width, below.max(above)));
        let node = self
            .contents
            .as_widget_mut()
            .layout(self.tree, renderer, &limits);
        let size = node.size();
        let x = self.anchor.x.min((viewport.width - size.width).max(0.0));
        let y = if size.height <= below || below >= above {
            self.anchor.y + self.anchor.height + gap
        } else {
            (self.anchor.y - gap - size.height).max(0.0)
        };
        node.move_to(Point::new(x, y))
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.contents.as_widget().draw(
            self.tree,
            renderer,
            theme,
            style,
            layout,
            cursor,
            &layout.bounds(),
        );
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
        let popover = matches!(
            self.placement,
            Placement::Popover { .. } | Placement::Context(_)
        );
        if popover
            && matches!(event, Event::Window(iced::window::Event::Unfocused))
            && let Some(commands) = self.commands
        {
            shell.publish(commands.dismiss.clone());
            return;
        }
        if let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event
            && let Some(commands) = self.commands
        {
            if let Some(message) = (commands.keyboard)(key, *modifiers) {
                shell.publish(message);
                shell.capture_event();
                return;
            }
            if !commands.forward_input {
                shell.capture_event();
                return;
            }
        }
        let bounds = layout.bounds();
        let over = cursor.is_over(bounds);
        if !over
            && (!popover || cursor.position().is_some())
            && matches!(event, Event::Mouse(mouse::Event::ButtonPressed(_)))
            && let Some(commands) = self.commands
        {
            shell.publish(commands.dismiss.clone());
            if !matches!(self.placement, Placement::Popover { nested: true }) {
                shell.capture_event();
            }
            return;
        }
        self.contents.as_widget_mut().update(
            self.tree, event, layout, cursor, renderer, clipboard, shell, &bounds,
        );
        if (popover || matches!(self.placement, Placement::Centered))
            && matches!(
                event,
                Event::Keyboard(_) | Event::Mouse(mouse::Event::WheelScrolled { .. })
            )
            || (popover || matches!(self.placement, Placement::Centered | Placement::Side))
                && over
                && matches!(
                    event,
                    Event::Mouse(mouse::Event::ButtonPressed(_) | mouse::Event::ButtonReleased(_))
                )
        {
            shell.capture_event();
        }
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.contents
            .as_widget_mut()
            .operate(self.tree, layout, renderer, operation);
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let interaction = self.contents.as_widget().mouse_interaction(
            self.tree,
            layout,
            cursor,
            &layout.bounds(),
            renderer,
        );
        if interaction == mouse::Interaction::None && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Idle
        } else {
            interaction
        }
    }

    fn overlay<'a>(
        &'a mut self,
        layout: Layout<'a>,
        renderer: &Renderer,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        let viewport = if matches!(self.placement, Placement::Popover { .. }) {
            Rectangle::with_size(self.window)
        } else {
            layout.bounds()
        };
        self.contents
            .as_widget_mut()
            .overlay(self.tree, layout, renderer, &viewport, Vector::ZERO)
    }
}

fn side_room(anchor: Rectangle, viewport_width: f32) -> (bool, f32) {
    let right = (viewport_width - anchor.x - anchor.width).max(0.0);
    let left = anchor.x.max(0.0);
    // Prefer the conventional right side whenever it fits a full menu.
    let right_side = right >= 306.0 || right >= left;
    (right_side, if right_side { right } else { left })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submenu_opens_toward_available_space() {
        let anchor = |x| Rectangle {
            x,
            y: 40.0,
            width: 150.0,
            height: 28.0,
        };
        assert_eq!(side_room(anchor(30.0), 600.0), (true, 420.0));
        assert_eq!(side_room(anchor(400.0), 600.0), (false, 400.0));
        assert_eq!(side_room(anchor(30.0), 220.0), (true, 40.0));
    }

    #[test]
    fn nested_context_menu_handles_its_row_before_the_background() {
        use iced::advanced::clipboard;
        use iced::widget::button;
        use iced::{Font, Pixels};

        let inner = context_action(
            button("Connection").width(200).height(80).into(),
            |_| 1u8,
            None,
            2,
            3,
            None,
        );
        let mut view = context_action(inner, |_| 4u8, None, 5, 6, None);
        let renderer = Renderer::new(Font::DEFAULT, Pixels(13.0));
        let size = Size::new(400.0, 300.0);
        let viewport = Rectangle::with_size(size);
        let mut tree = Tree::new(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        view.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)),
            Layout::new(&node),
            mouse::Cursor::Available(Point::new(30.0, 20.0)),
            &renderer,
            &mut clipboard::Null,
            &mut shell,
            &viewport,
        );
        assert!(shell.is_event_captured());
        assert_eq!(messages, [1]);
    }

    #[test]
    fn journal_screenshot_context_opens_at_the_click_and_handles_enter_and_escape() {
        use iced::advanced::clipboard;
        use iced::keyboard::{
            Location,
            key::{Code, Physical},
        };
        use iced::widget::button;
        use iced::{Font, Pixels};

        #[derive(Clone, Debug, PartialEq)]
        enum Control {
            Open(Point),
            Export,
            Dismiss,
        }

        let point = Point::new(52.0, 18.0);
        let mut view = context_action(
            button("Screenshot").width(200).height(150).into(),
            Control::Open,
            Some((point, button("Export screenshot").into())),
            Control::Export,
            Control::Dismiss,
            None,
        );
        let renderer = Renderer::new(Font::DEFAULT, Pixels(13.0));
        let size = Size::new(500.0, 400.0);
        let viewport = Rectangle::with_size(size);
        let mut tree = Tree::new(view.as_widget());
        let base = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        view.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)),
            Layout::new(&base),
            mouse::Cursor::Available(point),
            &renderer,
            &mut clipboard::Null,
            &mut shell,
            &viewport,
        );
        assert!(shell.is_event_captured());
        assert_eq!(messages, [Control::Open(point)]);

        let element = view
            .as_widget_mut()
            .overlay(
                &mut tree,
                Layout::new(&base),
                &renderer,
                &viewport,
                Vector::ZERO,
            )
            .unwrap();
        let mut popup = overlay::Nested::new(element);
        let panel = popup.layout(&renderer, size);
        assert_eq!(
            Layout::new(&panel)
                .children()
                .next()
                .unwrap()
                .bounds()
                .position(),
            point
        );
        for (key, physical, expected) in [
            (Named::Enter, Code::Enter, Control::Export),
            (Named::Escape, Code::Escape, Control::Dismiss),
        ] {
            let event = Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(key),
                modified_key: Key::Named(key),
                physical_key: Physical::Code(physical),
                location: Location::Standard,
                modifiers: Modifiers::empty(),
                text: None,
                repeat: false,
            });
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            popup.update(
                &event,
                Layout::new(&panel),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut clipboard::Null,
                &mut shell,
            );
            assert!(shell.is_event_captured());
            assert_eq!(messages, [expected]);
        }
    }

    #[test]
    fn journal_appearance_popover_dispatches_nested_picker_and_consumes_dismissal() {
        use iced::advanced::clipboard;
        use iced::keyboard::{
            Location,
            key::{Code, Physical},
        };
        use iced::widget::{button, container, pick_list};
        use iced::{Font, Pixels};

        #[derive(Clone, Debug, Eq, PartialEq)]
        enum Control {
            Dismiss,
            Font(&'static str),
        }

        let mut view = popover(
            button("Appearance").into(),
            Some(
                container(
                    pick_list(["One", "Two"], Some("One"), Control::Font).width(Length::Fill),
                )
                .width(220)
                .padding(6)
                .into(),
            ),
            Control::Dismiss,
        );
        let renderer = Renderer::new(Font::DEFAULT, Pixels(13.0));
        let size = Size::new(500.0, 400.0);
        let mut tree = Tree::new(view.as_widget());
        let base = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let element = view
            .as_widget_mut()
            .overlay(
                &mut tree,
                Layout::new(&base),
                &renderer,
                &Rectangle::with_size(size),
                Vector::ZERO,
            )
            .unwrap();
        let mut nested = overlay::Nested::new(element);
        let dispatch = |nested: &mut overlay::Nested<'_, Control, Theme, Renderer>,
                        node: &layout::Node,
                        event: Event,
                        cursor| {
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            nested.update(
                &event,
                Layout::new(node),
                cursor,
                &renderer,
                &mut clipboard::Null,
                &mut shell,
            );
            let captured = shell.is_event_captured();
            (messages, captured)
        };

        let layout = nested.layout(&renderer, size);
        let panel = Layout::new(&layout).children().next().unwrap().bounds();
        let click = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        assert_eq!(
            dispatch(
                &mut nested,
                &layout,
                click.clone(),
                mouse::Cursor::Available(panel.center())
            ),
            (Vec::new(), true)
        );

        let layout = nested.layout(&renderer, size);
        let menu = Layout::new(&layout)
            .children()
            .nth(1)
            .unwrap()
            .children()
            .next()
            .unwrap()
            .bounds();
        let option = Point::new(menu.center_x(), menu.y + menu.height * 0.75);
        assert!(!panel.contains(option));
        let cursor = mouse::Cursor::Available(option);
        assert_ne!(
            nested.mouse_interaction(Layout::new(&layout), cursor, &renderer),
            mouse::Interaction::None
        );
        let _ = dispatch(
            &mut nested,
            &layout,
            Event::Mouse(mouse::Event::CursorMoved { position: option }),
            cursor,
        );
        assert_eq!(
            dispatch(&mut nested, &layout, click.clone(), cursor),
            (vec![Control::Font("Two")], true)
        );

        let layout = nested.layout(&renderer, size);
        // Test double for a physical Iced keyboard event delivered by the window backend.
        let escape = Event::Keyboard(keyboard::Event::KeyPressed {
            key: Key::Named(Named::Escape),
            modified_key: Key::Named(Named::Escape),
            physical_key: Physical::Code(Code::Escape),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
            text: None,
            repeat: false,
        });
        assert_eq!(
            dispatch(&mut nested, &layout, escape, mouse::Cursor::Unavailable),
            (vec![Control::Dismiss], true)
        );
        assert_eq!(
            dispatch(
                &mut nested,
                &layout,
                click,
                mouse::Cursor::Available(Point::new(450.0, 350.0))
            ),
            (vec![Control::Dismiss], true)
        );
    }
}
