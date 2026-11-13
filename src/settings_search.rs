use iced::advanced::widget::{Operation, Tree, operation, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, renderer, text};
use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{Id, TextInput, text_input};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Task, Theme, Vector, mouse};

pub(crate) fn input<'a, Message: Clone + 'a>(
    input: TextInput<'a, Message>,
    up: Message,
    down: Message,
    escape: Option<Message>,
    refocus: Option<Message>,
) -> Element<'a, Message> {
    Element::new(SearchInput {
        input: input.into(),
        up,
        down,
        escape,
        refocus,
    })
}

pub(crate) fn find_shortcut(key: &Key, modifiers: Modifiers) -> bool {
    modifiers.command()
        && !modifiers.alt()
        && !modifiers.shift()
        && matches!(key, Key::Character(value) if value.eq_ignore_ascii_case("f"))
}

pub(crate) fn mark_input<'a, Message: Clone>(
    input: TextInput<'a, Message>,
    target: Option<&Id>,
) -> TextInput<'a, Message> {
    match target {
        Some(id) => input.id(id.clone()),
        None => input,
    }
}

pub(crate) fn focus_if_present<Message>(target: Id) -> Task<Message> {
    iced_runtime::task::effect(iced_runtime::Action::widget(FocusIfPresent {
        target,
        found: false,
    }))
}

struct FocusIfPresent {
    target: Id,
    found: bool,
}

impl Operation for FocusIfPresent {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn focusable(&mut self, id: Option<&Id>, _: Rectangle, _: &mut dyn operation::Focusable) {
        self.found |= id == Some(&self.target);
    }

    fn finish(&self) -> operation::Outcome<()> {
        // Iced clears existing focus when its focus operation cannot find the target.
        if self.found {
            operation::Outcome::Chain(Box::new(operation::focusable::focus(self.target.clone())))
        } else {
            operation::Outcome::None
        }
    }
}

pub(crate) fn scroll_to_row<Message>(
    page: iced::widget::Id,
    row: iced::widget::Id,
) -> Task<Message> {
    iced_runtime::task::effect(iced_runtime::Action::widget(ScrollToRow {
        page,
        row,
        viewport: None,
        target: None,
    }))
}

struct ScrollToRow {
    page: iced::widget::Id,
    row: iced::widget::Id,
    viewport: Option<Rectangle>,
    target: Option<Rectangle>,
}

impl Operation for ScrollToRow {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&iced::widget::Id>, bounds: Rectangle) {
        if id == Some(&self.row) {
            self.target = Some(bounds);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&iced::widget::Id>,
        bounds: Rectangle,
        _: Rectangle,
        _: Vector,
        _: &mut dyn operation::Scrollable,
    ) {
        if id == Some(&self.page) {
            self.viewport = Some(bounds);
        }
    }

    fn finish(&self) -> operation::Outcome<()> {
        let Some((viewport, target)) = self.viewport.zip(self.target) else {
            return operation::Outcome::None;
        };
        let y = (target.y - viewport.y - viewport.height / 3.0).max(0.0);
        operation::Outcome::Chain(Box::new(operation::scrollable::scroll_to(
            self.page.clone(),
            operation::scrollable::AbsoluteOffset {
                x: None,
                y: Some(y),
            },
        )))
    }
}

struct SearchInput<'a, Message> {
    input: Element<'a, Message>,
    up: Message,
    down: Message,
    escape: Option<Message>,
    refocus: Option<Message>,
}

impl<Message: Clone> Widget<Message, Theme, Renderer> for SearchInput<'_, Message> {
    fn tag(&self) -> tree::Tag {
        self.input.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.input.as_widget().state()
    }

    fn diff(&self, tree: &mut Tree) {
        self.input.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.input.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.input.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.input
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
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
        let state = tree
            .state
            .downcast_ref::<text_input::State<<Renderer as text::Renderer>::Paragraph>>();
        if state.is_focused()
            && let Event::Keyboard(iced::keyboard::Event::KeyPressed { key, modifiers, .. }) = event
        {
            let action = if find_shortcut(key, *modifiers) {
                self.refocus.as_ref()
            } else if modifiers.is_empty() {
                match key {
                    Key::Named(Named::ArrowUp) => Some(&self.up),
                    Key::Named(Named::ArrowDown) => Some(&self.down),
                    Key::Named(Named::Escape) => self.escape.as_ref(),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(action) = action {
                shell.publish(action.clone());
                shell.capture_event();
                return;
            }
        }
        self.input.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
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
        self.input
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.input
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }
}

#[cfg(test)]
pub(crate) mod test_input {
    use iced::advanced::clipboard;
    use iced::futures::{StreamExt, executor::block_on};
    use iced::keyboard::{self, Location, key};
    use iced_runtime::{Action, UserInterface, task};

    use super::*;

    pub(crate) fn press<Message>(
        interface: &mut UserInterface<'_, Message, Theme, Renderer>,
        renderer: &mut Renderer,
        key: Key,
        text: Option<&str>,
    ) -> Vec<Message> {
        let mut messages = Vec::new();
        let (_, statuses) = interface.update(
            &[Event::Keyboard(keyboard::Event::KeyPressed {
                modified_key: key.clone(),
                key,
                physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                location: Location::Standard,
                modifiers: Modifiers::empty(),
                text: text.map(Into::into),
                repeat: false,
            })],
            mouse::Cursor::Unavailable,
            renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert_eq!(statuses, [iced::event::Status::Captured]);
        messages
    }

    pub(crate) fn apply<Message>(
        interface: &mut UserInterface<'_, Message, Theme, Renderer>,
        renderer: &Renderer,
        task: Task<Message>,
    ) {
        let mut actions = task::into_stream(task).expect("expected a reveal task");
        while let Some(action) = block_on(actions.next()) {
            if let Action::Widget(mut operation) = action {
                loop {
                    interface.operate(renderer, operation.as_mut());
                    let operation::Outcome::Chain(next) = operation.finish() else {
                        break;
                    };
                    operation = next;
                }
            }
        }
    }
}
