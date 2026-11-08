use std::fmt;
use std::io;
use std::sync::Arc;

use iced::{widget::text_editor, window};

use crate::session_journal::entry_edit::{
    Changes, Draft, Edit, Fields, MAX_FIELD_BYTES, MarkerChoice,
};
use crate::workspace::journal_note_translation as note_translation;

use super::{Desktop, Message};

#[cfg(test)]
mod tests;
mod view;

const DEFAULT_SPLIT: u16 = 6200;
const MIN_SPLIT: u16 = 3200;
const MAX_SPLIT: u16 = 7400;
const BASIS_POINTS: f32 = 10_000.0;

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Toggle,
    Select(usize, String),
    Change(u64, Input),
    Translate(u64, note_translation::Event),
    Resize(f32),
    EndResize,
    Save(u64),
    Revert(u64),
    Delete(u64),
    Replace,
}

#[derive(Clone)]
pub(crate) enum Input {
    Title(String),
    Summary(text_editor::Action),
    Note(text_editor::Action),
    Marker(MarkerChoice),
}

impl fmt::Debug for Input {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Title(_) => "Title([redacted])",
            Self::Summary(_) => "Summary([redacted])",
            Self::Note(_) => "Note([redacted])",
            Self::Marker(_) => "Marker([redacted])",
        };
        formatter.write_str(name)
    }
}

pub(super) struct State {
    open: bool,
    split: u16,
    generation: u64,
    selected: Option<Selected>,
    pub(super) screenshot_menu: Option<(super::screenshot_access::Row, iced::Point)>,
    pub(super) screenshot_menu_choice: crate::workspace::journal_image_menu::Choice,
}

#[derive(Clone, Copy)]
pub(super) struct Controls<'a> {
    pub(super) enabled: bool,
    pub(super) delete_allowed: bool,
    pub(super) translation: note_translation::View<'a>,
}

struct Selected {
    index: usize,
    base: Arc<Draft>,
    fields: Fields,
    marker: MarkerChoice,
    summary: text_editor::Content,
    note: text_editor::Content,
    translation: note_translation::State,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            split: DEFAULT_SPLIT,
            generation: 0,
            selected: None,
            screenshot_menu: None,
            screenshot_menu_choice: Default::default(),
        }
    }
}

impl State {
    pub(super) fn refresh_marker_choices(
        &mut self,
        definitions: &[crate::session_journal::markers::Definition],
    ) {
        if let Some(selected) = &mut self.selected {
            selected.translation.cancel();
            let draft = Arc::new(selected.base.as_ref().clone().with_markers(definitions));
            if let Some(marker) = draft
                .markers()
                .iter()
                .find(|choice| choice.id() == selected.marker.id())
            {
                selected.marker = marker.clone();
            }
            selected.base = draft;
            self.generation = self.generation.wrapping_add(1);
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open = true;
            self.generation = self.generation.wrapping_add(1);
        }
    }

    pub(super) fn close(&mut self) {
        self.open = false;
        self.clear_selection();
    }

    pub(super) fn clear_selection(&mut self) {
        self.cancel_translation();
        self.selected = None;
        self.screenshot_menu = None;
        self.generation = self.generation.wrapping_add(1);
    }

    pub(super) fn set_split(&mut self, split: u16) {
        self.split = split.clamp(MIN_SPLIT, MAX_SPLIT);
    }

    pub(super) fn split(&self) -> u16 {
        self.split
    }

    pub(super) fn resize(&mut self, ratio: f32) {
        if ratio.is_finite() {
            self.set_split((ratio.clamp(0.0, 1.0) * BASIS_POINTS).round() as u16);
        }
    }

    pub(super) fn select(&mut self, index: usize, draft: Arc<Draft>) {
        if self.open {
            self.cancel_translation();
            self.selected = Some(Selected::new(index, draft));
            self.screenshot_menu = None;
            self.generation = self.generation.wrapping_add(1);
        }
    }

    pub(super) fn selection(&self) -> Option<(usize, &str)> {
        let selected = self.selected.as_ref()?;
        Some((selected.index, selected.base.id()))
    }

    pub(super) fn draft(&self) -> Option<&Draft> {
        self.selected
            .as_ref()
            .map(|selected| selected.base.as_ref())
    }

    pub(super) fn draft_handle(&self) -> Option<Arc<Draft>> {
        self.selected
            .as_ref()
            .map(|selected| Arc::clone(&selected.base))
    }

    pub(super) fn is_dirty(&self) -> bool {
        self.selected.as_ref().is_some_and(|selected| {
            let baseline = selected.base.fields();
            selected.fields.title != baseline.title
                || selected.fields.text != baseline.text
                || selected.fields.note != baseline.note
                || &selected.marker != selected.base.marker()
        })
    }

    pub(super) fn matches_generation(&self, generation: u64) -> bool {
        self.open && self.selected.is_some() && self.generation == generation
    }

    #[cfg(test)]
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn change(&mut self, generation: u64, input: Input) {
        if !self.matches_generation(generation) {
            return;
        }
        if let Some(selected) = &mut self.selected {
            selected.change(input);
        }
    }

    pub(super) fn can_translate(&self) -> bool {
        self.open
            && self
                .selection()
                .is_some_and(|(index, _)| index != usize::MAX)
    }

    pub(super) fn translate_note(
        &mut self,
        id: window::Id,
        generation: u64,
        event: note_translation::Event,
        setup: note_translation::Setup,
    ) -> Option<note_translation::Update> {
        let selected = self.selected.as_mut()?;
        let mut update =
            selected
                .translation
                .update(event, &selected.fields.note, setup, move |event| {
                    Desktop::SessionJournal(
                        id,
                        Message::EntryEditor(Event::Translate(generation, event)),
                    )
                });
        if let Some(note) = update.take_replacement() {
            selected.note = text_editor::Content::with_text(&note);
            selected
                .note
                .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
            selected.fields.note = note;
        }
        Some(update)
    }

    pub(super) fn cancel_translation(&mut self) {
        if let Some(selected) = &mut self.selected {
            selected.translation.cancel();
        }
    }

    #[cfg(test)]
    pub(super) fn fixture_pending_translation(&mut self, profile: crate::local_ai::Profile) -> u64 {
        let selected = self
            .selected
            .as_mut()
            .expect("expected an entry editor draft");
        selected
            .translation
            .fixture_pending(&selected.fields.note, profile)
    }

    pub(super) fn revert(&mut self) {
        self.cancel_translation();
        if let Some(selected) = &mut self.selected {
            *selected = Selected::new(selected.index, Arc::clone(&selected.base));
            self.generation = self.generation.wrapping_add(1);
        }
    }

    pub(super) fn reanchor(&mut self, document: &crate::session_journal::Journal) {
        self.screenshot_menu = self
            .screenshot_menu
            .take()
            .filter(|(row, _)| row.is_current(document));
        if let Some(selected) = &mut self.selected {
            let id = selected.base.id();
            let mut rows = document
                .timeline
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.id == id);
            selected.index = match (rows.next(), rows.next()) {
                (Some((index, _)), None) => index,
                _ => {
                    selected.translation.cancel();
                    usize::MAX
                }
            };
        }
    }

    pub(super) fn edit(&self) -> io::Result<Edit> {
        let selected = self.selected.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "No journal entry is selected")
        })?;
        selected.base.edit(Changes::Fields {
            fields: selected.fields.clone(),
            marker: selected.marker.clone(),
        })
    }
}

impl Selected {
    fn new(index: usize, base: Arc<Draft>) -> Self {
        let fields = base.fields().clone();
        let marker = base.marker().clone();
        let summary = text_editor::Content::with_text(&fields.text);
        let note = text_editor::Content::with_text(&fields.note);
        Self {
            index,
            base,
            fields,
            marker,
            summary,
            note,
            translation: note_translation::State::default(),
        }
    }

    fn change(&mut self, input: Input) {
        match input {
            Input::Title(value) if value.len() <= MAX_FIELD_BYTES => self.fields.title = value,
            Input::Summary(action) => {
                edit_text(&mut self.summary, &mut self.fields.text, action);
            }
            Input::Note(action) => {
                if edit_text(&mut self.note, &mut self.fields.note, action) {
                    self.translation.edited();
                }
            }
            Input::Marker(marker) if self.base.markers().contains(&marker) => self.marker = marker,
            Input::Title(_) | Input::Marker(_) => {}
        }
    }
}

pub(in crate::workspace) fn edit_text(
    content: &mut text_editor::Content,
    field: &mut String,
    action: text_editor::Action,
) -> bool {
    edit_text_limited(content, field, action, MAX_FIELD_BYTES)
}

pub(in crate::workspace) fn edit_text_limited(
    content: &mut text_editor::Content,
    field: &mut String,
    action: text_editor::Action,
    limit: usize,
) -> bool {
    if !action.is_edit() {
        content.perform(action);
        return false;
    }
    if matches!(&action, text_editor::Action::Edit(text_editor::Edit::Paste(value)) if value.len() > limit)
    {
        return false;
    }
    let cursor = content.cursor();
    content.perform(action);
    let value = content.text();
    if value.len() <= limit {
        if value == *field {
            return false;
        }
        *field = value;
        true
    } else {
        *content = text_editor::Content::with_text(field);
        content.move_to(cursor);
        false
    }
}
