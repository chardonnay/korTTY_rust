//! Local find panel for the native Markdown editors.

use iced::advanced::text::highlighter::PlainText;
use iced::keyboard::{Key, key::Named};
use iced::widget::{Id, TextEditor, button, column, operation, row, text, text_editor, text_input};
use iced::{Element, Length, Task};

use crate::editor_window::{MAX_QUERY_BYTES, SearchDirection, literal_cursor};

#[derive(Clone)]
pub(crate) enum Message {
    Open,
    Close,
    Query(String),
    Step(SearchDirection),
}

pub(super) struct Find {
    editor: Id,
    input: Id,
    visible: bool,
    query: String,
    status: Option<String>,
}

impl Default for Find {
    fn default() -> Self {
        Self {
            editor: Id::unique(),
            input: Id::unique(),
            visible: false,
            query: String::new(),
            status: None,
        }
    }
}

impl Find {
    pub(super) fn is_open(&self) -> bool {
        self.visible
    }

    pub(super) fn reset(&mut self) {
        self.visible = false;
        self.query.clear();
        self.status = None;
    }

    pub(super) fn bind<'a, M: Clone + 'a>(
        &self,
        editor: TextEditor<'a, PlainText, M>,
        wrap: fn(Message) -> M,
    ) -> TextEditor<'a, PlainText, M> {
        let open = self.visible;
        editor.id(self.editor.clone()).key_binding(move |press| {
            if let Some(message) = shortcut(open, &press) {
                return Some(text_editor::Binding::Custom(wrap(message)));
            }
            text_editor::Binding::from_key_press(press)
        })
    }

    pub(super) fn update<M>(
        &mut self,
        content: &mut text_editor::Content,
        message: Message,
    ) -> Task<M> {
        match message {
            Message::Open => {
                self.visible = true;
                return operation::focus(self.input.clone());
            }
            Message::Close if self.visible => {
                self.visible = false;
                return operation::focus(self.editor.clone());
            }
            Message::Query(query) if self.visible => {
                self.query =
                    query[..query.floor_char_boundary(query.len().min(MAX_QUERY_BYTES))].to_owned();
                self.status = None;
            }
            Message::Step(direction) if self.visible && !self.query.is_empty() => {
                match literal_cursor(content, &self.query, direction) {
                    Ok(cursor) => {
                        let line = cursor.selection.unwrap_or(cursor.position).line + 1;
                        self.status = Some(format!("Found on line {line}"));
                        content.move_to(cursor);
                        return operation::focus(self.editor.clone());
                    }
                    Err(error) => self.status = Some(error.into()),
                }
            }
            _ => {}
        }
        Task::none()
    }

    pub(super) fn view<'a, M: Clone + 'a>(
        &'a self,
        wrap: fn(Message) -> M,
    ) -> Option<Element<'a, M>> {
        if !self.visible {
            return None;
        }
        let next = || wrap(Message::Step(SearchDirection::Next));
        let previous = || wrap(Message::Step(SearchDirection::Previous));
        let input = crate::settings_search::input(
            text_input("Find in Markdown…", &self.query)
                .id(self.input.clone())
                .width(Length::Fill)
                .on_input(move |query| wrap(Message::Query(query)))
                .on_submit(next()),
            previous(),
            next(),
            Some(wrap(Message::Close)),
            Some(wrap(Message::Open)),
        );
        let has_query = !self.query.is_empty();
        Some(
            column![
                input,
                row![
                    button("Previous").on_press_maybe(has_query.then(previous)),
                    button("Next").on_press_maybe(has_query.then(next)),
                    button("Close find").on_press(wrap(Message::Close)),
                    self.status.as_deref().map(|status| text(status).size(13)),
                ]
                .spacing(5)
                .wrap(),
            ]
            .spacing(5)
            .into(),
        )
    }
}

fn shortcut(open: bool, press: &text_editor::KeyPress) -> Option<Message> {
    if !matches!(press.status, text_editor::Status::Focused { .. }) {
        return None;
    }
    let modifiers = press.modifiers;
    if crate::settings_search::find_shortcut(&press.key, modifiers) {
        return Some(Message::Open);
    }
    if open && modifiers.is_empty() && press.key == Key::Named(Named::Escape) {
        return Some(Message::Close);
    }
    if open && press.key == Key::Named(Named::F3) && !modifiers.command() && !modifiers.alt() {
        return Some(Message::Step(if modifiers.shift() {
            SearchDirection::Previous
        } else {
            SearchDirection::Next
        }));
    }
    None
}

#[cfg(test)]
mod tests {
    use iced::advanced::{Layout, Shell, clipboard, layout, widget::Tree};
    use iced::keyboard::{Location, Modifiers, key};
    use iced::{Event, Font, Renderer, Size, mouse};

    use super::*;

    #[derive(Clone)]
    enum TestMessage {
        Edited,
        Find(Message),
    }

    #[test]
    fn markdown_find_shortcut_is_captured_only_by_the_focused_editor() {
        let find = Find::default();
        let content = text_editor::Content::with_text("private draft");
        let mut view: Element<'_, TestMessage> = find
            .bind(
                TextEditor::new(&content).on_action(|_| TestMessage::Edited),
                TestMessage::Find,
            )
            .into();
        let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut tree = Tree::new(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(320.0, 180.0)),
        );
        let dispatch = |view: &mut Element<'_, TestMessage>, tree: &mut Tree| {
            let event = Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: Key::Character("f".into()),
                modified_key: Key::Character("f".into()),
                physical_key: key::Physical::Code(key::Code::KeyF),
                location: Location::Standard,
                modifiers: Modifiers::COMMAND,
                text: None,
                repeat: false,
            });
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            view.as_widget_mut().update(
                tree,
                &event,
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut clipboard::Null,
                &mut shell,
                &node.bounds(),
            );
            (shell.is_event_captured(), messages)
        };
        assert!(matches!(dispatch(&mut view, &mut tree), (false, messages) if messages.is_empty()));
        view.as_widget_mut().operate(
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut iced::advanced::widget::operation::focusable::focus(find.editor.clone()),
        );
        assert!(matches!(
            dispatch(&mut view, &mut tree),
            (true, messages) if matches!(messages.as_slice(), [TestMessage::Find(Message::Open)])
        ));
    }

    #[test]
    fn markdown_find_selects_unicode_matches_and_wraps_without_editing() {
        let mut find = Find::default();
        let mut content = text_editor::Content::with_text("ÄPFEL\r\nbefore äpfel after");
        let before = content.text();
        let _ = find.update::<()>(&mut content, Message::Open);
        let _ = find.update::<()>(&mut content, Message::Query("äpfel".into()));
        for (direction, expected, line) in [
            (SearchDirection::Next, "ÄPFEL", 0),
            (SearchDirection::Next, "äpfel", 1),
            (SearchDirection::Next, "ÄPFEL", 0),
            (SearchDirection::Previous, "äpfel", 1),
        ] {
            let _ = find.update::<()>(&mut content, Message::Step(direction));
            assert_eq!(content.selection().as_deref(), Some(expected));
            assert_eq!(content.cursor().position.line, line);
        }
        let _ = find.update::<()>(&mut content, Message::Close);
        assert!(!find.is_open());
        assert_eq!(content.text(), before);
        find.reset();
        assert!(find.query.is_empty());
    }
}
