use std::collections::HashSet;

use iced::widget::{button, checkbox, column, row, scrollable, text, text_editor, text_input};
use iced::{Element, Length};

use crate::snippets::exchange::{Action, Outcome};
use crate::snippets::{
    Library, MAX_VARIABLE_NAME_BYTES, MAX_VARIABLE_VALUE_BYTES, Variable, valid_variable_value,
};

#[derive(Clone)]
pub enum Message {
    Back,
    Search(String),
    New,
    Select(String),
    Name(String),
    Value(text_editor::Action),
    Save,
    Delete,
    Import,
    Export,
    Path(String),
    ExportSelected(String, bool),
    StartExchange,
    ExchangeFinished(Result<Outcome, String>),
    Merge(bool),
    Confirm,
    Cancel,
}

pub(super) enum Update {
    None,
    Back,
    Changed,
    Exchange(Action),
}

enum Intent {
    Back,
    New,
    Select(String),
    Delete,
    Import,
    Export,
}

enum Exchange {
    File {
        path: String,
        export: Option<HashSet<String>>,
    },
    Running,
    Review {
        entries: Vec<Variable>,
        conflicts: usize,
    },
}

pub(super) struct Page {
    editing: Option<String>,
    name: String,
    value: text_editor::Content,
    value_changed: bool,
    pub(super) dirty: bool,
    search: String,
    pending: Option<Intent>,
    exchange: Option<Exchange>,
    status: Option<String>,
}

impl Page {
    pub(super) fn new() -> Self {
        Self {
            editing: None,
            name: String::new(),
            value: text_editor::Content::new(),
            value_changed: false,
            dirty: false,
            search: String::new(),
            pending: None,
            exchange: None,
            status: None,
        }
    }

    pub(super) fn busy(&self) -> bool {
        matches!(self.exchange, Some(Exchange::Running))
    }

    pub(super) fn update(&mut self, message: Message, library: &mut Library) -> Update {
        if self.exchange.is_some() {
            return self.update_exchange(message, library);
        }
        match message {
            Message::Confirm => {
                if let Some(intent) = self.pending.take() {
                    return self.apply(intent, library);
                }
            }
            Message::Cancel => {
                if self.pending.take().is_none() {
                    return self.request(Intent::Back, library);
                }
            }
            _ if self.pending.is_some() => {}
            Message::Back => return self.request(Intent::Back, library),
            Message::Import => return self.request(Intent::Import, library),
            Message::Export => return self.request(Intent::Export, library),
            Message::Search(query) if query.len() <= MAX_VARIABLE_NAME_BYTES => self.search = query,
            Message::New => return self.request(Intent::New, library),
            Message::Select(name) if self.editing.as_deref() != Some(&name) => {
                return self.request(Intent::Select(name), library);
            }
            Message::Name(name) if name.len() <= MAX_VARIABLE_NAME_BYTES => {
                self.dirty |= self.name != name;
                self.name = name;
            }
            Message::Value(action) => self.edit_value(action),
            Message::Save => return self.save(library),
            Message::Delete if !self.dirty && self.editing.is_some() => {
                self.pending = Some(Intent::Delete);
            }
            _ => {}
        }
        Update::None
    }

    fn request(&mut self, intent: Intent, library: &mut Library) -> Update {
        if self.dirty {
            self.pending = Some(intent);
            Update::None
        } else {
            self.apply(intent, library)
        }
    }

    fn apply(&mut self, intent: Intent, library: &mut Library) -> Update {
        self.status = None;
        match intent {
            Intent::Back => return Update::Back,
            Intent::New => self.load(None),
            Intent::Select(name) => self.load(
                library
                    .variables()
                    .iter()
                    .find(|entry| entry.matches(&name)),
            ),
            Intent::Delete => {
                if let Some(name) = self.editing.as_deref() {
                    match library.remove_variable(name) {
                        Ok(()) => {
                            self.load(None);
                            self.status = Some("Variable deleted".into());
                            return Update::Changed;
                        }
                        Err(error) => {
                            self.status = Some(format!("Could not delete the variable ({error})"))
                        }
                    }
                }
            }
            Intent::Import | Intent::Export => {
                self.load(
                    self.editing.as_deref().and_then(|name| {
                        library.variables().iter().find(|entry| entry.matches(name))
                    }),
                );
                let export = matches!(intent, Intent::Export).then(|| {
                    library
                        .variables()
                        .iter()
                        .filter(|entry| {
                            self.editing
                                .as_deref()
                                .is_none_or(|name| entry.matches(name))
                        })
                        .map(|entry| entry.name.clone())
                        .collect()
                });
                self.exchange = Some(Exchange::File {
                    path: String::new(),
                    export,
                });
            }
        }
        Update::None
    }

    fn load(&mut self, entry: Option<&Variable>) {
        self.editing = entry.map(|entry| entry.name.clone());
        self.name = self.editing.clone().unwrap_or_default();
        self.value = entry.map_or_else(text_editor::Content::new, |entry| {
            text_editor::Content::with_text(&entry.value)
        });
        self.value_changed = false;
        self.dirty = false;
    }

    fn edit_value(&mut self, action: text_editor::Action) {
        if !action.is_edit() {
            self.value.perform(action);
            return;
        }
        if matches!(&action, text_editor::Action::Edit(text_editor::Edit::Paste(value)) if value.len() > MAX_VARIABLE_VALUE_BYTES)
        {
            return;
        }
        let previous = self.value.text();
        let cursor = self.value.cursor();
        self.value.perform(action);
        let value = self.value.text();
        if !valid_variable_value(&value) {
            self.value = text_editor::Content::with_text(&previous);
            self.value.move_to(cursor);
        } else if value != previous {
            self.dirty = true;
            self.value_changed = true;
        }
    }

    fn save(&mut self, library: &mut Library) -> Update {
        let value = self
            .editing
            .as_deref()
            .filter(|_| !self.value_changed)
            .and_then(|name| library.variables().iter().find(|entry| entry.matches(name)))
            .map_or_else(|| self.value.text(), |entry| entry.value.clone());
        let entry = Variable {
            name: self.name.clone(),
            value,
        };
        match library.save_variable(self.editing.as_deref(), entry) {
            Ok(()) => {
                self.load(
                    library
                        .variables()
                        .iter()
                        .find(|entry| entry.matches(self.name.trim())),
                );
                self.status = Some("Variable saved".into());
                Update::Changed
            }
            Err(error) => {
                self.status = Some(format!("Could not save the variable ({error})"));
                Update::None
            }
        }
    }

    fn update_exchange(&mut self, message: Message, library: &mut Library) -> Update {
        match (self.exchange.as_mut(), message) {
            (Some(Exchange::File { path, .. }), Message::Path(value)) if value.len() <= 8192 => {
                *path = value;
            }
            (
                Some(Exchange::File {
                    export: Some(selected),
                    ..
                }),
                Message::ExportSelected(name, true),
            ) => {
                if library.variables().iter().any(|entry| entry.name == name) {
                    selected.insert(name);
                }
            }
            (
                Some(Exchange::File {
                    export: Some(selected),
                    ..
                }),
                Message::ExportSelected(name, false),
            ) => {
                selected.remove(&name);
            }
            (Some(Exchange::File { path, export }), Message::StartExchange) if !path.is_empty() => {
                let action = match export {
                    Some(selected) if selected.is_empty() => return Update::None,
                    Some(selected) => Action::Export(
                        path.as_str().into(),
                        library
                            .variables()
                            .iter()
                            .filter(|entry| selected.contains(&entry.name))
                            .cloned()
                            .collect(),
                    ),
                    None => Action::Import(path.as_str().into()),
                };
                self.exchange = Some(Exchange::Running);
                return Update::Exchange(action);
            }
            (Some(Exchange::Running), Message::ExchangeFinished(result)) => {
                self.exchange = None;
                match result {
                    Ok(Outcome::Imported(entries)) if entries.is_empty() => {
                        self.status = Some("The file has no named variables".into());
                    }
                    Ok(Outcome::Imported(entries)) => {
                        let mut names: HashSet<_> = library
                            .variables()
                            .iter()
                            .map(|entry| entry.name.to_lowercase())
                            .collect();
                        let conflicts = entries
                            .iter()
                            .filter(|entry| !names.insert(entry.name.to_lowercase()))
                            .count();
                        if conflicts > 0 {
                            self.exchange = Some(Exchange::Review { entries, conflicts });
                        } else {
                            return self.merge(entries, false, library);
                        }
                    }
                    Ok(Outcome::Exported(count)) => {
                        self.status = Some(format!("Exported {count} variables"));
                    }
                    Err(error) => self.status = Some(format!("Variable exchange failed ({error})")),
                }
            }
            (Some(Exchange::Review { .. }), Message::Merge(replace_existing)) => {
                if let Some(Exchange::Review { entries, .. }) = self.exchange.take() {
                    return self.merge(entries, replace_existing, library);
                }
            }
            (Some(Exchange::File { .. } | Exchange::Review { .. }), Message::Cancel) => {
                self.exchange = None;
            }
            _ => {}
        }
        Update::None
    }

    fn merge(
        &mut self,
        entries: Vec<Variable>,
        replace_existing: bool,
        library: &mut Library,
    ) -> Update {
        match library.import_variables(entries, replace_existing) {
            Ok(count) => {
                self.load(
                    self.editing.as_deref().and_then(|name| {
                        library.variables().iter().find(|entry| entry.matches(name))
                    }),
                );
                self.status = Some(format!("Imported {count} variables"));
                if count > 0 {
                    return Update::Changed;
                }
            }
            Err(error) => self.status = Some(format!("Could not import variables ({error})")),
        }
        Update::None
    }

    pub(super) fn view(&self, library: &Library) -> Element<'_, Message> {
        if let Some(exchange) = &self.exchange {
            return exchange_view(exchange, library);
        }
        if let Some(intent) = self.pending.as_ref() {
            let (question, action) = match intent {
                Intent::Delete => (
                    "Delete this variable? Linked jobs may be blocked.",
                    "Delete",
                ),
                _ => ("Discard unsaved variable edits?", "Discard"),
            };
            return column![
                text(question).size(19),
                row![
                    button("Cancel").on_press(Message::Cancel),
                    button(action).on_press(Message::Confirm),
                ]
                .spacing(8)
            ]
            .padding(16)
            .spacing(12)
            .into();
        }
        let query = self.search.trim().to_lowercase();
        let mut entries = column![].spacing(4);
        for entry in library
            .variables()
            .iter()
            .filter(|entry| entry.name.to_lowercase().contains(&query))
        {
            let value_status = if entry.default_value().is_some() {
                "Stored default"
            } else {
                "Ask on use"
            };
            entries = entries.push(
                button(text(format!("{} · {value_status}", entry.name)))
                    .width(Length::Fill)
                    .style(if self.editing.as_deref() == Some(&entry.name) {
                        button::primary
                    } else {
                        button::secondary
                    })
                    .on_press(Message::Select(entry.name.clone())),
            );
        }
        let list = column![
            text_input("Search variable names", &self.search).on_input(Message::Search),
            button("New").on_press(Message::New),
            scrollable(entries).height(Length::Fill),
        ]
        .width(260)
        .spacing(8);
        let form = column![
            text_input("Variable name", &self.name).on_input(Message::Name),
            text("Use ${name} in a snippet. Leave the stored value blank to ask each time.").size(13),
            text_editor(&self.value).on_action(Message::Value).height(Length::Fill).placeholder("Stored default"),
            text("Defaults are stored in plain text. Keep passwords, tokens and other secrets out of this library.").size(13),
            row![button("Save").on_press(Message::Save),
                button("Delete").on_press_maybe((self.editing.is_some() && !self.dirty).then_some(Message::Delete))].spacing(8),
            text(self.status.as_deref().unwrap_or(if self.dirty { "Unsaved edits" } else { "" })),
        ].width(Length::Fill).spacing(8);
        column![
            row![
                button("Back to snippets").on_press(Message::Back),
                text("Snippet variables").size(20),
                button("Import variables").on_press(Message::Import),
                button("Export variables")
                    .on_press_maybe((!library.variables().is_empty()).then_some(Message::Export)),
            ]
            .spacing(12),
            row![list, form].spacing(12).height(Length::Fill),
        ]
        .padding(12)
        .spacing(12)
        .height(Length::Fill)
        .into()
    }
}

fn exchange_view<'a>(exchange: &'a Exchange, library: &Library) -> Element<'a, Message> {
    let contents = match exchange {
        Exchange::File { path, export } => {
            let title = if export.is_some() {
                "Export variables"
            } else {
                "Import variables"
            };
            let mut form = column![
                text(title).size(20),
                text("Enter an absolute path ending in .json, .xml, .yaml, or .yml."),
                text_input("File path", path).on_input(Message::Path),
            ]
            .spacing(8);
            if let Some(selected) = export {
                let mut choices = column![].spacing(4);
                for entry in library.variables() {
                    let name = entry.name.clone();
                    choices = choices.push(
                        checkbox(selected.contains(&name))
                            .label(name.clone())
                            .on_toggle(move |checked| {
                                Message::ExportSelected(name.clone(), checked)
                            }),
                    );
                }
                form = form
                    .push(text(
                        "The exported file contains stored defaults in plain text.",
                    ))
                    .push(scrollable(choices).height(Length::Fill));
            }
            form.push(
                row![
                    button("Cancel").on_press(Message::Cancel),
                    button(if export.is_some() { "Export" } else { "Import" }).on_press_maybe(
                        (!path.is_empty() && export.as_ref().is_none_or(|names| !names.is_empty()))
                            .then_some(Message::StartExchange),
                    )
                ]
                .spacing(8),
            )
        }
        Exchange::Running => column![text("Reading or writing the variable file…")],
        Exchange::Review { conflicts, .. } => column![
            text("Review variable conflicts").size(20),
            text(format!(
                "{conflicts} imported entries repeat a variable name."
            )),
            row![
                button("Cancel").on_press(Message::Cancel),
                button("Skip existing").on_press(Message::Merge(false)),
                button("Replace existing").on_press(Message::Merge(true)),
            ]
            .spacing(8),
        ]
        .spacing(8),
    };
    contents.padding(16).width(Length::Fill).into()
}
