//! Owner-scoped confirmation for one selected journal entry.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::sync::Arc;
use std::thread;

use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Element, Length, Size, Task, window};

use crate::session_journal::entry_edit::delete::{self, Deletion, Outcome};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::super::{ViewerSource, display, label};
use super::{Capture, Desktop, Journal, Message, Workspace, read_task};

pub(in crate::workspace::journal_access) const TITLE: &str = "Delete journal entry";

#[derive(Default)]
pub(in crate::workspace) struct Dialogs {
    pub(in crate::workspace::journal_access) dialogs: HashMap<window::Id, Dialog>,
}

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Prepared(Result<Arc<Deletion>, String>),
    Confirm,
    Dismiss,
    Focus,
    Finished(Result<Arc<Finished>, String>),
}

pub(in crate::workspace::journal_access) struct Dialog {
    pub(in crate::workspace::journal_access) owner: window::Id,
    pub(in crate::workspace::journal_access) source: window::Id,
    generation: u64,
    title: String,
    has_draft: bool,
    phase: Phase,
}

enum Phase {
    Preparing,
    Review(Arc<Deletion>),
    Deleting(Arc<Deletion>),
    Report(String),
}

pub(crate) struct Finished {
    outcome: Outcome,
    document: Option<Arc<Journal>>,
}

impl fmt::Debug for Finished {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("JournalEntryDeletionFinished")
            .finish_non_exhaustive()
    }
}

impl Dialog {
    pub(in crate::workspace::journal_access) fn view(&self) -> Element<'_, Event> {
        let (body, dismiss, confirm) = match &self.phase {
            Phase::Preparing => (
                column![text("Checking the selected entry…")],
                Some("Cancel"),
                false,
            ),
            Phase::Review(_) => (
                column![
                    text("Delete this journal entry?").size(19),
                    text(&self.title).size(14),
                    text(
                        "The entry and its unused screenshot will be removed permanently. The capture log will remain intact."
                    ),
                    self.has_draft
                        .then(|| text("Unsaved changes to this entry will be discarded.").size(13)),
                ],
                Some("Cancel"),
                true,
            ),
            Phase::Deleting(_) => (column![text("Deleting journal entry…")], None, false),
            Phase::Report(problem) => (
                column![text(display(problem, 800)).size(14)],
                Some("Close"),
                false,
            ),
        };
        container(
            column![
                scrollable(body.spacing(8)).height(Length::Fill),
                row![
                    space::horizontal(),
                    dismiss.map(|label| button(label).on_press(Event::Dismiss)),
                    confirm.then(|| button("Delete entry")
                        .style(button::danger)
                        .on_press(Event::Confirm)),
                ]
                .spacing(8),
            ]
            .spacing(10),
        )
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

impl Workspace {
    pub(super) fn offer_journal_entry_delete(
        &mut self,
        source: window::Id,
        generation: u64,
    ) -> Task<Desktop> {
        if !self.journal_delete_allowed() || !self.entry_interactive(source) {
            return Task::none();
        }
        let Some(owner) = self.journal_owner(source, &self.journals[&source]) else {
            return Task::none();
        };
        let Some(viewer) = self.entry_viewer_mut(source) else {
            return Task::none();
        };
        if !viewer.entry_flow.input_enabled() || !viewer.entry_editor.matches_generation(generation)
        {
            return Task::none();
        }
        let Some(draft) = viewer.entry_editor.draft_handle() else {
            return Task::none();
        };
        if viewer.entry_row_by_id(draft.id()).is_none() {
            return Task::none();
        }
        viewer.entry_editor.cancel_translation();
        viewer.entry_flow.close_context();
        let title = label(&draft.fields().title, 120);
        let title = if title.trim().is_empty() {
            "Selected journal entry".into()
        } else {
            title
        };
        let has_draft = viewer.entry_editor.is_dirty();
        let (id, open) = self.open_placed_dialog(
            DialogKind::JournalEntryDelete,
            window::Settings {
                size: Size::new(460.0, 224.0),
                min_size: Some(Size::new(340.0, 190.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.journal_entry_deletes.dialogs.insert(
            id,
            Dialog {
                owner,
                source,
                generation,
                title,
                has_draft,
                phase: Phase::Preparing,
            },
        );
        let prepare = read_task(
            id,
            move || draft.prepare_delete(),
            |result| Message::EntryDelete(Event::Prepared(result)),
        );
        Task::batch([open.map(|_| Desktop::Noop), prepare])
    }

    pub(in crate::workspace::journal_access) fn update_journal_entry_delete(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Desktop> {
        match event {
            Event::Prepared(result) => {
                if let Some(dialog) = self.journal_entry_deletes.dialogs.get_mut(&id)
                    && matches!(dialog.phase, Phase::Preparing)
                {
                    dialog.phase = match result {
                        Ok(target) => Phase::Review(target),
                        Err(error) => Phase::Report(error),
                    };
                }
                Task::none()
            }
            Event::Confirm => self.confirm_journal_entry_delete(id),
            Event::Dismiss => self
                .close_journal_entry_delete(id)
                .unwrap_or_else(Task::none),
            Event::Focus if self.journal_entry_deletes.dialogs.contains_key(&id) => {
                window::gain_focus(id)
            }
            Event::Focus => Task::none(),
            Event::Finished(result) => self.journal_entry_deleted(id, result),
        }
    }

    fn confirm_journal_entry_delete(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(dialog) = self.journal_entry_deletes.dialogs.get(&id) else {
            return Task::none();
        };
        let Phase::Review(target) = &dialog.phase else {
            return Task::none();
        };
        let source = dialog.source;
        let allowed = self.journal_delete_allowed()
            && !self.exiting
            && !self.scheduler_draining
            && self.visible_journal_surface_owner(source) == Some(dialog.owner)
            && self.journal_edit_for_owner(source) == Some(id)
            && !self.hosted_close_locks(source)
            && self.entry_viewer(source).is_some_and(|viewer| {
                viewer.can_edit_entries()
                    && viewer.entry_flow.input_enabled()
                    && viewer.entry_editor.matches_generation(dialog.generation)
                    && viewer.source.same_journal(target.source())
                    && viewer
                        .entry_editor
                        .draft()
                        .is_some_and(|draft| draft.id() == target.id())
                    && viewer.entry_row_by_id(target.id()).is_some()
            });
        if !allowed {
            if let Some(dialog) = self.journal_entry_deletes.dialogs.get_mut(&id) {
                dialog.phase = Phase::Report(
                    "The entry selection or deletion permission changed. Try again.".into(),
                );
            }
            return Task::none();
        }
        let target = Arc::clone(target);
        let capture = self
            .journal_metadata_captures()
            .into_iter()
            .find(|capture| ViewerSource::capture(capture).same_journal(target.source()));
        if let Some(viewer) = self.entry_viewer_mut(source) {
            viewer.entry_flow.deleting = true;
            viewer.entry_flow.status = None;
        }
        if let Some(dialog) = self.journal_entry_deletes.dialogs.get_mut(&id) {
            dialog.phase = Phase::Deleting(Arc::clone(&target));
        }
        let (send, receive) = async_channel::bounded(1);
        let (finished, done) = async_channel::bounded(1);
        let worker = thread::Builder::new()
            .name("kortty-journal-entry-delete".into())
            .spawn(move || {
                let result = remove_entry(target, capture)
                    .map(Arc::new)
                    .map_err(|error| error.to_string());
                let _ = send.try_send(result);
                drop(finished);
            });
        if let Err(error) = worker {
            return self.journal_entry_deleted(id, Err(error.to_string()));
        }
        self.track_session_shutdown(done);
        Task::future(async move {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("Journal entry deletion worker stopped.".into()));
            Desktop::SessionJournal(id, Message::EntryDelete(Event::Finished(result)))
        })
    }

    fn journal_entry_deleted(
        &mut self,
        id: window::Id,
        result: Result<Arc<Finished>, String>,
    ) -> Task<Desktop> {
        let Some(dialog) = self.journal_entry_deletes.dialogs.get(&id) else {
            return Task::none();
        };
        let Phase::Deleting(target) = &dialog.phase else {
            return Task::none();
        };
        let target = Arc::clone(target);
        let source = dialog.source;
        let allowed = self.journal_allowed();
        if let Some(viewer) = self.entry_viewer_mut(source) {
            viewer.entry_flow.deleting = false;
        }
        let finished = match result {
            Ok(finished) => finished,
            Err(error) => {
                if let Some(dialog) = self.journal_entry_deletes.dialogs.get_mut(&id) {
                    dialog.phase = Phase::Report(error);
                }
                return Task::none();
            }
        };
        let warning = if finished.outcome.durability_warning {
            Some(
                "The entry was removed from the current journal. Storage could not confirm a durable write. The screenshot was kept.",
            )
        } else if finished.outcome.cleanup_warning {
            Some("The entry was deleted. Its screenshot could not be removed safely.")
        } else {
            None
        };
        let status = warning.unwrap_or(if finished.document.is_some() {
            "Entry deleted."
        } else {
            "Entry deleted. Refresh to see the updated journal."
        });
        let viewer_update = if let Some(viewer) = self.entry_viewer_mut(source)
            && viewer.source.same_journal(target.source())
        {
            if viewer
                .entry_editor
                .draft()
                .is_some_and(|draft| draft.id() == target.id())
            {
                viewer.entry_editor.clear_selection();
            }
            viewer.entry_flow.status = Some(status.into());
            viewer.refresh.invalidate();
            if allowed {
                finished
                    .document
                    .as_ref()
                    .map_or_else(Task::none, |document| {
                        viewer.replace_timeline(source, Arc::clone(document))
                    })
            } else {
                Task::none()
            }
        } else {
            Task::none()
        };
        if let Some(dialog) = self.journal_entry_deletes.dialogs.get_mut(&id) {
            dialog.phase = Phase::Report(status.into());
        }
        let close = if warning.is_none() {
            self.close_journal_entry_delete(id)
                .unwrap_or_else(Task::none)
        } else {
            Task::none()
        };
        let managers = target
            .source()
            .directory()
            .parent()
            .map_or_else(Task::none, |root| {
                self.refresh_journal_action_managers(root)
            });
        Task::batch([
            viewer_update,
            close,
            managers,
            self.refresh_journal_entry_docks(target.source()),
            self.resume_hosted_tool_close(source),
        ])
    }

    pub(in crate::workspace::journal_access) fn close_journal_entry_delete(
        &mut self,
        id: window::Id,
    ) -> Option<Task<Desktop>> {
        let dialog = self.journal_entry_deletes.dialogs.get(&id)?;
        if matches!(dialog.phase, Phase::Deleting(_)) {
            return Some(Task::none());
        }
        let focus = self
            .hosted_tool_owner(dialog.source)
            .unwrap_or(dialog.source);
        self.journal_entry_deletes.dialogs.remove(&id);
        Some(
            self.close_placed_dialog(id)
                .chain(window::gain_focus(focus)),
        )
    }

    pub(in crate::workspace::journal_access) fn remove_journal_entry_delete_dialogs(
        &mut self,
        owner: window::Id,
    ) -> Task<Desktop> {
        let mut close = Vec::new();
        self.journal_entry_deletes.dialogs.retain(|&id, dialog| {
            let keep = dialog.owner != owner && dialog.source != owner;
            if !keep {
                close.push(id);
            }
            keep
        });
        Task::batch(close.into_iter().map(|id| self.close_placed_dialog(id)))
    }
}

fn remove_entry(target: Arc<Deletion>, capture: Option<Arc<Capture>>) -> io::Result<Finished> {
    let outcome = if let Some(capture) = capture {
        match capture.delete_entry(Arc::clone(&target)) {
            Ok(reply) => reply
                .recv_blocking()
                .map_err(io::Error::other)?
                .map_err(io::Error::other)?,
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                let _ = capture.done().recv_blocking();
                delete::commit(&target)?
            }
            Err(error) => return Err(error),
        }
    } else {
        delete::commit(&target)?
    };
    let document = target.source().read(None).ok().flatten().map(Arc::new);
    Ok(Finished { outcome, document })
}

#[cfg(test)]
mod tests;
