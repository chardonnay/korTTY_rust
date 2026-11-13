//! Native editing of the screenshot bound to a journal entry.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use iced::widget::{
    self, button, column, container, image, row, space, text, text_editor, text_input,
};
use iced::{Element, Length, Size, Task, window};

use crate::session_journal::annotation::{self, Annotation, Kind, MAX_TEXT_BYTES};
use crate::session_journal::entry_edit::MAX_FIELD_BYTES;
use crate::session_journal::screenshot_edit::{self, Draft, Edit, Outcome};
use crate::session_journal::{Capture, Status};
use crate::terminal_logging::Redactor;
use crate::workspace::dialog_placement::Kind as DialogKind;
use crate::workspace::journal_note_translation as note_translation;
use crate::workspace::journal_thumbnail::image_handle;

use super::screenshot_access::Origin;
use super::screenshot_export::{Displayed, Target};
use super::{Desktop, Journal, Message, Mode, ViewerSource, Workspace, read_task};

mod canvas;
mod form;
#[cfg(test)]
mod tests;

pub(super) const TITLE: &str = "Annotate screenshot";
const DEFAULT_SIZE: Size = Size::new(1000.0, 780.0);
const MIN_SIZE: Size = Size::new(620.0, 470.0);

#[derive(Default)]
pub(in crate::workspace) struct Editors {
    dialogs: HashMap<window::Id, Dialog>,
}

impl Editors {
    pub(super) fn title(&self, id: window::Id) -> Option<&'static str> {
        if self.dialogs.contains_key(&id) {
            Some(TITLE)
        } else {
            self.prompt_parent(id).map(|_| "Text")
        }
    }

    pub(super) fn for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.dialogs.iter().find_map(|(&id, dialog)| {
            let prompt = dialog.prompt.as_ref().map(|prompt| prompt.id);
            if dialog.owner == owner || dialog.origin.window() == owner {
                Some(prompt.unwrap_or(id))
            } else if id == owner {
                prompt
            } else {
                None
            }
        })
    }

    fn prompt_parent(&self, id: window::Id) -> Option<window::Id> {
        self.dialogs.iter().find_map(|(&parent, dialog)| {
            dialog
                .prompt
                .as_ref()
                .is_some_and(|prompt| prompt.id == id)
                .then_some(parent)
        })
    }
}

#[derive(Clone)]
pub(crate) enum Event {
    Focus,
    Loaded(Result<Arc<Loaded>, String>),
    Select(Kind),
    ChooseColour,
    CloseColour,
    Colour(String),
    Width(u8),
    Mark(u64, Annotation),
    Label(u64, Annotation),
    LabelText(String),
    ConfirmLabel,
    Undo,
    Clear,
    Note(text_editor::Action),
    Translate(note_translation::Event),
    Rendered(u64, Result<Arc<Preview>, String>),
    Save,
    Saved(Result<Arc<Saved>, String>),
    Close,
}

impl fmt::Debug for Event {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("JournalScreenshotEditorEvent")
            .finish_non_exhaustive()
    }
}

pub(crate) struct Loaded {
    draft: Arc<Draft>,
    preview: Preview,
}

pub(crate) struct Preview {
    image: image::Handle,
    dimensions: [u32; 2],
}

pub(crate) struct Saved {
    outcome: Outcome,
    document: Option<Arc<Journal>>,
}

struct Dialog {
    origin: Origin,
    owner: window::Id,
    target: ViewerSource,
    phase: Phase,
    prompt: Option<Prompt>,
}

enum Phase {
    Loading,
    Editing(Box<form::Form>),
    Saving(Box<form::Form>),
    Report(String),
}

struct Prompt {
    id: window::Id,
    input: widget::Id,
    value: String,
    mark: Annotation,
}

impl Loaded {
    fn read(target: Target) -> io::Result<Self> {
        let draft = Arc::new(match target {
            Target::Row {
                source,
                id,
                reference,
            } => Draft::read(&source, &id, &reference)?,
            Target::Displayed(asset) => Draft::from_reviewed(asset)?,
        });
        let preview = Preview::from_png(draft.visible())?;
        Ok(Self { draft, preview })
    }
}

impl Preview {
    fn from_png(bytes: &[u8]) -> io::Result<Self> {
        let pixmap = tiny_skia::Pixmap::decode_png(bytes).map_err(io::Error::other)?;
        Ok(Self::from_pixmap(pixmap))
    }

    fn from_pixmap(pixmap: tiny_skia::Pixmap) -> Self {
        let dimensions = [pixmap.width(), pixmap.height()];
        Self {
            image: image_handle(pixmap),
            dimensions,
        }
    }

    fn render(draft: &Draft, marks: &[Annotation]) -> io::Result<Self> {
        if marks == draft.annotations() {
            return Self::from_png(draft.visible());
        }
        let original = draft
            .original()
            .ok_or_else(|| io::Error::other("The original screenshot is unavailable."))?;
        let base = tiny_skia::Pixmap::decode_png(original).map_err(io::Error::other)?;
        if marks.is_empty() {
            return Ok(Self::from_pixmap(base));
        }
        Ok(Self::from_pixmap(annotation::render(&base, marks)?))
    }
}

impl Workspace {
    pub(super) fn open_journal_screenshot_editor(
        &mut self,
        source: window::Id,
        target: Target,
    ) -> Task<Desktop> {
        self.open_screenshot_editor(Origin::Viewer(source), target)
    }

    pub(in crate::workspace) fn open_dock_journal_screenshot_editor(
        &mut self,
        owner: window::Id,
        tab: u64,
        run: u64,
        displayed: &Displayed,
    ) -> Task<Desktop> {
        self.open_screenshot_editor(
            Origin::Dock { owner, tab, run },
            Target::displayed(displayed),
        )
    }

    fn open_screenshot_editor(&mut self, origin: Origin, target: Target) -> Task<Desktop> {
        let Some(owner) = origin.owner(self) else {
            return Task::none();
        };
        if !origin.is_current(self, owner, target.source())
            || !self.palette_can_open(owner)
            || self.palette.is_open(owner)
            || self.journal_edit_for_owner(origin.window()).is_some()
        {
            return Task::none();
        }
        let fallback = self
            .window_settings
            .journal_screenshot_geometry
            .map(|geometry| geometry.placement(MIN_SIZE));
        let (id, opened) = self.open_placed_dialog_with_fallback(
            DialogKind::JournalScreenshotEditor,
            window::Settings {
                size: DEFAULT_SIZE,
                min_size: Some(MIN_SIZE),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            fallback,
        );
        self.journal_screenshot_editors.dialogs.insert(
            id,
            Dialog {
                origin,
                owner,
                target: target.source().clone(),
                phase: Phase::Loading,
                prompt: None,
            },
        );
        let load = read_task(
            id,
            move || Loaded::read(target),
            |result| Message::Annotate(Event::Loaded(result)),
        );
        Task::batch([opened.discard(), load])
    }

    fn journal_screenshot_busy(&self, target: &ViewerSource) -> bool {
        self.journals.values().any(|surface| {
            matches!(&surface.mode, Mode::Viewer(viewer) if viewer.source.same_journal(target)
                && (viewer.entry_flow.saving() || viewer.entry_editor.is_dirty()))
        })
    }

    pub(super) fn journal_screenshot_edit_in_progress(&self, target: &ViewerSource) -> bool {
        self.journal_screenshot_editors
            .dialogs
            .values()
            .any(|dialog| {
                matches!(dialog.phase, Phase::Saving(_)) && dialog.target.same_journal(target)
            })
    }

    pub(super) fn journal_screenshot_editor_view(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, Desktop>> {
        let editors = &self.journal_screenshot_editors;
        let contents = if let Some(parent) = editors.prompt_parent(id) {
            editors.dialogs.get(&parent)?.prompt.as_ref()?.view()
        } else {
            let dialog = editors.dialogs.get(&id)?;
            let allowed = dialog.origin.is_current(self, dialog.owner, &dialog.target);
            let busy = self.journal_screenshot_busy(&dialog.target);
            let translation = self.journal_note_translation_view(
                allowed && dialog.prompt.is_none() && matches!(dialog.phase, Phase::Editing(_)),
            );
            dialog.view(allowed, busy, translation)
        };
        Some(contents.map(move |event| Desktop::SessionJournal(id, Message::Annotate(event))))
    }

    pub(super) fn update_journal_screenshot_editor(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Desktop> {
        if matches!(&event, Event::Close) {
            return self
                .close_journal_screenshot_editor(id)
                .unwrap_or_else(Task::none);
        }
        if let Some(parent) = self.journal_screenshot_editors.prompt_parent(id) {
            return self.update_screenshot_label(parent, id, event);
        }
        let Some(dialog) = self.journal_screenshot_editors.dialogs.get(&id) else {
            return Task::none();
        };
        if matches!(&event, Event::Focus) {
            return window::gain_focus(dialog.prompt.as_ref().map_or(id, |prompt| prompt.id));
        }
        if let Event::Saved(result) = event {
            return self.journal_screenshot_saved(id, result);
        }
        let allowed = dialog.origin.is_current(self, dialog.owner, &dialog.target)
            && self
                .journal_edit_for_owner(dialog.origin.window())
                .is_some_and(|modal| {
                    modal == id
                        || dialog
                            .prompt
                            .as_ref()
                            .is_some_and(|prompt| prompt.id == modal)
                });
        if !allowed {
            if matches!(&event, Event::Translate(_))
                && let Some(dialog) = self.journal_screenshot_editors.dialogs.get_mut(&id)
            {
                dialog.cancel_note_translation();
            }
            return Task::none();
        }
        if let Event::Translate(event) = event {
            return self.translate_screenshot_note(id, event);
        }
        if matches!(&event, Event::Save) {
            return self.save_journal_screenshot_editor(id);
        }
        if let Event::Label(revision, mark) = event {
            return self.open_screenshot_label(id, revision, mark);
        }
        let dialog = self
            .journal_screenshot_editors
            .dialogs
            .get_mut(&id)
            .expect("checked editor");
        match event {
            Event::Loaded(result) if matches!(dialog.phase, Phase::Loading) => {
                dialog.phase = match result {
                    Ok(loaded) if loaded.draft.source().same_journal(&dialog.target) => {
                        Phase::Editing(Box::new(form::Form::new(loaded)))
                    }
                    Ok(_) => Phase::Report("The selected screenshot changed.".into()),
                    Err(error) => Phase::Report(error),
                };
                Task::none()
            }
            Event::Rendered(revision, result) => match &mut dialog.phase {
                Phase::Editing(form) => form.rendered(id, revision, result),
                _ => Task::none(),
            },
            _ if dialog.prompt.is_some() => Task::none(),
            event => match &mut dialog.phase {
                Phase::Editing(form) => form.update(id, event),
                _ => Task::none(),
            },
        }
    }

    fn translate_screenshot_note(
        &mut self,
        id: window::Id,
        event: note_translation::Event,
    ) -> Task<Desktop> {
        let Some(dialog) = self.journal_screenshot_editors.dialogs.get(&id) else {
            return Task::none();
        };
        let Phase::Editing(form) = &dialog.phase else {
            return Task::none();
        };
        if dialog.prompt.is_some() {
            return Task::none();
        }
        let owner = dialog.owner;
        let capture = self.journal_screenshot_edit_capture(&dialog.target);
        let draft = Arc::clone(form.draft());
        let source = self.journal_note_entry_source(capture.as_ref(), move || {
            draft.require_translation_source()
        });
        let setup = self
            .journal_note_translation_setup(&event, capture.as_ref(), MAX_FIELD_BYTES)
            .with_outbound_source(source);
        let Some(Dialog {
            phase: Phase::Editing(form),
            ..
        }) = self.journal_screenshot_editors.dialogs.get_mut(&id)
        else {
            return Task::none();
        };
        let update = form.translate_note(id, event, setup);
        self.finish_journal_note_translation(update, Some(owner))
    }

    fn open_screenshot_label(
        &mut self,
        id: window::Id,
        revision: u64,
        mark: Annotation,
    ) -> Task<Desktop> {
        let Some(dialog) = self.journal_screenshot_editors.dialogs.get_mut(&id) else {
            return Task::none();
        };
        let Phase::Editing(form) = &mut dialog.phase else {
            return Task::none();
        };
        if dialog.prompt.is_some() || !form.can_mark(revision) || mark.kind != Kind::Text {
            return Task::none();
        }
        form.cancel_translation();
        let (prompt, opened) = self.open_placed_dialog(
            DialogKind::JournalScreenshotText,
            window::Settings {
                size: Size::new(430.0, 155.0),
                min_size: Some(Size::new(330.0, 135.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        let input = widget::Id::unique();
        if let Some(dialog) = self.journal_screenshot_editors.dialogs.get_mut(&id) {
            dialog.prompt = Some(Prompt {
                id: prompt,
                input: input.clone(),
                value: String::new(),
                mark,
            });
        }
        opened.discard().chain(widget::operation::focus(input))
    }

    fn update_screenshot_label(
        &mut self,
        parent: window::Id,
        id: window::Id,
        event: Event,
    ) -> Task<Desktop> {
        let Some(dialog) = self.journal_screenshot_editors.dialogs.get(&parent) else {
            return Task::none();
        };
        if !dialog.origin.is_current(self, dialog.owner, &dialog.target) {
            return self
                .close_journal_screenshot_editor(id)
                .unwrap_or_else(Task::none);
        }
        let dialog = self
            .journal_screenshot_editors
            .dialogs
            .get_mut(&parent)
            .expect("checked label parent");
        let Some(prompt) = &mut dialog.prompt else {
            return Task::none();
        };
        match event {
            Event::Focus => window::gain_focus(id),
            Event::LabelText(text) if text.len() <= MAX_TEXT_BYTES => {
                prompt.value = text;
                Task::none()
            }
            Event::ConfirmLabel if !prompt.value.trim().is_empty() => {
                let Some(mut prompt) = dialog.prompt.take() else {
                    return Task::none();
                };
                prompt.mark.text = prompt.value;
                let update = match &mut dialog.phase {
                    Phase::Editing(form) => form.add(parent, prompt.mark),
                    _ => Task::none(),
                };
                Task::batch([
                    self.close_placed_dialog(id)
                        .chain(window::gain_focus(parent)),
                    update,
                ])
            }
            _ => Task::none(),
        }
    }

    fn journal_screenshot_edit_capture(&self, target: &ViewerSource) -> Option<Arc<Capture>> {
        self.journal_metadata_captures()
            .into_iter()
            .find(|capture| {
                matches!(capture.status(), Status::Recording | Status::Finishing)
                    && ViewerSource::capture(capture).same_journal(target)
            })
    }

    fn save_journal_screenshot_editor(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(dialog) = self.journal_screenshot_editors.dialogs.get(&id) else {
            return Task::none();
        };
        let Phase::Editing(form) = &dialog.phase else {
            return Task::none();
        };
        if dialog.prompt.is_some()
            || self.journal_screenshot_busy(&dialog.target)
            || !form.can_save()
        {
            return Task::none();
        }
        let target = dialog.target.clone();
        let capture = self.journal_screenshot_edit_capture(&target);
        let redactor = match self.journal_entry_redactor(capture.as_ref()) {
            Ok(redactor) => redactor,
            Err(error) => {
                if let Some(Dialog {
                    phase: Phase::Editing(form),
                    ..
                }) = self.journal_screenshot_editors.dialogs.get_mut(&id)
                {
                    form.set_status(error.to_string());
                }
                return Task::none();
            }
        };
        let draft = Arc::clone(form.draft());
        let note = form.note_for_save();
        let marks = form.annotations().to_vec();
        let dialog = self
            .journal_screenshot_editors
            .dialogs
            .get_mut(&id)
            .expect("checked editor");
        if let Phase::Editing(mut form) = std::mem::replace(&mut dialog.phase, Phase::Loading) {
            form.cancel_translation();
            dialog.phase = Phase::Saving(form);
        }
        for surface in self.journals.values_mut() {
            if let Mode::Viewer(viewer) = &mut surface.mode
                && viewer.source.same_journal(&target)
            {
                viewer.entry_flow.set_screenshot_saving(true);
                viewer.refresh.invalidate();
            }
        }
        let (send, receive) = async_channel::bounded(1);
        let (finished, done) = async_channel::bounded(1);
        let worker = thread::Builder::new()
            .name("kortty-journal-screenshot-edit".into())
            .spawn(move || {
                let result = save_screenshot(draft, marks, note, redactor, capture)
                    .map(Arc::new)
                    .map_err(|error| error.to_string());
                let _ = send.try_send(result);
                drop(finished);
            });
        if let Err(error) = worker {
            return self.journal_screenshot_saved(id, Err(error.to_string()));
        }
        self.track_session_shutdown(done);
        Task::future(async move {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("The screenshot editor worker stopped.".into()));
            Desktop::SessionJournal(id, Message::Annotate(Event::Saved(result)))
        })
    }

    fn journal_screenshot_saved(
        &mut self,
        id: window::Id,
        result: Result<Arc<Saved>, String>,
    ) -> Task<Desktop> {
        let Some(dialog) = self.journal_screenshot_editors.dialogs.get_mut(&id) else {
            return Task::none();
        };
        if !matches!(dialog.phase, Phase::Saving(_)) {
            return Task::none();
        }
        let Phase::Saving(mut form) = std::mem::replace(&mut dialog.phase, Phase::Loading) else {
            return Task::none();
        };
        let target = dialog.target.clone();
        let entry_id = form.draft().id().to_owned();
        let mut close = false;
        dialog.phase = match &result {
            Ok(saved) => {
                let mut status = String::from("Screenshot saved.");
                if saved.outcome.durability_warning {
                    status.push_str(" Storage could not confirm a durable save.");
                }
                if saved.outcome.cleanup_warning {
                    status.push_str(" The screenshot recovery files could not be removed safely.");
                }
                if saved.document.is_none() {
                    status.push_str(" Refresh the viewer to reload the journal.");
                }
                close = !saved.outcome.durability_warning
                    && !saved.outcome.cleanup_warning
                    && saved.document.is_some();
                Phase::Report(status)
            }
            Err(error) => {
                form.set_status(format!(
                    "{error} Reopen the editor to load the current screenshot."
                ));
                Phase::Editing(form)
            }
        };
        let allowed = self.journal_allowed();
        let mut tasks = Vec::new();
        let mut affected = Vec::new();
        for (&viewer_id, surface) in &mut self.journals {
            let Mode::Viewer(viewer) = &mut surface.mode else {
                continue;
            };
            if !viewer.source.same_journal(&target) {
                continue;
            }
            viewer.entry_flow.set_screenshot_saving(false);
            affected.push(viewer_id);
            if let Ok(saved) = &result {
                viewer.invalidate_screenshot_thumbnail(&entry_id);
                if viewer
                    .screenshot
                    .as_ref()
                    .is_some_and(|screenshot| screenshot.entry_id() == entry_id)
                {
                    viewer.screenshot = None;
                    viewer.screenshot_menu = None;
                }
                if viewer.screenshot_request.as_deref() == Some(entry_id.as_str()) {
                    viewer.screenshot_request = None;
                    viewer.loading = false;
                }
                if let Some(document) = saved.document.as_ref().filter(|_| allowed) {
                    let mut document = Arc::clone(document);
                    if document.summary.directory != viewer.source.directory() {
                        Arc::make_mut(&mut document).summary.directory =
                            viewer.source.directory().to_owned();
                    }
                    if viewer.document.is_none() && viewer.loading {
                        tasks.push(viewer.update(viewer_id, Message::DocumentLoaded(Ok(document))));
                    } else {
                        tasks.push(viewer.replace_timeline(viewer_id, document));
                    }
                    tasks.push(viewer.reload_marker_draft(viewer_id));
                }
                if allowed {
                    tasks.push(viewer.request_screenshot_thumbnails(viewer_id));
                }
            }
        }
        if result.is_ok() {
            if let Some(root) = target.directory().parent() {
                tasks.push(self.refresh_journal_action_managers(root));
            }
            tasks.push(self.invalidate_journal_dock_screenshot(&target, &entry_id));
            tasks.push(self.refresh_journal_entry_docks(&target));
        }
        if close {
            tasks.push(
                self.close_journal_screenshot_editor(id)
                    .unwrap_or_else(Task::none),
            );
        }
        for id in affected {
            tasks.push(self.resume_hosted_tool_close(id));
        }
        Task::batch(tasks)
    }

    pub(super) fn close_journal_screenshot_editor(
        &mut self,
        id: window::Id,
    ) -> Option<Task<Desktop>> {
        if let Some(parent) = self.journal_screenshot_editors.prompt_parent(id) {
            self.journal_screenshot_editors
                .dialogs
                .get_mut(&parent)?
                .prompt = None;
            return Some(
                self.close_placed_dialog(id)
                    .chain(window::gain_focus(parent)),
            );
        }
        let dialog = self.journal_screenshot_editors.dialogs.get(&id)?;
        if let Some(prompt) = &dialog.prompt {
            return Some(window::gain_focus(prompt.id));
        }
        if matches!(dialog.phase, Phase::Saving(_)) {
            return Some(Task::none());
        }
        let mut dialog = self.journal_screenshot_editors.dialogs.remove(&id)?;
        dialog.cancel_note_translation();
        let source = dialog.origin.window();
        let focus = self.hosted_tool_owner(source).unwrap_or(source);
        Some(
            self.close_placed_dialog(id)
                .chain(window::gain_focus(focus)),
        )
    }

    pub(super) fn remove_journal_screenshot_editors(&mut self, owner: window::Id) -> Task<Desktop> {
        let mut closed = Vec::new();
        self.journal_screenshot_editors
            .dialogs
            .retain(|&id, dialog| {
                let retain = matches!(dialog.phase, Phase::Saving(_))
                    || (dialog.owner != owner && dialog.origin.window() != owner);
                if !retain {
                    dialog.cancel_note_translation();
                    if let Some(prompt) = &dialog.prompt {
                        closed.push(prompt.id);
                    }
                    closed.push(id);
                }
                retain
            });
        Task::batch(closed.into_iter().map(|id| self.close_placed_dialog(id)))
    }
}

fn save_screenshot(
    draft: Arc<Draft>,
    mut marks: Vec<Annotation>,
    mut note: String,
    redactor: Option<Redactor>,
    capture: Option<Arc<Capture>>,
) -> io::Result<Saved> {
    if let Some(redactor) = redactor {
        let deadline = Instant::now() + Duration::from_millis(100);
        let redact = |value: &str| {
            redactor
                .journal_text(value, deadline)
                .map(|text| text.to_string())
                .ok_or_else(|| {
                    io::Error::other("Could not safely apply the journal text redaction rules.")
                })
        };
        if note != draft.note() {
            note = redact(&note)?;
        }
        if marks != draft.annotations() {
            for mark in &mut marks {
                if mark.kind == Kind::Text && !draft.annotations().contains(mark) {
                    mark.text = redact(&mark.text)?;
                }
            }
            marks.retain(|mark| mark.kind != Kind::Text || !mark.text.trim().is_empty());
        }
    }
    let edit = Arc::new(draft.edit(marks, note)?);
    let outcome = publish_screenshot(&edit, capture)?;
    let document = draft.source().read(None).ok().flatten().map(Arc::new);
    Ok(Saved { outcome, document })
}

fn publish_screenshot(edit: &Arc<Edit>, capture: Option<Arc<Capture>>) -> io::Result<Outcome> {
    if let Some(capture) = capture {
        if capture.status() == Status::Recording {
            match capture.edit_screenshot(Arc::clone(edit)) {
                Ok(reply) => {
                    return reply
                        .recv_blocking()
                        .map_err(io::Error::other)?
                        .map_err(io::Error::other);
                }
                Err(error)
                    if error.kind() == io::ErrorKind::BrokenPipe
                        && capture.status() != Status::Recording => {}
                Err(error) => return Err(error),
            }
        }
        let _ = capture.done().recv_blocking();
    }
    screenshot_edit::apply(edit)
}

impl Dialog {
    fn cancel_note_translation(&mut self) {
        if let Phase::Editing(form) = &mut self.phase {
            form.cancel_translation();
        }
    }

    fn view<'a>(
        &'a self,
        allowed: bool,
        busy: bool,
        translation: note_translation::View<'a>,
    ) -> Element<'a, Event> {
        match &self.phase {
            Phase::Loading => simple_view("Reading screenshot…", true),
            Phase::Report(message) => simple_view(message, true),
            Phase::Editing(form) => {
                form.view(allowed && self.prompt.is_none(), busy, false, translation)
            }
            Phase::Saving(form) => form.view(false, true, true, translation),
        }
    }
}

fn simple_view(message: &str, close: bool) -> Element<'_, Event> {
    container(
        column![
            text(message),
            space::vertical(),
            row![
                space::horizontal(),
                button("Close").on_press_maybe(close.then_some(Event::Close))
            ],
        ]
        .spacing(12),
    )
    .padding(16)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

impl Prompt {
    fn view(&self) -> Element<'_, Event> {
        let valid = !self.value.trim().is_empty();
        container(
            column![
                text("Label text").size(18),
                text_input("Label text", &self.value)
                    .id(self.input.clone())
                    .on_input(Event::LabelText)
                    .on_submit(Event::ConfirmLabel),
                row![
                    space::horizontal(),
                    button("Cancel").on_press(Event::Close),
                    button("OK").on_press_maybe(valid.then_some(Event::ConfirmLabel)),
                ]
                .spacing(8),
            ]
            .spacing(8),
        )
        .padding(14)
        .width(Length::Fill)
        .into()
    }
}
