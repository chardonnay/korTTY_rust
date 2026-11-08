use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{self, button, checkbox, column, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length, Task, window};

use super::{Bindings, Chord, catalog, command};
use crate::workspace::{Message as Desktop, chrome::Action, settings_window};

const FILTER_BYTES: usize = 192;
const ROW_HEIGHT: f32 = 38.0;
const ROW_GAP: f32 = 2.0;

#[derive(Clone, Debug)]
pub enum Edit {
    Filter(String),
    OnlyChanged(bool),
    Select(Action),
    Record,
    Remove,
    Reset,
    ResetAll,
}

pub(in crate::workspace) struct Editor {
    list: widget::Id,
    filter: String,
    only_changed: bool,
    selected: Option<Action>,
    recording: bool,
    problem: Option<&'static str>,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            list: widget::Id::unique(),
            filter: String::new(),
            only_changed: false,
            selected: None,
            recording: false,
            problem: None,
        }
    }
}

impl Editor {
    pub(in crate::workspace) fn update(&mut self, bindings: &mut Bindings, edit: Edit) {
        match edit {
            Edit::Filter(mut filter) => {
                filter.truncate(filter.floor_char_boundary(FILTER_BYTES));
                self.filter = filter;
            }
            Edit::OnlyChanged(value) => self.only_changed = value,
            Edit::Select(action) => self.selected = Some(action),
            Edit::Record
                if self
                    .selected
                    .and_then(command)
                    .is_some_and(|entry| !entry.fixed()) =>
            {
                self.recording = true;
                self.problem = None;
                return;
            }
            Edit::Remove => {
                if let Some(action) = self.selected {
                    self.problem = bindings.update(action, None).err();
                    self.recording = false;
                    return;
                }
            }
            Edit::Reset => {
                if let Some(action) = self.selected {
                    bindings.reset(action);
                }
            }
            Edit::ResetAll => bindings.reset_all(),
            Edit::Record => {}
        }
        self.stop_recording();
    }

    pub(in crate::workspace) fn stop_recording(&mut self) {
        self.recording = false;
        self.problem = None;
    }

    pub(in crate::workspace) fn reveal(&mut self, action: Action) -> Task<Desktop> {
        self.selected = Some(action);
        self.only_changed = false;
        self.filter.clear();
        self.stop_recording();
        let Some(index) = catalog().iter().position(|entry| entry.action == action) else {
            return Task::none();
        };
        widget::operation::scroll_to(
            self.list.clone(),
            widget::scrollable::AbsoluteOffset {
                x: 0.0,
                y: index as f32 * (ROW_HEIGHT + ROW_GAP),
            },
        )
    }

    pub(in crate::workspace) fn key(
        &mut self,
        bindings: &mut Bindings,
        key: &Key,
        modifiers: Modifiers,
    ) -> bool {
        let Some(action) = self.selected.filter(|_| self.recording) else {
            return false;
        };
        match key {
            Key::Named(Named::Escape | Named::Tab) => self.stop_recording(),
            Key::Named(
                Named::Alt
                | Named::AltGraph
                | Named::Control
                | Named::Shift
                | Named::Meta
                | Named::Super,
            ) => {}
            _ => {
                self.problem = match Chord::capture(key, modifiers) {
                    Some(chord) => bindings.update(action, Some(chord)).err(),
                    None => Some("This key cannot be used for a shortcut."),
                };
            }
        }
        true
    }

    pub(in crate::workspace) fn view<'a>(
        &'a self,
        bindings: &'a Bindings,
        id: window::Id,
    ) -> Element<'a, Desktop> {
        let edit =
            move |message| Desktop::Settings(settings_window::Message::KeyBinding(id, message));
        let filter = self.filter.trim().to_ascii_lowercase();
        let conflicts = bindings.conflicts();
        let summary = (!conflicts.is_empty()).then(|| {
            let shortcuts = conflicts
                .iter()
                .map(|(chord, _)| chord.to_string())
                .collect::<Vec<_>>();
            text(format!(
                "Resolve the shared shortcuts before saving · {}",
                shortcuts.join(", ")
            ))
            .style(text::danger)
        });
        let unknown = bindings.unknown_count();
        let mut items = column![].spacing(ROW_GAP);
        for entry in catalog() {
            let shortcut = bindings
                .shortcut(entry)
                .map_or_else(|| "—".to_owned(), |chord| chord.to_string());
            if (self.only_changed && !bindings.changed(entry))
                || (!filter.is_empty() && !entry.matches_filter(&filter, &shortcut))
            {
                continue;
            }
            let also_used = conflicts.iter().find_map(|(_, actions)| {
                actions.contains(&entry.action).then(|| {
                    actions
                        .iter()
                        .filter(|&&action| action != entry.action)
                        .filter_map(|&action| command(action).map(|entry| entry.title))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
            });
            let status = if let Some(also_used) = &also_used {
                format!("Also used by {also_used}")
            } else if entry.fixed() {
                "Fixed".to_owned()
            } else if bindings.problem(entry).is_some() {
                "Not used here".to_owned()
            } else if bindings.changed(entry) {
                if bindings.shortcut(entry).is_some() {
                    "Changed"
                } else {
                    "Removed"
                }
                .to_owned()
            } else {
                String::new()
            };
            items = items.push(
                button(
                    row![
                        text(entry.title).width(Length::FillPortion(3)),
                        text(entry.menu).width(Length::FillPortion(2)),
                        text(shortcut).width(Length::FillPortion(2)),
                        text(status)
                            .width(Length::FillPortion(3))
                            .style(if also_used.is_some() {
                                text::danger
                            } else {
                                text::default
                            }),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .width(Length::Fill)
                .height(Length::Fixed(ROW_HEIGHT))
                .style(if self.selected == Some(entry.action) {
                    button::secondary
                } else {
                    button::text
                })
                .on_press(edit(Edit::Select(entry.action))),
            );
        }
        let selected = self.selected.and_then(command);
        let can_edit = selected.is_some_and(|entry| !entry.fixed());
        let detail = selected.map(|entry| {
            let default = entry
                .default_chord()
                .map_or_else(|| "None".to_owned(), |chord| chord.to_string());
            text(format!("{} · Default {}", entry.title, default)).size(13)
        });
        column![
            text("Keyboard Shortcuts").size(18),
            row![
                text_input("Filter by action, menu or shortcut", &self.filter)
                    .on_input(move |value| edit(Edit::Filter(value))),
                checkbox(self.only_changed)
                    .label("Only changed")
                    .on_toggle(move |value| edit(Edit::OnlyChanged(value))),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
            summary,
            (unknown != 0).then(|| text(format!(
                "{unknown} stored shortcuts refer to commands unavailable in this version."
            ))
            .size(13)),
            row![
                text("Action").width(Length::FillPortion(3)),
                text("Menu").width(Length::FillPortion(2)),
                text("Shortcut").width(Length::FillPortion(2)),
                text("Status").width(Length::FillPortion(3)),
            ]
            .spacing(8)
            .padding([0, 8]),
            scrollable(items)
                .id(self.list.clone())
                .height(Length::Fixed(270.0)),
            row![
                button(if self.recording {
                    "Press shortcut · Esc to stop"
                } else {
                    "Record Shortcut…"
                })
                .on_press_maybe(can_edit.then(|| edit(Edit::Record))),
                button("Remove Shortcut")
                    .style(button::secondary)
                    .on_press_maybe(can_edit.then(|| edit(Edit::Remove))),
                button("Reset to Default")
                    .style(button::secondary)
                    .on_press_maybe(
                        selected
                            .is_some_and(|entry| bindings.changed(entry))
                            .then(|| edit(Edit::Reset))
                    ),
                button("Reset All")
                    .style(button::secondary)
                    .on_press(edit(Edit::ResetAll)),
            ]
            .spacing(8)
            .wrap(),
            detail,
            self.problem
                .or_else(|| selected.and_then(|entry| bindings.problem(entry)))
                .map(|problem| text(problem).size(13).style(text::danger)),
        ]
        .spacing(8)
        .into()
    }
}
