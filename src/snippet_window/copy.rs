use iced::widget::{button, checkbox, column, scrollable, text, text_input};
use iced::{Element, Length};
use zeroize::Zeroizing;

use crate::snippets::{Library, SnippetId, one_liner, resolve, valid_variable_value};

use super::{SnippetWindow, Update};

#[derive(Clone)]
pub enum Message {
    Begin,
    Enter(u64, usize, Zeroizing<String>),
    Remember(u64, usize, bool),
    Confirm(u64),
    Cancel,
    ClipboardLoaded(u64, Option<Zeroizing<String>>),
}

pub(super) struct Pending {
    token: u64,
    snippet: SnippetId,
    requirements: resolve::CopyRequirements,
    fields: Vec<FieldOptions>,
    awaiting_clipboard: bool,
}

#[derive(Clone, Copy, Default)]
struct FieldOptions {
    remember: bool,
    invalid: bool,
}

impl SnippetWindow {
    pub(super) fn copy_snippet_command(&mut self, mode: one_liner::Mode) -> Update {
        match one_liner::convert(&self.draft.content.text(), &self.draft.language, mode) {
            Ok(command) => {
                self.status = Some("Snippet command copied.".into());
                Update::CopyResolved(command)
            }
            Err(error) => {
                self.status = Some(error.message().into());
                if mode == one_liner::Mode::Compact && error.offers_ai() && self.can_open_assist() {
                    Update::ReviewCompactCommand
                } else {
                    Update::None
                }
            }
        }
    }

    pub(super) fn update_copy(&mut self, message: Message) -> Update {
        match message {
            Message::Begin if self.copy.is_none() && !self.draft.dirty => {
                let Some(snippet) = self.draft.id.and_then(|id| self.library.get(id)) else {
                    return Update::None;
                };
                self.next_copy_token = self.next_copy_token.wrapping_add(1);
                let token = self.next_copy_token;
                let requirements =
                    resolve::copy_requirements(snippet.content(), self.library.variables());
                let fields = vec![FieldOptions::default(); requirements.missing.len()];
                let needs_prompt = !fields.is_empty();
                self.copy = Some(Pending {
                    token,
                    snippet: snippet.id(),
                    requirements,
                    fields,
                    awaiting_clipboard: false,
                });
                if !needs_prompt {
                    return self.continue_copy(token);
                }
            }
            Message::Enter(token, index, value) => {
                if let Some(pending) = self.active_copy_prompt(token)
                    && let Some(field) = pending.requirements.missing.get_mut(index)
                {
                    let invalid = !valid_variable_value(&value);
                    pending.fields[index].invalid = invalid;
                    field.value = if invalid {
                        Zeroizing::new(String::new())
                    } else {
                        value
                    };
                }
            }
            Message::Remember(token, index, remember) => {
                if let Some(options) = self
                    .active_copy_prompt(token)
                    .and_then(|pending| pending.fields.get_mut(index))
                {
                    options.remember = remember;
                }
            }
            Message::Confirm(token) => return self.continue_copy(token),
            Message::Cancel => self.copy = None,
            Message::ClipboardLoaded(token, clipboard)
                if self.copy.as_ref().is_some_and(|pending| {
                    pending.token == token && pending.awaiting_clipboard
                }) =>
            {
                return self.finish_copy(Some(clipboard.as_deref().map_or("", String::as_str)));
            }
            _ => {}
        }
        Update::None
    }

    fn active_copy_prompt(&mut self, token: u64) -> Option<&mut Pending> {
        self.copy
            .as_mut()
            .filter(|pending| pending.token == token && !pending.awaiting_clipboard)
    }

    fn continue_copy(&mut self, token: u64) -> Update {
        let Some(pending) = self
            .active_copy_prompt(token)
            .filter(|pending| pending.valid())
        else {
            return Update::None;
        };
        if pending.requirements.clipboard {
            pending.awaiting_clipboard = true;
            Update::ReadClipboard(token)
        } else {
            self.finish_copy(None)
        }
    }

    fn finish_copy(&mut self, clipboard: Option<&str>) -> Update {
        let Some(pending) = self.copy.take() else {
            return Update::None;
        };
        match pending.resolve(&self.library, clipboard) {
            Ok(value) => {
                self.status = Some(
                    match self
                        .library
                        .remember_missing_defaults(pending.selected_defaults())
                    {
                        Ok(true) => "Resolved snippet copied and selected defaults saved".into(),
                        Ok(false) => "Resolved snippet copied".into(),
                        Err(error) => {
                            format!("Resolved snippet copied. Could not save defaults ({error})")
                        }
                    },
                );
                Update::CopyResolved(value)
            }
            Err(error) => {
                self.status = Some(error.into());
                Update::None
            }
        }
    }
}

impl Pending {
    fn valid(&self) -> bool {
        self.fields.iter().all(|field| !field.invalid)
    }

    fn selected_defaults(&self) -> impl Iterator<Item = (&str, &str)> {
        self.requirements
            .missing
            .iter()
            .zip(&self.fields)
            .filter(|(_, options)| options.remember)
            .map(|(field, _)| (field.name.as_str(), field.value.as_str()))
    }

    fn resolve(&self, library: &Library, clipboard: Option<&str>) -> Result<String, &'static str> {
        let snippet = library
            .get(self.snippet)
            .ok_or("The snippet no longer exists")?;
        resolve::interactive(
            snippet.content(),
            library.variables(),
            &self.requirements.missing,
            clipboard,
        )
    }

    pub(super) fn view(&self) -> Element<'_, Message> {
        let mut fields = column![].spacing(8);
        let token = self.token;
        for (index, (field, options)) in self
            .requirements
            .missing
            .iter()
            .zip(&self.fields)
            .enumerate()
        {
            let mut entry = column![
                text(&field.name),
                text_input("Value for this copy", &field.value)
                    .secure(true)
                    .on_input(move |value| Message::Enter(token, index, Zeroizing::new(value))),
                checkbox(options.remember)
                    .label("Remember as default")
                    .on_toggle(move |remember| Message::Remember(token, index, remember)),
            ]
            .spacing(4);
            if options.invalid {
                entry = entry.push(text("Enter a text value of at most 4 KiB.").size(13));
            }
            fields = fields.push(entry);
        }
        let content = if self.awaiting_clipboard {
            column![text("Reading the clipboard for this copy…")]
        } else {
            column![
                text("Enter values for this copy").size(19),
                text("Remembered defaults are saved in plain text."),
                scrollable(fields).height(Length::Fill),
                button("Copy resolved text")
                    .on_press_maybe(self.valid().then_some(Message::Confirm(token))),
            ]
            .spacing(10)
        };
        content
            .push(button("Cancel").on_press(Message::Cancel))
            .spacing(12)
            .padding(16)
            .height(Length::Fill)
            .into()
    }
}
