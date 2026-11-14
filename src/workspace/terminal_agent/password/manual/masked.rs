//! Renders and edits a mask without passing the password to the widget or a message.

use std::sync::{Arc, Mutex, Weak};

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{
    Clipboard, Layout, Shell, Widget, input_method, layout, renderer, text as advanced_text,
};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::text_input::{State, Value, cursor};
use iced::widget::{Id, button, column, row, text_input};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, mouse};
use kortty_vault::Secret;

use super::super::super::Message;
use super::super::{Message as PasswordMessage, Phase, Run};

const MAX_PASSWORD_BYTES: usize = 1024;
type InputState = State<<Renderer as advanced_text::Renderer>::Paragraph>;

impl Run {
    pub(in crate::workspace::terminal_agent) fn manual_password_view(
        &self,
    ) -> Option<Element<'_, Message>> {
        if self.phase != Phase::SudoPassword || self.paused || self.cancel.requested() {
            return None;
        }
        let prompt = self.password.as_ref()?.manual.as_ref()?;
        let mask = {
            let value = prompt
                .result
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let length = value.as_ref().map_or(0, |secret| secret.chars().count());
            "•".repeat(length)
        };
        let input = Element::new(MaskedInput {
            inner: field(&prompt.field, &mask, self.epoch),
            id: prompt.field.clone(),
            mask: Value::new(&mask),
            result: Arc::downgrade(&prompt.result),
            epoch: self.epoch,
        });
        let confirm = Message::Password(PasswordMessage::ManualReady(self.epoch, true));
        let cancel = Message::Password(PasswordMessage::ManualReady(self.epoch, false));
        Some(
            column![
                input,
                row![
                    button("Submit password once")
                        .on_press_maybe((!mask.is_empty()).then_some(confirm)),
                    button("Cancel password entry").on_press(cancel),
                ]
                .spacing(6),
            ]
            .spacing(6)
            .into(),
        )
    }
}

struct MaskedInput {
    inner: Element<'static, Message>,
    id: Id,
    mask: Value,
    result: Weak<Mutex<Option<Secret>>>,
    epoch: u64,
}

fn field(id: &Id, mask: &str, epoch: u64) -> Element<'static, Message> {
    text_input("Sudo password", mask)
        .id(id.clone())
        .secure(true)
        .on_input(move |_| Message::Password(PasswordMessage::ManualEdited(epoch)))
        .into()
}

impl Widget<Message, Theme, Renderer> for MaskedInput {
    fn tag(&self) -> tree::Tag {
        self.inner.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.inner.as_widget().state()
    }

    fn diff(&self, tree: &mut Tree) {
        self.inner.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.inner.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.inner.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.inner
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        pointer: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let input = tree.state.downcast_mut::<InputState>();
        if input.is_focused() {
            if shell.is_event_captured()
                && matches!(event, Event::Keyboard(_) | Event::InputMethod(_))
            {
                return;
            }
            if self.edit(input, event, shell) {
                shell.capture_event();
                return;
            }
            if let Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modified_key,
                physical_key,
                location,
                modifiers,
                text: Some(_),
                repeat,
            }) = event
            {
                // Let Iced handle selection and navigation, with no raw input to cache.
                let shortcut = Event::Keyboard(keyboard::Event::KeyPressed {
                    key: key.clone(),
                    modified_key: modified_key.clone(),
                    physical_key: *physical_key,
                    location: *location,
                    modifiers: *modifiers,
                    text: None,
                    repeat: *repeat,
                });
                self.inner.as_widget_mut().update(
                    tree, &shortcut, layout, pointer, renderer, clipboard, shell, viewport,
                );
                return;
            }
        }
        self.inner.as_widget_mut().update(
            tree, event, layout, pointer, renderer, clipboard, shell, viewport,
        );
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        pointer: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.inner
            .as_widget()
            .draw(tree, renderer, theme, style, layout, pointer, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        pointer: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.inner
            .as_widget()
            .mouse_interaction(tree, layout, pointer, viewport, renderer)
    }
}

impl MaskedInput {
    fn edit(
        &mut self,
        input: &mut InputState,
        event: &Event,
        shell: &mut Shell<'_, Message>,
    ) -> bool {
        let (mut start, mut end) = match input.cursor().state(&self.mask) {
            cursor::State::Index(index) => (index, index),
            cursor::State::Selection { start, end } => (start.min(end), start.max(end)),
        };
        let replacement = match event {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                text,
                modifiers,
                physical_key,
                ..
            }) => match key.as_ref() {
                Key::Named(Named::Enter) => {
                    if !self.mask.is_empty() {
                        shell.publish(Message::Password(PasswordMessage::ManualReady(
                            self.epoch, true,
                        )));
                    }
                    return true;
                }
                Key::Named(Named::Escape) => {
                    shell.publish(Message::Password(PasswordMessage::ManualReady(
                        self.epoch, false,
                    )));
                    return true;
                }
                Key::Named(Named::Backspace) => {
                    if start == end {
                        start = start.saturating_sub(1);
                    }
                    ""
                }
                Key::Named(Named::Delete) => {
                    if start == end {
                        end = (end + 1).min(self.mask.len());
                    }
                    ""
                }
                _ if modifiers.command()
                    && !modifiers.alt()
                    && matches!(key.to_latin(*physical_key), Some('c' | 'x' | 'v')) =>
                {
                    return true;
                }
                _ if !modifiers.logo() && (!modifiers.control() || modifiers.alt()) => {
                    let Some(text) = text.as_deref() else {
                        return false;
                    };
                    text
                }
                _ => return false,
            },
            Event::InputMethod(input_method::Event::Commit(text)) => text.as_str(),
            Event::InputMethod(_) => return true,
            _ => return false,
        };
        let Some(result) = self.result.upgrade() else {
            return true;
        };
        let mut result = result.lock().unwrap_or_else(|error| error.into_inner());
        let previous = result.as_ref().map_or("", |secret| secret.as_str());
        if let Some(edited) = edit_secret(previous, start, end, replacement) {
            let mask = "•".repeat(edited.chars().count());
            self.mask = Value::new(&mask);
            self.inner = field(&self.id, &mask, self.epoch);
            *result = (!edited.is_empty()).then_some(edited);
            input.move_cursor_to(start + replacement.chars().count());
            shell.invalidate_layout();
            shell.publish(Message::Password(PasswordMessage::ManualEdited(self.epoch)));
        }
        true
    }
}

fn edit_secret(previous: &str, start: usize, end: usize, replacement: &str) -> Option<Secret> {
    if replacement.chars().any(char::is_control) {
        return None;
    }
    let index = |offset| {
        previous
            .char_indices()
            .nth(offset)
            .map_or(previous.len(), |(byte, _)| byte)
    };
    let (start, end) = (index(start), index(end));
    let length = previous.len() - (end - start) + replacement.len();
    if length > MAX_PASSWORD_BYTES {
        return None;
    }
    let mut result = Secret::new(String::with_capacity(length));
    result.push_str(&previous[..start]);
    result.push_str(replacement);
    result.push_str(&previous[end..]);
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::clipboard;
    use iced::{Font, Point};
    use iced_runtime::{UserInterface, user_interface::Cache};

    #[test]
    fn edit_secret_preserves_unicode_and_bounds_input() {
        assert_eq!(edit_secret("äñ", 1, 2, "💡").unwrap().as_str(), "ä💡");
        assert!(edit_secret("p", 1, 1, "\n").is_none());
        assert!(edit_secret("ä", 1, 1, &"x".repeat(MAX_PASSWORD_BYTES - 1)).is_none());
        assert_eq!(edit_secret("äñ", 0, 2, "").unwrap().as_str(), "");
    }

    #[test]
    fn keyboard_edits_and_ime_commit_do_not_put_secrets_in_widget_messages() {
        let result = Arc::new(Mutex::new(None));
        let id = iced::widget::Id::unique();
        let inner = text_input("Sudo password", "")
            .id(id.clone())
            .secure(true)
            .on_input(|_| panic!("Password edit reached the display widget"))
            .into();
        let input = Element::new(MaskedInput {
            inner,
            id: id.clone(),
            mask: Value::new(""),
            result: Arc::downgrade(&result),
            epoch: 42,
        });
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let mut ui = UserInterface::build(
            input,
            Size::new(300.0, 100.0),
            Cache::default(),
            &mut renderer,
        );
        crate::settings_search::test_input::apply(
            &mut ui,
            &renderer,
            iced::widget::operation::focus(id),
        );
        let messages = crate::settings_search::test_input::press(
            &mut ui,
            &mut renderer,
            Key::Character("ä".into()),
            Some("ä"),
        );
        assert!(matches!(
            &messages[..],
            [Message::Password(PasswordMessage::ManualEdited(42))]
        ));
        let mut messages = Vec::new();
        ui.update(
            &[
                Event::InputMethod(input_method::Event::Preedit("未".into(), None)),
                Event::InputMethod(input_method::Event::Commit("漢".into())),
            ],
            mouse::Cursor::Available(Point::ORIGIN),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(matches!(
            &messages[..],
            [Message::Password(PasswordMessage::ManualEdited(42))]
        ));
        let secret = result.lock().unwrap().take().unwrap();
        assert_eq!(secret.as_str(), "ä漢");
    }
}
