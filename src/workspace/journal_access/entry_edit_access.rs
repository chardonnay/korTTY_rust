//! Routes the entry editor through the verified journal source and the capture writer.

use std::sync::Arc;
use std::thread;
use std::{fmt, io};

use iced::widget::{button, column, container, mouse_area, pick_list, row, text};
use iced::{Alignment, Element, Task, window};

use crate::session_journal::entry_edit::{
    self, Changes, Draft, Edit, MAX_FIELD_BYTES, MarkerChoice,
};
use crate::session_journal::markers::Definition;
use crate::session_journal::{Capture, Status};
use crate::terminal_logging::{Redactor, Secrets};
use crate::workspace::chrome::popup;
use crate::workspace::journal_note_translation as note_translation;

use super::{Desktop, Journal, Message, Mode, Viewer, Workspace, entry_editor, read_task};

pub(super) mod deletion;

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Context(usize, String),
    DismissContext,
    ChooseMarker,
    Marker(MarkerChoice),
    ApplyMarker,
    Loaded(u64, Result<Arc<Draft>, String>),
    Saved(u64, Result<Arc<Saved>, String>),
    Discard(bool),
    CloseAnswer(u64, bool),
}

#[derive(Default)]
pub(super) struct Flow {
    revision: u64,
    loading: Option<Load>,
    saving: Option<(u64, Intent)>,
    deleting: bool,
    replacing: bool,
    applying_markers: bool,
    screenshot_saving: bool,
    status: Option<String>,
    discard: Option<Discard>,
    close_token: Option<u64>,
    context: Option<EntryRow>,
    marker: Option<MarkerForm>,
}

#[derive(Clone)]
struct EntryRow {
    index: usize,
    id: String,
}

enum Discard {
    CloseEditor,
    Select(EntryRow),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Intent {
    Editor,
    Marker,
}

struct Load {
    revision: u64,
    row: EntryRow,
    intent: Intent,
}

struct MarkerForm {
    draft: Arc<Draft>,
    selected: MarkerChoice,
}

pub(crate) struct Saved {
    document: Option<Arc<Journal>>,
    draft: Option<Arc<Draft>>,
}

impl fmt::Debug for Saved {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("SavedJournalEntry")
            .finish_non_exhaustive()
    }
}

impl Flow {
    pub(super) fn input_enabled(&self) -> bool {
        self.loading.is_none()
            && !self.saving()
            && self.discard.is_none()
            && self.close_token.is_none()
    }

    pub(super) fn saving(&self) -> bool {
        self.saving.is_some()
            || self.deleting
            || self.replacing
            || self.applying_markers
            || self.screenshot_saving
    }

    pub(super) fn set_replacing(&mut self, replacing: bool) {
        self.replacing = replacing;
    }

    pub(super) fn set_applying_markers(&mut self, applying: bool) {
        self.applying_markers = applying;
    }

    pub(super) fn applying_markers(&self) -> bool {
        self.applying_markers
    }

    pub(super) fn set_screenshot_saving(&mut self, saving: bool) {
        self.screenshot_saving = saving;
    }

    pub(super) fn screenshot_saving(&self) -> bool {
        self.screenshot_saving
    }

    pub(super) fn reset_after_replace(&mut self, status: &str) {
        self.loading = None;
        self.close_context();
        self.status = Some(status.into());
    }

    pub(super) fn context_enabled(&self) -> bool {
        self.discard.is_none() && self.close_token.is_none()
    }

    fn next_revision(&mut self) -> u64 {
        self.revision = self.revision.wrapping_add(1);
        self.revision
    }

    fn close_context(&mut self) {
        self.context = None;
        self.marker = None;
        if self
            .loading
            .as_ref()
            .is_some_and(|load| load.intent == Intent::Marker)
        {
            self.loading = None;
        }
    }

    pub(super) fn status_view(&self) -> Option<Element<'_, Message>> {
        if self.deleting {
            return Some(text("Deleting entry…").size(13).into());
        }
        if self.applying_markers {
            return Some(text("Applying marker rules…").size(13).into());
        }
        if self.saving.is_some() {
            return Some(text("Saving entry…").size(13).into());
        }
        let confirm = if let Some(token) = self.close_token {
            Some((
                Event::CloseAnswer(token, false),
                Event::CloseAnswer(token, true),
            ))
        } else if self.discard.is_some() {
            Some((Event::Discard(false), Event::Discard(true)))
        } else {
            None
        };
        if let Some((keep, discard)) = confirm {
            return Some(
                row![
                    text("Discard unsaved entry changes?").size(13),
                    button("Keep editing").on_press(Message::EntryEdit(keep)),
                    button("Discard").on_press(Message::EntryEdit(discard)),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .wrap()
                .into(),
            );
        }
        let status = if self.loading.is_some() {
            Some("Loading entry…")
        } else {
            self.status.as_deref()
        };
        status.map(|status| text(status).size(13).into())
    }

    pub(super) fn timeline_row<'a>(
        &'a self,
        trigger: Element<'a, Message>,
        index: usize,
        id: &str,
        enabled: bool,
    ) -> Element<'a, Message> {
        let row = if enabled && !id.is_empty() {
            mouse_area(trigger)
                .on_right_press(Message::EntryEdit(Event::Context(index, id.to_owned())))
                .into()
        } else {
            mouse_area(trigger).into()
        };
        let popup_contents = self
            .context
            .as_ref()
            .filter(|context| enabled && context.id == id)
            .map(|_| self.context_view());
        popup::popover(
            row,
            popup_contents,
            Message::EntryEdit(Event::DismissContext),
        )
    }

    fn context_view(&self) -> Element<'_, Message> {
        let body: Element<'_, Message> = if let Some(form) = &self.marker {
            column![
                text("Marker"),
                pick_list(form.draft.markers(), Some(&form.selected), |marker| {
                    Message::EntryEdit(Event::Marker(marker))
                }),
                (!form.draft.markers().contains(&form.selected))
                    .then(|| text("The selected marker is no longer available.").size(12)),
                row![
                    button("Cancel").on_press_maybe(
                        self.saving
                            .is_none()
                            .then_some(Message::EntryEdit(Event::DismissContext))
                    ),
                    button("OK").on_press_maybe(
                        (self.input_enabled() && form.draft.markers().contains(&form.selected))
                            .then_some(Message::EntryEdit(Event::ApplyMarker))
                    ),
                ]
                .spacing(6),
            ]
            .spacing(8)
            .into()
        } else {
            button("Set marker…")
                .on_press_maybe(
                    self.input_enabled()
                        .then_some(Message::EntryEdit(Event::ChooseMarker)),
                )
                .into()
        };
        container(body)
            .padding(8)
            .style(container::bordered_box)
            .into()
    }
}

impl Workspace {
    fn entry_viewer(&self, id: window::Id) -> Option<&Viewer> {
        match &self.journals.get(&id)?.mode {
            Mode::Viewer(viewer) => Some(viewer),
            Mode::Manager(_) => None,
        }
    }

    fn entry_viewer_mut(&mut self, id: window::Id) -> Option<&mut Viewer> {
        match &mut self.journals.get_mut(&id)?.mode {
            Mode::Viewer(viewer) => Some(viewer),
            Mode::Manager(_) => None,
        }
    }

    pub(super) fn entry_interactive(&self, id: window::Id) -> bool {
        self.viewer_navigation_interactive(id)
            && !self.hosted_close_locks(id)
            && self
                .entry_viewer(id)
                .is_some_and(|viewer| viewer.can_edit_entries())
    }

    pub(in crate::workspace) fn journal_entry_needs_close(&self, id: window::Id) -> bool {
        self.entry_viewer(id).is_some_and(|viewer| {
            viewer.entry_flow.saving() || (self.journal_allowed() && viewer.entry_editor.is_dirty())
        })
    }

    pub(in crate::workspace) fn native_journal_close_targets(
        &self,
        owner: Option<window::Id>,
    ) -> Vec<window::Id> {
        self.journals
            .iter()
            .filter_map(|(&id, surface)| {
                let native_owner = surface.native_owner?;
                (owner.is_none_or(|owner| owner == native_owner)
                    && self.journal_entry_needs_close(id))
                .then_some(id)
            })
            .collect()
    }

    pub(in crate::workspace) fn prompt_journal_entry_close(&mut self, id: window::Id, token: u64) {
        if let Some(viewer) = self.entry_viewer_mut(id) {
            viewer.entry_editor.cancel_translation();
            viewer.entry_flow.close_context();
            viewer.entry_flow.close_token = Some(token);
        }
    }

    pub(in crate::workspace) fn cancel_journal_entry_close(&mut self, id: window::Id) {
        if let Some(viewer) = self.entry_viewer_mut(id) {
            viewer.entry_flow.close_token = None;
        }
    }

    pub(super) fn update_journal_entry_editor(
        &mut self,
        id: window::Id,
        event: entry_editor::Event,
    ) -> Task<Desktop> {
        use entry_editor::Event;
        if let Event::Translate(generation, event) = event {
            return self.translate_journal_entry_note(id, generation, event);
        }
        if !self.entry_interactive(id) {
            return Task::none();
        }
        if let Event::Delete(generation) = event {
            return self.offer_journal_entry_delete(id, generation);
        }
        if let Event::Replace = event {
            return self.offer_journal_replacement(id, false);
        }
        let Some(viewer) = self.entry_viewer_mut(id) else {
            return Task::none();
        };
        if !viewer.entry_flow.input_enabled() {
            return Task::none();
        }
        match event {
            Event::Toggle if viewer.entry_editor.is_open() => {
                if viewer.entry_editor.is_dirty() {
                    viewer.entry_editor.cancel_translation();
                    viewer.entry_flow.discard = Some(Discard::CloseEditor);
                } else {
                    viewer.close_entry_editor();
                }
            }
            Event::Toggle => {
                viewer.entry_editor.toggle();
                viewer.entry_flow.close_context();
                if let Some(row) = viewer
                    .selected_entry
                    .and_then(|index| viewer.entry_row(index))
                {
                    return viewer.load_entry_draft(id, row, Intent::Editor);
                }
            }
            Event::Select(index, id_value) if viewer.entry_editor.is_open() => {
                let Some(row) = viewer.entry_row(index).filter(|row| row.id == id_value) else {
                    return Task::none();
                };
                if viewer
                    .entry_editor
                    .selection()
                    .is_some_and(|(_, selected)| selected == row.id)
                {
                    return Task::none();
                }
                if viewer.entry_editor.is_dirty() {
                    viewer.entry_editor.cancel_translation();
                    viewer.entry_flow.discard = Some(Discard::Select(row));
                } else {
                    return viewer.load_entry_draft(id, row, Intent::Editor);
                }
            }
            Event::Change(generation, input) => viewer.entry_editor.change(generation, input),
            Event::Save(generation) if viewer.entry_editor.matches_generation(generation) => {
                viewer.entry_editor.cancel_translation();
                let edit = viewer.entry_editor.edit();
                return self.start_journal_entry_save(id, edit, Intent::Editor);
            }
            Event::Revert(generation) if viewer.entry_editor.matches_generation(generation) => {
                viewer.entry_editor.revert();
                if let Some(row) = viewer
                    .entry_editor
                    .selection()
                    .and_then(|(_, key)| viewer.entry_row_by_id(key))
                {
                    return viewer.load_entry_draft(id, row, Intent::Editor);
                }
            }
            Event::Resize(ratio) => viewer.entry_editor.resize(ratio),
            Event::EndResize => {
                let split = viewer.entry_editor.split();
                if split != self.window_settings.journal_edit_split {
                    self.window_settings.journal_edit_split = split;
                    for surface in self.journals.values_mut() {
                        if let Mode::Viewer(viewer) = &mut surface.mode {
                            viewer.entry_editor.set_split(split);
                        }
                    }
                    self.save_window_settings(self.visible_journal_surface_owner(id));
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn translate_journal_entry_note(
        &mut self,
        id: window::Id,
        generation: u64,
        event: note_translation::Event,
    ) -> Task<Desktop> {
        let Some(viewer) = self.entry_viewer(id) else {
            return Task::none();
        };
        if !viewer.entry_editor.matches_generation(generation) {
            return Task::none();
        }
        let Some(owner) = self.visible_journal_surface_owner(id).filter(|_| {
            self.entry_interactive(id)
                && viewer.entry_flow.input_enabled()
                && viewer.entry_editor.can_translate()
        }) else {
            if let Some(viewer) = self.entry_viewer_mut(id) {
                viewer.entry_editor.cancel_translation();
            }
            return Task::none();
        };
        let capture = self.journal_entry_capture(&viewer.source);
        let setup = self.journal_note_translation_setup(&event, capture.as_ref(), MAX_FIELD_BYTES);
        let update = self.entry_viewer_mut(id).and_then(|viewer| {
            viewer
                .entry_editor
                .translate_note(id, generation, event, setup)
        });
        update.map_or_else(Task::none, |update| {
            self.finish_journal_note_translation(update, Some(owner))
        })
    }

    pub(super) fn update_journal_entry_edit(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Desktop> {
        let allowed = self.journal_allowed();
        let interactive = self.entry_interactive(id);
        let visible = self.visible_journal_surface_owner(id).is_some();
        let definitions = matches!(&event, Event::Loaded(..) | Event::Saved(..))
            .then(|| self.journal_marker_definitions().to_vec());
        let Some(viewer) = self.entry_viewer_mut(id) else {
            return Task::none();
        };
        match event {
            Event::Loaded(revision, result) => {
                let result = result.map(|draft| {
                    Arc::new(
                        Arc::unwrap_or_clone(draft)
                            .with_markers(definitions.as_deref().unwrap_or_default()),
                    )
                });
                return viewer.entry_draft_loaded(revision, result, allowed);
            }
            Event::Saved(revision, result) => {
                if viewer
                    .entry_flow
                    .saving
                    .as_ref()
                    .is_none_or(|(active, _)| *active != revision)
                {
                    return Task::none();
                }
                let updated = result.is_ok() && allowed;
                let source = updated.then(|| viewer.source.clone());
                let view = viewer.entry_saved(id, result, allowed);
                if updated {
                    viewer.refresh_marker_choices(definitions.as_deref().unwrap_or_default());
                }
                let dock = source.map_or_else(Task::none, |source| {
                    self.refresh_journal_entry_docks(&source)
                });
                let close = self.resume_hosted_tool_close(id);
                return Task::batch([view, dock, close]);
            }
            Event::CloseAnswer(token, approve)
                if visible
                    && viewer.entry_flow.close_token == Some(token)
                    && !viewer.entry_flow.saving() =>
            {
                viewer.entry_flow.close_token = None;
                return self.hosted_tool_close_answer(id, token, approve);
            }
            Event::DismissContext if viewer.entry_flow.saving.is_none() => {
                viewer.entry_flow.close_context()
            }
            _ if !interactive => {}
            Event::Discard(approve) => {
                let intent = viewer.entry_flow.discard.take();
                if approve {
                    match intent {
                        Some(Discard::CloseEditor) => viewer.close_entry_editor(),
                        Some(Discard::Select(row)) => {
                            return viewer.load_entry_draft(id, row, Intent::Editor);
                        }
                        None => {}
                    }
                }
            }
            _ if !viewer.entry_flow.input_enabled() => {}
            Event::Context(index, id_value) if !viewer.entry_editor.is_dirty() => {
                if let Some(row) = viewer.entry_row(index).filter(|row| row.id == id_value) {
                    viewer.entry_flow.close_context();
                    viewer.entry_flow.context = Some(row);
                }
            }
            Event::ChooseMarker => {
                if let Some(row) = viewer
                    .entry_flow
                    .context
                    .as_ref()
                    .and_then(|row| viewer.entry_row_by_id(&row.id))
                {
                    return viewer.load_entry_draft(id, row, Intent::Marker);
                }
            }
            Event::Marker(choice) => {
                if let Some(form) = &mut viewer.entry_flow.marker
                    && form.draft.markers().contains(&choice)
                {
                    form.selected = choice;
                }
            }
            Event::ApplyMarker => {
                if let Some(form) = &viewer.entry_flow.marker {
                    let edit = form.draft.edit(Changes::Marker(form.selected.clone()));
                    return self.start_journal_entry_save(id, edit, Intent::Marker);
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn start_journal_entry_save(
        &mut self,
        id: window::Id,
        edit: io::Result<Edit>,
        intent: Intent,
    ) -> Task<Desktop> {
        let prepared = edit.and_then(|edit| {
            let capture = self.journal_entry_capture(edit.source());
            let redactor = if matches!(edit.changes(), Changes::Fields { .. }) {
                self.journal_entry_redactor(capture.as_ref())?
            } else {
                None
            };
            Ok((edit, capture, redactor))
        });
        let Some(viewer) = self.entry_viewer_mut(id) else {
            return Task::none();
        };
        let (edit, capture, redactor) = match prepared {
            Ok(prepared) if prepared.0.source().same_journal(&viewer.source) => prepared,
            Ok(_) => return Task::none(),
            Err(error) => {
                viewer.entry_flow.status = Some(error.to_string());
                return Task::none();
            }
        };
        let revision = viewer.entry_flow.next_revision();
        viewer.entry_flow.saving = Some((revision, intent));
        viewer.entry_flow.status = None;
        let (send, receive) = async_channel::bounded(1);
        let (finished, done) = async_channel::bounded(1);
        let worker = thread::Builder::new()
            .name("kortty-journal-entry-edit".into())
            .spawn(move || {
                let result = save_entry(edit, capture, redactor)
                    .map(Arc::new)
                    .map_err(|error| error.to_string());
                let _ = send.try_send(result);
                drop(finished);
            });
        if let Err(error) = worker {
            return viewer.entry_saved(id, Err(error.to_string()), true);
        }
        self.track_session_shutdown(done);
        Task::future(async move {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("Journal entry writer stopped.".into()));
            Desktop::SessionJournal(id, Message::EntryEdit(Event::Saved(revision, result)))
        })
    }

    fn journal_entry_capture(&self, source: &super::ViewerSource) -> Option<Arc<Capture>> {
        self.journal_metadata_captures()
            .into_iter()
            .find(|capture| super::ViewerSource::capture(capture).same_journal(source))
    }

    pub(in crate::workspace) fn journal_entry_redactor(
        &self,
        capture: Option<&Arc<Capture>>,
    ) -> io::Result<Option<Redactor>> {
        let secrets = match capture {
            Some(capture) if capture.status() == Status::Recording => {
                self.live_journal_entry_secrets(capture).ok_or_else(|| {
                    io::Error::other(
                        "The recording is changing its terminal attachment. Try saving again.",
                    )
                })?
            }
            _ => Secrets::default().seal()?,
        };
        let policy = &self.clipboard.policy.terminal_logging;
        Ok((!secrets.is_empty() || policy.has_redactions())
            .then(|| Redactor::for_journal(secrets, policy.clone())))
    }
}

impl Viewer {
    pub(super) fn refresh_marker_choices(&mut self, definitions: &[Definition]) {
        self.entry_editor.refresh_marker_choices(definitions);
        if let Some(form) = &mut self.entry_flow.marker {
            let draft = Arc::new(form.draft.as_ref().clone().with_markers(definitions));
            if let Some(marker) = draft
                .markers()
                .iter()
                .find(|choice| choice.id() == form.selected.id())
            {
                form.selected = marker.clone();
            }
            form.draft = draft;
        }
    }

    pub(super) fn reload_marker_draft(&mut self, id: window::Id) -> Task<Desktop> {
        if self.entry_editor.is_dirty() || self.entry_flow.saving() {
            return Task::none();
        }
        self.entry_flow.close_context();
        let Some(row) = self
            .entry_editor
            .selection()
            .and_then(|(_, id)| self.entry_row_by_id(id))
        else {
            return Task::none();
        };
        self.load_entry_draft(id, row, Intent::Editor)
    }

    pub(super) fn can_edit_entries(&self) -> bool {
        self.document.is_some()
            && !self.loading
            && self.screenshot.is_none()
            && self.export_range.is_none()
            && !self.title_menu
    }

    fn close_entry_editor(&mut self) {
        self.entry_editor.close();
        self.entry_flow.loading = None;
        self.entry_flow.status = None;
    }

    fn entry_row(&self, index: usize) -> Option<EntryRow> {
        let id = self.document.as_ref()?.timeline.get(index)?.id.clone();
        (!id.is_empty()).then_some(EntryRow { index, id })
    }

    fn entry_row_by_id(&self, id: &str) -> Option<EntryRow> {
        let mut rows = self
            .document
            .as_ref()?
            .timeline
            .iter()
            .enumerate()
            .filter(|(_, row)| row.id == id);
        let (index, _) = rows.next()?;
        if rows.next().is_some() {
            return None;
        }
        self.entry_row(index)
    }

    fn load_entry_draft(&mut self, id: window::Id, row: EntryRow, intent: Intent) -> Task<Desktop> {
        if intent == Intent::Editor {
            self.entry_editor.cancel_translation();
            self.entry_flow.close_context();
        }
        let revision = self.entry_flow.next_revision();
        let entry = row.id.clone();
        self.entry_flow.loading = Some(Load {
            revision,
            row,
            intent,
        });
        self.entry_flow.status = None;
        let source = self.source.clone();
        read_task(
            id,
            move || Draft::read(&source, &entry),
            move |result| Message::EntryEdit(Event::Loaded(revision, result)),
        )
    }

    fn entry_draft_loaded(
        &mut self,
        revision: u64,
        result: Result<Arc<Draft>, String>,
        allowed: bool,
    ) -> Task<Desktop> {
        if self
            .entry_flow
            .loading
            .as_ref()
            .is_none_or(|load| load.revision != revision)
        {
            return Task::none();
        }
        let Some(load) = self.entry_flow.loading.take().filter(|_| allowed) else {
            return Task::none();
        };
        match result {
            Ok(draft) if draft.source().same_journal(&self.source) && draft.id() == load.row.id => {
                if let Some(row) = self.entry_row_by_id(draft.id()) {
                    match load.intent {
                        Intent::Editor if self.entry_editor.is_open() => {
                            self.entry_editor.select(row.index, draft)
                        }
                        Intent::Marker
                            if self
                                .entry_flow
                                .context
                                .as_ref()
                                .is_some_and(|context| context.id == row.id) =>
                        {
                            let selected = draft.marker().clone();
                            self.entry_flow.marker = Some(MarkerForm { draft, selected });
                        }
                        _ => {}
                    }
                }
            }
            Ok(_) => self.entry_flow.status = Some("The journal entry changed.".into()),
            Err(error) => self.entry_flow.status = Some(error),
        }
        Task::none()
    }

    fn entry_saved(
        &mut self,
        id: window::Id,
        result: Result<Arc<Saved>, String>,
        allowed: bool,
    ) -> Task<Desktop> {
        let Some((_, intent)) = self.entry_flow.saving.take() else {
            return Task::none();
        };
        let saved = match result {
            Ok(saved) => saved,
            Err(error) => {
                self.entry_flow.status = Some(error);
                return Task::none();
            }
        };
        self.entry_flow.close_context();
        self.refresh.invalidate();
        let updated = if allowed {
            saved.document.as_ref().map_or_else(Task::none, |document| {
                self.replace_timeline(id, Arc::clone(document))
            })
        } else {
            Task::none()
        };
        if intent == Intent::Editor {
            if allowed
                && let Some(draft) = &saved.draft
                && let Some(row) = self.entry_row_by_id(draft.id())
            {
                self.entry_editor.select(row.index, Arc::clone(draft));
            } else {
                self.entry_editor.clear_selection();
            }
        } else if let Some(draft) = &saved.draft
            && self
                .entry_editor
                .draft()
                .is_some_and(|active| active.id() == draft.id())
            && let Some(row) = self.entry_row_by_id(draft.id())
        {
            self.entry_editor.select(row.index, Arc::clone(draft));
        }
        self.entry_flow.status = Some(
            if saved.document.is_some() {
                "Entry saved."
            } else {
                "Entry saved. Refresh to see the updated journal."
            }
            .into(),
        );
        let focus = if allowed && self.can_navigate_timeline() {
            saved
                .draft
                .as_ref()
                .and_then(|draft| self.entry_row_by_id(draft.id()))
                .map_or_else(Task::none, |row| {
                    self.reveal_timeline(Some(row.index), true)
                })
        } else {
            Task::none()
        };
        Task::batch([updated, focus])
    }
}

fn save_entry(
    mut edit: Edit,
    capture: Option<Arc<Capture>>,
    redactor: Option<Redactor>,
) -> io::Result<Saved> {
    if let Some(redactor) = redactor {
        edit.redact_fields(&redactor)?;
    }
    let source = edit.source().clone();
    let id = edit.id().to_owned();
    let offline = match capture {
        Some(capture) => match capture.edit_entry(edit.clone()) {
            Ok(reply) => {
                reply
                    .recv_blocking()
                    .map_err(io::Error::other)?
                    .map_err(io::Error::other)?;
                false
            }
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                let _ = capture.done().recv_blocking();
                true
            }
            Err(error) => return Err(error),
        },
        None => true,
    };
    if offline {
        entry_edit::save(&edit)?;
    }
    Ok(Saved {
        document: source.read(None).ok().flatten().map(Arc::new),
        draft: Draft::read(&source, &id).ok().map(Arc::new),
    })
}

#[cfg(test)]
mod tests;
