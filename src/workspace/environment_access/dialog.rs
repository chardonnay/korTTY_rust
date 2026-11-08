use iced::widget::{
    button, checkbox, column, container, row, rule, scrollable, space, text, text_input,
};
use iced::{Alignment, Element, Length};

use super::catalog::{Catalog, Entry, MAX_NAME, Rgb};

#[derive(Clone, Debug)]
pub enum Message {
    Select(String),
    Add,
    Rename,
    Name(String),
    ApplyName,
    Delete,
    ConfirmDelete(String),
    CancelEdit,
    UseColor(bool),
    Color(String),
    Save,
    Cancel,
}

pub(super) struct Dialog {
    pub(super) draft: Catalog,
    pub(super) error: Option<String>,
    pub(super) read_only: bool,
    selection: Option<String>,
    color_text: String,
    prompt: Option<Prompt>,
}

enum Prompt {
    Add(String),
    Rename(String),
    Delete(String),
}

impl Dialog {
    pub(super) fn new(draft: Catalog, error: Option<String>) -> Self {
        Self {
            read_only: error.is_some(),
            draft,
            error,
            selection: None,
            color_text: String::new(),
            prompt: None,
        }
    }

    pub(super) fn selected(&self) -> Option<Entry<'_>> {
        self.draft.get(self.selection.as_deref()?)
    }

    pub(super) fn ready(&self) -> bool {
        !self.read_only && self.prompt.is_none() && !self.invalid_color()
    }

    fn invalid_color(&self) -> bool {
        self.selected().and_then(|entry| entry.color).is_some()
            && Rgb::parse(&self.color_text).is_none()
    }

    pub(super) fn update(&mut self, message: Message, usage: Option<usize>) {
        if let Message::Select(id) = message {
            if self.prompt.is_none() && self.draft.get(&id).is_some() {
                self.select(id);
            }
            return;
        }
        if self.read_only {
            return;
        }
        self.error = None;
        match message {
            Message::Add if self.prompt.is_none() => self.prompt = Some(Prompt::Add(String::new())),
            Message::Rename if self.prompt.is_none() => {
                if let Some(entry) = self.selected().filter(|entry| !entry.built_in) {
                    self.prompt = Some(Prompt::Rename(entry.label.to_owned()));
                }
            }
            Message::Name(mut value) => {
                value.truncate(value.floor_char_boundary(MAX_NAME));
                if let Some(Prompt::Add(name) | Prompt::Rename(name)) = &mut self.prompt {
                    *name = value;
                }
            }
            Message::ApplyName => self.apply_name(),
            Message::Delete if self.prompt.is_none() => {
                if let Some(entry) = self.selected().filter(|entry| !entry.built_in)
                    && usage == Some(0)
                {
                    self.prompt = Some(Prompt::Delete(entry.id.to_owned()));
                }
            }
            Message::ConfirmDelete(id) if matches!(&self.prompt, Some(Prompt::Delete(active)) if active == &id) =>
            {
                self.prompt = None;
                if usage == Some(0) {
                    if self.draft.remove(&id) {
                        self.selection = None;
                        self.color_text.clear();
                    }
                } else {
                    self.error = Some("A credential uses this environment.".into());
                }
            }
            Message::CancelEdit => self.prompt = None,
            Message::UseColor(enabled) if self.prompt.is_none() => {
                if let Some(id) = &self.selection {
                    let color =
                        enabled.then(|| Rgb::parse(&self.color_text).unwrap_or(Rgb::PRESETS[0].0));
                    self.draft.set_color(id, color);
                    self.color_text = color.map_or_else(String::new, |color| color.to_string());
                }
            }
            Message::Color(mut value) if self.prompt.is_none() => {
                if let Some(entry) = self.selected().filter(|entry| entry.color.is_some()) {
                    let id = entry.id.to_owned();
                    value.truncate(value.floor_char_boundary(16));
                    if let Some(color) = Rgb::parse(&value) {
                        self.draft.set_color(&id, Some(color));
                    }
                    self.color_text = value;
                }
            }
            _ => {}
        }
    }

    fn apply_name(&mut self) {
        let result = match self.prompt.as_ref() {
            Some(Prompt::Add(name)) => self.draft.add(name),
            Some(Prompt::Rename(name)) => {
                let Some(id) = &self.selection else {
                    return;
                };
                self.draft.rename(id, name).map(|()| id.clone())
            }
            _ => return,
        };
        match result {
            Ok(id) => {
                self.prompt = None;
                self.select(id);
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn select(&mut self, id: String) {
        self.color_text = self
            .draft
            .get(&id)
            .and_then(|entry| entry.color)
            .map_or_else(String::new, |color| color.to_string());
        self.selection = Some(id);
    }

    pub(super) fn view(&self, usage: Option<usize>) -> Element<'_, Message> {
        let editable = !self.read_only && self.prompt.is_none();
        let selected = self.selected();
        let mut entries = column![].spacing(3);
        for entry in self.draft.entries() {
            entries = entries.push(
                button(
                    row![
                        text(entry.label),
                        entry.built_in.then(|| text("(built-in)").size(12)),
                        space::horizontal(),
                        entry.color.map(|color| text("●").color(color.color())),
                    ]
                    .align_y(Alignment::Center)
                    .spacing(5),
                )
                .style(if self.selection.as_deref() == Some(entry.id) {
                    button::secondary
                } else {
                    button::text
                })
                .width(Length::Fill)
                .on_press_maybe(
                    self.prompt
                        .is_none()
                        .then(|| Message::Select(entry.id.to_owned())),
                ),
            );
        }
        let actions = row![
            button("Add").on_press_maybe(editable.then_some(Message::Add)),
            button("Rename").on_press_maybe(
                (editable && selected.is_some_and(|entry| !entry.built_in))
                    .then_some(Message::Rename)
            ),
            button("Delete").style(button::danger).on_press_maybe(
                (editable && usage == Some(0) && selected.is_some_and(|entry| !entry.built_in))
                    .then_some(Message::Delete)
            ),
        ]
        .spacing(7);
        let options = selected.map(|entry| {
            let colored = entry.color.is_some();
            let editable_color = editable && colored;
            let presets = Rgb::PRESETS.into_iter().fold(row![].spacing(4), |row, (color, label)| {
                row.push(button(row![text("●").color(color.color()), text(label).size(12)].spacing(4))
                    .style(button::secondary)
                    .on_press_maybe(editable_color.then(|| Message::Color(color.to_string()))))
            });
            column![
                text(entry.label).size(18),
                checkbox(colored).label("Show a color on terminal tabs")
                    .on_toggle_maybe(editable.then_some(Message::UseColor)),
                text_input("#RRGGBB", &self.color_text).on_input_maybe(editable_color.then_some(Message::Color)),
                self.invalid_color().then(|| text("Enter #RGB or #RRGGBB.").size(13).style(text::danger)),
                presets.wrap(),
                text("Applies to SSH tabs opened with a stored password credential in this environment.").size(13),
                usage.filter(|&count| count > 0).map(|count| text(format!("Used by {count} credential(s)")).size(13)),
            ].spacing(9)
        });
        let prompt = match &self.prompt {
            Some(Prompt::Add(value) | Prompt::Rename(value)) => Some(
                column![
                    text(if matches!(self.prompt, Some(Prompt::Add(_))) {
                        "Add environment"
                    } else {
                        "Rename environment"
                    })
                    .size(16),
                    text_input("Environment name", value)
                        .on_input(Message::Name)
                        .on_submit(Message::ApplyName),
                    row![
                        button("Apply").on_press(Message::ApplyName),
                        button("Cancel").on_press(Message::CancelEdit)
                    ]
                    .spacing(7),
                ]
                .spacing(6),
            ),
            Some(Prompt::Delete(id)) => Some(
                column![
                    text(format!(
                        "Delete {}?",
                        selected.map_or("this environment", |entry| entry.label)
                    ))
                    .size(16),
                    row![
                        button("Delete").style(button::danger).on_press_maybe(
                            (usage == Some(0)).then(|| Message::ConfirmDelete(id.clone()))
                        ),
                        button("Cancel").on_press(Message::CancelEdit),
                    ]
                    .spacing(7),
                ]
                .spacing(6),
            ),
            None => None,
        };
        column![
            text("Manage Credential Environments").size(21),
            text("Group credentials by environment and distinguish their terminal tabs by color.")
                .size(13),
            rule::horizontal(1),
            row![
                column![
                    text("Environments").size(15),
                    scrollable(entries).height(Length::Fill),
                    actions
                ]
                .spacing(7)
                .width(Length::FillPortion(2)),
                scrollable(container(column![options, prompt].spacing(12)).padding(8))
                    .width(Length::FillPortion(3)),
            ]
            .spacing(12)
            .height(Length::Fill),
            self.error
                .as_deref()
                .map(|error| text(error).size(13).style(text::danger)),
            row![
                space::horizontal(),
                button("Cancel")
                    .style(button::secondary)
                    .on_press(Message::Cancel),
                button("OK").on_press_maybe(self.ready().then_some(Message::Save)),
            ]
            .spacing(7),
        ]
        .padding(14)
        .spacing(10)
        .height(Length::Fill)
        .into()
    }
}
