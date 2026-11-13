//! Native export options for selected stored analysis records.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_channel::bounded;
use iced::futures::stream;
use iced::widget::{
    button, checkbox, column, container, pick_list, progress_bar, row, scrollable, text, text_input,
};
use iced::{Element, Length, Task, window};

use crate::session_journal::export_output::valid_component;
use crate::snippet_analysis::model::{
    ApplyStatus, ExportEntry, Record, Subject, content_hash, now_millis,
};
use crate::snippet_analysis::report::{
    self, ExportFolder, Format, Item, Options, Outcome, Packing, Post, Progress,
};
use crate::snippet_analysis::store::Store;
use crate::snippet_window::Message as SnippetMessage;
use crate::snippet_window::analysis::Message as AnalysisAction;
use crate::snippets::SnippetId;
use crate::workspace::tab_titles::WindowSettings;

use super::{Event as AnalysisEvent, Message, Workspace, file_task, store};

const MAX_RETAINED_BYTES: usize = 32 * 1024 * 1024;
static OUTPUTS: [Output; 5] = [
    Output::Report(Format::Html),
    Output::Report(Format::Pdf),
    Output::Report(Format::Markdown),
    Output::Report(Format::Json),
    Output::Script,
];
const PACKINGS: [Packing; 2] = [Packing::Combined, Packing::Zip];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Report(Format),
    Script,
}

impl std::fmt::Display for Output {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Report(format) => std::fmt::Display::fmt(format, formatter),
            Self::Script => formatter.write_str("Script (plain text)"),
        }
    }
}

#[derive(Clone)]
pub enum Action {
    Output(Output),
    Packing(Packing),
    Run(Option<SnippetId>),
    IncludeCode(bool),
    Path(String),
    Export,
    ShowInFolder,
    Cancel,
}

#[derive(Clone)]
pub(crate) enum Event {
    Loaded(u64, Result<Arc<Batch>, String>),
    Progress(u64, Progress),
    Exported(u64, Result<Arc<Outcome>, String>),
    FolderRevealed(u64, Result<(), String>),
}

pub(crate) struct Batch {
    items: Vec<Item>,
    skipped: Vec<String>,
    after_record: Option<Record>,
    after: Vec<(RunChoice, Post)>,
}

struct Script<'a> {
    name: &'a str,
    content: &'a str,
}

impl Batch {
    fn script(&self, run: Option<SnippetId>) -> Option<Script<'_>> {
        let [item] = self.items.as_slice() else {
            return None;
        };
        if !matches!(item.subject, Subject::Snippet(_)) {
            return None;
        }
        let (record, content) = if let Some(run) = run {
            self.after
                .iter()
                .find(|(choice, _)| choice.id == Some(run))?;
            let record = self.after_record.as_ref().unwrap_or(&item.record);
            let result = record.apply_runs.iter().find(|result| result.id == run)?;
            (record, result.after.as_str())
        } else {
            (&item.record, item.record.source.content.as_deref()?)
        };
        (!content.is_empty()).then_some(Script {
            name: &record.source.snippet_name,
            content,
        })
    }

    fn export_report(
        &self,
        path: PathBuf,
        options: Options,
        selected_run: Option<SnippetId>,
        receipt_store: Option<io::Result<Store>>,
        cancelled: &AtomicBool,
        progress: impl FnMut(Progress),
    ) -> io::Result<Outcome> {
        let path = report::formatted_destination(path, options.format, options.packing)?;
        let mut items = self.items.clone();
        if let Some(run_id) = selected_run
            && let Some((_, post)) = self
                .after
                .iter()
                .find(|(choice, _)| choice.id == Some(run_id))
        {
            let item = &mut items[0];
            if let Some(record) = &self.after_record {
                item.record = record.clone();
            }
            item.post = Some(post.clone());
        }
        let receipt = receipt_store.and_then(|store| {
            let [item] = items.as_slice() else {
                return None;
            };
            (options.packing == Packing::Single && matches!(item.subject, Subject::Snippet(_)))
                .then(|| {
                    (
                        store,
                        item.subject,
                        item.record.id,
                        item.record.source.sha256.clone(),
                        options.format,
                        item.post.as_ref().map(|post| post.run_id),
                    )
                })
        });
        let mut outcome = report::export_with_progress(path, items, options, cancelled, progress)?;
        if let Some((store, subject, record, source_hash, format, run)) = receipt {
            let recorded = store.and_then(|store| {
                let name = outcome
                    .written_files
                    .first()
                    .and_then(|path| path.file_name()?.to_str())
                    .ok_or_else(|| io::Error::other("The report file name is unavailable"))?;
                store.record_export(
                    subject,
                    record,
                    &source_hash,
                    ExportEntry::new(format, run, name),
                )
            });
            if let Err(error) = recorded {
                let warning = outcome.warning.get_or_insert_with(String::new);
                if !warning.is_empty() {
                    warning.push(' ');
                }
                warning.push_str(&format!(
                    "The export receipt could not be recorded ({error})."
                ));
            }
        }
        Ok(outcome)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RunChoice {
    id: Option<SnippetId>,
    label: String,
}

impl std::fmt::Display for RunChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.label)
    }
}

pub(super) struct Page {
    token: u64,
    packing: Packing,
    output: Output,
    include_code: bool,
    run: Option<SnippetId>,
    path: String,
    batch: Option<Arc<Batch>>,
    cancel: Option<Arc<AtomicBool>>,
    progress: Option<Progress>,
    status: Option<String>,
    published_folder: Option<ExportFolder>,
    revealing_folder: bool,
    reveal_error: Option<String>,
}

impl Page {
    fn new(token: u64, packing: Packing, include_code: bool) -> Self {
        Self {
            token,
            packing,
            output: Output::Report(Format::Html),
            include_code,
            run: None,
            path: String::new(),
            batch: None,
            cancel: None,
            progress: None,
            status: None,
            published_folder: None,
            revealing_folder: false,
            reveal_error: None,
        }
    }

    fn includes_code(&self) -> bool {
        self.include_code && matches!(self.output, Output::Report(format) if format != Format::Json)
    }

    fn script(&self) -> Option<Script<'_>> {
        if self.packing != Packing::Single {
            return None;
        }
        self.batch.as_deref()?.script(self.run)
    }

    fn can_export(&self) -> bool {
        self.batch
            .as_ref()
            .is_some_and(|batch| !batch.items.is_empty())
            && (self.output != Output::Script || self.script().is_some())
    }

    fn show_in_folder(&mut self, id: window::Id) -> Task<Message> {
        let Some(folder) = self
            .published_folder
            .as_ref()
            .filter(|_| !self.revealing_folder)
            .cloned()
        else {
            return Task::none();
        };
        self.revealing_folder = true;
        self.reveal_error = None;
        let token = self.token;
        let finished = move |result| {
            Message::SnippetAnalysis(
                id,
                AnalysisEvent::Reports(Event::FolderRevealed(token, result)),
            )
        };
        // AppKit's desktop opener must be called on the main thread.
        #[cfg(target_os = "macos")]
        {
            Task::done(finished(
                reveal_export_folder(&folder).map_err(|error| error.to_string()),
            ))
        }
        #[cfg(not(target_os = "macos"))]
        {
            file_task(move || reveal_export_folder(&folder), finished)
        }
    }

    fn set_include_code(
        &mut self,
        enabled: bool,
        settings: &mut WindowSettings,
        save: impl FnOnce(&WindowSettings) -> io::Result<()>,
    ) {
        if !matches!(self.output, Output::Report(format) if format != Format::Json) {
            return;
        }
        self.include_code = enabled;
        if self.packing != Packing::Single {
            return;
        }
        if settings.snippet_analysis_export_include_code == enabled {
            self.status = None;
            return;
        }
        let mut next = settings.clone();
        next.snippet_analysis_export_include_code = enabled;
        match save(&next) {
            Ok(()) => {
                *settings = next;
                self.status = None;
            }
            Err(error) => {
                self.status = Some(format!(
                    "Could not save the script export default ({error})."
                ))
            }
        }
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Release);
        }
    }
}

impl Workspace {
    pub(super) fn export_current_snippet_analysis(&mut self, id: window::Id) -> Task<Message> {
        let token = self.allocate();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        if surface.analysis.export.is_some() || surface.transfer.is_some() {
            return Task::none();
        }
        let Some(page) = surface.analysis.current_mut() else {
            return Task::none();
        };
        let Some(record) = page.record().cloned() else {
            return Task::none();
        };
        let subject = page.subject;
        let current_hash = page.hash.clone();
        let selection = page
            .selections
            .get(&record.id)
            .or_else(|| record.apply_runs.first().map(|run| &run.selected))
            .cloned()
            .unwrap_or_default();
        let records = page
            .history
            .as_ref()
            .map_or(&[][..], |history| history.records.as_slice());
        let linked = record
            .verification
            .as_ref()
            .filter(|_| record.apply_runs.is_empty())
            .and_then(|verification| {
                let pre = records
                    .iter()
                    .find(|record| record.id == verification.previous_record_id)?;
                Some((pre, verification.run_id))
            });
        let run_record = linked.map_or(&record, |(pre, _)| pre);
        let after = run_record
            .apply_runs
            .iter()
            .enumerate()
            .filter(|(_, run)| {
                linked.is_none_or(|(_, id)| run.id == id && run.status == ApplyStatus::Accepted)
            })
            .filter_map(|(index, run)| {
                let post = Post::capture(run_record, run.id, records)?;
                let date = jiff::Timestamp::from_millisecond(i64::try_from(run.started_at).ok()?)
                    .ok()?
                    .to_zoned(jiff::tz::TimeZone::system())
                    .strftime("%Y-%m-%d %H:%M")
                    .to_string();
                let number = run_record.apply_runs.len() - index;
                Some((
                    RunChoice {
                        id: Some(run.id),
                        label: format!(
                            "After applying · Run {number} · {date} · {}",
                            run.status.label()
                        ),
                    },
                    post,
                ))
            })
            .collect();
        let mut export = Page::new(
            token,
            Packing::Single,
            self.window_settings.snippet_analysis_export_include_code,
        );
        export.batch = Some(Arc::new(Batch {
            items: vec![Item {
                subject,
                record,
                folder_path: String::new(),
                current_hash,
                selection,
                post: None,
            }],
            skipped: Vec::new(),
            after_record: linked.map(|(pre, _)| pre.clone()),
            after,
        }));
        surface.analysis.export = Some(export);
        Task::none()
    }

    pub(in crate::workspace::snippet_access) fn open_snippet_analysis_reports(
        &mut self,
        id: window::Id,
        subject: Option<Subject>,
    ) -> Task<Message> {
        if self.hosted_close_locks(id) || self.exiting {
            return Task::none();
        }
        let token = self.allocate();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        if surface.analysis.export.is_some()
            || surface.transfer.is_some()
            || !surface.window.prepare_transfer()
        {
            return Task::none();
        }
        let library = surface.window.analysis_library();
        let ids = match subject {
            None => library.entries().iter().map(|entry| entry.id()).collect(),
            Some(Subject::Folder(folder)) => library.snippets_in_folder(Some(folder), true),
            Some(Subject::Snippet(snippet)) => surface.window.analysis_selection(snippet),
        };
        let mut page = Page::new(
            token,
            Packing::Combined,
            self.window_settings.snippet_analysis_export_include_code,
        );
        if ids.is_empty() || ids.len() > report::MAX_REPORTS {
            page.status = Some("Select between 1 and 256 snippets to export their reports.".into());
            surface.analysis.export = Some(page);
            return Task::none();
        }
        let candidates: Vec<_> = ids
            .into_iter()
            .filter_map(|id| {
                let entry = library.get(id)?;
                let folder = library
                    .folder_id(entry)
                    .and_then(|id| library.folder(id))
                    .map_or_else(String::new, |folder| folder.path.clone());
                Some((
                    entry.id(),
                    entry.name().to_owned(),
                    folder,
                    entry.shared_content(),
                ))
            })
            .collect();
        let store = match store(&surface.window) {
            Ok(store) => store,
            Err(error) => {
                page.status = Some(error.to_string());
                surface.analysis.export = Some(page);
                return Task::none();
            }
        };
        surface.analysis.export = Some(page);
        file_task(
            move || {
                let mut batch = Batch {
                    items: Vec::new(),
                    skipped: Vec::new(),
                    after_record: None,
                    after: Vec::new(),
                };
                let mut bytes = 0_usize;
                for (id, name, folder_path, content) in candidates {
                    let subject = Subject::Snippet(id);
                    let history = store.load(subject)?;
                    let Some(item) = Item::batch(&history, folder_path, content_hash(&content)?)
                    else {
                        batch.skipped.push(name);
                        continue;
                    };
                    bytes = bytes.saturating_add(item.retained_source_bytes());
                    if bytes > MAX_RETAINED_BYTES {
                        return Err(io::Error::other(
                            "Select fewer reports. Their retained source exceeds 32 MiB.",
                        ));
                    }
                    batch.items.push(item);
                }
                Ok(Arc::new(batch))
            },
            move |result| {
                Message::SnippetAnalysis(id, AnalysisEvent::Reports(Event::Loaded(token, result)))
            },
        )
    }

    pub(super) fn update_snippet_analysis_reports(
        &mut self,
        id: window::Id,
        action: Action,
    ) -> Task<Message> {
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.export.as_mut() else {
            return Task::none();
        };
        if let Some(cancel) = &page.cancel {
            if matches!(action, Action::Cancel) {
                cancel.store(true, Ordering::Release);
                page.status = Some("Cancelling export…".into());
            }
            return Task::none();
        }
        match action {
            Action::Cancel => surface.analysis.export = None,
            Action::Output(output) if output != Output::Script || page.script().is_some() => {
                if (page.output == Output::Script) != (output == Output::Script) {
                    page.path.clear();
                }
                page.output = output;
            }
            Action::Packing(packing)
                if page.packing != Packing::Single && packing != Packing::Single =>
            {
                page.packing = packing
            }
            Action::Run(run) if page.packing == Packing::Single => {
                if run.is_none()
                    || page
                        .batch
                        .as_ref()
                        .is_some_and(|batch| batch.after.iter().any(|(choice, _)| choice.id == run))
                {
                    if page.output == Output::Script && page.run != run {
                        page.path.clear();
                    }
                    page.run = run;
                }
            }
            Action::IncludeCode(enabled) => {
                page.set_include_code(enabled, &mut self.window_settings, |settings| {
                    crate::profile::window_settings_path().and_then(|path| settings.save(&path))
                });
            }
            Action::Path(path) if path.len() <= 4096 => page.path = path,
            Action::Export => return self.write_snippet_analysis_reports(id),
            Action::ShowInFolder => return page.show_in_folder(id),
            _ => {}
        }
        Task::none()
    }

    fn write_snippet_analysis_reports(&mut self, id: window::Id) -> Task<Message> {
        let token = self.allocate();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.export.as_mut() else {
            return Task::none();
        };
        let Some(batch) = page.batch.as_ref().filter(|_| page.can_export()).cloned() else {
            return Task::none();
        };
        let path = PathBuf::from(page.path.trim());
        if !path.is_absolute() {
            page.status = Some("Enter a new absolute export path.".into());
            return Task::none();
        }
        let options = match page.output {
            Output::Report(format) => Some(Options {
                format,
                packing: page.packing,
                include_code: page.includes_code(),
                branding: self.window_settings.export_branding.resolve(),
                exported_at: now_millis(),
                locale: super::REPORT_LANGUAGE.into(),
                skipped: batch.skipped.clone(),
            }),
            Output::Script => None,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&cancel);
        let selected_run = page.run;
        let receipt_store =
            (options.is_some() && page.packing == Packing::Single).then(|| store(&surface.window));
        // An export emits at most one start, one event per report, one
        // publication status and one outcome. The worker never waits on the UI.
        let (send, reply) = bounded(report::MAX_REPORTS + 3);
        let (finished, done) = bounded(1);
        let worker = std::thread::Builder::new()
            .name("kortty-snippet-report".into())
            .spawn(move || {
                let _done = finished;
                let result = if let Some(options) = options {
                    batch.export_report(
                        path,
                        options,
                        selected_run,
                        receipt_store,
                        &cancelled,
                        |progress| {
                            let _ = send.try_send(Event::Progress(token, progress));
                        },
                    )
                } else {
                    batch
                        .script(selected_run)
                        .ok_or_else(|| io::Error::other("The selected script is unavailable"))
                        .and_then(|script| report::export_script(path, script.content, &cancelled))
                }
                .map(Arc::new)
                .map_err(|error| error.to_string());
                let _ = send.try_send(Event::Exported(token, result));
            });
        if let Err(error) = worker {
            page.status = Some(error.to_string());
            return Task::none();
        }
        page.token = token;
        page.cancel = Some(cancel);
        page.progress = None;
        page.published_folder = None;
        page.revealing_folder = false;
        page.reveal_error = None;
        page.status = Some(match page.output {
            Output::Report(_) => "Writing reports…".into(),
            Output::Script => "Writing script…".into(),
        });
        self.track_session_shutdown(done);
        let events = stream::unfold(Some(reply), move |reply| async move {
            let reply = reply?;
            let event = reply.recv().await.unwrap_or_else(|_| {
                Event::Exported(token, Err("The report worker stopped.".into()))
            });
            let remaining = (!matches!(&event, Event::Exported(..))).then_some(reply);
            Some((event, remaining))
        });
        Task::run(events, move |event| {
            Message::SnippetAnalysis(id, AnalysisEvent::Reports(event))
        })
    }

    pub(super) fn snippet_analysis_reports_event(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Message> {
        let Some(page) = self
            .snippets
            .get_mut(&id)
            .and_then(|surface| surface.analysis.export.as_mut())
        else {
            return Task::none();
        };
        match event {
            Event::Loaded(token, result) if token == page.token => match result {
                Ok(batch) => page.batch = Some(batch),
                Err(error) => page.status = Some(error),
            },
            Event::Progress(token, progress)
                if token == page.token
                    && page
                        .cancel
                        .as_ref()
                        .is_some_and(|cancel| !cancel.load(Ordering::Acquire)) =>
            {
                page.progress = Some(progress);
            }
            Event::Exported(token, result) if token == page.token && page.cancel.is_some() => {
                let cancelled = page
                    .cancel
                    .take()
                    .is_some_and(|cancel| cancel.load(Ordering::Acquire));
                page.progress = None;
                page.published_folder = result.as_ref().ok().map(|outcome| outcome.folder.clone());
                page.status = Some(match result {
                    Ok(outcome) => {
                        let path = outcome
                            .written_files
                            .first()
                            .map_or("", |path| path.to_str().unwrap_or(""));
                        let mut message = if page.output == Output::Script {
                            format!("Exported script. {path}")
                        } else {
                            format!(
                                "Exported {} report(s), skipped {}. {path}",
                                outcome.exported, outcome.skipped
                            )
                        };
                        let companions = outcome.written_files.len().saturating_sub(1);
                        if companions != 0 {
                            message.push_str(&format!(
                                " Saved {companions} diagram file(s) alongside the report."
                            ));
                        }
                        if outcome.failed_diagrams != 0 {
                            message.push_str(&format!(
                                " {} diagram(s) could not be rendered.",
                                outcome.failed_diagrams
                            ));
                        }
                        if let Some(warning) = &outcome.warning {
                            message.push(' ');
                            message.push_str(warning);
                        }
                        message
                    }
                    Err(error) if cancelled => format!("Export cancelled. {error}"),
                    Err(error) => error,
                });
            }
            Event::FolderRevealed(token, result)
                if token == page.token && page.revealing_folder =>
            {
                page.revealing_folder = false;
                page.reveal_error = result
                    .err()
                    .map(|error| format!("Could not open the export folder ({error})."));
            }
            _ => {}
        }
        Task::none()
    }
}

fn reveal_export_folder(folder: &ExportFolder) -> io::Result<()> {
    if kortty_windowing::open_desktop_directory(folder.current_path()?) {
        Ok(())
    } else {
        Err(io::Error::other(
            "The desktop could not open the selected folder",
        ))
    }
}

pub(super) fn view(page: &Page) -> Element<'_, SnippetMessage> {
    let busy = page.cancel.is_some();
    let mut body = column![text("Export analysis reports").size(20)].spacing(8);
    if let Some(batch) = &page.batch {
        body = body.push(text(format!("{} report(s) available", batch.items.len())));
        let skipped = batch
            .skipped
            .iter()
            .take(12)
            .map(String::as_str)
            .collect::<Vec<_>>();
        if !skipped.is_empty() {
            body = body.push(text(format!(
                "No saved analysis for {} snippet(s): {}",
                batch.skipped.len(),
                skipped.join(", ")
            )));
        }
        if !busy && !batch.items.is_empty() {
            if page.packing != Packing::Single {
                body = body.push(pick_list(PACKINGS, Some(page.packing), |value| {
                    action(Action::Packing(value))
                }));
            } else if !batch.after.is_empty() {
                let choices: Vec<_> = std::iter::once(RunChoice {
                    id: None,
                    label: "Before applying".into(),
                })
                .chain(batch.after.iter().map(|(choice, _)| choice.clone()))
                .collect();
                let selected = choices.iter().find(|choice| choice.id == page.run).cloned();
                body = body.push(pick_list(choices, selected, |choice| {
                    action(Action::Run(choice.id))
                }));
            }
            let outputs = if page.script().is_some() || page.output == Output::Script {
                &OUTPUTS[..]
            } else {
                &OUTPUTS[..OUTPUTS.len() - 1]
            };
            body = body.push(pick_list(outputs, Some(page.output), |value| {
                action(Action::Output(value))
            }));
            match page.output {
                Output::Report(format) => {
                    let extension = page.packing.extension(format);
                    body = body
                        .push(
                            checkbox(page.includes_code())
                                .label("Include full script")
                                .on_toggle_maybe(
                                    (format != Format::Json)
                                        .then_some(|value| action(Action::IncludeCode(value))),
                                ),
                        )
                        .push(
                            text(match (format, page.packing) {
                                (Format::Json, _) => {
                                    "Full script is available for PDF, HTML and Markdown reports."
                                }
                                (_, Packing::Single) => {
                                    "Changes here set the default for future analysis reports."
                                }
                                _ => "Changes here apply to this batch export.",
                            })
                            .size(13),
                        )
                        .push(text(format!(
                            "Enter a new absolute path. A .{extension} suffix is added when missing."
                        )));
                    if format == Format::Markdown {
                        body = body.push(
                            text(if page.packing == Packing::Zip {
                                "Rendered diagrams are included as PNG files inside the ZIP."
                            } else {
                                "Rendered diagrams are saved as PNG files next to the report."
                            })
                            .size(13),
                        );
                    }
                }
                Output::Script => {
                    let after = page.run.is_some();
                    body = body.push(text(if after {
                        "Export final script"
                    } else {
                        "Export analysed script"
                    }));
                    if let Some(script) = page.script() {
                        let suggested = script_filename(script.name, after);
                        body = body.push(
                            text(format!(
                                "Enter a new absolute path for the exact saved script. Suggested filename {suggested}"
                            ))
                            .size(13),
                        );
                    } else {
                        body = body.push(text("This analysis did not retain the selected script."));
                    }
                }
            }
            body = body.push(
                text_input("Export path", &page.path).on_input(|path| action(Action::Path(path))),
            );
        }
    } else if page.status.is_none() {
        body = body.push(text("Reading saved analysis histories…"));
    }
    if let Some(status) = &page.status {
        body = body.push(text(status));
    }
    let progress = page.progress.as_ref().map(|progress| {
        let (completed, total, label) = match progress {
            Progress::Rendering {
                completed,
                total,
                last,
            } => {
                let mut label = format!("{completed} of {total} reports rendered");
                if let Some(name) = last {
                    label.push_str(" · ");
                    label.push_str(name);
                }
                (*completed, *total, label)
            }
            Progress::Publishing { total } => (*total, *total, "Finalizing export…".into()),
        };
        column![
            progress_bar(0.0..=total.max(1) as f32, completed as f32),
            text(label).wrapping(text::Wrapping::WordOrGlyph),
        ]
        .spacing(4)
    });
    if let Some(error) = &page.reveal_error {
        body = body.push(text(error));
    }
    let actions = row![
        button(if busy { "Cancel" } else { "Close" }).on_press(action(Action::Cancel)),
        button("Export").on_press_maybe(
            (!busy && page.can_export() && !page.path.trim().is_empty())
                .then_some(action(Action::Export))
        ),
        (!busy && page.published_folder.is_some()).then(|| {
            button("Show in folder")
                .on_press_maybe((!page.revealing_folder).then_some(action(Action::ShowInFolder)))
        }),
    ]
    .spacing(8);
    container(column![scrollable(body).height(Length::Fill), progress, actions].spacing(10))
        .padding(12)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn script_filename(name: &str, after: bool) -> String {
    let mut safe = String::new();
    for character in name.trim().chars() {
        if safe.len() + character.len_utf8() > 200 {
            break;
        }
        if character.is_alphanumeric() || matches!(character, '.' | '_' | '-') {
            safe.push(character);
        } else if !safe.ends_with('_') {
            safe.push('_');
        }
    }
    let safe = safe.trim_matches(['.', '_']);
    let (stem, extension) = safe.rsplit_once('.').unwrap_or((safe, "txt"));
    let suffix = if after { "final" } else { "analysed" };
    let name = format!("{stem}.{suffix}.{extension}");
    if stem.is_empty() || !valid_component(&name) {
        return format!("script.{suffix}.txt");
    }
    name
}

fn action(value: Action) -> SnippetMessage {
    SnippetMessage::Analysis(AnalysisAction::Reports(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatted_report_export_records_only_a_published_file_on_its_saved_analysis() {
        use crate::snippet_analysis::model::{
            Analysis, ApplyRun, ExportPhase, Finding, Provenance, Purpose, Selection, Source,
        };
        use crate::snippets::tests::Profile;

        let profile = Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let store = Store::new(root.clone());
        let subject = Subject::Snippet(SnippetId::new().unwrap());
        let make_record = |source| {
            Record::new(
                Purpose::Analysis,
                None,
                Source::new(source, "bash", "en", "en", "Deploy").unwrap(),
                Provenance::default(),
                Analysis {
                    improvements: vec![Finding {
                        id: "F1".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let before = make_record("echo old\n");
        let before_id = before.id;
        store.append(subject, before.clone()).unwrap();
        let selection = Selection {
            findings: vec!["F1".into()],
            ..Default::default()
        };
        let run = ApplyRun::new(
            "echo old\n".into(),
            "echo revised\n".into(),
            selection.clone(),
            Vec::new(),
            now_millis(),
        )
        .unwrap();
        let run_id = run.id;
        let history = store
            .append_apply_while(subject, before_id, &before.source.sha256, run, || true)
            .unwrap();
        let earlier = history.current().unwrap().clone();
        let post = Post::capture(&earlier, run_id, &history.records).unwrap();
        let latest = make_record("echo current\n");
        let latest_id = latest.id;
        store.append(subject, latest.clone()).unwrap();
        let batch = Batch {
            items: vec![Item {
                subject,
                record: latest,
                folder_path: String::new(),
                current_hash: None,
                selection,
                post: None,
            }],
            skipped: Vec::new(),
            after_record: Some(earlier),
            after: vec![(
                RunChoice {
                    id: Some(run_id),
                    label: "After".into(),
                },
                post,
            )],
        };
        let export = |name: &str, run, cancelled| {
            batch.export_report(
                root.join(name),
                Options {
                    format: Format::Json,
                    packing: Packing::Single,
                    include_code: false,
                    branding: Default::default(),
                    exported_at: now_millis(),
                    locale: "en".into(),
                    skipped: Vec::new(),
                },
                run,
                Some(Ok(store.clone())),
                &AtomicBool::new(cancelled),
                |_| {},
            )
        };
        assert!(export("cancelled.json", Some(run_id), true).is_err());
        assert!(!root.join("cancelled.json").exists());
        assert!(
            export("after.json", Some(run_id), false)
                .unwrap()
                .warning
                .is_none()
        );
        assert!(export("before", None, false).unwrap().warning.is_none());
        assert!(root.join("before.json").is_file());
        assert!(export("after.json", Some(run_id), false).is_err());
        let loaded = store.load(subject).unwrap();
        let [latest, earlier] = loaded.records.as_slice() else {
            panic!("expected two saved analyses");
        };
        assert_eq!(latest.id, latest_id);
        let [pre] = latest.exports.as_slice() else {
            panic!("expected one before receipt");
        };
        assert_eq!(pre.phase, ExportPhase::BeforeApply);
        assert_eq!(pre.run_id, None);
        assert_eq!(pre.file_name, "before.json");
        assert_eq!(earlier.id, before_id);
        let [post] = earlier.exports.as_slice() else {
            panic!("expected one after receipt");
        };
        assert_eq!(post.phase, ExportPhase::AfterApply);
        assert_eq!(post.run_id, Some(run_id));
        assert_eq!(post.file_name, "after.json");
        assert_eq!(post.format, Format::Json);

        let directory = crate::private_files::Directory::open(&root, false)
            .unwrap()
            .private_child("snippet-analyses", false)
            .unwrap();
        let lock = directory.open_lock(".history.lock").unwrap();
        lock.lock().unwrap();
        let contended = std::thread::scope(|scope| {
            let (send, reply) = std::sync::mpsc::channel();
            let export = &export;
            scope.spawn(move || {
                let _ = send.send(export("contended.json", None, false));
            });
            let completed = reply.recv_timeout(std::time::Duration::from_secs(10));
            drop(lock);
            completed
                .expect("a report export must finish while another writer holds the history lock")
        })
        .unwrap();
        assert!(
            contended
                .warning
                .unwrap()
                .contains("The analysis history is busy")
        );
        assert!(root.join("contended.json").is_file());
        assert_eq!(store.load(subject).unwrap().records[0].exports.len(), 1);

        store.remove(subject, before_id).unwrap();
        let orphan = export("removed.json", Some(run_id), false).unwrap();
        assert!(orphan.warning.unwrap().contains("export receipt"));
        assert!(root.join("removed.json").is_file());
        assert_eq!(store.load(subject).unwrap().records.len(), 1);
    }

    #[test]
    fn plain_script_export_uses_the_selected_saved_analysis_or_apply_run() {
        use crate::snippet_analysis::model::{
            Analysis, ApplyRun, Provenance, Purpose, Selection, Source,
        };

        let make_record = |source| {
            Record::new(
                Purpose::Analysis,
                None,
                Source::new(source, "bash", "en", "en", "Release α.sh").unwrap(),
                Provenance::default(),
                Analysis::default(),
            )
            .unwrap()
        };
        let mut earlier = make_record("# Earlier analysis\r\n");
        let run = ApplyRun::new(
            "# Earlier analysis\r\n".into(),
            "printf 'α'\r\n".into(),
            Selection {
                findings: vec!["F1".into()],
                ..Default::default()
            },
            Vec::new(),
            1,
        )
        .unwrap();
        let selected = run.id;
        earlier.apply_runs.push(run);
        let post = Post::capture(&earlier, selected, &[]).unwrap();
        let mut batch = Batch {
            items: vec![Item {
                subject: Subject::Snippet(SnippetId::new().unwrap()),
                record: make_record("# Latest analysis\r\n"),
                folder_path: String::new(),
                current_hash: Some(content_hash("# Unsaved editor text\r\n").unwrap()),
                selection: Selection::default(),
                post: None,
            }],
            skipped: Vec::new(),
            after_record: Some(earlier),
            after: vec![(
                RunChoice {
                    id: Some(selected),
                    label: "After".into(),
                },
                post,
            )],
        };
        assert_eq!(batch.script(None).unwrap().content, "# Latest analysis\r\n");
        let final_script = batch.script(Some(selected)).unwrap();
        assert_eq!(final_script.content, "printf 'α'\r\n");
        assert_eq!(
            script_filename(final_script.name, true),
            "Release_α.final.sh"
        );
        assert!(batch.script(Some(SnippetId::new().unwrap())).is_none());
        batch.items[0].record.source.content = None;
        assert!(batch.script(None).is_none());
        assert!(batch.script(Some(selected)).is_some());

        let mut page = Page::new(1, Packing::Combined, false);
        page.batch = Some(Arc::new(batch));
        assert!(page.script().is_none());
        page.packing = Packing::Single;
        page.output = Output::Script;
        assert!(!page.can_export());
        page.run = Some(selected);
        assert!(page.can_export());
    }

    #[test]
    fn snippet_report_code_default_persists_for_single_and_stays_local_for_batch() {
        let mut settings: WindowSettings = serde_json::from_str("{}").unwrap();
        let mut single = Page::new(
            1,
            Packing::Single,
            settings.snippet_analysis_export_include_code,
        );
        assert!(!single.includes_code());

        let mut stored = Vec::new();
        single.set_include_code(true, &mut settings, |settings| {
            stored = serde_json::to_vec(settings)?;
            Ok(())
        });
        assert!(single.includes_code());
        let saved: WindowSettings = serde_json::from_slice(&stored).unwrap();
        let reopened = |packing| Page::new(2, packing, saved.snippet_analysis_export_include_code);
        assert!(reopened(Packing::Single).includes_code());
        for packing in [Packing::Combined, Packing::Zip] {
            let mut batch = reopened(packing);
            assert!(batch.includes_code());
            batch.set_include_code(false, &mut settings, |_| panic!("batch choice was saved"));
            assert!(!batch.includes_code());
            assert!(settings.snippet_analysis_export_include_code);
        }

        single.output = Output::Report(Format::Json);
        assert!(!single.includes_code());
        single.set_include_code(false, &mut settings, |_| panic!("JSON choice was saved"));
        single.output = Output::Report(Format::Markdown);
        assert!(single.includes_code());
        single.set_include_code(false, &mut settings, |_| {
            Err(io::Error::other("test failure"))
        });
        assert!(!single.includes_code());
        assert!(settings.snippet_analysis_export_include_code);
        assert!(single.status.as_deref().unwrap().contains("test failure"));
        single.set_include_code(true, &mut settings, |_| {
            panic!("saved choice was rewritten")
        });
        assert!(single.includes_code());
        assert!(single.status.is_none());
    }
}
