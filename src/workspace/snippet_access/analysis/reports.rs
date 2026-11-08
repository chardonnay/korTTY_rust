//! Native export options for selected stored analysis records.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_channel::bounded;
use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::{Element, Length, Task, window};

use crate::snippet_analysis::model::{Subject, content_hash, now_millis};
use crate::snippet_analysis::report::{self, Format, Item, Options, Outcome, Packing};
use crate::snippet_window::Message as SnippetMessage;
use crate::snippet_window::analysis::Message as AnalysisAction;

use super::{Event as AnalysisEvent, Message, Workspace, file_task, store};

const MAX_RETAINED_BYTES: usize = 32 * 1024 * 1024;
const FORMATS: [Format; 3] = [Format::Html, Format::Markdown, Format::Json];
const PACKINGS: [Packing; 2] = [Packing::Combined, Packing::Zip];

#[derive(Clone)]
pub enum Action {
    Format(Format),
    Packing(Packing),
    IncludeCode(bool),
    Path(String),
    Export,
    Cancel,
}

#[derive(Clone)]
pub(crate) enum Event {
    Loaded(u64, Result<Arc<Batch>, String>),
    Exported(u64, Result<Arc<Outcome>, String>),
}

pub(crate) struct Batch {
    items: Vec<Item>,
    skipped: Vec<String>,
}

pub(super) struct Page {
    token: u64,
    packing: Packing,
    format: Format,
    include_code: bool,
    path: String,
    batch: Option<Arc<Batch>>,
    cancel: Option<Arc<AtomicBool>>,
    status: Option<String>,
}

impl Page {
    fn new(token: u64, packing: Packing) -> Self {
        Self {
            token,
            packing,
            format: Format::Html,
            include_code: false,
            path: String::new(),
            batch: None,
            cancel: None,
            status: None,
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
        let mut export = Page::new(token, Packing::Single);
        export.batch = Some(Arc::new(Batch {
            items: vec![Item {
                subject,
                record,
                folder_path: String::new(),
                current_hash,
            }],
            skipped: Vec::new(),
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
        let mut page = Page::new(token, Packing::Combined);
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
                };
                let mut bytes = 0_usize;
                for (id, name, folder_path, content) in candidates {
                    let subject = Subject::Snippet(id);
                    let history = store.load(subject)?;
                    let Some(record) = history.current().cloned() else {
                        batch.skipped.push(name);
                        continue;
                    };
                    bytes = bytes.saturating_add(
                        record
                            .source
                            .content
                            .as_ref()
                            .map_or(0, |source| source.len()),
                    );
                    if bytes > MAX_RETAINED_BYTES {
                        return Err(io::Error::other(
                            "Select fewer reports. Their retained source exceeds 32 MiB.",
                        ));
                    }
                    batch.items.push(Item {
                        subject,
                        record,
                        folder_path,
                        current_hash: Some(content_hash(&content)?),
                    });
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
            Action::Format(format) => page.format = format,
            Action::Packing(packing)
                if page.packing != Packing::Single && packing != Packing::Single =>
            {
                page.packing = packing
            }
            Action::IncludeCode(enabled) => page.include_code = enabled,
            Action::Path(path) if path.len() <= 4096 => page.path = path,
            Action::Export => return self.write_snippet_analysis_reports(id),
            _ => {}
        }
        Task::none()
    }

    fn write_snippet_analysis_reports(&mut self, id: window::Id) -> Task<Message> {
        let token = self.allocate();
        let Some(page) = self
            .snippets
            .get_mut(&id)
            .and_then(|surface| surface.analysis.export.as_mut())
        else {
            return Task::none();
        };
        let Some(batch) = page
            .batch
            .as_ref()
            .filter(|batch| !batch.items.is_empty())
            .cloned()
        else {
            return Task::none();
        };
        let path = PathBuf::from(page.path.trim());
        if !path.is_absolute() {
            page.status = Some("Enter a new absolute export path.".into());
            return Task::none();
        }
        let options = Options {
            format: page.format,
            packing: page.packing,
            include_code: page.include_code && page.format != Format::Json,
            exported_at: now_millis(),
            locale: super::REPORT_LANGUAGE.into(),
            skipped: batch.skipped.clone(),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&cancel);
        let (send, reply) = bounded(1);
        let (finished, done) = bounded(1);
        let worker = std::thread::Builder::new()
            .name("kortty-snippet-report".into())
            .spawn(move || {
                let _done = finished;
                let result = report::export(path, batch.items.clone(), options, &cancelled)
                    .map(Arc::new)
                    .map_err(|error| error.to_string());
                let _ = send.try_send(result);
            });
        if let Err(error) = worker {
            page.status = Some(error.to_string());
            return Task::none();
        }
        page.token = token;
        page.cancel = Some(cancel);
        page.status = Some("Writing reports…".into());
        self.track_session_shutdown(done);
        Task::perform(
            async move {
                reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The report worker stopped.".into()))
            },
            move |result| {
                Message::SnippetAnalysis(id, AnalysisEvent::Reports(Event::Exported(token, result)))
            },
        )
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
            Event::Exported(token, result) if token == page.token => {
                let cancelled = page
                    .cancel
                    .take()
                    .is_some_and(|cancel| cancel.load(Ordering::Acquire));
                page.status = Some(match result {
                    Ok(outcome) => {
                        let path = outcome
                            .written_files
                            .first()
                            .map_or("", |path| path.to_str().unwrap_or(""));
                        let mut message = format!(
                            "Exported {} report(s), skipped {}. {path}",
                            outcome.exported, outcome.skipped
                        );
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
            _ => {}
        }
        Task::none()
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
            }
            let extension = if page.packing == Packing::Zip {
                "zip"
            } else {
                page.format.extension()
            };
            body = body
                .push(pick_list(FORMATS, Some(page.format), |value| {
                    action(Action::Format(value))
                }))
                .push(
                    checkbox(page.include_code && page.format != Format::Json)
                        .label("Include full analyzed source")
                        .on_toggle_maybe(
                            (page.format != Format::Json)
                                .then_some(|value| action(Action::IncludeCode(value))),
                        ),
                )
                .push(text(format!("Enter a new absolute .{extension} path.")))
                .push(
                    text_input("Export path", &page.path)
                        .on_input(|path| action(Action::Path(path))),
                );
        }
    } else if page.status.is_none() {
        body = body.push(text("Reading saved analysis histories…"));
    }
    if let Some(status) = &page.status {
        body = body.push(text(status));
    }
    let actions = row![
        button(if busy { "Cancel" } else { "Close" }).on_press(action(Action::Cancel)),
        button("Export").on_press_maybe(
            (!busy
                && page
                    .batch
                    .as_ref()
                    .is_some_and(|batch| !batch.items.is_empty())
                && !page.path.trim().is_empty())
            .then_some(action(Action::Export))
        ),
    ]
    .spacing(8);
    container(column![scrollable(body).height(Length::Fill), actions].spacing(10))
        .padding(12)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn action(value: Action) -> SnippetMessage {
    SnippetMessage::Analysis(AnalysisAction::Reports(value))
}
