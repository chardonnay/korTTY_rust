use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use iced::Task;
use iced::widget::{pane_grid, text_editor};
use kortty_jobs::{
    Blocker, JobGraph, JobId, JournalRecording, MAX_JOURNAL_RECORDS, RunBatch, RunId, RunOutcome,
    RunRecord, RunRequest, RunTrigger, Scheduler, SchedulerStore, Time, TimeZone, Weekday,
};
use kortty_ssh::Endpoint;
use serde::{Deserialize, Serialize};

use crate::connection_store::Store as ConnectionStore;
use crate::connection_window::CredentialChoice;
use crate::diagram_window::{self, Format};
use crate::job_executor::{Completion, virtual_terminal};
use crate::snippets::{self, Library, SnippetId};

mod connection;
mod form;
mod journal_search;
mod remote;
mod script;
mod view;

#[cfg(test)]
mod tests;

pub(crate) use connection::{ConnectionChoice, ConnectionRef, MAX_TARGETS};
use form::{Draft, Field, Mode, Source};
use journal_search::{JournalColumn, JournalSearchMode};
pub(crate) use remote::{Archive, ArchiveFormat, RemoteAction, RemotePath};
use script::{Language, MAX_COMMAND_BYTES};
pub(crate) use script::{Script, SnippetLink};
use view::{DiagramView, Layout};

const SAVE_RETRY: Duration = Duration::from_secs(2);

/// Persisted metadata for one unattended SSH action.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SshCommand {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub port: u16,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(default, skip_serializing_if = "path_is_empty")]
    pub key_file: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) connection: Option<ConnectionRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) additional_connections: Vec<ConnectionRef>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) script: Option<Script>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) snippet: Option<SnippetLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) remote: Option<RemoteAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub virtual_terminal: Option<virtual_terminal::Options>,
}

impl fmt::Debug for SshCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("SshCommand").finish_non_exhaustive()
    }
}

impl SshCommand {
    pub(crate) fn validated(mut self) -> Result<Self, String> {
        self.validate_target_selection()?;
        if self.connection.is_some() {
            if !self.host.is_empty()
                || self.port != 0
                || !self.username.is_empty()
                || !path_is_empty(&self.key_file)
            {
                return Err("Choose either a saved SSH connection or manual SSH details".into());
            }
        } else {
            self.validate_manual_connection()?;
        }
        match self.source()? {
            ActionSource::Command(_) => (),
            ActionSource::Script(script) => {
                script.command()?;
            }
            ActionSource::Snippet(link) => link.validate()?,
            ActionSource::Remote(action) => action.validate()?,
        }
        if let Some(options) = self.virtual_terminal {
            options.validate()?;
        }
        Ok(self)
    }

    pub(crate) fn targets(&self) -> impl Iterator<Item = &ConnectionRef> {
        self.connection
            .iter()
            .chain(self.additional_connections.iter())
    }

    pub(crate) fn validate_target_selection(&self) -> Result<(), &'static str> {
        if !self.additional_connections.is_empty() && self.connection.is_none() {
            return Err("Choose a primary saved SSH connection before adding targets");
        }
        if self.additional_connections.len() >= MAX_TARGETS {
            return Err("A job can use up to eight saved SSH connections");
        }
        let mut selected = BTreeSet::new();
        for target in self.targets() {
            target.validate()?;
            if !selected.insert(target.id) {
                return Err("Choose each saved SSH connection only once");
            }
        }
        if !self.additional_connections.is_empty() {
            if self.virtual_terminal.is_some() {
                return Err("Virtual terminal recordings require one SSH connection");
            }
            if self
                .remote
                .as_ref()
                .is_some_and(RemoteAction::writes_local_file)
            {
                return Err("A local download destination requires one SSH connection");
            }
        }
        Ok(())
    }

    fn validate_manual_connection(&mut self) -> Result<(), String> {
        let endpoint = Endpoint::new(&self.host, self.port).map_err(|error| error.to_string())?;
        self.host = endpoint.wire_host().into_owned();
        if self.username.trim().is_empty()
            || self.username.len() > 255
            || self.username.chars().any(char::is_control)
        {
            return Err("Enter a valid SSH username".into());
        }
        if !self.key_file.is_absolute()
            || self.key_file.as_os_str().len() > 4096
            || self
                .key_file
                .to_str()
                .is_none_or(|path| path.chars().any(char::is_control))
        {
            return Err("Enter an absolute OpenSSH private-key path".into());
        }
        Ok(())
    }

    pub(crate) fn command_for_exec(
        &self,
        snippets_path: Option<&Path>,
    ) -> Result<Cow<'_, str>, &'static str> {
        match self.source()? {
            ActionSource::Command(command) => Ok(Cow::Borrowed(command)),
            ActionSource::Script(script) => script.command().map(Cow::Owned),
            ActionSource::Snippet(link) => link
                .command(snippets_path.ok_or("The snippet library is unavailable")?)
                .map(Cow::Owned),
            ActionSource::Remote(_) => Err("SFTP actions require the SSH file subsystem"),
        }
    }

    pub(crate) fn remote_action(&self) -> Result<&RemoteAction, &'static str> {
        match self.source()? {
            ActionSource::Remote(action) => {
                action.validate()?;
                Ok(action)
            }
            _ => Err("This job does not contain an SFTP action"),
        }
    }

    fn source(&self) -> Result<ActionSource<'_>, &'static str> {
        match (
            self.command.as_str(),
            self.script.as_ref(),
            self.snippet.as_ref(),
            self.remote.as_ref(),
        ) {
            ("", Some(script), None, None) => Ok(ActionSource::Script(script)),
            ("", None, Some(link), None) => Ok(ActionSource::Snippet(link)),
            ("", None, None, Some(action)) if self.virtual_terminal.is_none() => {
                Ok(ActionSource::Remote(action))
            }
            ("", None, None, Some(_)) => Err("SFTP actions cannot use a virtual terminal"),
            (command, None, None, None)
                if !command.trim().is_empty()
                    && command.len() <= MAX_COMMAND_BYTES
                    && !command.contains('\0') =>
            {
                Ok(ActionSource::Command(command))
            }
            (_, None, None, None) => Err("Enter a nonempty SSH command of at most 16 KiB"),
            _ => Err("Choose one action for the SSH job"),
        }
    }
}

fn is_zero(value: &u16) -> bool {
    *value == 0
}

fn path_is_empty(path: &Path) -> bool {
    path.as_os_str().is_empty()
}

enum ActionSource<'a> {
    Command(&'a str),
    Script(&'a Script),
    Snippet(&'a SnippetLink),
    Remote(&'a RemoteAction),
}

#[derive(Clone, Eq, PartialEq)]
pub struct SnippetChoice {
    id: SnippetId,
    label: String,
}

impl fmt::Display for SnippetChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Page {
    #[default]
    Job,
    Action,
    Journal,
    Diagram,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Scope {
    #[default]
    All,
    Connected,
    Selected,
}

/// UI and runtime messages for the scheduler. Command and detail text is redacted from Debug.
#[derive(Clone)]
pub enum Message {
    Close,
    CloseConfirmed,
    HostedClose(u64, bool),
    Tick,
    Finished(Completion),
    Page(Page),
    ResizeJobs(pane_grid::ResizeEvent),
    ResizeJournal(pane_grid::ResizeEvent),
    JournalSearch(String),
    JournalSearchMode(JournalSearchMode),
    JournalColumns,
    JournalColumn(JournalColumn, bool),
    MenuStatusVisible(bool),
    New,
    Select(JobId),
    Edit(Field, String),
    EditCommand(text_editor::Action),
    EditScriptArguments(text_editor::Action),
    EditArchiveSources(text_editor::Action),
    EditArchiveExcludes(text_editor::Action),
    ArchiveFormat(ArchiveFormat),
    ArchivePassword(CredentialChoice),
    OpenArchiveVault,
    CommandSource(Source),
    ConnectionSelected(ConnectionChoice),
    ConnectionAdded(ConnectionChoice),
    ConnectionRemoved(u64),
    ScriptLanguage(Language),
    SnippetSelected(SnippetChoice),
    Enabled(bool),
    ScheduleEnabled(bool),
    RecursiveDelete(bool),
    VirtualTerminal(bool),
    CaptureChanges(bool),
    LimitIsSuccess(bool),
    Weekday(Weekday, bool),
    AllWeekdays(bool),
    FixedTimeInput(String),
    AddFixedTimes,
    SelectFixedTime(Time),
    RemoveFixedTime(Time),
    Dependency(JobId, bool),
    Mode(Mode),
    Save,
    Run(bool),
    CancelRun(RunId),
    Delete,
    SwitchChain,
    Confirm,
    Dismiss,
    SelectRecord(RunId),
    OpenRecording(RunId),
    DeleteRecord,
    DiagramScope(Scope),
    DiagramDirection,
    DiagramTheme,
    DiagramCopy,
    DiagramPath(String),
    DiagramExport(Format),
    DiagramExported(Result<PathBuf, String>),
    Zoom(f32),
}

impl fmt::Debug for Message {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Finished(completion) => formatter
                .debug_tuple("Finished")
                .field(&completion.id)
                .field(&completion.outcome)
                .finish_non_exhaustive(),
            Self::Close => formatter.write_str("Close"),
            Self::CloseConfirmed => formatter.write_str("CloseConfirmed"),
            Self::Tick => formatter.write_str("Tick"),
            _ => formatter.write_str("Scheduler edit"),
        }
    }
}

struct Active {
    job: JobId,
    started_at: SystemTime,
    trigger: RunTrigger,
    cancellation: kortty_jobs::Cancellation,
}

pub(crate) struct MenuRun<'a> {
    pub(crate) id: RunId,
    pub(crate) name: &'a str,
    pub(crate) cancelling: bool,
}

pub(crate) struct MenuDeadline<'a> {
    pub(crate) name: &'a str,
    pub(crate) due: SystemTime,
    pub(crate) local_time: String,
}

pub(crate) struct MenuStatus<'a> {
    pub(crate) running: Vec<MenuRun<'a>>,
    pub(crate) upcoming: Vec<MenuDeadline<'a>>,
}

struct PendingRecord {
    record: RunRecord,
    detail: Option<String>,
    recording: Option<JournalRecording>,
}

enum Confirmation {
    Close,
    Run(JobId, bool, Vec<Blocker>),
    Delete(JobId, Vec<String>),
    SwitchChain(JobId, bool),
    DeleteRecord(RunId),
}

/// One scheduler, store and job lifecycle shared by every window and tab.
pub struct SchedulerWindow {
    store: SchedulerStore,
    scheduler: Scheduler<SshCommand>,
    zone: TimeZone,
    execution_allowed: bool,
    recording_allowed: bool,
    recording_directory: PathBuf,
    snippet_path: PathBuf,
    snippet_choices: Vec<SnippetChoice>,
    connection_choices: Vec<ConnectionChoice>,
    archive_passwords: Vec<CredentialChoice>,
    draining: bool,
    active: BTreeMap<RunId, Active>,
    pending_starts: Vec<RunRequest<SshCommand>>,
    pending_records: VecDeque<PendingRecord>,
    retry_at: Option<SystemTime>,
    graph_revision: u64,
    diagram_revision: u64,
    status: Option<String>,
}

#[derive(Default)]
pub(crate) struct SchedulerView {
    page: Page,
    layout: Layout,
    draft: Draft,
    baseline: Option<(kortty_jobs::JobDefinition, SshCommand)>,
    dirty: bool,
    selected_run: Option<RunId>,
    journal_search: journal_search::Search,
    confirm: Option<Confirmation>,
    diagram: DiagramView,
    status: Option<String>,
    graph_revision: u64,
    diagram_revision: u64,
    pub(crate) hosted_close: Option<u64>,
}

impl SchedulerView {
    fn load_draft(&mut self, job: Option<&kortty_jobs::Job<SshCommand>>) {
        self.draft = job.map(Draft::from_job).unwrap_or_default();
        self.baseline = job.map(|job| (job.definition(), job.action().clone()));
        self.dirty = false;
    }

    pub(crate) fn dirty(&self) -> bool {
        self.dirty
    }

    pub(crate) fn report_status(&mut self, status: String) {
        self.status = Some(status);
    }

    pub(crate) fn request_close(&mut self) -> bool {
        self.confirm = self.dirty.then_some(Confirmation::Close);
        !self.dirty
    }

    pub(crate) fn close_pending(&self) -> bool {
        matches!(self.confirm, Some(Confirmation::Close))
    }

    pub(crate) fn cancel_close(&mut self) {
        if self.close_pending() {
            self.confirm = None;
        }
    }
}

impl SchedulerWindow {
    /// Opens an exclusive, native scheduler profile. Execution starts disabled.
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let store = SchedulerStore::open(&path).map_err(|error| error.to_string())?;
        let recording_directory = path.with_file_name("job-recordings");
        let mut scheduler = store
            .restore_with_actions::<SshCommand>()
            .map_err(|error| error.to_string())?;
        for job in scheduler.graph().jobs() {
            job.action()
                .clone()
                .validated()
                .map_err(|error| format!("Job {} has invalid settings ({error})", job.id()))?;
        }
        let (zone, status) = match TimeZone::try_system() {
            Ok(zone) => (zone, None),
            Err(_) => (
                TimeZone::UTC,
                Some("System time zone unavailable. Scheduler uses UTC.".into()),
            ),
        };
        scheduler
            .initialize_schedules(zone.clone(), SystemTime::now())
            .map_err(|error| error.to_string())?;
        virtual_terminal::remove_orphans(&recording_directory, store.journal_recordings());
        let mut window = Self {
            store,
            scheduler,
            zone,
            execution_allowed: false,
            recording_allowed: false,
            recording_directory,
            snippet_path: path.with_file_name(snippets::FILE_NAME),
            snippet_choices: Vec::new(),
            connection_choices: vec![ConnectionChoice::Manual],
            archive_passwords: Vec::new(),
            draining: false,
            active: BTreeMap::new(),
            pending_starts: Vec::new(),
            pending_records: VecDeque::new(),
            retry_at: None,
            graph_revision: 0,
            diagram_revision: 0,
            status,
        };
        window.refresh_snippets();
        Ok(window)
    }

    pub(crate) fn new_view(&self) -> SchedulerView {
        let mut view = SchedulerView {
            graph_revision: self.graph_revision,
            diagram_revision: self.diagram_revision,
            ..SchedulerView::default()
        };
        view.load_draft(self.scheduler.graph().jobs().first());
        view
    }

    pub(crate) fn sync_view(&self, view: &mut SchedulerView) {
        view.reconcile_journal_selection(self);
        if view.graph_revision != self.graph_revision {
            view.graph_revision = self.graph_revision;
            if !view.dirty
                && !self.draft_matches(view)
                && let Some(id) = &view.draft.id
            {
                let graph = self.scheduler.graph();
                view.load_draft(graph.job(id).or_else(|| graph.jobs().first()));
            }
            if !view.close_pending() && view.confirm.take().is_some() {
                view.status =
                    Some("Jobs changed. Review the current settings and try again.".into());
            }
        }
        if view.diagram_revision != self.diagram_revision {
            self.refresh_diagram(view);
        }
    }

    pub(crate) fn snippet_path(&self) -> &Path {
        &self.snippet_path
    }

    pub(crate) fn refresh_snippets(&mut self) {
        let mut choices = Library::open(self.snippet_path.clone()).map_or_else(
            |_| Vec::new(),
            |library| {
                library
                    .entries()
                    .iter()
                    .filter(|entry| Language::from_snippet(entry.language()).is_some())
                    .map(|entry| SnippetChoice {
                        id: entry.id(),
                        label: format!("{} ({})", entry.name(), entry.language()),
                    })
                    .collect()
            },
        );
        choices.sort_by_cached_key(|choice| choice.label.to_lowercase());
        self.snippet_choices = choices;
    }

    pub(crate) fn set_connections(&mut self, store: Option<&ConnectionStore>) {
        self.connection_choices.truncate(1);
        if let Some(store) = store {
            self.connection_choices
                .extend(
                    store
                        .connections()
                        .iter()
                        .map(|profile| ConnectionChoice::Saved {
                            reference: profile.into(),
                            name: profile.name.clone(),
                        }),
                );
            self.connection_choices[1..]
                .sort_by_cached_key(|choice| choice.to_string().to_lowercase());
        }
    }

    pub(crate) fn set_archive_passwords(&mut self, mut passwords: Vec<CredentialChoice>) {
        passwords.sort_by_cached_key(|choice| choice.label.to_lowercase());
        self.archive_passwords = passwords;
    }

    /// Applies the host application's execution policy and cancels work on revocation.
    pub fn set_execution_allowed(&mut self, allowed: bool) {
        if self.execution_allowed == allowed {
            return;
        }
        self.execution_allowed = allowed;
        if !allowed {
            self.cancel_runs();
        }
    }

    pub fn set_recording_allowed(&mut self, allowed: bool) {
        self.recording_allowed = allowed;
    }

    pub fn capture_directory(&self) -> Option<PathBuf> {
        self.recording_allowed
            .then(|| self.recording_directory.clone())
    }

    pub fn recording_path(&self, id: RunId) -> Option<PathBuf> {
        let name = self.store.journal_recording(id)?;
        Some(virtual_terminal::path_for(
            &self.recording_directory,
            id,
            name,
        ))
    }

    pub(crate) fn selected_recording_path(
        &self,
        view: &SchedulerView,
        id: RunId,
    ) -> Option<PathBuf> {
        if view.page != Page::Journal
            || view.selected_run != Some(id)
            || !view.journal_record_visible(self, id)
        {
            return None;
        }
        self.recording_path(id)
    }

    /// Earliest scheduler or journal retry deadline, independent of window visibility.
    pub fn next_due(&self) -> Option<SystemTime> {
        self.retry_at
            .or_else(|| self.can_run().then(|| self.scheduler.next_due()).flatten())
    }

    /// True while remote work has been dispatched or is waiting for its durable predecessor record.
    pub fn has_active_runs(&self) -> bool {
        !self.active.is_empty() || !self.pending_starts.is_empty()
    }

    pub(crate) fn menu_status(&self, now: SystemTime, limit: usize) -> Option<MenuStatus<'_>> {
        let graph = self.scheduler.graph();
        let scheduled = graph.jobs().iter().filter(|job| {
            job.enabled()
                && job
                    .schedule()
                    .is_some_and(kortty_jobs::JobSchedule::enabled)
        });
        if self.active.is_empty() && scheduled.clone().next().is_none() {
            return None;
        }
        let running = self
            .active
            .iter()
            .filter_map(|(&id, active)| {
                Some(MenuRun {
                    id,
                    name: graph.job(&active.job)?.name(),
                    cancelling: active.cancellation.is_requested(),
                })
            })
            .collect();
        let mut upcoming: Vec<_> = scheduled
            .filter(|job| self.can_run() && !self.scheduler.is_running(job.id()))
            .filter_map(|job| Some((self.scheduler.scheduled_at(job.id())?, job)))
            .filter(|(due, _)| *due > now)
            .collect();
        upcoming.sort_by(|(time_a, job_a), (time_b, job_b)| {
            time_a.cmp(time_b).then_with(|| job_a.id().cmp(job_b.id()))
        });
        let upcoming = upcoming
            .into_iter()
            .take(limit)
            .map(|(due, job)| MenuDeadline {
                name: job.name(),
                due,
                local_time: self.local_time(due),
            })
            .collect();
        Some(MenuStatus { running, upcoming })
    }

    pub(crate) fn cancel_menu_run(&self, ticket: u64) -> bool {
        self.active.iter().any(|(&id, active)| {
            id.value() == ticket
                && !active.cancellation.is_requested()
                && self.scheduler.cancel(id).is_ok()
        })
    }

    pub(crate) fn requires_awake(&self) -> bool {
        self.has_active_runs()
            || (self.execution_allowed
                && !self.draining
                && self.scheduler.graph().jobs().iter().any(|job| {
                    job.enabled()
                        && job
                            .schedule()
                            .is_some_and(kortty_jobs::JobSchedule::enabled)
                }))
    }

    /// Discards an unsaved draft after the user confirms the pending close.
    pub(crate) fn confirm_close(&self, view: &mut SchedulerView) -> bool {
        if !view.close_pending() {
            return false;
        }
        view.load_draft(self.selected_job(view));
        view.confirm = None;
        true
    }

    /// Stops new runs and signals cancellation to work that was already dispatched.
    pub fn begin_drain(&mut self) {
        self.draining = true;
        self.scheduler.begin_drain();
        self.cancel_runs();
    }

    fn cancel_runs(&mut self) {
        for &id in self.active.keys() {
            let _ = self.scheduler.cancel(id);
        }
        self.cancel_pending_starts(SystemTime::now());
    }

    /// The desktop dispatches a runtime event once, independently of open views.
    pub(crate) fn update_runtime(&mut self, message: Message) -> Vec<RunRequest<SshCommand>> {
        let now = SystemTime::now();
        let result = match message {
            Message::Tick => {
                if !self.flush(now) || !self.can_run() {
                    return Vec::new();
                }
                self.scheduler.tick(now).map_err(|error| error.to_string())
            }
            Message::Finished(completion) => {
                match self
                    .scheduler
                    .complete(completion.id, completion.outcome, now)
                {
                    Ok(batch) => {
                        self.active.remove(&completion.id);
                        return self.receive(batch, completion.detail, completion.recording, now);
                    }
                    Err(error) => Err(error.to_string()),
                }
            }
            _ => return Vec::new(),
        };
        match result {
            Ok(batch) => self.receive(batch, None, None, now),
            Err(error) => {
                self.status = Some(error);
                Vec::new()
            }
        }
    }

    pub(crate) fn update_view(
        &mut self,
        view: &mut SchedulerView,
        message: Message,
    ) -> Vec<RunRequest<SshCommand>> {
        self.sync_view(view);
        if view.close_pending() && !matches!(message, Message::Dismiss) {
            return Vec::new();
        }
        let now = SystemTime::now();
        let result = match message {
            Message::Confirm => self.confirm(view, now),
            Message::Run(cascade) if self.can_run() && !view.dirty => {
                self.ask_run(view, cascade, now)
            }
            message => {
                self.apply_view(view, message, now);
                self.sync_view(view);
                return Vec::new();
            }
        };
        let requests = match result {
            Ok(batch) => self.receive(batch, None, None, now),
            Err(error) => {
                view.status = Some(error);
                Vec::new()
            }
        };
        self.sync_view(view);
        requests
    }

    fn apply_view(&mut self, view: &mut SchedulerView, message: Message, now: SystemTime) {
        match message {
            Message::Page(page) => {
                view.page = page;
                if page != Page::Journal {
                    view.journal_search.collapse();
                }
                self.refresh_diagram(view);
            }
            Message::ResizeJobs(event) => view.layout.jobs.resize(event.split, event.ratio),
            Message::ResizeJournal(event) => view.layout.journal.resize(event.split, event.ratio),
            Message::JournalSearch(search) if view.page == Page::Journal => {
                view.journal_search.set_text(search);
            }
            Message::JournalSearchMode(mode) if view.page == Page::Journal => {
                view.journal_search.set_mode(mode);
            }
            Message::JournalColumns if view.page == Page::Journal => {
                view.journal_search.toggle_columns();
            }
            Message::JournalColumn(column, enabled) if view.page == Page::Journal => {
                view.journal_search.set_column(column, enabled);
            }
            Message::New => {
                view.load_draft(None);
                view.page = Page::Job;
                view.confirm = None;
                view.status = None;
            }
            Message::Select(id) => self.select(view, &id),
            Message::Edit(field, value) => {
                view.draft.set(field, value);
                view.dirty = true;
            }
            Message::EditCommand(action) => {
                view.dirty |= action.is_edit();
                view.draft.edit_command(action);
            }
            Message::EditScriptArguments(action) => {
                view.dirty |= action.is_edit();
                view.draft.edit_script_arguments(action);
            }
            Message::EditArchiveSources(action) => {
                view.dirty |= action.is_edit();
                view.draft.edit_archive_sources(action);
            }
            Message::EditArchiveExcludes(action) => {
                view.dirty |= action.is_edit();
                view.draft.edit_archive_excludes(action);
            }
            Message::ArchiveFormat(format) => {
                view.draft.archive_format = format;
                view.dirty = true;
            }
            Message::ArchivePassword(choice) => {
                if self
                    .archive_passwords
                    .iter()
                    .any(|entry| entry.id == choice.id)
                {
                    view.draft.archive_password = Some(choice.id);
                    view.dirty = true;
                }
            }
            Message::OpenArchiveVault => {}
            Message::CommandSource(source) => {
                view.draft.select_source(source);
                view.dirty = true;
            }
            Message::ConnectionSelected(connection) => {
                view.draft.select_connection(connection);
                view.dirty = true;
            }
            Message::ConnectionAdded(connection) => match view.draft.add_connection(connection) {
                Ok(changed) => view.dirty |= changed,
                Err(error) => view.status = Some(error.into()),
            },
            Message::ConnectionRemoved(id) => {
                view.dirty |= view.draft.remove_connection(id);
            }
            Message::ScriptLanguage(language) => {
                view.draft.script_language = language;
                view.dirty = true;
            }
            Message::SnippetSelected(choice) => {
                view.draft.snippet_id = Some(choice.id);
                view.dirty = true;
            }
            Message::Enabled(enabled) => {
                view.draft.enabled = enabled;
                view.dirty = true;
            }
            Message::ScheduleEnabled(enabled) => {
                view.draft.schedule_enabled = enabled;
                view.dirty = true;
            }
            Message::RecursiveDelete(recursive) => {
                view.draft.set_recursive_delete(recursive);
                view.dirty = true;
            }
            Message::VirtualTerminal(enabled) => {
                view.draft.virtual_terminal.enabled = enabled;
                view.dirty = true;
            }
            Message::CaptureChanges(enabled) => {
                view.draft.virtual_terminal.capture_changes = enabled;
                view.dirty = true;
            }
            Message::LimitIsSuccess(enabled) => {
                view.draft.virtual_terminal.limit_is_success = enabled;
                view.dirty = true;
            }
            Message::Weekday(day, checked) => {
                view.draft.set_day(day, checked);
                view.dirty = true;
            }
            Message::AllWeekdays(checked) if view.page == Page::Job => {
                view.dirty |= view.draft.set_all_days(checked);
            }
            Message::FixedTimeInput(input) if view.page == Page::Job => {
                view.status = view.draft.set_fixed_input(input).err();
            }
            Message::AddFixedTimes if view.page == Page::Job => {
                match view.draft.add_fixed_times() {
                    Ok(changed) => {
                        view.dirty |= changed;
                        view.status = (!changed).then(|| "These times are already listed.".into());
                    }
                    Err(error) => view.status = Some(error),
                }
            }
            Message::SelectFixedTime(time) if view.page == Page::Job => {
                view.draft.select_fixed_time(time);
            }
            Message::RemoveFixedTime(time) if view.page == Page::Job => {
                view.dirty |= view.draft.remove_fixed_time(time);
            }
            Message::Dependency(id, checked) => {
                view.draft.set_dependency(id, checked);
                view.dirty = true;
            }
            Message::Mode(mode) => {
                view.draft.mode = mode;
                view.dirty = true;
            }
            Message::Save if self.can_edit() => {
                view.status = Some(self.save(view, now).unwrap_or_else(|error| error));
            }
            Message::Delete if self.can_edit() && self.draft_matches(view) => self.ask_delete(view),
            Message::SwitchChain if self.can_edit() && self.draft_matches(view) => {
                if let Some(job) = self.selected_job(view) {
                    view.confirm =
                        Some(Confirmation::SwitchChain(job.id().clone(), !job.enabled()));
                }
            }
            Message::CancelRun(id)
                if view.page == Page::Journal && view.journal_active_visible(self, id) =>
            {
                view.status = Some(match self.scheduler.cancel(id) {
                    Ok(()) => "Cancellation requested".into(),
                    Err(error) => error.to_string(),
                });
            }
            Message::Dismiss => view.confirm = None,
            Message::Close => {
                view.request_close();
            }
            Message::SelectRecord(id)
                if view.page == Page::Journal && view.journal_record_visible(self, id) =>
            {
                view.selected_run = Some(id);
            }
            Message::DeleteRecord if view.page == Page::Journal && self.can_edit() => {
                view.confirm = view.selected_run.map(Confirmation::DeleteRecord);
            }
            Message::DiagramScope(scope) => {
                view.diagram.scope = scope;
                self.refresh_diagram(view);
            }
            Message::DiagramDirection => {
                view.diagram.toggle_direction();
                self.refresh_diagram(view);
            }
            Message::DiagramTheme => {
                view.diagram.toggle_theme();
                self.refresh_diagram(view);
            }
            Message::DiagramPath(path) => view.diagram.path = path,
            Message::DiagramExported(result) => {
                view.status = Some(match result {
                    Ok(path) => format!("Saved {}", path.display()),
                    Err(error) => format!("Diagram export failed ({error})"),
                });
            }
            Message::Zoom(factor) => view.diagram.zoom(factor),
            _ => {}
        }
    }

    fn ask_run(
        &mut self,
        view: &mut SchedulerView,
        cascade: bool,
        now: SystemTime,
    ) -> Result<RunBatch<SshCommand>, String> {
        let Some(id) = view.draft.id.clone() else {
            return Ok(RunBatch::default());
        };
        let blockers = self
            .scheduler
            .blockers(&id, now)
            .map_err(|error| error.to_string())?;
        if blockers.is_empty() {
            self.start(&id, cascade, now)
        } else {
            view.confirm = Some(Confirmation::Run(id, cascade, blockers));
            Ok(RunBatch::default())
        }
    }

    fn start(
        &mut self,
        id: &JobId,
        cascade: bool,
        now: SystemTime,
    ) -> Result<RunBatch<SshCommand>, String> {
        let result = if cascade {
            self.scheduler.start_cascade(id, now)
        } else {
            self.scheduler.start_manual(id, now)
        };
        result.map_err(|error| error.to_string())
    }

    fn confirm(
        &mut self,
        view: &mut SchedulerView,
        now: SystemTime,
    ) -> Result<RunBatch<SshCommand>, String> {
        match view.confirm.take() {
            Some(Confirmation::Run(id, cascade, _)) if self.can_run() && !view.dirty => {
                return self.start(&id, cascade, now);
            }
            Some(Confirmation::Delete(id, _)) if self.can_edit() && self.draft_matches(view) => {
                self.delete(view, &id, now)?;
                view.status = Some("Job deleted".into());
            }
            Some(Confirmation::SwitchChain(id, enabled))
                if self.can_edit() && self.draft_matches(view) =>
            {
                let mut next = self
                    .store
                    .restore_with_actions::<SshCommand>()
                    .map_err(|error| error.to_string())?;
                next.initialize_schedules(self.zone.clone(), now)
                    .map_err(|error| error.to_string())?;
                next.set_chain_enabled(&id, enabled, now)
                    .map_err(|error| error.to_string())?;
                self.store
                    .commit(next.graph(), &[])
                    .map_err(|error| error.to_string())?;
                self.scheduler = next;
                self.graph_changed();
                self.select(view, &id);
                view.status = Some(
                    if enabled {
                        "Chain enabled"
                    } else {
                        "Chain disabled"
                    }
                    .into(),
                );
            }
            Some(Confirmation::DeleteRecord(id))
                if self.can_edit()
                    && view.page == Page::Journal
                    && view.selected_run == Some(id)
                    && view.journal_record_visible(self, id) =>
            {
                let recording = self.store.journal_recording(id);
                self.store
                    .delete_records(&[id])
                    .map_err(|error| error.to_string())?;
                let recording_removed = recording.is_none_or(|name| {
                    virtual_terminal::remove_recording(&self.recording_directory, id, name).is_ok()
                });
                self.store.apply_history(&mut self.scheduler);
                self.diagram_changed();
                view.selected_run = None;
                view.status = Some(
                    if recording_removed {
                        "Journal entry deleted"
                    } else {
                        "Journal entry deleted. Could not remove its file from job-recordings."
                    }
                    .into(),
                );
            }
            _ => {}
        }
        Ok(RunBatch::default())
    }

    fn ask_delete(&self, view: &mut SchedulerView) {
        let Some(id) = view.draft.id.clone() else {
            return;
        };
        let dependents = self
            .scheduler
            .graph()
            .jobs()
            .iter()
            .filter(|job| job.dependencies().contains(&id))
            .map(|job| job.name().to_owned())
            .collect();
        view.confirm = Some(Confirmation::Delete(id, dependents));
    }

    fn save(&mut self, view: &mut SchedulerView, now: SystemTime) -> Result<String, String> {
        if !self.draft_matches(view) {
            return Err("This job changed in another view. Select it again before saving.".into());
        }
        let id = match &view.draft.id {
            Some(id) => id.clone(),
            None => new_id()?,
        };
        let replacement = view.draft.build(id.clone())?;
        let mut jobs = self
            .scheduler
            .graph()
            .jobs()
            .iter()
            .map(|job| {
                job.definition()
                    .with_action(job.action().clone())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(index) = jobs.iter().position(|job| job.id() == &id) {
            jobs[index] = replacement;
        } else {
            jobs.push(replacement);
        }
        self.replace_graph(JobGraph::new(jobs).map_err(|error| error.to_string())?, now)?;
        self.select(view, &id);
        Ok("Job saved".into())
    }

    fn delete(
        &mut self,
        view: &mut SchedulerView,
        id: &JobId,
        now: SystemTime,
    ) -> Result<(), String> {
        let jobs = self
            .scheduler
            .graph()
            .jobs()
            .iter()
            .filter(|job| job.id() != id)
            .map(|job| {
                let mut definition = job.definition();
                definition
                    .dependencies
                    .retain(|predecessor| predecessor != id);
                definition
                    .with_action(job.action().clone())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.replace_graph(JobGraph::new(jobs).map_err(|error| error.to_string())?, now)?;
        view.load_draft(self.scheduler.graph().jobs().first());
        self.refresh_diagram(view);
        Ok(())
    }

    fn replace_graph(
        &mut self,
        graph: JobGraph<SshCommand>,
        now: SystemTime,
    ) -> Result<(), String> {
        let mut next = Scheduler::new(graph);
        next.initialize_schedules(self.zone.clone(), now)
            .map_err(|error| error.to_string())?;
        self.store
            .commit_with_actions(next.graph(), &[], &[])
            .map_err(|error| error.to_string())?;
        self.store.apply_history(&mut next);
        self.scheduler = next;
        self.graph_changed();
        Ok(())
    }

    fn select(&self, view: &mut SchedulerView, id: &JobId) {
        if let Some(job) = self.scheduler.graph().job(id) {
            view.load_draft(Some(job));
            view.confirm = None;
            view.status = None;
            self.refresh_diagram(view);
        }
    }

    pub(crate) fn select_journal_job(&self, view: &mut SchedulerView, id: &JobId) {
        if self.scheduler.graph().job(id).is_none() || view.hosted_close.is_some() {
            return;
        }
        if view.dirty && view.draft.id.as_ref() != Some(id) {
            view.status = Some("Save or discard your draft before selecting another job.".into());
            return;
        }
        if !view.dirty {
            self.select(view, id);
        }
        view.page = Page::Job;
    }

    fn can_run(&self) -> bool {
        self.execution_allowed && !self.draining && self.pending_records.is_empty()
    }

    fn can_edit(&self) -> bool {
        !self.draining && !self.has_active_runs() && self.pending_records.is_empty()
    }

    fn selected_job(&self, view: &SchedulerView) -> Option<&kortty_jobs::Job<SshCommand>> {
        self.scheduler.graph().job(view.draft.id.as_ref()?)
    }

    fn draft_matches(&self, view: &SchedulerView) -> bool {
        let Some((definition, action)) = &view.baseline else {
            return view.draft.id.is_none();
        };
        self.selected_job(view)
            .is_some_and(|job| job.definition() == *definition && job.action() == action)
    }

    fn receive(
        &mut self,
        batch: RunBatch<SshCommand>,
        detail: Option<String>,
        mut recording: Option<JournalRecording>,
        now: SystemTime,
    ) -> Vec<RunRequest<SshCommand>> {
        let changed = !batch.finished.is_empty() || !batch.started.is_empty();
        let mut detail = detail.as_deref().and_then(safe_detail);
        self.pending_records
            .extend(batch.finished.into_iter().map(|record| PendingRecord {
                record,
                // A completion's own record is always first, before derived blocked runs.
                detail: detail.take(),
                recording: recording.take(),
            }));
        self.pending_starts.extend(batch.started);
        let starts = if self.flush(now) && self.can_run() {
            let starts = std::mem::take(&mut self.pending_starts);
            for run in &starts {
                self.active.insert(
                    run.id,
                    Active {
                        job: run.job.clone(),
                        started_at: run.started_at,
                        trigger: run.trigger,
                        cancellation: run.cancellation.clone(),
                    },
                );
            }
            starts
        } else {
            Vec::new()
        };
        if changed || !starts.is_empty() {
            self.diagram_changed();
        }
        starts
    }

    fn flush(&mut self, now: SystemTime) -> bool {
        while !self.pending_records.is_empty() {
            let chunk = self.pending_records.iter().take(MAX_JOURNAL_RECORDS);
            let records: Vec<_> = chunk.clone().map(|item| item.record.clone()).collect();
            let details: Vec<_> = chunk
                .clone()
                .filter_map(|item| Some((item.record.id, item.detail.clone()?)))
                .collect();
            let recordings: Vec<_> = chunk
                .filter_map(|item| Some((item.record.id, item.recording?)))
                .collect();
            let previous: Vec<_> = self.store.journal_recordings().collect();
            if let Err(error) = self.store.commit_records_with_recordings(
                self.scheduler.graph(),
                &records,
                &details,
                &recordings,
            ) {
                self.retry_at = now.checked_add(SAVE_RETRY);
                self.status = Some(format!(
                    "Could not save the run journal ({error}). New runs are paused."
                ));
                return false;
            }
            for (id, name) in previous {
                if self.store.journal_recording(id).is_none() {
                    let _ = virtual_terminal::remove_recording(&self.recording_directory, id, name);
                }
            }
            self.pending_records.drain(..records.len());
        }
        if self.retry_at.take().is_some() {
            self.status = Some("Run journal saved".into());
        }
        true
    }

    fn cancel_pending_starts(&mut self, now: SystemTime) {
        for request in std::mem::take(&mut self.pending_starts) {
            if let Ok(batch) = self
                .scheduler
                .complete(request.id, RunOutcome::Cancelled, now)
            {
                self.pending_records
                    .extend(batch.finished.into_iter().map(|record| PendingRecord {
                        record,
                        detail: None,
                        recording: None,
                    }));
            }
        }
        self.flush(now);
        self.diagram_changed();
    }

    fn graph_changed(&mut self) {
        self.graph_revision = self.graph_revision.wrapping_add(1);
        self.diagram_changed();
    }

    fn diagram_changed(&mut self) {
        self.diagram_revision = self.diagram_revision.wrapping_add(1);
    }

    fn refresh_diagram(&self, view: &mut SchedulerView) {
        view.diagram_revision = self.diagram_revision;
        if view.page == Page::Diagram {
            view.diagram
                .refresh(&self.scheduler, view.draft.id.as_ref());
        }
    }

    fn local_time(&self, instant: SystemTime) -> String {
        let Ok(timestamp) = instant.try_into() else {
            return "Unavailable".into();
        };
        let date_time = self.zone.to_datetime(timestamp);
        format!(
            "{} {:02}:{:02}:{:02}",
            date_time.date(),
            date_time.hour(),
            date_time.minute(),
            date_time.second()
        )
    }
}

impl SchedulerView {
    pub(crate) fn diagram_mermaid(&self) -> Option<String> {
        Some(self.diagram.preview.as_ref()?.mermaid.clone())
    }

    pub(crate) fn export_diagram(&self, format: Format) -> Task<Message> {
        let Some(preview) = &self.diagram.preview else {
            return Task::none();
        };
        let path = PathBuf::from(self.diagram.path.trim());
        let scene = Arc::clone(&preview.scene);
        let theme = self.diagram.theme;
        Task::future(async move {
            Message::DiagramExported(
                diagram_window::export(&path, &scene, theme, format)
                    .map(|()| path)
                    .map_err(|error| error.to_string()),
            )
        })
    }
}

fn safe_detail(detail: &str) -> Option<String> {
    let mut safe = String::new();
    for ch in detail.chars() {
        if ch.is_control()
            || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            continue;
        }
        if safe.len() + ch.len_utf8() > 1024 {
            break;
        }
        safe.push(ch);
    }
    (!safe.trim().is_empty()).then_some(safe)
}

fn new_id() -> Result<JobId, String> {
    use std::fmt::Write as _;
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|_| "Could not allocate a job identifier")?;
    let mut id = String::from("job_");
    for byte in random {
        let _ = write!(id, "{byte:02x}");
    }
    JobId::new(id).map_err(|error| error.to_string())
}
