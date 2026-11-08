//! Analysis tabs keep source snapshots and in-flight work separate from editor drafts.

use std::collections::HashSet;
use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use iced::{Task, window};

use crate::ai_manager::Configuration;
use crate::local_ai;
use crate::snippet_analysis::{
    model::{Analysis, History, Provenance, Purpose, Record, Source, Subject, content_hash},
    request::{InputFile, MAX_SOURCE_BYTES, Request, Snapshot},
    store::Store,
};
use crate::snippet_window::{
    AnalysisPane, Message as SnippetMessage, SnippetWindow,
    analysis::{self, FileChoice, Message as Action, Phase, ProfileChoice},
    compare,
};
use crate::snippets::{SnippetId, archive};

use super::{Message, Workspace, file_task};

mod reports;
pub(crate) use reports::Action as SnippetReportAction;

const MAX_TABS: usize = 12;
const MAX_INSTRUCTIONS_BYTES: usize = 4096;
const REPORT_LANGUAGE: &str = "en";
const PREVIEW_PAGE_BYTES: usize = 12 * 1024;

#[derive(Default)]
pub(super) struct Tabs {
    pages: Vec<Page>,
    active: Option<Subject>,
    export: Option<reports::Page>,
}

struct Page {
    subject: Subject,
    name: String,
    files: Vec<FileChoice>,
    profiles: Vec<ProfileChoice>,
    profile: Option<u64>,
    history: Option<Arc<History>>,
    record: Option<SnippetId>,
    instructions: String,
    hash: Option<String>,
    estimated_tokens: Option<usize>,
    scroll: iced::widget::Id,
    files_before_edit: Option<Vec<bool>>,
    state: State,
    status: Option<String>,
}

enum State {
    Ready,
    Files,
    Review(Box<Review>),
    Requesting(Box<Run>),
    HistoryWork(u64, &'static str, Option<Arc<AtomicBool>>),
    ConfirmDiscard,
}

struct Review {
    request: Request,
    source: Source,
    config: Arc<Configuration>,
    transport: local_ai::Profile,
    profile: ProfileChoice,
    rendered_hash: String,
    parts: Vec<PreviewPart>,
    page: usize,
}

struct PreviewPart {
    system: bool,
    start: usize,
    end: usize,
}

impl Review {
    fn preview(&self) -> Phase<'_> {
        let part = &self.parts[self.page];
        let (text, label) = if part.system {
            (self.request.system(), "System prompt")
        } else {
            (self.request.input(), "User prompt")
        };
        Phase::ConfirmStart {
            preview: &text[part.start..part.end],
            part: label,
            page: self.page,
            pages: self.parts.len(),
        }
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        if let State::HistoryWork(_, _, Some(cancelled)) = &self.state {
            cancelled.store(true, Ordering::Release);
        }
    }
}

struct Run {
    token: u64,
    source: Source,
    profile: ProfileChoice,
    previous: Option<SnippetId>,
    started: Instant,
    _cancel: local_ai::Cancel,
}

#[derive(Clone)]
pub(crate) enum Event {
    History(Subject, u64, Result<Arc<History>, String>, HistoryChange),
    Analyzed(Subject, u64, Result<Analysis, String>),
    Reports(reports::Event),
    Cleared(Result<(), String>),
}

impl fmt::Debug for Event {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Snippet analysis event")
    }
}

#[derive(Clone, Copy)]
pub(crate) enum HistoryChange {
    Load,
    New,
    Pin,
    Remove,
}

impl Tabs {
    pub(super) fn editor(&mut self) {
        self.active = None;
    }

    pub(super) fn export_open(&self) -> bool {
        self.export.is_some()
    }

    fn page(&self, subject: Subject) -> Option<&Page> {
        self.pages.iter().find(|page| page.subject == subject)
    }

    fn page_mut(&mut self, subject: Subject) -> Option<&mut Page> {
        self.pages.iter_mut().find(|page| page.subject == subject)
    }

    pub(super) fn view(&self) -> AnalysisPane<'_> {
        let page = self.export.as_ref().map(reports::view).or_else(|| {
            let page = self.page(self.active?)?;
            Some(analysis::view(&page.view()))
        });
        AnalysisPane {
            tabs: self
                .pages
                .iter()
                .map(|page| (page.subject, page.name.as_str()))
                .collect(),
            active: self.active,
            page,
        }
    }

    pub(super) fn escape(&self) -> Option<Action> {
        if self.export.is_some() {
            return Some(Action::Reports(reports::Action::Cancel));
        }
        let page = self.page(self.active?)?;
        Some(match page.state {
            State::Files => Action::CancelFiles,
            State::Review(_) => Action::CancelStart,
            State::Requesting(_) => Action::Abort,
            State::ConfirmDiscard => Action::CancelDiscard,
            State::HistoryWork(..) | State::Ready => Action::Editor,
        })
    }

    fn current_mut(&mut self) -> Option<&mut Page> {
        self.page_mut(self.active?)
    }
}

impl Page {
    fn loading(subject: Subject, token: u64) -> Self {
        Self {
            subject,
            name: "Code analysis".into(),
            files: Vec::new(),
            profiles: Vec::new(),
            profile: None,
            history: None,
            record: None,
            instructions: String::new(),
            hash: None,
            estimated_tokens: None,
            scroll: iced::widget::Id::unique(),
            files_before_edit: None,
            state: State::HistoryWork(token, "Loading analysis history…", None),
            status: None,
        }
    }

    fn reset_scroll(&self) -> Task<Message> {
        iced::widget::operation::scroll_to(
            self.scroll.clone(),
            iced::widget::operation::AbsoluteOffset { x: 0.0, y: 0.0 },
        )
    }
    fn record(&self) -> Option<&Record> {
        let history = self.history.as_ref()?;
        self.record
            .and_then(|id| history.records.iter().find(|record| record.id == id))
            .or_else(|| history.current())
    }

    fn previous(&self) -> Option<&Record> {
        let current = self.record()?;
        let history = self.history.as_ref()?;
        let index = history
            .records
            .iter()
            .position(|record| record.id == current.id)?;
        history.records.get(index + 1)
    }

    fn stale(&self) -> bool {
        self.record()
            .is_some_and(|record| self.hash.as_deref() != Some(record.source.sha256.as_str()))
    }

    fn view(&self) -> analysis::View<'_> {
        let phase = match &self.state {
            State::Ready => Phase::Ready,
            State::Files => Phase::Files,
            State::Review(review) => review.preview(),
            State::Requesting(_) => Phase::Running {
                status: "Analyzing code…",
            },
            State::HistoryWork(_, status, _) => Phase::Running { status },
            State::ConfirmDiscard => Phase::ConfirmDiscard,
        };
        let bytes = self
            .files
            .iter()
            .filter(|file| file.included)
            .map(|file| file.bytes)
            .sum::<usize>();
        analysis::View {
            subject: self.subject,
            name: &self.name,
            profiles: &self.profiles,
            profile: self.profile,
            files: &self.files,
            estimated_tokens: self
                .estimated_tokens
                .unwrap_or_else(|| bytes.saturating_mul(5).div_ceil(16)),
            history: self.history.as_deref(),
            record: self.record,
            phase,
            stale: self.stale(),
            status: self.status.as_deref(),
            instructions: &self.instructions,
            can_export: self.record().is_some(),
            can_compare: self.previous().is_some(),
            can_jump: !self.stale(),
            can_diagram: false,
            scroll: self.scroll.clone(),
        }
    }

    fn configure(&mut self, config: Option<&Configuration>) {
        self.profiles = config
            .into_iter()
            .flat_map(Configuration::code_analysis_profiles)
            .map(|(id, name, model, endpoint)| ProfileChoice {
                id,
                name: name.into(),
                model: model.into(),
                endpoint,
            })
            .collect();
        if self
            .profile
            .is_none_or(|id| !self.profiles.iter().any(|profile| profile.id == id))
        {
            self.profile = config
                .and_then(Configuration::code_analysis_profile_id)
                .filter(|id| self.profiles.iter().any(|profile| profile.id == *id))
                .or_else(|| self.profiles.first().map(|profile| profile.id));
        }
    }

    fn refresh(&mut self, window: &SnippetWindow) -> Result<(), String> {
        let (name, files) = scope(window, self.subject)?;
        let previous = std::mem::take(&mut self.files);
        self.files = files
            .into_iter()
            .map(|file| FileChoice {
                id: file.snippet.id(),
                path: file.path,
                bytes: file.snippet.content().len(),
                included: previous
                    .iter()
                    .find(|old| old.id == file.snippet.id())
                    .is_none_or(|old| old.included),
            })
            .collect();
        self.name = name;
        let snapshot = source_snapshot(window, self).ok();
        self.estimated_tokens = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.estimated_tokens().ok());
        self.refresh_record_hash(window);
        Ok(())
    }

    fn refresh_record_hash(&mut self, window: &SnippetWindow) {
        self.hash = self
            .record()
            .and_then(|record| record_hash(window, self, record));
    }
}

impl Workspace {
    pub(super) fn open_snippet_analysis(
        &mut self,
        id: window::Id,
        subject: Subject,
    ) -> Task<Message> {
        if self.exiting || self.scheduler_draining || self.hosted_close_locks(id) {
            return Task::none();
        }
        let config = self.ai_configuration();
        let token = self.allocate();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        if surface.transfer.is_some()
            || surface.analysis.export.is_some()
            || !surface.window.prepare_transfer()
        {
            return Task::none();
        }
        let analysis = &mut surface.analysis;
        if let Some(page) = analysis.page_mut(subject) {
            page.configure(config.as_deref());
            if let Err(error) = page.refresh(&surface.window) {
                page.status = Some(error);
            }
            analysis.active = Some(subject);
            return Task::none();
        }
        if analysis.pages.len() >= MAX_TABS {
            return Task::none();
        }
        let mut page = Page::loading(subject, token);
        page.configure(config.as_deref());
        let store = store(&surface.window);
        let load = match (page.refresh(&surface.window), store) {
            (Ok(()), Ok(store)) => {
                history_task(id, subject, token, HistoryChange::Load, move || {
                    store.load(subject)
                })
            }
            (result, store) => {
                page.status = result
                    .err()
                    .or_else(|| store.err().map(|error| error.to_string()));
                page.state = State::Ready;
                Task::none()
            }
        };
        analysis.pages.push(page);
        analysis.active = Some(subject);
        load
    }

    pub(super) fn update_snippet_analysis(
        &mut self,
        id: window::Id,
        action: Action,
    ) -> Task<Message> {
        if self.hosted_close_locks(id) || self.exiting || self.scheduler_draining {
            return Task::none();
        }
        if self
            .snippets
            .get(&id)
            .is_some_and(|surface| surface.analysis.export_open())
            && !matches!(action, Action::Reports(_))
        {
            return Task::none();
        }
        match action {
            Action::Tab(subject) => return self.open_snippet_analysis(id, subject),
            Action::RequestStart => return self.review_snippet_analysis(id),
            Action::ConfirmStart => return self.start_snippet_analysis(id),
            Action::Pin(pinned) => {
                return self.mutate_snippet_history(id, HistoryChange::Pin, pinned);
            }
            Action::Discard => {
                return self.mutate_snippet_history(id, HistoryChange::Remove, false);
            }
            Action::Compare => return self.compare_snippet_analyses(id),
            Action::Export => return self.export_current_snippet_analysis(id),
            Action::Reports(action) => return self.update_snippet_analysis_reports(id, action),
            Action::Jump { file, line } => return self.jump_from_snippet_analysis(id, file, line),
            _ => {}
        }
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let tabs = &mut surface.analysis;
        match action {
            Action::Editor => tabs.editor(),
            Action::Close => {
                if let Some(subject) = tabs.active.take() {
                    tabs.pages.retain(|page| page.subject != subject);
                }
            }
            action => {
                let Some(page) = tabs.current_mut() else {
                    return Task::none();
                };
                match action {
                    Action::Profile(profile)
                        if matches!(page.state, State::Ready)
                            && page.profiles.iter().any(|choice| choice.id == profile) =>
                    {
                        page.profile = Some(profile)
                    }
                    Action::Instructions(value)
                        if matches!(page.state, State::Ready)
                            && value.len() <= MAX_INSTRUCTIONS_BYTES =>
                    {
                        page.instructions = value
                    }
                    Action::ReviewPage(forward) => {
                        if let State::Review(review) = &mut page.state {
                            review.page = if forward {
                                (review.page + 1).min(review.parts.len() - 1)
                            } else {
                                review.page.saturating_sub(1)
                            };
                            return page.reset_scroll();
                        }
                    }
                    Action::Files(true) if matches!(page.state, State::Ready) => {
                        page.files_before_edit =
                            Some(page.files.iter().map(|file| file.included).collect());
                        page.state = State::Files;
                    }
                    Action::IncludeFile(id, include) if matches!(page.state, State::Files) => {
                        if let Some(file) = page.files.iter_mut().find(|file| file.id == id) {
                            file.included = include;
                        }
                    }
                    Action::Files(false) | Action::CancelFiles
                        if matches!(page.state, State::Files) =>
                    {
                        if let Some(before) = page.files_before_edit.take()
                            && matches!(action, Action::CancelFiles)
                        {
                            for (file, included) in page.files.iter_mut().zip(before) {
                                file.included = included;
                            }
                        }
                        page.state = State::Ready;
                        let _ = page.refresh(&surface.window);
                    }
                    Action::CancelStart if matches!(page.state, State::Review(_)) => {
                        page.state = State::Ready
                    }
                    Action::Abort if matches!(page.state, State::Requesting(_)) => {
                        page.state = State::Ready;
                        page.status = Some("Analysis cancelled.".into());
                    }
                    Action::SelectRecord(id)
                        if page.history.as_ref().is_some_and(|history| {
                            history.records.iter().any(|record| record.id == id)
                        }) =>
                    {
                        page.record = Some(id);
                        page.refresh_record_hash(&surface.window);
                        return page.reset_scroll();
                    }
                    Action::RequestDiscard
                        if matches!(page.state, State::Ready) && page.record().is_some() =>
                    {
                        page.state = State::ConfirmDiscard;
                    }
                    Action::CancelDiscard if matches!(page.state, State::ConfirmDiscard) => {
                        page.state = State::Ready
                    }
                    _ => {}
                }
            }
        }
        Task::none()
    }

    fn review_snippet_analysis(&mut self, id: window::Id) -> Task<Message> {
        let config = self.ai_configuration();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface
            .analysis
            .current_mut()
            .filter(|page| matches!(page.state, State::Ready))
        else {
            return Task::none();
        };
        page.configure(config.as_deref());
        let review = (|| {
            page.refresh(&surface.window)?;
            let config = config.ok_or("AI features are disabled or unavailable.")?;
            let profile = page
                .profiles
                .iter()
                .find(|choice| Some(choice.id) == page.profile)
                .cloned()
                .ok_or("Choose a ready coding profile in AI Manager.")?;
            let (transport, limit) = config.code_analysis_transport(profile.id)?;
            let snapshot = source_snapshot(&surface.window, page)?;
            if snapshot
                .files()
                .iter()
                .map(|file| file.content.chars().count())
                .sum::<usize>()
                > limit
            {
                return Err("The selected source exceeds the AI profile's selection limit.".into());
            }
            let source = snapshot
                .source(REPORT_LANGUAGE, REPORT_LANGUAGE)
                .map_err(|error| error.to_string())?;
            let rendered = snapshot.render()?;
            let rendered_hash = content_hash(&rendered).map_err(|error| error.to_string())?;
            let guidance = config.skill_guidance(&rendered);
            let request = Request::new(&snapshot, REPORT_LANGUAGE, &page.instructions, &guidance)?;
            let parts = preview_parts(&request);
            Ok(Review {
                request,
                source,
                config,
                transport,
                profile,
                rendered_hash,
                parts,
                page: 0,
            })
        })();
        match review {
            Ok(review) => {
                page.status = None;
                page.state = State::Review(Box::new(review));
                return page.reset_scroll();
            }
            Err(error) => page.status = Some(error),
        }
        Task::none()
    }

    fn start_snippet_analysis(&mut self, id: window::Id) -> Task<Message> {
        let config = self.ai_configuration();
        let token = self.allocate();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.current_mut() else {
            return Task::none();
        };
        if !matches!(page.state, State::Review(_)) {
            return Task::none();
        }
        let State::Review(review) = std::mem::replace(&mut page.state, State::Ready) else {
            unreachable!("checked review state");
        };
        let same_source = source_snapshot(&surface.window, page)
            .and_then(|snapshot| snapshot.render())
            .ok()
            .and_then(|rendered| content_hash(&rendered).ok())
            .is_some_and(|hash| hash == review.rendered_hash);
        if !same_source
            || config
                .as_ref()
                .is_none_or(|config| !Arc::ptr_eq(config, &review.config))
        {
            page.status =
                Some("The source or AI settings changed. Review the request again.".into());
            return Task::none();
        }
        let Review {
            request,
            source,
            transport,
            profile,
            ..
        } = *review;
        let started = Instant::now();
        let pending = match request.start(transport) {
            Ok(pending) => pending,
            Err(error) => {
                page.status = Some(error);
                return Task::none();
            }
        };
        let subject = page.subject;
        let previous = page.record().map(|record| record.id);
        let local_ai::Pending { reply, cancel } = pending;
        page.state = State::Requesting(Box::new(Run {
            token,
            source,
            profile,
            previous,
            started,
            _cancel: cancel,
        }));
        Task::perform(
            async move {
                reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The analysis worker stopped.".into()))
            },
            move |result| Message::SnippetAnalysis(id, Event::Analyzed(subject, token, result)),
        )
    }

    pub(in crate::workspace) fn snippet_analysis_event(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Message> {
        match event {
            Event::History(subject, token, result, change) => {
                let Some(surface) = self.snippets.get_mut(&id) else {
                    return Task::none();
                };
                let Some(page) = surface.analysis.page_mut(subject) else {
                    return Task::none();
                };
                if !matches!(page.state, State::HistoryWork(active, ..) if active == token) {
                    return Task::none();
                }
                page.state = State::Ready;
                match result {
                    Ok(history) => {
                        if matches!(change, HistoryChange::Load | HistoryChange::New)
                            || page.record.is_none_or(|id| {
                                !history.records.iter().any(|record| record.id == id)
                            })
                        {
                            page.record = history.current().map(|record| record.id);
                        }
                        page.history = Some(history);
                        page.status =
                            matches!(change, HistoryChange::New).then(|| "Analysis saved.".into());
                        if let Err(error) = page.refresh(&surface.window) {
                            page.status = Some(error);
                        }
                    }
                    Err(error) => page.status = Some(error),
                }
                Task::none()
            }
            Event::Analyzed(subject, token, result) => {
                self.finish_snippet_analysis(id, subject, token, result)
            }
            Event::Reports(event) => self.snippet_analysis_reports_event(id, event),
            Event::Cleared(result) => {
                if let Err(error) = result
                    && let Some(surface) = self.snippets.get_mut(&id)
                {
                    surface.window.analysis_notice(format!(
                        "Could not clear deleted analysis history ({error})"
                    ));
                }
                Task::none()
            }
        }
    }

    fn finish_snippet_analysis(
        &mut self,
        id: window::Id,
        subject: Subject,
        token: u64,
        result: Result<Analysis, String>,
    ) -> Task<Message> {
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.page_mut(subject) else {
            return Task::none();
        };
        if !matches!(&page.state, State::Requesting(run) if run.token == token) {
            return Task::none();
        }
        let State::Requesting(run) = std::mem::replace(&mut page.state, State::Ready) else {
            unreachable!("checked analysis request");
        };
        let record = (|| {
            scope(&surface.window, subject)?;
            let analysis = result?;
            let provenance = Provenance {
                profile_id: run.profile.id,
                profile_name: run.profile.name,
                model: run.profile.model,
                additional_instructions: page.instructions.clone(),
                duration_millis: run
                    .started
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
                ..Provenance::default()
            };
            let purpose = if run.previous.is_some() {
                Purpose::Rerun
            } else {
                Purpose::Analysis
            };
            let record = Record::new(purpose, run.previous, run.source, provenance, analysis)
                .map_err(|error| error.to_string())?;
            let store = store(&surface.window).map_err(|error| error.to_string())?;
            Ok((store, record))
        })();
        match record {
            Ok((store, record)) => {
                let cancelled = Arc::new(AtomicBool::new(false));
                page.state =
                    State::HistoryWork(token, "Saving analysis…", Some(Arc::clone(&cancelled)));
                history_task(id, subject, token, HistoryChange::New, move || {
                    store.append_while(subject, record, || !cancelled.load(Ordering::Acquire))
                })
            }
            Err(error) => {
                page.status = Some(error);
                Task::none()
            }
        }
    }

    fn mutate_snippet_history(
        &mut self,
        id: window::Id,
        change: HistoryChange,
        pin: bool,
    ) -> Task<Message> {
        let token = self.allocate();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.current_mut() else {
            return Task::none();
        };
        let valid = match change {
            HistoryChange::Pin => matches!(page.state, State::Ready),
            HistoryChange::Remove => matches!(page.state, State::ConfirmDiscard),
            _ => false,
        };
        let Some(record) = page.record().map(|record| record.id).filter(|_| valid) else {
            return Task::none();
        };
        let subject = page.subject;
        let store = match store(&surface.window) {
            Ok(store) => store,
            Err(error) => {
                page.status = Some(error.to_string());
                page.state = State::Ready;
                return Task::none();
            }
        };
        page.state = State::HistoryWork(token, "Updating analysis history…", None);
        history_task(id, subject, token, change, move || match change {
            HistoryChange::Pin => store.set_pinned(subject, record, pin),
            HistoryChange::Remove => store.remove(subject, record),
            _ => unreachable!("validated history mutation"),
        })
    }

    fn compare_snippet_analyses(&mut self, id: window::Id) -> Task<Message> {
        let Some(page) = self
            .snippets
            .get(&id)
            .and_then(|surface| surface.analysis.page(surface.analysis.active?))
        else {
            return Task::none();
        };
        let (Some(current), Some(previous)) = (page.record(), page.previous()) else {
            return Task::none();
        };
        self.open_snippet_comparison(
            id,
            compare::Request::texts(
                (
                    format!("Earlier analysis · {}", previous.provenance.model),
                    comparison_text(&previous.analysis),
                ),
                (
                    format!("Selected analysis · {}", current.provenance.model),
                    comparison_text(&current.analysis),
                ),
            ),
        )
    }

    fn jump_from_snippet_analysis(
        &mut self,
        id: window::Id,
        file: Option<String>,
        line: usize,
    ) -> Task<Message> {
        let Some(surface) = self.snippets.get(&id) else {
            return Task::none();
        };
        let Some(page) = surface
            .analysis
            .active
            .and_then(|subject| surface.analysis.page(subject))
        else {
            return Task::none();
        };
        let Some(record) = page.record() else {
            return Task::none();
        };
        let fresh = record_hash(&surface.window, page, record).as_deref()
            == Some(record.source.sha256.as_str());
        if !fresh
            || !record
                .analysis
                .improvements
                .iter()
                .any(|finding| finding.file == file && finding.line == Some(line))
        {
            return Task::none();
        }
        let snippet = match page.subject {
            Subject::Snippet(id) => Some(id),
            Subject::Folder(_) => page
                .files
                .iter()
                .find(|entry| Some(entry.path.as_str()) == file.as_deref())
                .map(|entry| entry.id),
        };
        let Some(snippet) = snippet else {
            return Task::none();
        };
        let open = self.update_snippets(id, SnippetMessage::Open(snippet));
        let Some(surface) = self.snippets.get_mut(&id) else {
            return open;
        };
        if surface.window.jump_to_analysis_line(snippet, line) {
            Task::batch([
                open,
                surface
                    .window
                    .focus_editor()
                    .map(move |action| Message::Snippets(id, action)),
            ])
        } else {
            open
        }
    }
}

impl Workspace {
    pub(super) fn cleanup_snippet_analyses(
        &mut self,
        source: window::Id,
        before: Vec<Subject>,
    ) -> Task<Message> {
        let Some(surface) = self.snippets.get(&source) else {
            return Task::none();
        };
        let library = surface.window.analysis_library();
        let removed: std::collections::HashSet<_> = before
            .into_iter()
            .filter(|subject| match subject {
                Subject::Snippet(id) => library.get(*id).is_none(),
                Subject::Folder(id) => library.folder(*id).is_none(),
            })
            .collect();
        if removed.is_empty() {
            return Task::none();
        }
        let Ok(store) = store(&surface.window) else {
            return Task::none();
        };
        let path = library.path().to_path_buf();
        for peer in self
            .snippets
            .values_mut()
            .filter(|peer| peer.window.analysis_library().path() == path)
        {
            peer.analysis
                .pages
                .retain(|page| !removed.contains(&page.subject));
            if peer
                .analysis
                .active
                .is_some_and(|subject| removed.contains(&subject))
            {
                peer.analysis.editor();
            }
        }
        file_task(
            move || {
                let mut error = None;
                for subject in removed {
                    if let Err(failed) = store.clear(subject) {
                        error.get_or_insert(failed);
                    }
                }
                error.map_or(Ok(()), Err)
            },
            move |result| Message::SnippetAnalysis(source, Event::Cleared(result)),
        )
    }
}

pub(super) fn subjects(window: &SnippetWindow) -> Vec<Subject> {
    let library = window.analysis_library();
    library
        .entries()
        .iter()
        .map(|entry| Subject::Snippet(entry.id()))
        .chain(
            library
                .folders()
                .iter()
                .map(|folder| Subject::Folder(folder.id)),
        )
        .collect()
}

fn preview_parts(request: &Request) -> Vec<PreviewPart> {
    let mut parts = Vec::new();
    for (system, text) in [(false, request.input()), (true, request.system())] {
        let mut start = 0;
        while start < text.len() {
            let mut end = (start + PREVIEW_PAGE_BYTES).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            parts.push(PreviewPart { system, start, end });
            start = end;
        }
    }
    parts
}

fn store(window: &SnippetWindow) -> io::Result<Store> {
    let directory = window
        .analysis_library()
        .path()
        .parent()
        .ok_or_else(|| io::Error::other("Snippet profile directory is unavailable"))?;
    Ok(Store::new(directory.to_path_buf()))
}

fn scope(
    window: &SnippetWindow,
    subject: Subject,
) -> Result<(String, Vec<archive::AnalysisFile<'_>>), String> {
    let library = window.analysis_library();
    let (name, scope, ids) = match subject {
        Subject::Snippet(id) => (
            library
                .get(id)
                .ok_or("The snippet no longer exists.")?
                .name()
                .into(),
            None,
            vec![id],
        ),
        Subject::Folder(id) => (
            library
                .folder(id)
                .ok_or("The snippet folder no longer exists.")?
                .path
                .clone(),
            Some(id),
            library.snippets_in_folder(Some(id), true),
        ),
    };
    if ids.is_empty() {
        return Ok((name, Vec::new()));
    }
    let files = archive::analysis_files(library, &ids, scope).map_err(|error| error.to_string())?;
    Ok((name, files))
}

fn source_snapshot(window: &SnippetWindow, page: &Page) -> Result<Snapshot, String> {
    snapshot_for(window, page, None)
}

fn record_hash(window: &SnippetWindow, page: &Page, record: &Record) -> Option<String> {
    snapshot_for(window, page, Some(record))
        .ok()?
        .source(
            &record.source.report_language_code,
            &record.source.code_text_language_code,
        )
        .ok()
        .map(|source| source.sha256)
}

fn snapshot_for(
    window: &SnippetWindow,
    page: &Page,
    record: Option<&Record>,
) -> Result<Snapshot, String> {
    let (name, files) = scope(window, page.subject)?;
    match page.subject {
        Subject::Snippet(id) => {
            let (_, language, content) = window
                .analysis_snippet(id)
                .ok_or("The snippet no longer exists.")?;
            let file = files
                .first()
                .ok_or("The snippet filename is unavailable.")?;
            let path = file.path.rsplit('/').next().unwrap_or(&file.path).into();
            Ok(Snapshot::snippet(name, language, content).with_snippet_file(path, file.executable))
        }
        Subject::Folder(_) => {
            let recorded = record.map(|record| {
                record
                    .analysis
                    .project_files
                    .iter()
                    .map(|file| file.path.as_str())
                    .collect::<HashSet<_>>()
            });
            let included: HashSet<_> = page
                .files
                .iter()
                .filter(|file| file.included)
                .map(|file| file.id)
                .collect();
            let files = files.into_iter().filter(|file| {
                recorded.as_ref().map_or_else(
                    || included.contains(&file.snippet.id()),
                    |paths| paths.contains(file.path.as_str()),
                )
            });
            let mut inputs = Vec::new();
            let mut bytes = 0_usize;
            for file in files {
                let content = file.snippet.content();
                bytes = bytes.saturating_add(content.len());
                if bytes > MAX_SOURCE_BYTES {
                    return Err("Select fewer files. The analysis source exceeds 2 MiB.".into());
                }
                let content = content.strip_prefix('\u{feff}').unwrap_or(content);
                inputs.push(InputFile {
                    path: file.path,
                    language: file.snippet.language().into(),
                    executable: file.executable,
                    content: content.replace("\r\n", "\n").replace('\r', "\n"),
                });
            }
            if recorded
                .as_ref()
                .is_some_and(|paths| paths.len() != inputs.len())
            {
                return Err("The analysis files changed.".into());
            }
            Ok(Snapshot::folder(name, inputs))
        }
    }
}

fn history_task(
    id: window::Id,
    subject: Subject,
    token: u64,
    change: HistoryChange,
    job: impl FnOnce() -> io::Result<History> + Send + 'static,
) -> Task<Message> {
    file_task(
        move || job().map(Arc::new),
        move |result| Message::SnippetAnalysis(id, Event::History(subject, token, result, change)),
    )
}

fn comparison_text(analysis: &Analysis) -> String {
    let mut lines = vec!["Summary".to_owned(), analysis.summary.clone()];
    for finding in &analysis.improvements {
        lines.extend([
            String::new(),
            format!(
                "{} · {} · {}",
                finding.category, finding.severity, finding.title
            ),
            finding.detail.clone(),
            finding.recommendation.clone(),
        ]);
    }
    for dependency in &analysis.dependencies {
        lines.extend([
            String::new(),
            dependency.name.clone(),
            dependency.purpose.clone(),
            dependency.suggestion.clone(),
        ]);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippet_analysis::model::ProjectFileNote;
    use crate::snippets::{Input, Library, Metadata, tests::Profile};

    #[test]
    fn snippet_analysis_rechecks_the_files_in_the_saved_report_independently_of_rerun_selection() {
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let folder = library.create_folder(None, "Operations").unwrap();
        for (name, content) in [("Deploy", "echo deploy\n"), ("Private", "echo private\n")] {
            library
                .save_with_metadata(
                    None,
                    Input {
                        name: name.into(),
                        language: "bash".into(),
                        category: String::new(),
                        content: content.into(),
                    },
                    Some(Metadata {
                        folder_path: "Operations".into(),
                        ..Metadata::default()
                    }),
                )
                .unwrap();
        }
        let window = SnippetWindow::open(profile.path()).unwrap();
        let mut page = Page::loading(Subject::Folder(folder), 1);
        page.refresh(&window).unwrap();
        page.files[1].included = false;
        let snapshot = source_snapshot(&window, &page).unwrap();
        assert_eq!(snapshot.files().len(), 1);
        let path = snapshot.files()[0].path.clone();
        let source = snapshot.source("en", "en").unwrap();
        let record = Record::new(
            Purpose::Analysis,
            None,
            source,
            Provenance::default(),
            Analysis {
                summary: "Fixture analysis".into(),
                project_files: vec![ProjectFileNote {
                    path,
                    purpose: String::new(),
                }],
                ..Analysis::default()
            },
        )
        .unwrap();
        page.files[1].included = true;
        assert_eq!(source_snapshot(&window, &page).unwrap().files().len(), 2);
        assert_eq!(
            record_hash(&window, &page, &record).as_deref(),
            Some(record.source.sha256.as_str())
        );
    }
}
