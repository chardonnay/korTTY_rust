//! Count and confirm replacements in a bound journal.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use iced::widget::{
    self, button, checkbox, column, container, row, scrollable, space, text, text_input,
};
use iced::{Alignment, Element, Length, Size, Task, window};
use zeroize::{Zeroize, Zeroizing};

use crate::session_journal::Status;
use crate::session_journal::replace::{self, MAX_RULE_BYTES, Outcome, Preview, Rule};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::super::journal_appearance_access::Target as AppearanceTarget;
use super::{Desktop, Journal, Message, Mode, ViewerSource, Workspace, read_task};

#[cfg(test)]
mod tests;

pub(super) const TITLE: &str = "Search & replace in the journal";

#[derive(Default)]
pub(in crate::workspace) struct Dialogs {
    pub(super) dialogs: HashMap<window::Id, Dialog>,
}

#[derive(Clone, Debug)]
pub(crate) enum Event {
    FromSearch,
    Change(Change),
    Count,
    Counted(u64, Result<Arc<Preview>, String>),
    Apply,
    Applied(Result<Arc<Applied>, String>),
    Dismiss,
    Focus,
}

#[derive(Clone)]
pub(crate) enum Change {
    Search(Zeroizing<String>),
    Replacement(Zeroizing<String>),
    Regex(bool),
    IgnoreCase(bool),
    IncludeLog(bool),
}

impl fmt::Debug for Change {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::Search(_) => "Search([redacted])",
            Self::Replacement(_) => "Replacement([redacted])",
            Self::Regex(_) => "Regex",
            Self::IgnoreCase(_) => "IgnoreCase",
            Self::IncludeLog(_) => "IncludeLog",
        })
    }
}

pub(super) struct Dialog {
    pub(super) owner: window::Id,
    pub(super) source: window::Id,
    target: ViewerSource,
    search_input: widget::Id,
    managed: bool,
    phase: Phase,
}

enum Phase {
    Form(Form),
    Applying,
    Report(String),
}

struct Form {
    search: Zeroizing<String>,
    replacement: Zeroizing<String>,
    regex: bool,
    ignore_case: bool,
    include_log: bool,
    revision: u64,
    counting: Option<Count>,
    reviewed: Option<Arc<Preview>>,
    problem: Option<String>,
}

struct Count {
    revision: u64,
    cancelled: Arc<AtomicBool>,
}

impl Drop for Count {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

pub(crate) struct Applied {
    outcome: Outcome,
    document: Option<Arc<Journal>>,
}

impl fmt::Debug for Applied {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("JournalReplacementApplied")
            .finish_non_exhaustive()
    }
}

impl Form {
    fn new(search: &str) -> Self {
        Self {
            search: Zeroizing::new(search.to_owned()),
            replacement: Zeroizing::new("***".into()),
            regex: false,
            ignore_case: false,
            include_log: true,
            revision: 0,
            counting: None,
            reviewed: None,
            problem: None,
        }
    }

    fn change(&mut self, change: Change) {
        self.revision = self.revision.wrapping_add(1);
        self.counting = None;
        self.reviewed = None;
        self.problem = None;
        match change {
            Change::Search(value) if value.len() <= MAX_RULE_BYTES => self.search = value,
            Change::Replacement(value) if value.len() <= MAX_RULE_BYTES => self.replacement = value,
            Change::Regex(value) => self.regex = value,
            Change::IgnoreCase(value) => self.ignore_case = value,
            Change::IncludeLog(value) => self.include_log = value,
            _ => {
                self.problem = Some(format!(
                    "The search and replacement fields each allow up to {MAX_RULE_BYTES} bytes."
                ))
            }
        }
    }

    fn can_count(&self) -> bool {
        self.counting.is_none() && !self.search.is_empty()
    }

    fn applicable(&self) -> Option<&Arc<Preview>> {
        self.reviewed.as_ref().filter(|preview| {
            let counts = preview.counts();
            counts.fields != 0 || counts.records != 0
        })
    }
}

impl Dialog {
    pub(super) fn view(&self, blocker: Option<&'static str>) -> Element<'_, Event> {
        let (body, dismiss, apply) = match &self.phase {
            Phase::Form(form) => (
                self.form(form, blocker),
                Some("Cancel"),
                blocker.is_none() && form.applicable().is_some(),
            ),
            Phase::Applying => (column![text("Replacing journal text…")], None, false),
            Phase::Report(status) => (column![text(status)], Some("Close"), false),
        };
        container(
            column![
                scrollable(body.spacing(8)).height(Length::Fill),
                row![
                    space::horizontal(),
                    dismiss.map(|label| button(label).on_press(Event::Dismiss)),
                    matches!(self.phase, Phase::Form(_)).then(|| {
                        button("Replace")
                            .style(button::danger)
                            .on_press_maybe(apply.then_some(Event::Apply))
                    }),
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

    fn form<'a>(
        &self,
        form: &'a Form,
        blocker: Option<&'static str>,
    ) -> iced::widget::Column<'a, Event> {
        let status = if form.counting.is_some() {
            Some("Counting matches…".into())
        } else if let Some(preview) = &form.reviewed {
            let counts = preview.counts();
            Some(format!(
                "{} journal fields and {} capture records would change.",
                counts.fields, counts.records
            ))
        } else {
            form.problem.clone()
        };
        column![
            text("Replace text throughout the journal").size(19),
            text("Search for").size(13),
            text_input("Text or regular expression", &form.search)
                .id(self.search_input.clone())
                .on_input(|value| Event::Change(Change::Search(Zeroizing::new(value)))),
            text("Replace with").size(13),
            text_input("Replacement text", &form.replacement)
                .on_input(|value| Event::Change(Change::Replacement(Zeroizing::new(value)))),
            checkbox(form.regex)
                .label("Regular expression")
                .on_toggle(|value| Event::Change(Change::Regex(value))),
            checkbox(form.ignore_case)
                .label("Ignore case")
                .on_toggle(|value| Event::Change(Change::IgnoreCase(value))),
            checkbox(form.include_log)
                .label("Rewrite the capture log as well")
                .on_toggle(|value| Event::Change(Change::IncludeLog(value))),
            row![
                button("Count matches").on_press_maybe(form.can_count().then_some(Event::Count)),
                status.map(|value| text(value).size(13)),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .wrap(),
            text("Applying a replacement permanently rewrites the matching journal files.")
                .size(13),
            blocker.map(|message| text(message).size(13)),
            self.managed.then(|| text(
                "This installation also applies managed redaction rules during capture."
            )
            .size(13)),
        ]
    }
}

impl Workspace {
    pub(super) fn journal_replacement_in_progress(&self, target: &ViewerSource) -> bool {
        self.journal_replacements.dialogs.values().any(|dialog| {
            matches!(dialog.phase, Phase::Applying) && target.same_journal(&dialog.target)
        })
    }

    pub(super) fn offer_journal_replacement(
        &mut self,
        source: window::Id,
        from_search: bool,
    ) -> Task<Desktop> {
        if !self.entry_interactive(source)
            || self
                .journal_appearance_page(AppearanceTarget::Viewer(source))
                .open
        {
            return Task::none();
        }
        let Some(owner) = self.visible_journal_surface_owner(source) else {
            return Task::none();
        };
        let Some(Mode::Viewer(viewer)) = self.journals.get(&source).map(|surface| &surface.mode)
        else {
            return Task::none();
        };
        if viewer.capture_shown || !viewer.entry_flow.input_enabled() {
            return Task::none();
        }
        let search = if from_search {
            let Some(query) = viewer.navigation.replace_query() else {
                return Task::none();
            };
            query
        } else if viewer.entry_editor.is_open() {
            ""
        } else {
            return Task::none();
        };
        let dialog = Dialog {
            owner,
            source,
            target: viewer.source.clone(),
            search_input: widget::Id::unique(),
            managed: self.clipboard.policy.terminal_logging.has_redactions(),
            phase: Phase::Form(Form::new(search)),
        };
        let search_input = dialog.search_input.clone();
        let (id, open) = self.open_placed_dialog(
            DialogKind::JournalReplace,
            window::Settings {
                size: Size::new(530.0, 460.0),
                min_size: Some(Size::new(350.0, 320.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.journal_replacements.dialogs.insert(id, dialog);
        open.discard().chain(widget::operation::focus(search_input))
    }

    fn journal_replacement_owner_current(&self, id: window::Id, dialog: &Dialog) -> bool {
        self.journal_allowed()
            && !self.exiting
            && !self.scheduler_draining
            && self.visible_journal_surface_owner(dialog.source) == Some(dialog.owner)
            && self.journal_edit_for_owner(dialog.source) == Some(id)
            && !self.hosted_close_locks(dialog.source)
            && self.journals.get(&dialog.source).is_some_and(|surface| {
                matches!(&surface.mode, Mode::Viewer(viewer) if viewer.source.same_journal(&dialog.target))
            })
    }

    fn journal_replacement_capture_is_live(&self, source: &ViewerSource) -> bool {
        self.journal_metadata_captures().iter().any(|capture| {
            matches!(capture.status(), Status::Recording | Status::Finishing)
                && ViewerSource::capture(capture).same_journal(source)
        })
    }

    pub(super) fn journal_replacement_blocker(
        &self,
        id: window::Id,
        dialog: &Dialog,
    ) -> Option<&'static str> {
        if !self.journal_replacement_owner_current(id, dialog) {
            return Some(
                "This journal or its permissions changed. Close the dialog and try again.",
            );
        }
        if self.journal_replacement_capture_is_live(&dialog.target) {
            return Some("Stop the journal capture before applying a replacement.");
        }
        self.journals.values().any(|surface| {
            matches!(&surface.mode, Mode::Viewer(viewer) if viewer.source.same_journal(&dialog.target)
                && (viewer.entry_editor.is_dirty() || viewer.entry_flow.saving()))
        }).then_some("Save or revert open entry changes before applying a replacement.")
    }

    pub(super) fn update_journal_replacement(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Desktop> {
        match event {
            Event::FromSearch => self.offer_journal_replacement(id, true),
            Event::Count => self.count_journal_replacements(id),
            Event::Apply => self.apply_journal_replacements(id),
            Event::Applied(result) => self.journal_replacements_applied(id, result),
            Event::Dismiss => self
                .close_journal_replacement(id)
                .unwrap_or_else(Task::none),
            Event::Focus if self.journal_replacements.dialogs.contains_key(&id) => {
                window::gain_focus(id)
            }
            event => {
                if let Some(Dialog {
                    phase: Phase::Form(form),
                    ..
                }) = self.journal_replacements.dialogs.get_mut(&id)
                {
                    match event {
                        Event::Change(change) => form.change(change),
                        Event::Counted(revision, result)
                            if form
                                .counting
                                .as_ref()
                                .is_some_and(|count| count.revision == revision) =>
                        {
                            form.counting = None;
                            match result {
                                Ok(preview) => form.reviewed = Some(preview),
                                Err(problem) => form.problem = Some(problem),
                            }
                        }
                        _ => {}
                    }
                }
                Task::none()
            }
        }
    }

    fn count_journal_replacements(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(dialog) = self.journal_replacements.dialogs.get(&id) else {
            return Task::none();
        };
        if !self.journal_replacement_owner_current(id, dialog) {
            return Task::none();
        }
        let live = self.journal_replacement_capture_is_live(&dialog.target);
        let Some(Dialog {
            phase: Phase::Form(form),
            target,
            ..
        }) = self.journal_replacements.dialogs.get_mut(&id)
        else {
            return Task::none();
        };
        if !form.can_count() {
            return Task::none();
        }
        form.reviewed = None;
        form.problem = None;
        form.revision = form.revision.wrapping_add(1);
        let revision = form.revision;
        let cancelled = Arc::new(AtomicBool::new(false));
        form.counting = Some(Count {
            revision,
            cancelled: Arc::clone(&cancelled),
        });
        let target = target.clone();
        let search = form.search.clone();
        let replacement = form.replacement.clone();
        let regex = form.regex;
        let ignore_case = form.ignore_case;
        let include_log = form.include_log;
        read_task(
            id,
            move || {
                let rule = Rule::compile(&search, &replacement, regex, ignore_case)?;
                replace::preview(&target, rule, include_log, live, || {
                    cancelled.load(Ordering::Relaxed)
                })
            },
            move |result| Message::Replace(Event::Counted(revision, result)),
        )
    }

    fn apply_journal_replacements(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(dialog) = self.journal_replacements.dialogs.get(&id) else {
            return Task::none();
        };
        let Phase::Form(form) = &dialog.phase else {
            return Task::none();
        };
        let Some(preview) = form.applicable() else {
            return Task::none();
        };
        if self.journal_replacement_blocker(id, dialog).is_some()
            || !dialog.target.same_journal(preview.source())
        {
            return Task::none();
        }
        let preview = Arc::clone(preview);
        let target = preview.source();
        for surface in self.journals.values_mut() {
            if let Mode::Viewer(viewer) = &mut surface.mode
                && viewer.source.same_journal(target)
            {
                viewer.entry_flow.set_replacing(true);
            }
        }
        self.journal_replacements
            .dialogs
            .get_mut(&id)
            .unwrap()
            .phase = Phase::Applying;

        let (send, receive) = async_channel::bounded(1);
        let (finished, done) = async_channel::bounded(1);
        let worker = thread::Builder::new()
            .name("kortty-journal-replace".into())
            .spawn(move || {
                let result = replace::apply(&preview).map(|outcome| {
                    let document = if outcome.published > 0 {
                        preview.source().read(None).ok().flatten().map(Arc::new)
                    } else {
                        None
                    };
                    Arc::new(Applied { outcome, document })
                });
                let _ = send.try_send(result.map_err(|error| error.to_string()));
                drop(finished);
            });
        if let Err(error) = worker {
            return self.journal_replacements_applied(id, Err(error.to_string()));
        }
        self.track_session_shutdown(done);
        Task::future(async move {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("Journal replacement worker stopped.".into()));
            Desktop::SessionJournal(id, Message::Replace(Event::Applied(result)))
        })
    }

    fn journal_replacements_applied(
        &mut self,
        id: window::Id,
        result: Result<Arc<Applied>, String>,
    ) -> Task<Desktop> {
        let Some(dialog) = self.journal_replacements.dialogs.get_mut(&id) else {
            return Task::none();
        };
        if !matches!(dialog.phase, Phase::Applying) {
            return Task::none();
        }
        let status = match &result {
            Ok(applied) => applied.status(),
            Err(error) => error.clone(),
        };
        dialog.phase = Phase::Report(status.clone());
        let source = dialog.target.clone();
        let published = result
            .as_ref()
            .ok()
            .filter(|applied| applied.outcome.published > 0);
        let allowed = self.journal_allowed();
        let mut tasks = Vec::new();
        let mut affected = Vec::new();
        for (&viewer_id, surface) in &mut self.journals {
            let Mode::Viewer(viewer) = &mut surface.mode else {
                continue;
            };
            if !viewer.source.same_journal(&source) {
                continue;
            }
            viewer.entry_flow.set_replacing(false);
            affected.push(viewer_id);
            if let Some(applied) = published {
                viewer.entry_flow.reset_after_replace(&status);
                viewer.entry_editor.clear_selection();
                viewer.navigation.reset_after_replace();
                viewer.refresh.invalidate();
                viewer.query.zeroize();
                viewer.search.zeroize();
                viewer.capture = None;
                viewer.page_revision = viewer.page_revision.wrapping_add(1);
                viewer.screenshot_request = None;
                viewer.screenshot_menu = None;
                viewer.loading = false;
                if let Some(document) = applied.document.as_ref().filter(|_| allowed) {
                    let mut document = Arc::clone(document);
                    if document.summary.directory != viewer.source.directory() {
                        Arc::make_mut(&mut document).summary.directory =
                            viewer.source.directory().to_owned();
                    }
                    tasks.push(viewer.replace_timeline(viewer_id, document));
                    if viewer.capture_shown && viewer.screenshot.is_none() {
                        tasks.push(viewer.restart_capture(viewer_id));
                    }
                } else {
                    viewer.document = None;
                    viewer.capture_shown = false;
                    viewer.screenshot = None;
                    viewer.export_range = None;
                    viewer.selected_entry = None;
                }
            }
        }
        if published.is_some() {
            if let Some(root) = source.directory().parent() {
                tasks.push(self.refresh_journal_action_managers(root));
            }
            tasks.push(self.refresh_journal_entry_docks(&source));
        }
        for viewer in affected {
            tasks.push(self.resume_hosted_tool_close(viewer));
        }
        Task::batch(tasks)
    }

    pub(super) fn close_journal_replacement(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        let dialog = self.journal_replacements.dialogs.get(&id)?;
        if matches!(dialog.phase, Phase::Applying) {
            return Some(Task::none());
        }
        let focus = self
            .hosted_tool_owner(dialog.source)
            .unwrap_or(dialog.source);
        self.journal_replacements.dialogs.remove(&id);
        Some(
            self.close_placed_dialog(id)
                .chain(window::gain_focus(focus)),
        )
    }

    pub(super) fn remove_journal_replacements(&mut self, owner: window::Id) -> Task<Desktop> {
        let mut close = Vec::new();
        self.journal_replacements.dialogs.retain(|&id, dialog| {
            let keep = dialog.owner != owner && dialog.source != owner;
            if !keep {
                close.push(id);
            }
            keep
        });
        Task::batch(close.into_iter().map(|id| self.close_placed_dialog(id)))
    }
}

impl Applied {
    fn status(&self) -> String {
        let outcome = &self.outcome;
        let mut status = if outcome.pending > 0 {
            format!(
                "Replacement stopped after updating {} files. {} files still need changes. Count again before retrying.",
                outcome.published, outcome.pending
            )
        } else {
            format!(
                "Replaced text in {} journal fields and {} capture records.",
                outcome.counts.fields, outcome.counts.records
            )
        };
        if outcome.durability_warning {
            status.push_str(" Storage could not confirm that all updates are durable.");
        }
        if outcome.cleanup_warning {
            status.push_str(" Storage could not remove all private staging files.");
        }
        if self.document.is_none() && outcome.published > 0 {
            status.push_str(" Refresh the viewer to reload the journal.");
        }
        status
    }
}
