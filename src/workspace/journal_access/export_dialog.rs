//! Journal export options, destination selection and background publication.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Element, Length, Size, Task, window};
use jiff::Timestamp;

use crate::local_ai;
use crate::session_journal::Summary;
use crate::session_journal::deletion::DeletionIdentity;
use crate::session_journal::export::{self, Snapshot, TopicSelection, topic_ai};
use crate::session_journal::export_output::{self, Format, Protection, Published, Request};
use crate::session_journal::topic_ai_settings;
use crate::terminal_logging::{Redactor, Secrets};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Desktop, Message as JournalMessage, Mode, Workspace, display, export_options, label};

pub(in crate::workspace) mod browser;
pub(super) mod ranges;
#[cfg(test)]
mod tests;

#[derive(Default)]
pub(in crate::workspace) struct Exports {
    pub(super) dialogs: HashMap<window::Id, Dialog>,
}

impl Exports {
    pub(in crate::workspace) fn ai_active(&self) -> bool {
        self.dialogs.values().any(|dialog| {
            matches!(&dialog.phase, Phase::Destination(save)
                if save.working.as_ref().is_some_and(|work| work.ai_cancel.is_some()))
        })
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Open(Format),
    OpenBrowser,
    BeginRange,
    Range(u64, ranges::Event),
    Options(export_options::Event),
    Previewed(Result<Arc<PreviewData>, String>),
    Browser(browser::Event),
    Saved(u64, Result<Arc<Published>, String>),
    OpenDocument,
    DocumentOpened(Result<(), String>),
    OpenFolder,
    FolderOpened(bool),
    Focus,
    Close,
}

pub struct PreviewData {
    snapshots: Arc<[Snapshot]>,
    skipped: usize,
}

impl fmt::Debug for PreviewData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExportPreview")
            .field("loaded", &self.snapshots.len())
            .field("skipped", &self.skipped)
            .finish()
    }
}

pub(super) struct Dialog {
    pub(super) source: window::Id,
    pub(super) owner: window::Id,
    root: PathBuf,
    targets: Arc<[Target]>,
    open_in_browser: bool,
    ai_profile: Option<local_ai::Profile>,
    preview_cancelled: Arc<AtomicBool>,
    phase: Phase,
    problem: Option<String>,
}

enum Phase {
    Options(Box<export_options::Draft>),
    Destination(Box<Save>),
    Result(Arc<Published>),
}

struct Save {
    browser: browser::Browser,
    format: Format,
    options: export::Options,
    protection: Protection,
    ai_topic: bool,
    working: Option<Work>,
}

struct Work {
    ticket: u64,
    cancelled: Arc<AtomicBool>,
    ai_cancel: Option<local_ai::Cancel>,
}

impl Work {
    fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        drop(self.ai_cancel.take());
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        self.cancel();
    }
}

struct Target {
    directory: PathBuf,
    id: String,
    title: String,
    identity: Option<DeletionIdentity>,
}

impl Target {
    fn from_summary(summary: &Summary) -> Self {
        Self {
            directory: summary.directory.clone(),
            id: summary.id.clone(),
            title: label(&summary.title, 160),
            identity: summary.deletion_identity.clone(),
        }
    }

    fn identity(&self) -> io::Result<&DeletionIdentity> {
        self.identity.as_ref().ok_or_else(|| {
            io::Error::other("The journal could not be verified. Refresh the journal list.")
        })
    }

    fn load(&self, root: &Path, format: Format) -> io::Result<Snapshot> {
        let loader = match format {
            Format::BrowserHtml => export::load_for_browser,
            Format::HtmlBundle => export::load_for_bundle,
            _ => export::load,
        };
        loader(root, &self.directory, &self.id, self.identity()?)
    }

    fn require_browser_source(&self, root: &Path) -> io::Result<()> {
        export::require_reviewed_source(root, &self.directory, self.identity()?)
    }
}

fn require_browser_policy(root: &Path) -> io::Result<()> {
    let policy = crate::clipboard_policy::ClipboardPolicy::load();
    if policy.problem.is_some()
        || !policy.session_journal_allowed
        || policy
            .terminal_logging
            .journal_directory()
            .is_some_and(|path| path != root)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Session Journal access changed. Open the journal again.",
        ));
    }
    Ok(())
}

impl Drop for Dialog {
    fn drop(&mut self) {
        self.preview_cancelled.store(true, Ordering::Relaxed);
    }
}

impl Dialog {
    pub(super) fn title(&self) -> &'static str {
        if self.open_in_browser {
            return "Open Session Journal in Browser";
        }
        match self.phase {
            Phase::Options(_) => "Export Session Journal",
            Phase::Destination(_) => "Save Session Journal",
            Phase::Result(_) => "Journal Export",
        }
    }

    pub(super) fn view(&self) -> Element<'_, Message> {
        match &self.phase {
            Phase::Options(options) => column![
                self.problem
                    .as_deref()
                    .map(
                        |problem| container(text(display(problem, 8192)).size(13)).padding([6, 16])
                    ),
                options.view().map(Message::Options),
            ]
            .into(),
            Phase::Destination(save) if save.working.is_none() => {
                let browser = if self.open_in_browser {
                    save.browser.view_with_notice(
                        "Save a Local Journal Page",
                        "The HTML file is unencrypted and can contain captured terminal text, screenshots, and agent conversations. Choose where to save it before opening it in your browser.",
                    )
                } else {
                    save.browser.view()
                };
                browser.map(Message::Browser)
            }
            Phase::Destination(save) => {
                let cancelling = save
                    .working
                    .as_ref()
                    .is_some_and(|work| work.cancelled.load(Ordering::Relaxed));
                container(
                    column![
                        text(if cancelling {
                            "Cancelling export…"
                        } else {
                            "Exporting session journals…"
                        })
                        .size(18),
                        button("Cancel Export")
                            .on_press_maybe((!cancelling).then_some(Message::Close)),
                    ]
                    .spacing(12)
                    .align_x(iced::Alignment::Center),
                )
                .center(Length::Fill)
                .into()
            }
            Phase::Result(result) => {
                let mut details = column![
                    if self.open_in_browser {
                        text("Review the local file before opening it in your browser").size(19)
                    } else {
                        text(format!("Exported {} journal(s)", result.exported)).size(19)
                    },
                    text(display(&result.path.to_string_lossy(), 4096)).size(14),
                ]
                .spacing(8);
                if result.skipped_assets != 0 {
                    details = details.push(text(format!(
                        "{} screenshot(s) could not be included.",
                        result.skipped_assets
                    )));
                }
                if result.incomplete_capture_journals != 0 {
                    details = details.push(text(format!(
                        "{} journal(s) had missing or incomplete capture data.",
                        result.incomplete_capture_journals
                    )));
                }
                for problem in &result.skipped {
                    details = details.push(text(display(problem, 8192)).size(13));
                }
                for notice in &result.topic_notices {
                    details = details.push(text(display(notice, 8192)).size(13));
                }
                if let Some(warning) = &result.warning {
                    details = details.push(text(display(warning, 8192)).size(13));
                }
                if let Some(problem) = &self.problem {
                    details = details.push(text(display(problem, 8192)).size(13));
                }
                container(
                    column![
                        scrollable(details).height(Length::Fill),
                        row![
                            space::horizontal(),
                            result
                                .document
                                .as_ref()
                                .map(|_| button(if self.open_in_browser {
                                    "Open in Browser"
                                } else {
                                    "Open Export"
                                })
                                .on_press(Message::OpenDocument)),
                            (!self.open_in_browser)
                                .then(|| button("Open Folder").on_press(Message::OpenFolder)),
                            button("Close").on_press(Message::Close),
                        ]
                        .spacing(8),
                    ]
                    .spacing(10),
                )
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(16)
                .into()
            }
        }
    }
}

impl Workspace {
    fn journal_export_ai_profile(&self) -> Option<local_ai::Profile> {
        (self.ai_features_enabled() && topic_ai_settings::allowed(&self.clipboard.policy))
            .then(|| self.configured_journal_ai(false))
            .flatten()
    }

    pub(in crate::workspace) fn journal_options_modal(
        &self,
        window: window::Id,
    ) -> Option<window::Id> {
        self.journal_exports
            .dialogs
            .iter()
            .find_map(|(&id, dialog)| {
                (id != window && matches!(dialog.phase, Phase::Options(_))).then_some(id)
            })
    }

    pub(in crate::workspace) fn cancel_journal_export_ai(&mut self) {
        for dialog in self.journal_exports.dialogs.values_mut() {
            if let Phase::Destination(save) = &mut dialog.phase
                && let Some(work) = save.working.as_mut()
            {
                work.ai_cancel = None;
            }
        }
    }

    fn journal_export_owner(&self, source: window::Id) -> Option<window::Id> {
        if self.exiting
            || self.scheduler_draining
            || !self.journal_allowed()
            || self.journal_edit_for_owner(source).is_some()
        {
            return None;
        }
        let owner = self.visible_journal_surface_owner(source)?;
        (self.palette_can_open(owner) && !self.palette.is_open(owner)).then_some(owner)
    }

    fn open_journal_export(
        &mut self,
        source: window::Id,
        format: Format,
        preset: &[(Timestamp, Timestamp)],
    ) -> Task<Desktop> {
        let Some(owner) = self.journal_export_owner(source) else {
            return Task::none();
        };
        let (root, targets, can_pick_range): (_, Arc<[Target]>, _) =
            match &self.journals[&source].mode {
                Mode::Manager(manager) if !manager.loading => {
                    let Some((root, catalog)) = manager.root.as_ref().zip(manager.catalog.as_ref())
                    else {
                        return Task::none();
                    };
                    let targets = manager
                        .tree
                        .selected_indices(catalog)
                        .into_iter()
                        .map(|index| Target::from_summary(&catalog.journals[index]))
                        .collect::<Vec<_>>();
                    (root.clone(), targets.into(), false)
                }
                Mode::Viewer(viewer) if viewer.export_range.is_none() => {
                    let Some(summary) = viewer.export_summary() else {
                        return Task::none();
                    };
                    let Some(root) = summary.directory.parent() else {
                        return Task::none();
                    };
                    (
                        root.to_owned(),
                        Arc::new([Target::from_summary(summary)]),
                        viewer.can_pick_export_range(),
                    )
                }
                _ => return Task::none(),
            };
        if targets.is_empty() {
            return Task::none();
        }
        self.dismiss_journal_menu(source);
        let mut options =
            export_options::Draft::new(format, targets.len(), self.backup_recipient_choices());
        let ai_profile = self.journal_export_ai_profile();
        options.set_topic_ai_available(ai_profile.is_some());
        options.set_viewer_ranges(can_pick_range, preset);
        let (id, opened) = self.open_journal_export_window();
        let cancelled = Arc::new(AtomicBool::new(false));
        let preview = if options.wants_preview() {
            let targets = Arc::clone(&targets);
            let root = root.clone();
            let cancelled = Arc::clone(&cancelled);
            super::read_task(
                id,
                move || load_preview(&root, &targets, &cancelled),
                |result| JournalMessage::Export(Message::Previewed(result)),
            )
        } else {
            Task::none()
        };
        self.journal_exports.dialogs.insert(
            id,
            Dialog {
                source,
                owner,
                root,
                targets,
                open_in_browser: false,
                ai_profile,
                preview_cancelled: cancelled,
                phase: Phase::Options(Box::new(options)),
                problem: None,
            },
        );
        Task::batch([opened.map(|_| Desktop::Noop), preview])
    }

    fn open_journal_in_browser(&mut self, source: window::Id) -> Task<Desktop> {
        let Some(owner) = self.journal_export_owner(source) else {
            return Task::none();
        };
        let Mode::Viewer(viewer) = &self.journals[&source].mode else {
            return Task::none();
        };
        let Some(summary) = viewer
            .export_summary()
            .filter(|_| viewer.export_range.is_none())
        else {
            return Task::none();
        };
        let Some(root) = summary.directory.parent().map(Path::to_owned) else {
            return Task::none();
        };
        let target = Target::from_summary(summary);
        let filename = format!("{}.html", export_output::export_basename(&target.title));
        self.dismiss_journal_menu(source);
        let (id, opened) = self.open_journal_export_window();
        let mut browser = browser::Browser::new(filename, ".html");
        let listing = browser.update(id, browser::Event::List);
        self.journal_exports.dialogs.insert(
            id,
            Dialog {
                source,
                owner,
                root,
                targets: Arc::new([target]),
                open_in_browser: true,
                ai_profile: None,
                preview_cancelled: Arc::new(AtomicBool::new(false)),
                phase: Phase::Destination(Box::new(Save {
                    browser,
                    format: Format::BrowserHtml,
                    options: export::Options::default(),
                    protection: Protection::None,
                    ai_topic: false,
                    working: None,
                })),
                problem: None,
            },
        );
        Task::batch([opened.map(|_| Desktop::Noop), listing])
    }

    fn open_journal_export_window(&mut self) -> (window::Id, Task<window::Id>) {
        self.open_placed_dialog(
            DialogKind::JournalExport,
            window::Settings {
                size: Size::new(730.0, 650.0),
                min_size: Some(Size::new(480.0, 380.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        )
    }

    fn journal_export_scope_is_current(&self, id: window::Id) -> bool {
        if self.exiting
            || self.scheduler_draining
            || !self.journal_allowed()
            || self.journal_options_modal(id).is_some()
        {
            return false;
        }
        let Some(dialog) = self.journal_exports.dialogs.get(&id) else {
            return false;
        };
        if self.visible_journal_surface_owner(dialog.source) != Some(dialog.owner) {
            return false;
        }
        match &self.journals[&dialog.source].mode {
            Mode::Manager(manager) => {
                manager.root.as_ref() == Some(&dialog.root)
                    && ((!manager.inherited && manager.locked_root.is_none())
                        || self.journal_directory().ok().as_ref() == Some(&dialog.root))
            }
            Mode::Viewer(viewer) => {
                viewer.document.as_ref().is_some_and(|document| {
                    let target = &dialog.targets[0];
                    document.summary.id == target.id
                        && document.summary.directory == target.directory
                        && (!dialog.open_in_browser
                            || (!viewer.loading
                                && viewer.export_range.is_none()
                                && viewer.screenshot.is_none()
                                && document.summary.deletion_identity == target.identity))
                }) && (self
                    .clipboard
                    .policy
                    .terminal_logging
                    .journal_directory()
                    .is_none()
                    || self.journal_directory().ok().as_ref() == Some(&dialog.root))
            }
        }
    }

    pub(super) fn update_journal_export(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        match message {
            Message::Open(format) if format != Format::BrowserHtml => {
                return self.open_journal_export(id, format, &[]);
            }
            Message::OpenBrowser => return self.open_journal_in_browser(id),
            Message::BeginRange => {
                if self.journal_export_owner(id).is_some() {
                    self.begin_journal_export_range(id, Format::Pdf);
                }
                return Task::none();
            }
            Message::Range(ticket, event) => {
                return self.update_journal_export_ranges(id, ticket, event);
            }
            Message::Options(export_options::Event::PickRange) => {
                return self.pick_journal_export_ranges(id);
            }
            Message::Close => {
                return self.close_journal_export(id).unwrap_or_else(Task::none);
            }
            Message::Options(export_options::Event::Confirm) => {
                return self.choose_journal_export_destination(id);
            }
            Message::Browser(browser::Event::Save) => return self.save_journal_export(id),
            Message::OpenDocument => return self.open_journal_export_document(id),
            Message::OpenFolder => return self.open_journal_export_folder(id),
            _ => {}
        }
        let Some(dialog) = self.journal_exports.dialogs.get_mut(&id) else {
            return Task::none();
        };
        match (&mut dialog.phase, message) {
            (_, Message::Focus) => window::gain_focus(id),
            (Phase::Options(_), Message::Options(export_options::Event::Cancel))
            | (Phase::Destination(_), Message::Browser(browser::Event::Cancel)) => {
                self.close_journal_export(id).unwrap_or_else(Task::none)
            }
            (Phase::Options(options), Message::Options(event)) => options
                .update(event)
                .map(move |event| wrap(id, Message::Options(event))),
            (Phase::Options(options), Message::Previewed(result)) => {
                let result = result.map(|preview| {
                    if preview.skipped > 0 {
                        dialog.problem = Some(format!(
                            "{} journal(s) could not be loaded for preview.",
                            preview.skipped
                        ));
                    }
                    Arc::clone(&preview.snapshots)
                });
                options
                    .set_preview(result)
                    .map(move |event| wrap(id, Message::Options(event)))
            }
            (Phase::Destination(save), Message::Browser(event)) if save.working.is_none() => {
                save.browser.update(id, event)
            }
            (Phase::Destination(save), Message::Saved(ticket, result))
                if save
                    .working
                    .as_ref()
                    .is_some_and(|work| work.ticket == ticket) =>
            {
                save.working = None;
                match result {
                    Ok(result) => {
                        dialog.problem = None;
                        dialog.phase = Phase::Result(result);
                    }
                    Err(problem) => save.browser.set_problem(problem),
                }
                Task::none()
            }
            (Phase::Result(_), Message::FolderOpened(opened)) => {
                dialog.problem =
                    (!opened).then(|| "The desktop could not open the export folder.".into());
                Task::none()
            }
            (Phase::Result(_), Message::DocumentOpened(result)) => {
                dialog.problem = result.err();
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn pick_journal_export_ranges(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(dialog) = self.journal_exports.dialogs.get(&id) else {
            return Task::none();
        };
        let Phase::Options(options) = &dialog.phase else {
            return Task::none();
        };
        let source = dialog.source;
        let format = options.format();
        if !self.journal_export_scope_is_current(id)
            || !self.begin_journal_export_range(source, format)
        {
            return Task::none();
        }
        self.close_journal_export(id).unwrap_or_else(Task::none)
    }

    fn begin_journal_export_range(&mut self, source: window::Id, format: Format) -> bool {
        let ticket = self.allocate();
        let Some(Mode::Viewer(viewer)) = self
            .journals
            .get_mut(&source)
            .map(|surface| &mut surface.mode)
        else {
            return false;
        };
        if viewer.export_range.is_some()
            || viewer.export_summary().is_none()
            || !viewer.can_pick_export_range()
        {
            return false;
        }
        let mut picker = ranges::Picker::new(ticket, format);
        picker.page = viewer
            .selected_entry
            .map_or(0, |index| index / super::PAGE_ROWS);
        viewer.export_range = Some(picker);
        viewer.capture_shown = false;
        true
    }

    fn update_journal_export_ranges(
        &mut self,
        source: window::Id,
        ticket: u64,
        event: ranges::Event,
    ) -> Task<Desktop> {
        if self.journal_export_owner(source).is_none() {
            return Task::none();
        }
        let Some(Mode::Viewer(viewer)) = self
            .journals
            .get_mut(&source)
            .map(|surface| &mut surface.mode)
        else {
            return Task::none();
        };
        let Some((picker, document)) = viewer.export_range.as_mut().zip(viewer.document.as_ref())
        else {
            return Task::none();
        };
        if picker.ticket != ticket {
            return Task::none();
        }
        match event {
            ranges::Event::Select(index, id) => picker.select(document, index, &id),
            ranges::Event::Add => picker.add(document),
            ranges::Event::Cancel => {
                viewer.export_range = None;
                return viewer.resume_timeline_navigation(true);
            }
            ranges::Event::Apply => {
                let format = picker.format;
                let windows = picker.apply(document);
                if !windows.is_empty() {
                    viewer.export_range = None;
                    let resume = viewer.resume_timeline_navigation(false);
                    return Task::batch([
                        resume,
                        self.open_journal_export(source, format, &windows),
                    ]);
                }
            }
        }
        Task::none()
    }

    fn choose_journal_export_destination(&mut self, id: window::Id) -> Task<Desktop> {
        let current = self.journal_export_scope_is_current(id);
        let ai_profile = self.journal_export_ai_profile();
        let Some(dialog) = self.journal_exports.dialogs.get_mut(&id) else {
            return Task::none();
        };
        let Phase::Options(options) = &dialog.phase else {
            return Task::none();
        };
        if !current {
            dialog.problem =
                Some("The journal folder or permission changed. Reopen the export.".into());
            return Task::none();
        }
        let Some(choice) = options.choice() else {
            return Task::none();
        };
        if choice.ai_topic && (ai_profile.is_none() || dialog.ai_profile != ai_profile) {
            dialog.problem = Some(AI_PROFILE_CHANGED.into());
            return Task::none();
        }
        let protection = match choice.protection {
            export_options::ProtectionChoice::None => Protection::None,
            export_options::ProtectionChoice::Password(password) => Protection::Password(password),
            export_options::ProtectionChoice::OpenPgp(recipient) => match crate::profile::root() {
                Ok(profile) => Protection::OpenPgp { profile, recipient },
                Err(error) => {
                    dialog.problem = Some(error.to_string());
                    return Task::none();
                }
            },
        };
        dialog.preview_cancelled.store(true, Ordering::Relaxed);
        let multiple = dialog.targets.len() > 1;
        let suffix = protection.extension(choice.format, multiple);
        let name = if multiple {
            "session-journals".into()
        } else if matches!(self.journals[&dialog.source].mode, Mode::Viewer(_)) {
            "session-journal".into()
        } else {
            export_output::export_basename(&dialog.targets[0].title)
        };
        let mut browser = browser::Browser::new(format!("{name}{suffix}"), suffix);
        let listing = browser.update(id, browser::Event::List);
        dialog.phase = Phase::Destination(Box::new(Save {
            browser,
            format: choice.format,
            options: choice.options,
            protection,
            ai_topic: choice.ai_topic,
            working: None,
        }));
        listing
    }

    fn save_journal_export(&mut self, id: window::Id) -> Task<Desktop> {
        let current = self.journal_export_scope_is_current(id);
        let links_allowed = self.journal_links_allowed();
        let branding = self.window_settings.export_branding.resolve();
        let ai_profile = self.journal_export_ai_profile();
        let ticket = self.allocate();
        let redaction = &self.clipboard.policy.terminal_logging;
        let Some(dialog) = self.journal_exports.dialogs.get_mut(&id) else {
            return Task::none();
        };
        let Phase::Destination(save) = &mut dialog.phase else {
            return Task::none();
        };
        if save.working.is_some() {
            return Task::none();
        }
        if !current {
            save.browser
                .set_problem("The journal folder or permission changed. Reopen the export.".into());
            return Task::none();
        }
        let destination = match save.browser.destination() {
            Ok(destination) => destination,
            Err(problem) => {
                save.browser.set_problem(problem.to_string());
                return Task::none();
            }
        };
        let (topic_ai, ai_cancel) = if save.ai_topic {
            let Some(profile) =
                ai_profile.filter(|profile| dialog.ai_profile.as_ref() == Some(profile))
            else {
                save.browser.set_problem(AI_PROFILE_CHANGED.into());
                return Task::none();
            };
            let (topic, cancel) =
                begin_topic_selection(profile, &save.protection, redaction.clone());
            (Some(topic), cancel)
        } else {
            (None, None)
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        save.working = Some(Work {
            ticket,
            cancelled: Arc::clone(&cancelled),
            ai_cancel,
        });
        let targets = Arc::clone(&dialog.targets);
        let root = dialog.root.clone();
        let mut options = save.options.clone();
        options.branding = branding;
        let protection = save.protection.clone();
        let format = save.format;
        let (send, receive) = async_channel::bounded(1);
        let (finished, done) = async_channel::bounded(1);
        self.track_session_shutdown(done);
        let worker = std::thread::Builder::new()
            .name("kortty-journal-export".into())
            .spawn(move || {
                let journals = targets
                    .iter()
                    .map(|target| (target.title.clone(), target.load(&root, format)));
                let request = Request {
                    format,
                    options: &options,
                    protection: &protection,
                    links_allowed,
                    topic_ai: topic_ai.as_ref(),
                };
                let cancelled = || cancelled.load(Ordering::Relaxed);
                let result = if format == Format::BrowserHtml {
                    export_output::publish_reviewed(
                        &destination,
                        targets.len(),
                        journals,
                        request,
                        &cancelled,
                        &|| require_browser_policy(&root),
                    )
                } else {
                    export_output::publish(
                        &destination,
                        targets.len(),
                        journals,
                        request,
                        &cancelled,
                    )
                }
                .map(Arc::new)
                .map_err(|error| error.to_string());
                let _ = send.try_send(result);
                drop(finished);
            });
        if let Err(problem) = worker {
            return self
                .update_journal_export(id, Message::Saved(ticket, Err(problem.to_string())));
        }
        Task::future(async move {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("Journal export worker stopped.".into()));
            wrap(id, Message::Saved(ticket, result))
        })
    }

    fn open_journal_export_document(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(Dialog {
            phase: Phase::Result(result),
            open_in_browser,
            root,
            targets,
            ..
        }) = self.journal_exports.dialogs.get(&id)
        else {
            return Task::none();
        };
        let Some(document) = result.document.clone() else {
            return Task::none();
        };
        const CHANGED: &str =
            "Session Journal access changed before the desktop could open the export.";
        if !self.journal_export_scope_is_current(id) {
            return self.update_journal_export(id, Message::DocumentOpened(Err(CHANGED.into())));
        }
        let browser_source = open_in_browser.then(|| (root.clone(), Arc::clone(targets)));
        let open = move || {
            if let Some((root, targets)) = browser_source {
                require_browser_policy(&root).map_err(|error| error.to_string())?;
                targets[0]
                    .require_browser_source(&root)
                    .map_err(|error| error.to_string())?;
                return document.open();
            }
            let policy = crate::clipboard_policy::ClipboardPolicy::load();
            if policy.problem.is_some() || !policy.session_journal_allowed {
                return Err(CHANGED.into());
            }
            document.open()
        };
        #[cfg(target_os = "macos")]
        {
            self.update_journal_export(id, Message::DocumentOpened(open()))
        }
        #[cfg(not(target_os = "macos"))]
        {
            Task::perform(async move { open() }, move |result| {
                wrap(id, Message::DocumentOpened(result))
            })
        }
    }

    fn open_journal_export_folder(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(Dialog {
            phase: Phase::Result(result),
            open_in_browser: false,
            ..
        }) = self.journal_exports.dialogs.get(&id)
        else {
            return Task::none();
        };
        let Some(parent) = result.path.parent() else {
            return Task::none();
        };
        #[cfg(target_os = "macos")]
        {
            let opened = kortty_windowing::open_desktop_directory(parent);
            self.update_journal_export(id, Message::FolderOpened(opened))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let path = parent.to_owned();
            Task::perform(
                async move { kortty_windowing::open_desktop_directory(&path) },
                move |opened| wrap(id, Message::FolderOpened(opened)),
            )
        }
    }

    pub(super) fn close_journal_export(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        let dialog = self.journal_exports.dialogs.get_mut(&id)?;
        if let Phase::Destination(save) = &mut dialog.phase
            && let Some(work) = &mut save.working
        {
            work.cancel();
            return Some(Task::none());
        }
        let source = dialog.source;
        let focus = self.hosted_tool_owner(source).unwrap_or(source);
        self.journal_exports.dialogs.remove(&id);
        Some(
            self.close_placed_dialog(id)
                .chain(window::gain_focus(focus)),
        )
    }

    pub(super) fn remove_journal_exports(&mut self, source: window::Id) -> Task<Desktop> {
        let mut closed = Vec::new();
        self.journal_exports.dialogs.retain(|&id, dialog| {
            let keep = dialog.owner != source && dialog.source != source;
            if !keep {
                closed.push(id);
            }
            keep
        });
        Task::batch(closed.into_iter().map(|id| self.close_placed_dialog(id)))
    }
}

const AI_PROFILE_CHANGED: &str = "The journal AI profile or permission changed. Reopen the export.";

fn begin_topic_selection(
    profile: local_ai::Profile,
    protection: &Protection,
    policy: crate::terminal_logging::Policy,
) -> (TopicSelection, Option<local_ai::Cancel>) {
    let secrets = Secrets::default();
    if let Protection::Password(password) = protection {
        secrets.add(password.as_str());
    }
    let Ok(secrets) = secrets.seal() else {
        return (
            TopicSelection::LocalFallback(topic_ai::Fallback::Redaction),
            None,
        );
    };
    match topic_ai::Run::begin(profile) {
        Ok((run, cancel)) => (
            TopicSelection::Model {
                run: Box::new(run),
                redactor: Redactor::for_journal(secrets, policy),
            },
            Some(cancel),
        ),
        Err(reason) => (TopicSelection::LocalFallback(reason), None),
    }
}

fn load_preview(
    root: &Path,
    targets: &[Target],
    cancelled: &AtomicBool,
) -> io::Result<PreviewData> {
    let mut loaded = Vec::new();
    let mut skipped = 0;
    for target in targets {
        if cancelled.load(Ordering::Relaxed) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        match target.load(root, Format::Markdown) {
            Ok(snapshot) => loaded.push(snapshot),
            Err(_) => skipped += 1,
        }
    }
    if loaded.is_empty() {
        return Err(io::Error::other(
            "None of the selected journals could be read for preview.",
        ));
    }
    Ok(PreviewData {
        snapshots: loaded.into(),
        skipped,
    })
}

fn wrap(id: window::Id, message: Message) -> Desktop {
    Desktop::SessionJournal(id, JournalMessage::Export(message))
}
