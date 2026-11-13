//! Native selectors for the structured options included in an apply request.

use std::fmt;

use iced::advanced::text::Wrapping;
use iced::widget::{Column, button, checkbox, column, pick_list, row, text, text_input, tooltip};
use iced::{Alignment, Element, Length};

use crate::snippet_analysis::model::hardening::{
    HardeningOption, HeaderRef, InputOption, MAX_FILE_SIZE_MIB, Preferences,
};
use crate::snippets::SnippetId;

use super::{WindowMessage, apply};

#[derive(Clone)]
pub enum Edit {
    ExpandClassic,
    ExpandInput,
    Classic(HardeningOption, bool),
    AllClassic(bool),
    InputEnabled(bool),
    Input(InputOption, bool),
    AllInput(bool),
    FileSize(String),
    Header(Option<HeaderRef>),
    SaveClassic,
    SaveInput,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct HeaderChoice {
    pub(crate) id: Option<HeaderRef>,
    pub(crate) name: String,
}

impl fmt::Display for HeaderChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

pub(crate) struct Draft {
    preferences: Preferences,
    pub(crate) header: Option<HeaderRef>,
    classic_open: bool,
    input_open: bool,
    size: String,
}

impl Draft {
    pub(crate) fn new(mut preferences: Preferences) -> Self {
        preferences.max_file_size_mib = preferences.max_file_size_mib.min(MAX_FILE_SIZE_MIB);
        preferences.classic = HardeningOption::ALL
            .into_iter()
            .filter(|option| preferences.classic.contains(option))
            .collect();
        preferences.input = InputOption::ALL
            .into_iter()
            .filter(|option| preferences.input.contains(option))
            .collect();
        Self {
            size: preferences.max_file_size_mib.to_string(),
            preferences,
            header: None,
            classic_open: false,
            input_open: false,
        }
    }

    pub(crate) fn preferences(&self, input_supported: bool) -> Result<Preferences, &'static str> {
        let mut preferences = self.preferences.clone();
        if input_supported && self.size_enabled() {
            preferences.max_file_size_mib = self
                .file_size_mib()
                .ok_or("Enter an input file size limit from 0 to 1024 MiB.")?;
        }
        Ok(preferences)
    }

    pub(crate) fn classic(&self) -> &[HardeningOption] {
        &self.preferences.classic
    }

    pub(crate) fn update(&mut self, edit: Edit, input_supported: bool) {
        match edit {
            Edit::ExpandClassic => self.classic_open = !self.classic_open,
            Edit::ExpandInput if input_supported => self.input_open = !self.input_open,
            Edit::Classic(option, checked) => {
                select(&mut self.preferences.classic, option, checked)
            }
            Edit::AllClassic(checked) => {
                self.preferences.classic = if checked {
                    HardeningOption::ALL.to_vec()
                } else {
                    Vec::new()
                };
            }
            Edit::InputEnabled(enabled) if input_supported => {
                self.preferences.input_enabled = enabled
            }
            Edit::Input(option, checked) if input_supported && self.preferences.input_enabled => {
                select(&mut self.preferences.input, option, checked);
            }
            Edit::AllInput(checked) if input_supported && self.preferences.input_enabled => {
                self.preferences.input = if checked {
                    InputOption::ALL.to_vec()
                } else {
                    Vec::new()
                };
            }
            Edit::FileSize(value) if input_supported && self.size_enabled() => {
                if value.len() <= 4 && value.bytes().all(|byte| byte.is_ascii_digit()) {
                    if let Ok(mib) = value.parse::<u16>()
                        && mib <= MAX_FILE_SIZE_MIB
                    {
                        self.preferences.max_file_size_mib = mib;
                    }
                    self.size = value;
                }
            }
            Edit::Header(id) => self.header = id,
            _ => {}
        }
    }

    pub(crate) fn has_work(&self, input_supported: bool) -> bool {
        !self.preferences.classic.is_empty()
            || self.input_count(input_supported) != 0
            || self.header.is_some()
    }

    fn size_enabled(&self) -> bool {
        self.preferences.input_enabled
            && self.preferences.input.contains(&InputOption::FileSizeLimit)
    }

    fn file_size_mib(&self) -> Option<u16> {
        self.size
            .parse::<u16>()
            .ok()
            .filter(|size| *size <= MAX_FILE_SIZE_MIB)
    }

    fn input_count(&self, supported: bool) -> usize {
        if supported && self.preferences.input_enabled {
            self.preferences.input.len()
        } else {
            0
        }
    }
}

fn select<T: Eq>(options: &mut Vec<T>, option: T, checked: bool) {
    if checked && !options.contains(&option) {
        options.push(option);
    } else if !checked {
        options.retain(|current| current != &option);
    }
}

pub(crate) struct Controls<'a> {
    pub(crate) draft: &'a Draft,
    pub(crate) input_supported: bool,
    pub(crate) headers: Vec<HeaderChoice>,
}

pub(super) fn view<'a>(controls: &Controls<'a>, pre: SnippetId) -> Column<'a, WindowMessage> {
    let draft = controls.draft;
    let input_supported = controls.input_supported;
    let edit = move |change| apply::msg(apply::Action::Hardening(pre, change));
    let classic = button(text(format!(
        "Hardening options ({})",
        draft.classic().len()
    )))
    .on_press(edit(Edit::ExpandClassic));
    let input = button(text(format!(
        "Input hardening ({})",
        draft.input_count(input_supported)
    )))
    .on_press_maybe(input_supported.then(|| edit(Edit::ExpandInput)));
    let mut body = column![classic].spacing(6);
    if draft.classic_open {
        let mut options = column![].spacing(4);
        for group in [&HardeningOption::ALL[..7], &HardeningOption::ALL[7..]] {
            if group.len() == 4 {
                options = options.push(text("Runtime behavior options").size(13));
            }
            let mut choices = row![].spacing(8);
            for &option in group {
                choices = choices.push(option_hint(
                    checkbox(draft.classic().contains(&option))
                        .label(option.label())
                        .on_toggle(move |checked| edit(Edit::Classic(option, checked))),
                    option.instruction(!input_supported),
                ));
            }
            options = options.push(choices.wrap());
        }
        body = body.push(options).push(
            row![
                button("All").on_press(edit(Edit::AllClassic(true))),
                button("Clear").on_press(edit(Edit::AllClassic(false))),
                button("Save hardening defaults").on_press(edit(Edit::SaveClassic)),
            ]
            .spacing(6)
            .wrap(),
        );
    }
    body = body.push(input);
    if !input_supported {
        body = body.push(
            text("Ansible and YAML sources do not support an imperative input guard.").size(13),
        );
    } else if draft.input_open {
        let enabled = draft.preferences.input_enabled;
        let mut choices = row![].spacing(8);
        for option in InputOption::ALL {
            choices = choices.push(option_hint(
                checkbox(draft.preferences.input.contains(&option))
                    .label(option.label())
                    .on_toggle_maybe(
                        enabled.then_some(move |checked| edit(Edit::Input(option, checked))),
                    ),
                option.instruction(draft.preferences.max_file_size_mib),
            ));
        }
        let size = text_input("10", &draft.size)
            .width(Length::Fixed(72.0))
            .on_input_maybe(
                draft
                    .size_enabled()
                    .then_some(move |value| edit(Edit::FileSize(value))),
            );
        body = body
            .push(
                checkbox(enabled)
                    .label("Add a runtime input guard")
                    .on_toggle(move |checked| edit(Edit::InputEnabled(checked))),
            )
            .push(choices.wrap())
            .push(
                row![
                    text("Maximum input file size (MiB)"),
                    size,
                    text("0 means unlimited").size(13)
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .wrap(),
            )
            .push(
                (draft.size_enabled() && draft.file_size_mib().is_none())
                    .then(|| text("Enter a size from 0 to 1024 MiB.").size(13)),
            )
            .push(
                row![
                    button("All").on_press_maybe(enabled.then(|| edit(Edit::AllInput(true)))),
                    button("Clear").on_press_maybe(enabled.then(|| edit(Edit::AllInput(false)))),
                    button("Save input hardening defaults").on_press(edit(Edit::SaveInput)),
                ]
                .spacing(6)
                .wrap(),
            );
    }
    let selected = controls
        .headers
        .iter()
        .find(|header| header.id == draft.header)
        .cloned();
    let selected_missing = draft.header.is_some() && selected.is_none();
    body.push(row![
        text("Script header"),
        pick_list(controls.headers.clone(), selected, move |header: HeaderChoice| edit(Edit::Header(header.id)))
            .placeholder(if selected_missing { "Selected header unavailable" } else { "Choose a script header" }),
    ].spacing(8).align_y(Alignment::Center).wrap())
        .push(text("Choose a saved Script-Header snippet or a managed Bash template. Review the resolved header before sending. Analysis retention rules apply to the saved history.").size(13).wrapping(Wrapping::WordOrGlyph))
}

fn option_hint<'a>(
    control: impl Into<Element<'a, WindowMessage>>,
    description: impl text::IntoFragment<'a>,
) -> Element<'a, WindowMessage> {
    tooltip(
        control,
        text(description)
            .size(13)
            .width(Length::Fixed(340.0))
            .wrapping(Wrapping::WordOrGlyph),
        tooltip::Position::FollowCursor,
    )
    .into()
}
