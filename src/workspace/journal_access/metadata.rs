use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{
    button, column, container, row, scrollable, space, text, text_editor, text_input,
};
use iced::{Element, Length, Size, Task, window};

use crate::private_files::Directory;
use crate::session_journal::viewer::Source as ViewerSource;
use crate::session_journal::{self, Capture, Journal, MetadataEdit, Status};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Desktop, Message as JournalMessage, Mode, Workspace};

pub(super) const TITLE: &str = "Rename Session Journal";
const MAX_EDITOR_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug)]
pub enum Message {
    Rename,
    RenameTarget((PathBuf, String)),
    TitleMenu,
    DismissMenu,
    Title(String),
    SaveTitle,
    Cancel,
    Focus,
    Description((PathBuf, String), text_editor::Action),
    SaveDescription((PathBuf, String)),
    Saved(u64, Result<Arc<SavedMetadata>, String>),
    RefreshAfterEdit(u64, PathBuf),
    LiveAliasesLoaded(u64, PathBuf, Result<Arc<Vec<LiveAlias>>, String>),
}

#[derive(Clone, Debug)]
pub struct SavedMetadata {
    document: Arc<Journal>,
    edit: MetadataEdit,
    source: ViewerSource,
    aliases: Vec<PathBuf>,
}

impl SavedMetadata {
    /// Resolves known UI paths on the worker after the actual metadata save.
    fn new(
        document: Journal,
        edit: MetadataEdit,
        source: ViewerSource,
        mut aliases: Vec<PathBuf>,
    ) -> io::Result<Self> {
        if document.summary.id != source.id() {
            return Err(io::Error::other("The journal changed while saving."));
        }
        aliases.sort_unstable();
        aliases.dedup();
        aliases.retain(|path| {
            Directory::open(path, false)
                .and_then(|opened| source.require_location(path, &opened))
                .is_ok()
        });
        if !aliases.iter().any(|path| path == source.directory()) {
            return Err(io::Error::other("The journal changed while saving."));
        }
        Ok(Self {
            document: Arc::new(document),
            edit,
            source,
            aliases,
        })
    }

    fn matches(&self, directory: &Path, id: &str) -> bool {
        id == self.document.summary.id && self.aliases.iter().any(|alias| alias == directory)
    }

    fn update_summary(&self, summary: &mut session_journal::Summary) {
        // The save may have atomically replaced the document seen by the catalog.
        summary.deletion_identity = None;
        match self.edit {
            MetadataEdit::Title(_) => summary.title.clone_from(&self.document.summary.title),
            MetadataEdit::Description(_) => {
                summary
                    .description
                    .clone_from(&self.document.summary.description);
            }
            MetadataEdit::Pinned(_) => summary.pinned = self.document.summary.pinned,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LiveAlias {
    directory: PathBuf,
    id: String,
    capture_directory: PathBuf,
}

#[derive(Default)]
pub(in crate::workspace) struct Edits {
    pub(super) dialogs: HashMap<window::Id, RenameDialog>,
    pending: HashMap<u64, Pending>,
}

pub(super) struct RenameDialog {
    input: iced::widget::Id,
    source: window::Id,
    owner: window::Id,
    directory: PathBuf,
    expected_id: String,
    original: String,
    title: String,
    saving: bool,
    problem: Option<String>,
}

struct Pending {
    source: window::Id,
    dialog: Option<window::Id>,
    directory: PathBuf,
    expected_id: String,
    bound_source: Option<ViewerSource>,
    edit: MetadataEdit,
    candidates: Vec<PathBuf>,
}

impl RenameDialog {
    pub(super) fn view(&self) -> Element<'_, Message> {
        let mut input = text_input("New title", &self.title).id(self.input.clone());
        if !self.saving {
            input = input.on_input(Message::Title).on_submit(Message::SaveTitle);
        }
        container(
            column![
                scrollable(
                    column![
                        text("New title:").size(19),
                        input,
                        self.problem
                            .as_deref()
                            .map(|problem| text(problem).size(13)),
                        self.saving
                            .then_some(text("Saving journal title…").size(13)),
                    ]
                    .spacing(12)
                )
                .height(Length::Fill),
                row![
                    space::horizontal(),
                    button("Cancel").on_press_maybe((!self.saving).then_some(Message::Cancel)),
                    button("OK").on_press_maybe(
                        (!self.saving
                            && MetadataEdit::Title(self.title.clone()).normalized().is_ok())
                        .then_some(Message::SaveTitle)
                    ),
                ]
                .spacing(8),
            ]
            .spacing(12),
        )
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

impl Workspace {
    pub(super) fn journal_metadata_is_live(
        &self,
        manager: &super::Manager,
        summary: &session_journal::Summary,
    ) -> bool {
        self.journal_metadata_captures().iter().any(|capture| {
            capture.id() == summary.id
                && matches!(capture.status(), Status::Recording | Status::Finishing)
                && manager.live_aliases.as_ref().is_some_and(|aliases| {
                    aliases.iter().any(|alias| {
                        alias.directory == summary.directory
                            && alias.id == summary.id
                            && alias.capture_directory == capture.directory()
                    })
                })
        })
    }

    pub(in crate::workspace) fn dismiss_journal_menu(&mut self, id: window::Id) -> bool {
        match self.journals.get_mut(&id).map(|surface| &mut surface.mode) {
            Some(Mode::Viewer(viewer)) => std::mem::take(&mut viewer.title_menu),
            Some(Mode::Manager(manager)) => manager.tree.dismiss_context(),
            None => false,
        }
    }

    pub(super) fn refresh_journal_live_aliases(&mut self, source: window::Id) -> Task<Desktop> {
        let captures: Vec<_> = self
            .journal_metadata_captures()
            .into_iter()
            .filter(|capture| matches!(capture.status(), Status::Recording | Status::Finishing))
            .collect();
        let Some(Mode::Manager(manager)) = self
            .journals
            .get_mut(&source)
            .map(|surface| &mut surface.mode)
        else {
            return Task::none();
        };
        if manager.loading {
            return Task::none();
        }
        let Some(catalog) = &manager.catalog else {
            return Task::none();
        };
        let Some(root) = manager.root.clone() else {
            return Task::none();
        };
        if captures.is_empty() {
            manager.live_aliases = Some(Vec::new());
            return Task::none();
        }
        manager.live_aliases = None;
        let revision = manager.revision;
        let candidates = catalog
            .journals
            .iter()
            .map(|row| (row.directory.clone(), row.id.clone()))
            .collect();
        super::read_task(
            source,
            move || resolve_live_aliases(candidates, captures),
            move |result| {
                JournalMessage::Metadata(Message::LiveAliasesLoaded(revision, root, result))
            },
        )
    }

    fn journal_metadata_paths(&self, expected_id: &str, captures: &[Arc<Capture>]) -> Vec<PathBuf> {
        let mut paths: Vec<_> = captures
            .iter()
            .filter(|capture| capture.id() == expected_id)
            .map(|capture| capture.directory().to_owned())
            .collect();
        for surface in self.journals.values() {
            match &surface.mode {
                Mode::Viewer(viewer) if viewer.source.id() == expected_id => {
                    paths.push(viewer.source.directory().to_owned())
                }
                Mode::Manager(manager) => {
                    if let Some(catalog) = &manager.catalog {
                        paths.extend(
                            catalog
                                .journals
                                .iter()
                                .filter(|row| row.id == expected_id)
                                .map(|row| row.directory.clone()),
                        );
                    }
                }
                _ => {}
            }
        }
        paths.sort();
        paths.dedup();
        paths
    }
    pub(super) fn journal_rename_allowed(&self) -> bool {
        self.journal_allowed()
            && self
                .clipboard
                .policy
                .terminal_logging
                .journal_rename_allowed()
    }

    pub(super) fn journal_edit_busy(&self, source: window::Id) -> bool {
        self.journal_actions.pinning.contains_key(&source)
            || self
                .journal_edits
                .pending
                .values()
                .any(|pending| pending.source == source)
    }

    pub(in crate::workspace) fn journal_edit_for_owner(
        &self,
        owner: window::Id,
    ) -> Option<window::Id> {
        self.journal_screenshot_editors
            .for_owner(owner)
            .or_else(|| self.journal_markers.for_owner(owner))
            .or_else(|| {
                self.journal_edits.dialogs.iter().find_map(|(&id, dialog)| {
                    (dialog.owner == owner || dialog.source == owner).then_some(id)
                })
            })
            .or_else(|| {
                self.journal_actions
                    .dialogs
                    .iter()
                    .find_map(|(&id, dialog)| {
                        (dialog.owner == owner || dialog.source == owner).then_some(id)
                    })
            })
            .or_else(|| {
                self.journal_entry_deletes
                    .dialogs
                    .iter()
                    .find_map(|(&id, dialog)| {
                        (dialog.owner == owner || dialog.source == owner).then_some(id)
                    })
            })
            .or_else(|| {
                self.journal_replacements
                    .dialogs
                    .iter()
                    .find_map(|(&id, dialog)| {
                        (dialog.owner == owner || dialog.source == owner).then_some(id)
                    })
            })
            .or_else(|| {
                self.journal_screenshot_exports
                    .dialogs
                    .iter()
                    .find_map(|(&id, dialog)| {
                        (dialog.owner == owner || dialog.origin.window() == owner).then_some(id)
                    })
            })
            .or_else(|| {
                self.journal_exports
                    .dialogs
                    .iter()
                    .find_map(|(&id, dialog)| {
                        (dialog.owner == owner || dialog.source == owner).then_some(id)
                    })
            })
    }

    pub(in crate::workspace) fn close_journal_edit(
        &mut self,
        id: window::Id,
    ) -> Option<Task<Desktop>> {
        if let Some(close) = self.close_journal_screenshot_editor(id) {
            return Some(close);
        }
        if let Some(close) = self.close_journal_markers(id) {
            return Some(close);
        }
        if let Some(close) = self.close_journal_screenshot_export(id) {
            return Some(close);
        }
        if let Some(close) = self.close_journal_export(id) {
            return Some(close);
        }
        if let Some(close) = self.close_delete_journal_dialog(id) {
            return Some(close);
        }
        if let Some(close) = self.close_journal_entry_delete(id) {
            return Some(close);
        }
        if let Some(close) = self.close_journal_replacement(id) {
            return Some(close);
        }
        if let Some(dialog) = self.journal_edits.dialogs.get(&id) {
            if dialog.saving {
                return Some(Task::none());
            }
            let focus = self
                .hosted_tool_owner(dialog.source)
                .unwrap_or(dialog.source);
            self.journal_edits.dialogs.remove(&id);
            return Some(
                self.close_placed_dialog(id)
                    .chain(window::gain_focus(focus)),
            );
        }
        if let Some(dialog) = self.journal_edit_for_owner(id) {
            return Some(window::gain_focus(dialog));
        }
        self.journal_edit_busy(id).then(Task::none)
    }

    pub(super) fn remove_journal_edit_owner(&mut self, source: window::Id) -> Task<Desktop> {
        let mut close = Vec::new();
        self.journal_edits.dialogs.retain(|&id, dialog| {
            let retain = dialog.owner != source && dialog.source != source;
            if !retain {
                close.push(id);
            }
            retain
        });
        let renamed = Task::batch(close.into_iter().map(|id| self.close_placed_dialog(id)));
        Task::batch([
            renamed,
            self.remove_delete_journal_dialogs(source),
            self.remove_journal_entry_delete_dialogs(source),
            self.remove_journal_replacements(source),
            self.remove_journal_screenshot_exports(source),
            self.remove_journal_screenshot_editors(source),
            self.remove_journal_markers(source),
            self.remove_journal_exports(source),
        ])
    }

    pub(super) fn update_journal_metadata(
        &mut self,
        source: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        match message {
            Message::Saved(ticket, result) => return self.journal_metadata_saved(ticket, result),
            Message::RefreshAfterEdit(revision, root) => {
                if self.journals.get(&source).is_some_and(|surface| {
                    matches!(&surface.mode, Mode::Manager(manager) if
                        manager.revision == revision && manager.root.as_ref() == Some(&root))
                }) {
                    return self.refresh_journal_manager(source, false);
                }
                return Task::none();
            }
            Message::LiveAliasesLoaded(revision, root, result) => {
                if let Some(Mode::Manager(manager)) = self.journals.get_mut(&source).map(|surface| &mut surface.mode)
                    && manager.revision == revision && manager.root.as_ref() == Some(&root) && !manager.loading
                {
                    match result {
                        Ok(aliases) => manager.live_aliases = Some((*aliases).clone()),
                        Err(error) => manager.error = Some(format!("Could not check recording journals ({error}).")),
                    }
                }
                return Task::none();
            }
            Message::Cancel => return self.close_journal_edit(source).unwrap_or_else(Task::none),
            Message::Focus => {
                return self.journal_edits.dialogs.get(&source).map_or_else(Task::none, |dialog| {
                    window::gain_focus(source)
                        .chain(iced::widget::operation::focus(dialog.input.clone()))
                        .chain(iced::widget::operation::select_all(dialog.input.clone()))
                });
            }
            _ if !self.journal_allowed() || self.exiting || self.scheduler_draining => {
                return Task::none();
            }
            Message::Description(_, _) | Message::SaveDescription(_)
                if self.journal_actions.dialogs.values().any(|dialog| dialog.source == source) =>
            {
                return Task::none();
            }
            Message::Rename => return self.prompt_journal_rename(source),
            Message::RenameTarget(target) => {
                if self.journals.get(&source).is_some_and(|surface| matches!(&surface.mode,
                    Mode::Manager(manager) if manager.selected().is_some_and(|row| row.directory == target.0 && row.id == target.1)))
                {
                    return self.prompt_journal_rename(source);
                }
            }
            Message::Title(value) if value.len() <= MAX_EDITOR_BYTES => {
                if let Some(dialog) = self.journal_edits.dialogs.get_mut(&source)
                    && !dialog.saving
                {
                    dialog.title = value;
                    dialog.problem = None;
                }
            }
            Message::SaveTitle if self.journal_rename_allowed() => {
                return self.save_journal_title(source);
            }
            Message::TitleMenu | Message::DismissMenu => {
                if let Some(Mode::Viewer(viewer)) = self
                    .journals
                    .get_mut(&source)
                    .map(|surface| &mut surface.mode)
                {
                    viewer.title_menu = matches!(message, Message::TitleMenu);
                }
            }
            Message::Description(target, action) if !self.journal_edit_busy(source) => {
                if let Some(Mode::Manager(manager)) = self
                    .journals
                    .get_mut(&source)
                    .map(|surface| &mut surface.mode)
                    && manager.description_target.as_ref() == Some(&target)
                    && manager.selected().is_some_and(|row| row.directory == target.0 && row.id == target.1)
                {
                    let previous = action.is_edit().then(|| manager.description.text());
                    manager.description.perform(action);
                    if let Some(previous) = previous
                        && manager.description.text().len() > MAX_EDITOR_BYTES
                    {
                        manager.description = text_editor::Content::with_text(&previous);
                        manager.description_status =
                            Some("Description exceeds the size limit.".into());
                    } else {
                        manager.description_status = None;
                    }
                }
            }
            Message::SaveDescription(target) if !self.journal_edit_busy(source) => {
                let Some(Mode::Manager(manager)) =
                    self.journals.get(&source).map(|surface| &surface.mode)
                else {
                    return Task::none();
                };
                let Some(selected) = manager.selected().filter(|row| row.directory == target.0 && row.id == target.1) else {
                    return Task::none();
                };
                let target = (selected.directory.clone(), selected.id.clone());
                let edit = MetadataEdit::Description(manager.description.text());
                return self.start_journal_metadata(source, None, target.0, target.1, edit);
            }
            _ => {}
        }
        Task::none()
    }

    fn prompt_journal_rename(&mut self, source: window::Id) -> Task<Desktop> {
        if !self.journal_rename_allowed() || self.journal_edit_busy(source) {
            return Task::none();
        }
        if let Some(id) = self.journal_edit_for_owner(source) {
            return window::gain_focus(id);
        }
        let Some(surface) = self.journals.get(&source) else {
            return Task::none();
        };
        let Some(owner) = self.journal_owner(source, surface) else {
            return Task::none();
        };
        if !self.palette_can_open(owner) {
            return Task::none();
        }
        let summary = match &surface.mode {
            Mode::Manager(manager) if manager.live_aliases.is_some() => manager
                .selected()
                .filter(|row| !self.journal_metadata_is_live(manager, row)),
            Mode::Manager(_) => None,
            Mode::Viewer(viewer) => viewer
                .document
                .as_ref()
                .filter(|_| !viewer.loading)
                .map(|document| &document.summary),
        };
        let Some(summary) = summary else {
            return Task::none();
        };
        let (directory, expected_id) = match &surface.mode {
            Mode::Viewer(viewer) => (
                viewer.source.directory().to_owned(),
                viewer.source.id().to_owned(),
            ),
            Mode::Manager(_) => (summary.directory.clone(), summary.id.clone()),
        };
        let dialog = RenameDialog {
            input: iced::widget::Id::unique(),
            source,
            owner,
            directory,
            expected_id,
            original: summary.title.clone(),
            title: summary.title.clone(),
            saving: false,
            problem: None,
        };
        if let Some(Mode::Viewer(viewer)) = self
            .journals
            .get_mut(&source)
            .map(|surface| &mut surface.mode)
        {
            viewer.title_menu = false;
        }
        let (id, open) = self.open_placed_dialog(
            DialogKind::JournalRename,
            window::Settings {
                size: Size::new(460.0, 210.0),
                min_size: Some(Size::new(360.0, 190.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.journal_edits.dialogs.insert(id, dialog);
        open.map(move |_| Desktop::SessionJournal(id, JournalMessage::Metadata(Message::Focus)))
    }

    fn save_journal_title(&mut self, dialog_id: window::Id) -> Task<Desktop> {
        let Some(dialog) = self
            .journal_edits
            .dialogs
            .get(&dialog_id)
            .filter(|dialog| !dialog.saving)
        else {
            return Task::none();
        };
        let edit = match MetadataEdit::Title(dialog.title.clone()).normalized() {
            Ok(edit) => edit,
            Err(error) => {
                let source = dialog.source;
                self.journal_metadata_problem(source, Some(dialog_id), error.to_string());
                return Task::none();
            }
        };
        if matches!(&edit, MetadataEdit::Title(title) if title == &dialog.original) {
            return self
                .close_journal_edit(dialog_id)
                .unwrap_or_else(Task::none);
        }
        let (source, directory, expected_id) = (
            dialog.source,
            dialog.directory.clone(),
            dialog.expected_id.clone(),
        );
        self.start_journal_metadata(source, Some(dialog_id), directory, expected_id, edit)
    }

    fn start_journal_metadata(
        &mut self,
        source: window::Id,
        dialog: Option<window::Id>,
        directory: PathBuf,
        expected_id: String,
        edit: MetadataEdit,
    ) -> Task<Desktop> {
        let edit = match edit.normalized() {
            Ok(edit) => edit,
            Err(error) => {
                self.journal_metadata_problem(source, dialog, error.to_string());
                return Task::none();
            }
        };
        if self
            .journal_edits
            .pending
            .values()
            .any(|pending| pending.expected_id == expected_id)
        {
            self.journal_metadata_problem(
                source,
                dialog,
                "This journal is still being saved. Try again shortly.".into(),
            );
            return Task::none();
        }
        let ticket = self.allocate();
        let captures = self.journal_metadata_captures();
        let mut candidates = self.journal_metadata_paths(&expected_id, &captures);
        candidates.push(directory.clone());
        let bound_source = match self.journals.get(&source).map(|surface| &surface.mode) {
            Some(Mode::Viewer(viewer)) => Some(viewer.source.clone()),
            Some(Mode::Manager(_)) => None,
            None => return Task::none(),
        };
        let request = Pending {
            source,
            dialog,
            directory: directory.clone(),
            expected_id: expected_id.clone(),
            bound_source: bound_source.clone(),
            edit: edit.clone(),
            candidates: candidates.clone(),
        };
        self.journal_edits.pending.insert(ticket, request);
        if let Some(dialog) = dialog.and_then(|id| self.journal_edits.dialogs.get_mut(&id)) {
            dialog.saving = true;
        }
        let (send, receive) = async_channel::bounded(1);
        let (finished, done) = async_channel::bounded(1);
        self.track_session_shutdown(done);
        let spawn = std::thread::Builder::new()
            .name("kortty-journal-metadata".into())
            .spawn(move || {
                let result = save_verified_metadata(
                    &directory,
                    &expected_id,
                    edit,
                    captures,
                    bound_source,
                    candidates,
                )
                .map(Arc::new)
                .map_err(|error| error.to_string());
                let _ = send.try_send(result);
                drop(finished);
            });
        if let Err(error) = spawn {
            return self.journal_metadata_saved(ticket, Err(error.to_string()));
        }
        Task::future(async move {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("Journal metadata worker stopped.".into()));
            Desktop::SessionJournal(
                source,
                JournalMessage::Metadata(Message::Saved(ticket, result)),
            )
        })
    }

    fn journal_metadata_problem(
        &mut self,
        source: window::Id,
        dialog: Option<window::Id>,
        problem: String,
    ) {
        if let Some(dialog) = dialog.and_then(|id| self.journal_edits.dialogs.get_mut(&id)) {
            dialog.saving = false;
            dialog.problem = Some(problem);
        } else if let Some(surface) = self.journals.get_mut(&source) {
            match &mut surface.mode {
                Mode::Manager(manager) => manager.description_status = Some(problem),
                Mode::Viewer(viewer) => viewer.error = Some(problem),
            }
        }
    }

    fn journal_metadata_saved(
        &mut self,
        ticket: u64,
        result: Result<Arc<SavedMetadata>, String>,
    ) -> Task<Desktop> {
        let Some(pending) = self.journal_edits.pending.remove(&ticket) else {
            return Task::none();
        };
        let saved = match result {
            Ok(saved) => saved,
            Err(problem) => {
                self.journal_metadata_problem(pending.source, pending.dialog, problem);
                return Task::none();
            }
        };
        if !saved.matches(&pending.directory, &pending.expected_id)
            || saved.edit != pending.edit
            || pending
                .bound_source
                .as_ref()
                .is_some_and(|source| !saved.source.same_journal(source))
            || saved
                .aliases
                .iter()
                .any(|alias| !pending.candidates.contains(alias))
        {
            self.journal_metadata_problem(
                pending.source,
                pending.dialog,
                "The journal changed while saving.".into(),
            );
            return Task::none();
        }
        let mut refresh = Vec::new();
        for (&id, surface) in &mut self.journals {
            match &mut surface.mode {
                Mode::Viewer(viewer)
                    if saved.source.same_journal(&viewer.source)
                        && saved
                            .aliases
                            .iter()
                            .any(|path| path == viewer.source.directory()) =>
                {
                    viewer.refresh.invalidate();
                    if let Some(document) = &mut viewer.document {
                        if document.summary.id != pending.expected_id {
                            continue;
                        }
                        saved.update_summary(&mut Arc::make_mut(document).summary);
                    } else if viewer.loading {
                        match saved.edit {
                            MetadataEdit::Title(_) => {
                                viewer.deferred_title = Some((
                                    pending.expected_id.clone(),
                                    saved.document.summary.title.clone(),
                                ))
                            }
                            MetadataEdit::Description(_) => {
                                viewer.deferred_description = Some((
                                    pending.expected_id.clone(),
                                    saved.document.summary.description.clone(),
                                ))
                            }
                            MetadataEdit::Pinned(_) => {}
                        }
                        continue;
                    } else {
                        continue;
                    }
                    if matches!(saved.edit, MetadataEdit::Title(_)) {
                        viewer.name.clone_from(&saved.document.summary.title);
                    }
                }
                Mode::Manager(manager) => {
                    let selected = manager.description_target.as_ref().is_some_and(
                        |(directory, expected_id)| saved.matches(directory, expected_id),
                    );
                    let pristine = selected
                        && manager.catalog.as_ref().is_some_and(|catalog| {
                            catalog
                                .journals
                                .iter()
                                .find(|row| saved.matches(&row.directory, &row.id))
                                .is_some_and(|row| row.description == manager.description.text())
                        });
                    if manager.reading.is_some() {
                        manager.revision = manager.revision.wrapping_add(1);
                        manager.pending.clone_from(&manager.root);
                    }
                    let mut changed = false;
                    if let Some(catalog) = &mut manager.catalog
                        && catalog
                            .journals
                            .iter()
                            .any(|row| saved.matches(&row.directory, &row.id))
                    {
                        for row in &mut Arc::make_mut(catalog).journals {
                            if saved.matches(&row.directory, &row.id) {
                                saved.update_summary(row);
                            }
                        }
                        manager.content_search.invalidate();
                        changed = true;
                    }
                    if changed
                        && manager.reading.is_none()
                        && let Some(root) = &manager.root
                    {
                        refresh.push(Task::done(Desktop::SessionJournal(
                            id,
                            JournalMessage::Metadata(Message::RefreshAfterEdit(
                                manager.revision,
                                root.clone(),
                            )),
                        )));
                    }
                    manager.rebuild_tree();
                    manager.sync_description();
                    if selected
                        && manager.description_target.as_ref().is_some_and(
                            |(directory, expected_id)| saved.matches(directory, expected_id),
                        )
                        && matches!(saved.edit, MetadataEdit::Description(_))
                        && (pristine || id == pending.source)
                    {
                        manager.description =
                            text_editor::Content::with_text(&saved.document.summary.description);
                        manager.description_status =
                            (id == pending.source).then(|| "Description saved".into());
                    }
                }
                _ => {}
            }
        }
        if matches!(saved.edit, MetadataEdit::Title(_)) {
            self.journal_metadata_title_saved(
                &saved.aliases,
                &pending.expected_id,
                &saved.document.summary.title,
            );
        }
        if let Some(dialog) = pending
            .dialog
            .and_then(|id| self.journal_edits.dialogs.get_mut(&id))
        {
            dialog.saving = false;
        }
        let close = pending
            .dialog
            .and_then(|id| self.close_journal_edit(id))
            .unwrap_or_else(Task::none);
        Task::batch(refresh.into_iter().chain([close]))
    }
}

fn save_verified_metadata(
    directory: &Path,
    expected_id: &str,
    edit: MetadataEdit,
    captures: Vec<Arc<Capture>>,
    bound_source: Option<ViewerSource>,
    candidates: Vec<PathBuf>,
) -> io::Result<SavedMetadata> {
    let opened = Directory::open(directory, false)?;
    let allow_live_title = bound_source.is_some();
    let source = if let Some(source) = bound_source {
        if source.id() != expected_id {
            return Err(io::Error::other("The journal changed while saving."));
        }
        source.require_location(directory, &opened)?;
        source
    } else {
        ViewerSource::from_open_directory(directory, expected_id, &opened)?
    };
    let document = save_metadata_impl(
        directory,
        expected_id,
        &edit,
        captures,
        allow_live_title,
        Some((&source, opened)),
    )?;
    SavedMetadata::new(document, edit, source, candidates)
}

pub(super) fn save_metadata(
    directory: &Path,
    expected_id: &str,
    edit: &MetadataEdit,
    captures: Vec<Arc<Capture>>,
    allow_live_title: bool,
) -> io::Result<Journal> {
    save_metadata_impl(
        directory,
        expected_id,
        edit,
        captures,
        allow_live_title,
        None,
    )
}

fn save_metadata_impl(
    directory: &Path,
    expected_id: &str,
    edit: &MetadataEdit,
    captures: Vec<Arc<Capture>>,
    allow_live_title: bool,
    verified: Option<(&ViewerSource, Directory)>,
) -> io::Result<Journal> {
    for capture in captures
        .into_iter()
        .filter(|capture| capture.id() == expected_id)
    {
        let same_directory = match &verified {
            Some((source, _)) => source.same_journal(&ViewerSource::capture(&capture)),
            None => capture.matches_directory(directory)?,
        };
        if !same_directory {
            continue;
        }
        if !allow_live_title
            && matches!(edit, MetadataEdit::Title(_))
            && matches!(capture.status(), Status::Recording | Status::Finishing)
        {
            return Err(io::Error::other(
                "A running journal can be renamed in its viewer.",
            ));
        }
        match capture.edit_metadata(edit.clone()) {
            Ok(reply) => {
                reply
                    .recv_blocking()
                    .map_err(io::Error::other)?
                    .map_err(io::Error::other)?;
                if let Some((source, _)) = &verified {
                    return source
                        .read(None)?
                        .ok_or_else(|| io::Error::other("The journal changed while saving."));
                }
                let document = session_journal::read(capture.directory())?;
                if document.summary.id != expected_id {
                    return Err(io::Error::other("The journal changed while saving."));
                }
                return Ok(document);
            }
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                // Finalization owns the writer lock until done closes.
                let _ = capture.done().recv_blocking();
            }
            Err(error) => return Err(error),
        }
    }
    if let Some((source, opened)) = verified {
        session_journal::edit_metadata_in(directory, opened, source, edit)
    } else {
        session_journal::edit_metadata(directory, expected_id, edit)
    }
}

pub(super) fn resolve_live_aliases(
    candidates: Vec<(PathBuf, String)>,
    captures: Vec<Arc<Capture>>,
) -> io::Result<Vec<LiveAlias>> {
    let mut aliases = Vec::new();
    for (directory, id) in candidates {
        for capture in captures.iter().filter(|capture| capture.id() == id) {
            if capture.matches_directory(&directory)? {
                aliases.push(LiveAlias {
                    directory: directory.clone(),
                    id: id.clone(),
                    capture_directory: capture.directory().to_owned(),
                });
                break;
            }
        }
    }
    Ok(aliases)
}

#[cfg(test)]
mod tests;
