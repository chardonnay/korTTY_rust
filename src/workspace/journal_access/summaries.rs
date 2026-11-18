//! Workspace-owned summary jobs outlive the journal manager and publish through journal storage.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use iced::futures::{StreamExt as _, stream::BoxStream};
use iced::widget::{button, checkbox, column, pick_list, row, scrollable, text, text_input};
use iced::{Element, Length, Subscription, Task, window};
use jiff::Timestamp;

use crate::ai_manager::ProfileRef;
use crate::local_ai::{Cancel, Profile};
use crate::session_journal::automation::retention::ClosingSummary;
use crate::session_journal::summary_ai::{Preview, Reporter, Run, Stage, Work};
use crate::session_journal::summary_job::{Closed, Mode as PassMode, Outcome, Pass};
use crate::session_journal::summary_plan::{End, EvidenceLimits, RUNNING_AFTER};
use crate::session_journal::summary_queue::{
    AutomaticApproval, AutomationProfile, AutomationSource, MAX_PENDING, ProfileBinding,
    ProfileChoice, Queue, Receipt, SourceApproval,
};
use crate::session_journal::summary_store::Progress;
use crate::session_journal::viewer::Source;
use crate::session_journal::{
    self, Capture, Catalog, ConnectionConfig, JournalProfileSelection, Summary,
};
use crate::terminal_logging::{Redactor, Secrets};
use crate::workspace::terminal_ai::mask;

use super::{Desktop, Manager, Message as JournalMessage, Mode, Workspace, label};

const CLOCK: Duration = Duration::from_secs(1);
const IMPORT_CHANGED: &str =
    "The imported journal profile changed. Reopen summary settings and select it again.";
const EVIDENCE_IMPORT_CHANGED: &str =
    "The imported Java evidence settings changed. Select the current values again.";
const TITLE_REQUEST_SCOPE: &str = "The additional request includes the masked connection name and journal summary evidence when the journal still has a default title.";
pub(in crate::workspace) const PROGRESS_TITLE: &str = "Journal Summary Progress";

mod automation_probe;
mod https;
pub(in crate::workspace) mod preflight;
mod progress;
#[cfg(test)]
mod tests;
mod vision;

#[derive(Clone, Debug)]
pub enum Event {
    Review(bool),
    Pick(Box<Choice>),
    PickAutomation(Box<AutomationChoice>),
    UseImportedProfile(bool),
    PickAutomationSource(AutomationSourceChoice),
    PickSourceProfile(Box<Choice>),
    SourceFullOutput(bool),
    SourceTitleRequest(bool),
    ApproveAutomationSource,
    RevokeAutomationSource,
    Screenshots(bool),
    FullOutput(bool),
    SummarizeNow,
    Start,
    Dismiss,
    Settings,
    Enable(bool),
    Interval(String),
    CustomEvidence(bool),
    EvidenceLines(String),
    EvidenceTokens(String),
    UseImportedEvidence,
    UseImportedPreferences,
    AutomaticTitle(bool),
    RecordingTitleRequest(bool),
    AutomationTitleRequest(bool),
    SaveSettings,
    Cancel,
    Retry,
    ShowProgress,
    CloseProgress,
    CancelProgress(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Choice {
    profile: Option<ProfileChoice>,
    label: String,
}

impl Choice {
    fn reviewed(&self) -> Option<&ProfileChoice> {
        self.profile
            .as_ref()
            .filter(|choice| choice.binding().is_some())
    }
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AutomationChoice {
    Inherit,
    Local,
    Profile(Box<Choice>),
}

impl fmt::Display for AutomationChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inherit => formatter.write_str("Use the terminal recording profile"),
            Self::Local => formatter.write_str("Use local journal profile"),
            Self::Profile(choice) => choice.fmt(formatter),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutomationSourceChoice {
    source: AutomationSource,
    label: String,
}

impl fmt::Display for AutomationSourceChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

#[derive(Clone, Debug)]
pub enum Worker {
    StartupRecovered(Result<usize, String>),
    Pulse,
    Progress(u64),
    Finished(u64, Result<Outcome, String>),
}

#[derive(Default)]
pub(in crate::workspace) struct State {
    queue: Queue,
    automation_probes: Arc<automation_probe::Outcomes>,
    startup_recovery: Option<(PathBuf, Arc<AtomicBool>)>,
    active: Option<Active>,
    live: Vec<Live>,
    paused: HashSet<String>,
    reviews: HashMap<window::Id, Review>,
    status: Option<String>,
    details: Option<progress::Details>,
    work_rates: progress::ProfileRates,
    batch_progress: HashMap<String, progress::BatchProgress>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RowStatus {
    Running,
    Waiting,
    Finishing,
    FinishWaiting,
}

impl RowStatus {
    fn for_receipt(receipt: &Receipt, running: bool) -> Self {
        match (receipt.automatic(), running) {
            (false, true) => Self::Running,
            (false, false) => Self::Waiting,
            (true, true) => Self::Finishing,
            (true, false) => Self::FinishWaiting,
        }
    }

    pub(super) fn running(self) -> bool {
        matches!(self, Self::Running | Self::Finishing)
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Running => "↻ re-evaluating",
            Self::Waiting => "waiting to re-evaluate",
            Self::Finishing => "↻ finishing",
            Self::FinishWaiting => "waiting to finish",
        }
    }
}

struct Live {
    capture: Arc<Capture>,
    receipt: Receipt,
    interval: Duration,
    next: Instant,
    redactor: Arc<Redactor>,
    force: bool,
    running_preview: Arc<Mutex<Option<(u64, Instant)>>>,
}

struct LivePass {
    capture: Arc<Capture>,
    forced: bool,
    running_preview: Arc<Mutex<Option<(u64, Instant)>>>,
    now: Instant,
    age_now: Timestamp,
}

struct Active {
    ticket: u64,
    receipt: Receipt,
    periodic: bool,
    started: Instant,
    stop: Arc<AtomicBool>,
    transport: Option<Cancel>,
    vision: Option<(Profile, Cancel)>,
    vision_profile: Option<Profile>,
    progress: Arc<Mutex<PassProgress>>,
    visible: PassProgress,
    configuration: Arc<crate::ai_manager::Configuration>,
    profile: Option<Profile>,
    settings: session_journal::topic_ai_settings::Preferences,
    vault_revision: Option<Weak<()>>,
}

#[derive(Clone, Default)]
struct PassProgress {
    outcome: Outcome,
    preview: Option<Preview>,
    work: progress::WorkProgress,
    following: HashMap<String, f64>,
}

impl Active {
    fn cancel(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.transport.take();
        self.vision.take();
        self.visible.preview = None;
    }
}

enum Review {
    Run {
        receipts: Vec<(Source, String)>,
        mode: PassMode,
        choices: Vec<Choice>,
        selected: Choice,
        screenshots: bool,
        full_output: bool,
        automatic_title: bool,
        evidence_limits: Option<EvidenceLimits>,
    },
    Settings {
        enabled: bool,
        interval: String,
        imported_preferences: Option<(Option<u16>, Option<bool>)>,
        automatic_title: bool,
        recording_title_request: bool,
        automation_title_request: bool,
        full_output: bool,
        evidence_limits: EvidenceDraft,
        choices: Vec<Choice>,
        selected: Choice,
        automation_choices: Vec<AutomationChoice>,
        automation: AutomationChoice,
        automation_edited: bool,
        imported_recording: bool,
        imported_automation: bool,
        automation_source: Option<AutomationSourceChoice>,
        source_profile: Option<Box<Choice>>,
        source_full_output: bool,
        source_title_request: bool,
    },
}

struct EvidenceDraft {
    custom: bool,
    lines: String,
    tokens: String,
    imported: Option<(EvidenceLimits, bool)>,
}

impl EvidenceDraft {
    fn new(limits: Option<EvidenceLimits>) -> Self {
        let custom = limits.is_some();
        let limits = limits.unwrap_or_default();
        Self {
            custom,
            lines: limits.max_lines_per_direction.to_string(),
            tokens: limits.context_tokens.to_string(),
            imported: None,
        }
    }

    fn limits(&self) -> Result<Option<EvidenceLimits>, &'static str> {
        if !self.custom {
            return Ok(None);
        }
        let lines = self
            .lines
            .parse()
            .map_err(|_| "Enter a valid input and output line limit.")?;
        let tokens = self
            .tokens
            .parse()
            .map_err(|_| "Enter a valid context token budget.")?;
        let limits = EvidenceLimits {
            max_lines_per_direction: lines,
            context_tokens: tokens,
        };
        limits
            .valid()
            .then_some(Some(limits))
            .ok_or("Enter evidence limits within the displayed ranges.")
    }

    fn view(&self) -> Element<'_, Event> {
        column![
            checkbox(self.custom).label("Use custom summary evidence limits").on_toggle(Event::CustomEvidence),
            self.custom.then(|| column![
                row![text("Lines per direction").width(185), text_input("Line limit", &self.lines).on_input(Event::EvidenceLines)].spacing(8),
                row![text("Context token budget").width(185), text_input("Token budget", &self.tokens).on_input(Event::EvidenceTokens)].spacing(8),
                text("Enter 0–1,000,000 lines and 1,000–10,000,000 tokens. Use 0 lines to let the token budget and app limits determine the excerpt size.").size(13),
            ].spacing(8)),
        ].spacing(8).into()
    }
}

impl State {
    pub(in crate::workspace) fn load(&mut self, profile: &Path) {
        self.automation_probes = Arc::default();
        self.work_rates = progress::ProfileRates::default();
        self.batch_progress.clear();
        match Queue::load(profile) {
            Ok(queue) => self.queue = queue,
            Err(_) => self.status = Some("Journal summary settings could not be loaded.".into()),
        }
    }

    fn subscription(&self) -> Subscription<Desktop> {
        if self.startup_recovery.is_some()
            || self.live.is_empty()
                && self
                    .queue
                    .pending()
                    .iter()
                    .all(|receipt| self.paused.contains(&receipt.generation))
                && self.active.is_none()
        {
            Subscription::none()
        } else {
            Subscription::run(clock)
        }
    }

    pub(in crate::workspace) fn shutdown(&mut self) {
        if let Some(active) = &mut self.active {
            active.cancel();
        }
    }

    pub(in crate::workspace) fn cancel_startup_recovery(&self) {
        if let Some((_, stop)) = &self.startup_recovery {
            stop.store(true, Ordering::Release);
        }
    }

    pub(in crate::workspace) fn close_review(&mut self, id: window::Id) {
        self.reviews.remove(&id);
    }

    fn row_statuses(&self, catalog: &Catalog) -> Vec<Option<RowStatus>> {
        let active = self.active.as_ref();
        let closing_enabled = self.queue.preferences().enabled;
        let mut waiting = HashMap::<&Path, Vec<&Receipt>>::new();
        for receipt in self.queue.pending().iter().filter(|receipt| {
            !self.paused.contains(&receipt.generation)
                && active.is_none_or(|active| active.receipt.generation != receipt.generation)
                && (receipt.mode != PassMode::Closing || closing_enabled)
                && !self.live.iter().any(|live| {
                    live.receipt.generation == receipt.generation
                        && live.capture.status() == session_journal::Status::Recording
                })
        }) {
            waiting
                .entry(receipt.source.directory())
                .or_default()
                .push(receipt);
        }
        let active =
            active.filter(|active| !active.periodic && !active.stop.load(Ordering::Acquire));
        if active.is_none() && waiting.is_empty() {
            return Vec::new();
        }
        catalog
            .journals
            .iter()
            .map(|journal| {
                let path = journal.directory.as_path();
                let running = active.filter(|active| active.receipt.source.directory() == path);
                let queued = waiting.get(path);
                if running.is_none() && queued.is_none() {
                    return None;
                }
                let source = Source::catalog(journal).ok()?.summary_receipt();
                if let Some(active) = running.filter(|active| active.receipt.source == source) {
                    return Some(RowStatus::for_receipt(&active.receipt, true));
                }
                let receipt = queued?.iter().find(|receipt| receipt.source == source)?;
                Some(RowStatus::for_receipt(receipt, false))
            })
            .collect()
    }

    fn pause_batch(&mut self, batch: &str) {
        self.paused.extend(
            self.queue
                .pending()
                .iter()
                .filter(|receipt| receipt.batch == batch)
                .map(|receipt| receipt.generation.clone()),
        );
    }
}

fn clock() -> BoxStream<'static, Desktop> {
    let (send, receive) = async_channel::bounded(1);
    let _ = thread::Builder::new()
        .name("kortty-journal-summary-clock".into())
        .spawn(move || {
            while !send.is_closed() {
                thread::sleep(CLOCK);
                let _ = send.try_send(());
            }
        });
    receive
        .map(|()| Desktop::JournalSummary(Worker::Pulse))
        .boxed()
}

fn pins_lm_studio_auto(receipt: &Receipt) -> bool {
    receipt
        .profile
        .as_ref()
        .is_some_and(ProfileChoice::pins_lm_studio_auto)
}

impl Workspace {
    pub(in crate::workspace) fn recover_journals_at_startup(&mut self) -> Task<Desktop> {
        let Ok(root) = self.journal_directory() else {
            return self.start_next_journal_summary();
        };
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_root = root.clone();
        let (send, receive) = async_channel::bounded(1);
        let (finished, done) = async_channel::bounded(1);
        let spawn = thread::Builder::new()
            .name("kortty-journal-recovery".into())
            .spawn(move || {
                let _finished = finished;
                let result =
                    session_journal::summary_store::recover_interactive(&worker_root, &worker_stop);
                let _ = send.try_send(result.map_err(|error| error.to_string()));
            });
        if let Err(error) = spawn {
            eprintln!("kortty: could not start session journal recovery ({error})");
            return self.start_next_journal_summary();
        }
        self.track_session_shutdown(done);
        self.journal_summaries.startup_recovery = Some((root, stop));
        Task::future(async move {
            Desktop::JournalSummary(Worker::StartupRecovered(
                receive
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("Session journal recovery stopped.".into())),
            ))
        })
    }

    pub(super) fn journal_summary_row_statuses(&self, manager: &Manager) -> Vec<Option<RowStatus>> {
        manager
            .catalog
            .as_deref()
            .filter(|_| self.journal_summaries_allowed())
            .map_or_else(Vec::new, |catalog| {
                self.journal_summaries.row_statuses(catalog)
            })
    }

    pub(in crate::workspace) fn journal_summary_subscription(&self) -> Subscription<Desktop> {
        if self.journal_summaries_allowed() {
            self.journal_summaries.subscription()
        } else {
            Subscription::none()
        }
    }

    pub(in crate::workspace) fn preserve_journal_summary_for_policy_projection(
        &mut self,
        previous: &Arc<crate::ai_manager::Configuration>,
    ) {
        let Some(active) = &self.journal_summaries.active else {
            return;
        };
        if !Arc::ptr_eq(previous, &active.configuration)
            || active.profile
                != self.journal_summary_profile(
                    active.receipt.profile.as_ref(),
                    active.receipt.screenshots,
                )
        {
            return;
        }
        if let Some(configuration) = self.ai_configuration()
            && let Some(active) = &mut self.journal_summaries.active
        {
            active.configuration = configuration;
        }
    }

    pub(in crate::workspace) fn revalidate_journal_summary(&mut self) {
        if let Some((root, _)) = &self.journal_summaries.startup_recovery
            && self.journal_directory().ok().as_ref() != Some(root)
        {
            self.journal_summaries.cancel_startup_recovery();
        }
        let Some(active) = &self.journal_summaries.active else {
            return;
        };
        let vision_changed = active.vision.as_ref().is_some_and(|(profile, _)| {
            self.automation_screenshot_profile(active.receipt.automation_vision.as_ref())
                .as_ref()
                != Some(profile)
        });
        let settings = &self.window_settings.journal_ai;
        let settings_match = if active.receipt.mode == PassMode::Automation {
            // The image controls have their own cancellable transport.
            settings.enabled == active.settings.enabled
                && settings.endpoint == active.settings.endpoint
                && settings.model == active.settings.model
        } else {
            settings == &active.settings
        };
        let valid = self.journal_summaries_allowed()
            && (!active.receipt.automatic()
                || self
                    .journal_summaries
                    .queue
                    .pending()
                    .contains(&active.receipt))
            && self
                .ai_configuration()
                .is_some_and(|config| Arc::ptr_eq(&config, &active.configuration))
            && settings_match
            && self.journal_directory().ok().as_deref()
                == active.receipt.source.directory().parent()
            && (!active.receipt.screenshots
                || session_journal::topic_ai_settings::vision_allowed(&self.clipboard.policy))
            && (!active.receipt.reviewed()
                || self
                    .journal_summaries
                    .queue
                    .authorizes_reviewed(&active.receipt)
                    && https::vault_matches(active.vault_revision.as_ref(), self.vault.as_ref()))
            && (!(active.receipt.reviewed() || pins_lm_studio_auto(&active.receipt))
                || self.journal_summary_profile(
                    active.receipt.profile.as_ref(),
                    active.receipt.screenshots,
                ) == active.profile);
        if !valid {
            if matches!(
                active.receipt.mode,
                PassMode::Reevaluate | PassMode::CatchUp
            ) && !active.stop.load(Ordering::Acquire)
            {
                let batch = active.receipt.batch.clone();
                self.journal_summaries.pause_batch(&batch);
                self.journal_summaries.status = Some(
                    "Journal or AI settings changed. Review the saved pass before retrying.".into(),
                );
                self.journal_summaries.update_details(&batch);
            }
            self.journal_summaries.shutdown();
        } else if vision_changed && let Some(active) = &mut self.journal_summaries.active {
            active.vision.take();
        }
    }

    pub(in crate::workspace) fn journal_summaries_allowed(&self) -> bool {
        session_journal::topic_ai_settings::allowed(&self.clipboard.policy)
            && self.window_settings.journal_ai.allows_summaries()
            && self.ai_features_enabled()
            && !self.exiting
    }

    pub(super) fn journal_summary_live_ready(&self, source: &Source) -> bool {
        self.journal_summary_live_index(source).is_some()
    }

    fn journal_summary_live_index(&self, source: &Source) -> Option<usize> {
        let state = &self.journal_summaries;
        if !self.journal_summaries_allowed() || !state.queue.preferences().enabled {
            return None;
        }
        let receipt = source.summary_receipt();
        let root = self.journal_directory().ok()?;
        if receipt.directory().parent() != Some(root.as_path()) {
            return None;
        }
        state.live.iter().position(|live| {
            live.receipt.source == receipt
                && !live.force
                && live.capture.status() == session_journal::Status::Recording
                && !state.paused.contains(&live.receipt.generation)
                && !self.journal_summary_model_pending(&live.receipt)
        })
    }

    fn summarize_live_journal(&mut self, id: window::Id) -> Task<Desktop> {
        if self.journal_viewer_appearance_owner(id).is_none() || self.hosted_close_locks(id) {
            return Task::none();
        }
        let Some(Mode::Viewer(viewer)) = self.journals.get(&id).map(|surface| &surface.mode) else {
            return Task::none();
        };
        let Some(index) = self.journal_summary_live_index(&viewer.source) else {
            return Task::none();
        };
        if viewer.source.require_summary_location().is_err() {
            return Task::none();
        }
        self.journal_summaries.live[index].force = true;
        self.start_next_journal_summary()
    }

    pub(in crate::workspace) fn journal_summary_pending(&self, journal: &Summary) -> bool {
        if self.automation_journal_protects(journal) {
            return true;
        }
        let Ok(source) = Source::catalog(journal) else {
            return false;
        };
        let source = source.summary_receipt();
        self.journal_summaries
            .active
            .as_ref()
            .is_some_and(|active| active.receipt.source == source)
            || self
                .journal_summaries
                .queue
                .pending()
                .iter()
                .any(|receipt| receipt.source == source)
    }

    /// The caller supplies the capture's sealed redactor and original per-connection consent.
    pub(in crate::workspace) fn register_journal_summaries(
        &mut self,
        capture: Arc<Capture>,
        config: ConnectionConfig,
        redactor: Arc<Redactor>,
        title: String,
    ) {
        if !config.ai_summaries
            || !self.journal_summaries_allowed()
            || !self.journal_summaries.queue.preferences().enabled
        {
            return;
        }
        let Ok(generation) = crate::private_files::nonce() else {
            return;
        };
        let preferences = self.journal_summaries.queue.preferences();
        let interval = if config.summary_interval_minutes > 0 {
            config.summary_interval_minutes
        } else {
            preferences.interval_minutes
        };
        let interval = Duration::from_secs(u64::from(interval) * 60);
        let (grant, profile) = preflight::recording_route(
            preferences,
            capture.directory().parent(),
            capture.started_at(),
        );
        let profile = profile
            .cloned()
            .or_else(|| self.inherited_lm_studio_summary_profile());
        let automatic_title = preferences.automatic_title && self.journal_rename_allowed();
        let receipt = Receipt {
            batch: generation.clone(),
            generation,
            source: Source::capture(&capture).summary_receipt(),
            title: label(&title, 120),
            mode: PassMode::Closing,
            profile,
            source_approval: None,
            raw_only: false,
            automatic_title,
            title_approval: grant
                .filter(|_| automatic_title)
                .and_then(|grant| grant.title_approval_at(capture.started_at())),
            screenshots: false,
            automation_vision: None,
            full_output: preferences.full_output,
            evidence_limits: preferences.evidence_limits,
        };
        match self.journal_summaries.queue.enqueue([receipt.clone()]) {
            Ok(()) => self.journal_summaries.live.push(Live {
                capture,
                receipt,
                interval,
                next: Instant::now() + interval,
                redactor,
                force: false,
                running_preview: Arc::default(),
            }),
            Err(error) => {
                self.journal_summaries.status =
                    Some(format!("Could not schedule journal summaries ({error})."))
            }
        }
    }

    pub(super) fn update_journal_summary_review(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Desktop> {
        match event {
            Event::Review(catch_up) => self.review_journal_summaries(id, catch_up),
            Event::Pick(choice) => match self.journal_summaries.reviews.get_mut(&id) {
                Some(Review::Run {
                    choices,
                    selected,
                    screenshots,
                    automatic_title,
                    ..
                }) if choices.contains(choice.as_ref()) => {
                    if selected != choice.as_ref() {
                        *automatic_title = false;
                    }
                    *selected = *choice;
                    *screenshots = false;
                }
                Some(Review::Settings {
                    choices,
                    selected,
                    imported_recording,
                    recording_title_request,
                    ..
                }) if choices.contains(choice.as_ref()) => {
                    if selected != choice.as_ref() {
                        *recording_title_request = false;
                    }
                    *selected = *choice;
                    *imported_recording = false;
                }
                _ => {}
            },
            Event::PickAutomation(choice) => {
                if let Some(Review::Settings {
                    automation_choices,
                    automation,
                    automation_edited,
                    imported_automation,
                    automation_title_request,
                    ..
                }) = self.journal_summaries.reviews.get_mut(&id)
                    && automation_choices.contains(choice.as_ref())
                {
                    if automation != choice.as_ref() {
                        *automation_title_request = false;
                    }
                    *automation = *choice;
                    *automation_edited = true;
                    *imported_automation = false;
                }
            }
            Event::UseImportedProfile(for_automation) => {
                self.use_imported_journal_profile(id, for_automation)
            }
            Event::PickAutomationSource(source) => {
                let available = self.automation_summary_sources();
                let saved_approval = self
                    .journal_summaries
                    .queue
                    .preferences()
                    .source_approval(&source.source);
                if available.contains(&source)
                    && let Some(Review::Settings {
                        automation_source,
                        source_profile,
                        source_full_output,
                        source_title_request,
                        ..
                    }) = self.journal_summaries.reviews.get_mut(&id)
                {
                    *automation_source = Some(source);
                    *source_profile = None;
                    *source_full_output =
                        saved_approval.is_some_and(|approval| approval.full_output);
                    *source_title_request = false;
                }
            }
            Event::SourceFullOutput(value) => {
                if let Some(Review::Settings {
                    source_full_output, ..
                }) = self.journal_summaries.reviews.get_mut(&id)
                {
                    *source_full_output = value;
                }
            }
            Event::SourceTitleRequest(value) if self.journal_rename_allowed() => {
                if let Some(Review::Settings {
                    source_title_request,
                    ..
                }) = self.journal_summaries.reviews.get_mut(&id)
                {
                    *source_title_request = value;
                }
            }
            Event::PickSourceProfile(choice) => {
                if choice.reviewed().is_some()
                    && let Some(Review::Settings { automation_choices, source_profile, source_title_request, .. }) =
                        self.journal_summaries.reviews.get_mut(&id)
                    && automation_choices.iter().any(|item| {
                        matches!(item, AutomationChoice::Profile(current) if current == &choice)
                    })
                {
                    if source_profile.as_deref() != Some(choice.as_ref()) {
                        *source_title_request = false;
                    }
                    *source_profile = Some(choice);
                }
            }
            Event::ApproveAutomationSource => self.save_source_summary_approval(id, true),
            Event::RevokeAutomationSource => self.save_source_summary_approval(id, false),
            Event::Screenshots(value) => {
                if let Some(Review::Run { screenshots, .. }) =
                    self.journal_summaries.reviews.get_mut(&id)
                {
                    *screenshots = value;
                }
            }
            Event::FullOutput(value) => match self.journal_summaries.reviews.get_mut(&id) {
                Some(Review::Run { full_output, .. }) => {
                    *full_output = value;
                }
                Some(Review::Settings {
                    full_output,
                    evidence_limits,
                    ..
                }) => {
                    *full_output = value;
                    evidence_limits.imported = None;
                }
                None => {}
            },
            Event::SummarizeNow => return self.summarize_live_journal(id),
            Event::Start => return self.enqueue_reviewed_journal_summaries(id),
            Event::Dismiss => self.journal_summaries.close_review(id),
            Event::Settings => {
                self.initialize_scheduler(id, true);
                let settings = self.journal_summaries.queue.preferences();
                let mut choices = vec![Choice {
                    profile: None,
                    label: "Use local journal profile".into(),
                }];
                choices.extend(self.journal_summary_choices());
                let selected = if let Some(grant) = settings.reviewed_grant() {
                    choices
                        .iter()
                        .find(|choice| choice.profile.as_ref() == Some(&grant.profile))
                        .cloned()
                } else if let Some(profile) = &settings.recording_profile {
                    Some(
                        choices
                            .iter()
                            .find(|choice| choice.profile.as_ref() == Some(profile))
                            .cloned()
                            .unwrap_or_else(|| Choice {
                                profile: Some(profile.clone()),
                                label: format!(
                                    "Unavailable · {} · {}",
                                    profile.model, profile.destination
                                ),
                            }),
                    )
                } else if !settings.enabled {
                    self.preferred_journal_summary_cli(&choices).cloned()
                } else {
                    None
                }
                .unwrap_or_else(|| choices[0].clone());
                let mut automation_choices =
                    vec![AutomationChoice::Inherit, AutomationChoice::Local];
                automation_choices.extend(
                    choices
                        .iter()
                        .skip(1)
                        .cloned()
                        .map(|choice| AutomationChoice::Profile(Box::new(choice))),
                );
                let automation = match &settings.automation_profile {
                    None => AutomationChoice::Inherit,
                    Some(AutomationProfile::Local) => AutomationChoice::Local,
                    Some(
                        AutomationProfile::Selected(expected)
                        | AutomationProfile::Reviewed(AutomaticApproval {
                            profile: expected, ..
                        }),
                    ) => automation_choices
                        .iter()
                        .find(|candidate| {
                            matches!(candidate, AutomationChoice::Profile(choice)
                                    if choice.profile.as_ref() == Some(expected))
                        })
                        .cloned()
                        .unwrap_or_else(|| {
                            AutomationChoice::Profile(Box::new(Choice {
                                profile: Some(expected.clone()),
                                label: format!(
                                    "Unavailable · {} · {}",
                                    expected.model, expected.destination
                                ),
                            }))
                        }),
                };
                self.journal_summaries.reviews.insert(
                    id,
                    Review::Settings {
                        enabled: settings.enabled,
                        interval: settings.interval_minutes.to_string(),
                        imported_preferences: None,
                        automatic_title: settings.automatic_title,
                        recording_title_request: settings
                            .reviewed_grant()
                            .is_some_and(|grant| grant.title_since.is_some()),
                        automation_title_request: settings.automation_profile.as_ref().is_some_and(
                            |profile| {
                                matches!(profile,
                                AutomationProfile::Reviewed(grant) if grant.title_since.is_some())
                            },
                        ),
                        full_output: settings.full_output,
                        evidence_limits: EvidenceDraft::new(settings.evidence_limits),
                        choices,
                        selected,
                        automation_choices,
                        automation,
                        automation_edited: false,
                        imported_recording: false,
                        imported_automation: false,
                        automation_source: None,
                        source_profile: None,
                        source_full_output: false,
                        source_title_request: false,
                    },
                );
            }
            Event::Enable(value) => {
                if let Some(Review::Settings { enabled, .. }) =
                    self.journal_summaries.reviews.get_mut(&id)
                {
                    *enabled = value;
                }
            }
            Event::Interval(value) => {
                if value.len() <= 4
                    && let Some(Review::Settings { interval, .. }) =
                        self.journal_summaries.reviews.get_mut(&id)
                {
                    *interval = value;
                }
            }
            Event::AutomaticTitle(value) if self.journal_rename_allowed() => {
                match self.journal_summaries.reviews.get_mut(&id) {
                    Some(Review::Settings {
                        automatic_title, ..
                    }) => {
                        *automatic_title = value;
                    }
                    Some(Review::Run {
                        automatic_title, ..
                    }) => *automatic_title = value,
                    None => {}
                }
            }
            Event::RecordingTitleRequest(value) if self.journal_rename_allowed() => {
                if let Some(Review::Settings {
                    recording_title_request,
                    ..
                }) = self.journal_summaries.reviews.get_mut(&id)
                {
                    *recording_title_request = value;
                }
            }
            Event::AutomationTitleRequest(value) if self.journal_rename_allowed() => {
                if let Some(Review::Settings {
                    automation_title_request,
                    ..
                }) = self.journal_summaries.reviews.get_mut(&id)
                {
                    *automation_title_request = value;
                }
            }
            Event::CustomEvidence(value) => {
                if let Some(Review::Settings {
                    evidence_limits, ..
                }) = self.journal_summaries.reviews.get_mut(&id)
                {
                    evidence_limits.custom = value;
                    evidence_limits.imported = None;
                }
            }
            Event::EvidenceLines(value) => {
                if value.len() <= 7
                    && let Some(Review::Settings {
                        evidence_limits, ..
                    }) = self.journal_summaries.reviews.get_mut(&id)
                {
                    evidence_limits.lines = value;
                    evidence_limits.imported = None;
                }
            }
            Event::EvidenceTokens(value) => {
                if value.len() <= 8
                    && let Some(Review::Settings {
                        evidence_limits, ..
                    }) = self.journal_summaries.reviews.get_mut(&id)
                {
                    evidence_limits.tokens = value;
                    evidence_limits.imported = None;
                }
            }
            Event::UseImportedPreferences => {
                if self.journal_summaries_allowed()
                    && let Some(imported @ (minutes, title)) = self
                        .ai_configuration()
                        .and_then(|config| config.imported_journal_preferences())
                    && (!title.unwrap_or(false) || self.journal_rename_allowed())
                    && let Some(Review::Settings {
                        interval,
                        automatic_title,
                        imported_preferences,
                        ..
                    }) = self.journal_summaries.reviews.get_mut(&id)
                {
                    if let Some(minutes) = minutes {
                        *interval = minutes.to_string();
                    }
                    if let Some(title) = title {
                        *automatic_title = title;
                    }
                    *imported_preferences = Some(imported);
                }
            }
            Event::UseImportedEvidence => {
                if self.journal_summaries_allowed()
                    && let Some(imported @ (limits, chunking)) =
                        self.imported_summary_evidence_limits()
                    && let Some(Review::Settings {
                        full_output,
                        evidence_limits,
                        ..
                    }) = self.journal_summaries.reviews.get_mut(&id)
                {
                    *evidence_limits = EvidenceDraft::new(Some(limits));
                    evidence_limits.imported = Some(imported);
                    *full_output = chunking;
                }
            }
            Event::SaveSettings if self.journal_summaries_allowed() => {
                if let Some(Review::Settings {
                    enabled,
                    interval,
                    imported_preferences,
                    automatic_title,
                    recording_title_request,
                    automation_title_request,
                    full_output,
                    evidence_limits,
                    selected,
                    automation,
                    automation_edited,
                    imported_recording,
                    imported_automation,
                    ..
                }) = self.journal_summaries.reviews.get(&id)
                    && let Ok(interval_minutes) = interval.parse()
                {
                    let rename_allowed = self.journal_rename_allowed();
                    let now = Timestamp::now();
                    if imported_preferences.is_some()
                        && self
                            .ai_configuration()
                            .and_then(|config| config.imported_journal_preferences())
                            != *imported_preferences
                    {
                        self.journal_summaries.status = Some("Imported Java Journal preferences changed. Review the current suggestions again.".into());
                        return Task::none();
                    }
                    if let Some(imported) = evidence_limits.imported
                        && self.imported_summary_evidence_limits() != Some(imported)
                    {
                        self.journal_summaries.status = Some(EVIDENCE_IMPORT_CHANGED.into());
                        return Task::none();
                    }
                    let evidence_limits = match evidence_limits.limits() {
                        Ok(limits) => limits,
                        Err(error) => {
                            self.journal_summaries.status = Some(error.into());
                            return Task::none();
                        }
                    };
                    if (*imported_recording
                        && !self.matches_imported_journal_profile(selected, false))
                        || (*imported_automation
                            && !matches!(automation, AutomationChoice::Profile(choice)
                            if self.matches_imported_journal_profile(choice, true)))
                    {
                        self.journal_summaries.status = Some(IMPORT_CHANGED.into());
                        return Task::none();
                    }
                    let grant = if *enabled {
                        if let Some(choice) = selected.reviewed() {
                            if self.journal_summary_profile(Some(choice), false).is_none() {
                                let message = if choice.cli.is_some() {
                                    https::CLI_CHANGED
                                } else {
                                    https::CHANGED
                                };
                                self.journal_summaries.status = Some(message.into());
                                return Task::none();
                            }
                            let preferences = self.journal_summaries.queue.preferences();
                            let previous = preferences.reviewed_grant().filter(|grant| {
                                preferences.full_output == *full_output
                                    && grant.evidence_limits == evidence_limits
                            });
                            Some(
                                previous
                                    .filter(|grant| &grant.profile == choice)
                                    .cloned()
                                    .unwrap_or_else(|| AutomaticApproval {
                                        profile: choice.clone(),
                                        since: now,
                                        evidence_limits,
                                        title_since: None,
                                    })
                                    .with_title_scope(
                                        *recording_title_request && rename_allowed,
                                        now,
                                    ),
                            )
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    let (https, cli) = match grant {
                        Some(grant) if grant.profile.cli.is_some() => (None, Some(grant)),
                        grant => (grant, None),
                    };
                    if *imported_recording && !*enabled && selected.reviewed().is_some() {
                        self.journal_summaries.status = Some(
                            "Enable automatic recording summaries before approving the imported outbound profile."
                                .into(),
                        );
                        return Task::none();
                    }
                    let recording_profile = selected
                        .profile
                        .as_ref()
                        .filter(|choice| choice.binding().is_none());
                    if let Some(profile) = recording_profile
                        && self
                            .journal_summaries
                            .queue
                            .preferences()
                            .recording_profile
                            .as_ref()
                            != Some(profile)
                        && self.journal_summary_profile(Some(profile), false).is_none()
                    {
                        self.journal_summaries.status =
                            Some("The recording journal profile changed. Select it again.".into());
                        return Task::none();
                    }
                    let preferences = self.journal_summaries.queue.preferences();
                    let automation_profile = match automation {
                        AutomationChoice::Inherit
                            if *imported_recording
                                && !*automation_edited
                                && selected.profile.as_ref().is_some_and(|choice| {
                                    preferences
                                        .reviewed_grant()
                                        .map(|grant| &grant.profile)
                                        .or(preferences.recording_profile.as_ref())
                                        != Some(choice)
                                }) =>
                        {
                            Some(
                                if let Some(grant) =
                                    preferences.automation_grant().filter(|grant| {
                                        preferences.full_output == *full_output
                                            && grant.evidence_limits == evidence_limits
                                    })
                                {
                                    let mut grant = grant.clone();
                                    if let Some(since) = preferences.automation_inherited_since {
                                        grant.since = grant.since.max(since);
                                        grant.title_since =
                                            grant.title_since.map(|title| title.max(grant.since));
                                    }
                                    AutomationProfile::Reviewed(grant)
                                } else if let Some(profile) = preferences.automation_local_profile()
                                {
                                    AutomationProfile::Selected(profile.clone())
                                } else {
                                    AutomationProfile::Local
                                },
                            )
                        }
                        AutomationChoice::Inherit => None,
                        AutomationChoice::Local => Some(AutomationProfile::Local),
                        AutomationChoice::Profile(choice) => {
                            let Some(profile) = &choice.profile else {
                                return Task::none();
                            };
                            match &preferences.automation_profile {
                                Some(AutomationProfile::Reviewed(old))
                                    if &old.profile == profile
                                        && preferences.full_output == *full_output
                                        && old.evidence_limits == evidence_limits =>
                                {
                                    Some(AutomationProfile::Reviewed(old.clone().with_title_scope(
                                        *automation_title_request && rename_allowed,
                                        now,
                                    )))
                                }
                                previous @ Some(AutomationProfile::Selected(old))
                                    if old == profile =>
                                {
                                    previous.clone()
                                }
                                _ => {
                                    if self.journal_summary_profile(Some(profile), false).is_none()
                                    {
                                        self.journal_summaries.status = Some(
                                            "The automation journal profile changed. Select it again.".into(),
                                        );
                                        return Task::none();
                                    }
                                    Some(if profile.binding().is_some() {
                                        AutomationProfile::Reviewed(AutomaticApproval {
                                            profile: profile.clone(),
                                            since: now,
                                            evidence_limits,
                                            title_since: (*automation_title_request
                                                && rename_allowed)
                                                .then_some(now),
                                        })
                                    } else {
                                        AutomationProfile::Selected(profile.clone())
                                    })
                                }
                            }
                        }
                    };
                    let settings = session_journal::summary_queue::Preferences {
                        enabled: *enabled,
                        interval_minutes,
                        automatic_title: *automatic_title,
                        full_output: *full_output,
                        evidence_limits,
                        https,
                        cli,
                        recording_profile: recording_profile.cloned(),
                        automation_inherited_since: if automation_profile.is_some() {
                            None
                        } else if preferences.automation_profile.is_some() {
                            Some(Timestamp::now())
                        } else {
                            preferences.automation_inherited_since
                        },
                        automation_profile,
                        source_approvals: preferences.source_approvals.clone(),
                    };
                    match self.journal_summaries.queue.set_preferences(settings) {
                        Ok(()) => {
                            self.journal_summaries.close_review(id);
                            let queue = &self.journal_summaries.queue;
                            self.journal_summaries
                                .live
                                .retain(|live| queue.pending().contains(&live.receipt));
                            self.revalidate_journal_summary();
                            self.invalidate_automation_summary_estimates();
                            if !self.journal_summaries.queue.preferences().enabled {
                                self.journal_summaries.live.clear();
                                if let Some(active) = &mut self.journal_summaries.active
                                    && active.receipt.mode == PassMode::Closing
                                {
                                    active.cancel();
                                }
                            }
                        }
                        Err(error) => {
                            self.journal_summaries.status =
                                Some(format!("Could not save summary settings ({error})."))
                        }
                    }
                }
            }
            Event::Cancel => self.cancel_journal_summary(),
            Event::Retry => {
                self.journal_summaries.paused.clear();
                return self.start_next_journal_summary();
            }
            Event::ShowProgress => return self.show_current_journal_summary_progress(id),
            Event::CloseProgress => {
                return self
                    .close_journal_summary_progress(id)
                    .unwrap_or_else(Task::none);
            }
            Event::CancelProgress(batch) => {
                if self
                    .journal_summaries
                    .details
                    .as_ref()
                    .is_some_and(|details| details.matches(id, &batch))
                {
                    self.cancel_journal_summary_batch(&batch);
                }
            }
            Event::SaveSettings
            | Event::SourceTitleRequest(_)
            | Event::AutomaticTitle(_)
            | Event::RecordingTitleRequest(_)
            | Event::AutomationTitleRequest(_) => {}
        }
        Task::none()
    }

    fn journal_summary_profile(
        &self,
        choice: Option<&ProfileChoice>,
        vision: bool,
    ) -> Option<Profile> {
        if !self.journal_summaries_allowed() {
            return None;
        }
        if vision && !session_journal::topic_ai_settings::vision_allowed(&self.clipboard.policy) {
            return None;
        }
        let root = choice
            .filter(|choice| choice.binding().is_some())
            .and_then(|_| self.journal_directory().ok());
        let config = self.ai_configuration()?;
        let profile = preflight::recording_profile(
            &config,
            &self.window_settings.journal_ai,
            choice,
            root.as_deref(),
            &self.clipboard.policy,
            vision,
        )?;
        profile.require_current_policy_authority().ok()?;
        Some(profile)
    }

    fn inherited_lm_studio_summary_profile(&self) -> Option<ProfileChoice> {
        if self.uses_legacy_journal_profile() {
            return None;
        }
        let configuration = self.ai_configuration()?;
        let id = configuration.journal_summary_default_id()?;
        if !configuration.is_lm_studio_auto(id) {
            return None;
        }
        let profile = self.journal_summary_profile(None, false)?;
        let fingerprint = profile.lm_studio_auto_binding()?;
        let reviewed = configuration
            .reviewed_local_ai_profiles()
            .find(|entry| entry.id == id)?;
        Some(ProfileChoice {
            id: configuration.profile_reference(id)?,
            model: reviewed.model.into(),
            destination: reviewed.destination,
            policy_model_fingerprint: Some(fingerprint),
            https: None,
            cli: None,
        })
    }

    fn bind_inherited_journal_model(&mut self, receipt: &mut Receipt) -> io::Result<()> {
        if receipt.automatic()
            && !receipt.raw_only
            && receipt.profile.is_none()
            && let Some(profile) = self.inherited_lm_studio_summary_profile()
        {
            let bound = self
                .journal_summaries
                .queue
                .bind_inherited_profile(receipt, profile)?;
            for live in self
                .journal_summaries
                .live
                .iter_mut()
                .filter(|live| &live.receipt == receipt)
            {
                live.receipt = bound.clone();
            }
            *receipt = bound;
        }
        Ok(())
    }

    fn journal_summary_choices(&self) -> Vec<Choice> {
        let mut choices = Vec::new();
        let preferences = &self.window_settings.journal_ai;
        if preferences.profile().is_ok_and(|profile| profile.is_some()) {
            choices.push(Choice {
                profile: Some(ProfileChoice {
                    id: ProfileRef::Numeric(0),
                    model: preferences.model.clone(),
                    destination: preferences.endpoint.clone(),
                    policy_model_fingerprint: None,
                    https: None,
                    cli: None,
                }),
                label: format!("Journal profile · {}", preferences.model),
            });
        }
        if let Some(config) = self.ai_configuration() {
            choices.extend(config.reviewed_local_ai_profiles().filter_map(|entry| {
                Some(Choice {
                    profile: Some(ProfileChoice {
                        id: config.profile_reference(entry.id)?,
                        model: entry.model.into(),
                        destination: entry.destination,
                        policy_model_fingerprint: entry.policy_model_fingerprint,
                        https: None,
                        cli: None,
                    }),
                    label: format!("{} · {}", entry.name, entry.model),
                })
            }));
            if let Some(default) = config
                .journal_summary_default_id()
                .and_then(|id| config.profile_reference(id))
                && let Some(index) = choices.iter().position(|choice| {
                    choice
                        .profile
                        .as_ref()
                        .is_some_and(|profile| profile.id == default)
                })
            {
                choices.swap(0, index);
            }
        }
        choices.extend(self.journal_summary_https_choices());
        choices.extend(self.journal_summary_cli_choices());
        choices
    }

    fn imported_summary_evidence_limits(&self) -> Option<(EvidenceLimits, bool)> {
        self.ai_configuration()?.imported_journal_evidence_limits()
    }

    fn imported_journal_profile_ref(&self, automation: bool) -> Option<ProfileRef> {
        let config = self.ai_configuration()?;
        config.profile_reference(config.resolve_imported_journal_summary_profile(automation)?)
    }

    fn matches_imported_journal_profile(&self, choice: &Choice, automation: bool) -> bool {
        choice.profile.as_ref().is_some_and(|profile| {
            self.imported_journal_profile_ref(automation).as_ref() == Some(&profile.id)
                && self.journal_summary_profile(Some(profile), false).is_some()
        })
    }

    fn use_imported_journal_profile(&mut self, id: window::Id, for_automation: bool) {
        if !self.journal_summaries_allowed() {
            return;
        }
        let Some(Review::Settings {
            choices,
            automation_choices,
            ..
        }) = self.journal_summaries.reviews.get(&id)
        else {
            return;
        };
        let choice = if for_automation {
            automation_choices.iter().find_map(|choice| match choice {
                AutomationChoice::Profile(choice)
                    if self.matches_imported_journal_profile(choice, true) =>
                {
                    Some(choice.as_ref())
                }
                _ => None,
            })
        } else {
            choices
                .iter()
                .find(|choice| self.matches_imported_journal_profile(choice, false))
        };
        let Some(choice) = choice.cloned() else {
            self.journal_summaries.status = Some(IMPORT_CHANGED.into());
            return;
        };
        if let Some(Review::Settings {
            selected,
            automation,
            automation_edited,
            imported_recording,
            imported_automation,
            recording_title_request,
            automation_title_request,
            ..
        }) = self.journal_summaries.reviews.get_mut(&id)
        {
            if for_automation {
                if !matches!(automation, AutomationChoice::Profile(current) if current.as_ref() == &choice)
                {
                    *automation_title_request = false;
                }
                *automation = AutomationChoice::Profile(Box::new(choice));
                *automation_edited = true;
                *imported_automation = true;
            } else {
                if selected != &choice {
                    *recording_title_request = false;
                }
                *selected = choice;
                *imported_recording = true;
            }
        }
    }

    fn imported_journal_profile_view<'a>(
        &'a self,
        automation: bool,
        options: impl Iterator<Item = &'a Choice>,
        selected: Option<&Choice>,
        adopted_in_draft: bool,
    ) -> Option<Element<'a, Event>> {
        let config = self.ai_configuration()?;
        let hint = config.imported_journal_summary_profile(automation)?;
        let matching = self.imported_journal_profile_ref(automation);
        let candidate = matching.as_ref().and_then(|reference| {
            options
                .filter(|choice| {
                    choice
                        .profile
                        .as_ref()
                        .is_some_and(|profile| &profile.id == reference)
                })
                .find(|choice| self.matches_imported_journal_profile(choice, automation))
        });
        Some(if let Some(candidate) = candidate {
            let preferences = self.journal_summaries.queue.preferences();
            let saved = if automation {
                match &preferences.automation_profile {
                    Some(AutomationProfile::Selected(profile)) => Some(profile),
                    Some(AutomationProfile::Reviewed(grant)) => Some(&grant.profile),
                    _ => None,
                }
            } else {
                preferences
                    .reviewed_grant()
                    .map(|grant| &grant.profile)
                    .or(preferences.recording_profile.as_ref())
            };
            let adopted = Some(candidate) == selected
                && (adopted_in_draft || candidate.profile.as_ref() == saved);
            row![
                text(format!("Imported from Java · {}", candidate.label)).size(13),
                button("Use imported profile")
                    .on_press_maybe((!adopted).then_some(Event::UseImportedProfile(automation)),),
            ]
            .spacing(8)
            .wrap()
            .into()
        } else {
            text(if matching.is_some() {
                "The imported Java profile needs review in AI Manager."
            } else if hint.source().is_some() {
                "The matching imported Java profile is unavailable."
            } else {
                "The Java backup did not include a matching profile definition."
            })
            .size(13)
            .into()
        })
    }

    pub(in crate::workspace) fn automation_summary_providers(
        &self,
    ) -> Vec<crate::scheduler_window::AutomationSummaryProvider> {
        use crate::scheduler_window::AutomationSummaryProvider;

        let choices = if self.journal_summaries_allowed() {
            self.journal_summary_choices()
        } else {
            Vec::new()
        };
        let configuration = self.ai_configuration();
        std::iter::once(AutomationSummaryProvider(
            None,
            "Use the automation journal profile".into(),
            None,
        ))
        .chain(choices.into_iter().map(|choice| {
            let source = choice.profile.as_ref().and_then(|profile| {
                configuration
                    .as_ref()?
                    .java_profile_source(&profile.id)
                    .cloned()
            });
            AutomationSummaryProvider(choice.profile, choice.label, source)
        }))
        .collect()
    }

    fn automation_summary_sources(&self) -> Vec<AutomationSourceChoice> {
        let scheduler = self.scheduler.iter().flat_map(|scheduler| {
            scheduler
                .automation_journal_jobs()
                .map(|(id, name)| AutomationSourceChoice {
                    source: AutomationSource::Job(id.clone()),
                    label: format!("{} · {}", label(name, 100), id.as_str()),
                })
        });
        let mut sources: Vec<_> = std::iter::once(AutomationSourceChoice {
            source: AutomationSource::Swarm,
            label: "Interactive Swarm".into(),
        })
        .chain(scheduler)
        .collect();
        for (source, _) in &self.journal_summaries.queue.preferences().source_approvals {
            if !sources.iter().any(|choice| &choice.source == source)
                && let AutomationSource::Job(id) = source
            {
                sources.push(AutomationSourceChoice {
                    source: source.clone(),
                    label: format!("Unavailable job · {}", id.as_str()),
                });
            }
        }
        sources
    }

    fn save_source_summary_approval(&mut self, id: window::Id, approve: bool) {
        if !self.journal_summaries_allowed() {
            return;
        }
        let Some(Review::Settings {
            automation_source: Some(source),
            source_profile,
            source_full_output,
            source_title_request,
            evidence_limits,
            ..
        }) = self.journal_summaries.reviews.get(&id)
        else {
            return;
        };
        let source = source.source.clone();
        let mut preferences = self.journal_summaries.queue.preferences().clone();
        let approval = if approve {
            if evidence_limits.limits() != Ok(preferences.evidence_limits) {
                self.journal_summaries.status =
                    Some("Save the new evidence limits before approving this source.".into());
                return;
            }
            let Some(profile) = source_profile.as_deref().and_then(Choice::reviewed) else {
                return;
            };
            let available = matches!(source, AutomationSource::Swarm)
                || matches!(&source, AutomationSource::Job(id) if self.scheduler.as_ref()
                    .is_some_and(|scheduler| scheduler.automation_journal_jobs().any(|(job, _)| job == id)));
            if !available || self.journal_summary_profile(Some(profile), false).is_none() {
                self.journal_summaries.status = Some(
                    "The automation source or provider changed. Open the summary settings again."
                        .into(),
                );
                return;
            }
            let now = Timestamp::now();
            Some((
                AutomaticApproval {
                    profile: profile.clone(),
                    since: now,
                    evidence_limits: preferences.evidence_limits,
                    title_since: (*source_title_request && self.journal_rename_allowed())
                        .then_some(now),
                },
                *source_full_output,
            ))
        } else {
            None
        };
        let result = preferences
            .set_source_approval(source, approval)
            .and_then(|()| self.journal_summaries.queue.set_preferences(preferences));
        if result.is_ok() {
            self.invalidate_automation_summary_estimates();
        }
        self.journal_summaries.status = Some(match result {
            Ok(()) if approve => "Automation source approval saved.".into(),
            Ok(()) => "Automation source approval revoked.".into(),
            Err(error) => format!("Could not save the automation source approval ({error})."),
        });
        self.revalidate_journal_summary();
    }

    pub(in crate::workspace) fn revoke_deleted_automation_summary_job(
        &mut self,
        job: &kortty_jobs::JobId,
    ) -> io::Result<()> {
        self.journal_summaries
            .queue
            .revoke_source(&AutomationSource::Job(job.clone()))?;
        self.revalidate_journal_summary();
        Ok(())
    }

    pub(in crate::workspace) fn automation_summary_estimate_profile(
        &self,
        source: Option<&AutomationSource>,
        selected: Option<&ProfileChoice>,
    ) -> Option<Profile> {
        let preferences = self.journal_summaries.queue.preferences();
        let grant = preferences.automation_grant_at(Timestamp::now());
        let choice = selected.or_else(|| {
            preferences
                .automation_local_profile()
                .or_else(|| grant.map(|grant| &grant.profile))
        });
        if let Some(choice) = choice
            && choice.binding().is_some()
            && !grant.is_some_and(|grant| &grant.profile == choice)
            && !source
                .and_then(|source| preferences.source_grant_at(source, Timestamp::now()))
                .is_some_and(|approval| &approval.grant.profile == choice)
        {
            return None;
        }
        self.journal_summary_profile(choice, false)
    }

    pub(in crate::workspace) fn automation_summary_evidence_limits(
        &self,
        source: Option<&AutomationSource>,
        selected: Option<&ProfileChoice>,
    ) -> Option<EvidenceLimits> {
        let preferences = self.journal_summaries.queue.preferences();
        if let Some(approval) =
            source.and_then(|source| preferences.source_grant_at(source, Timestamp::now()))
            && selected == Some(&approval.grant.profile)
        {
            approval.grant.evidence_limits
        } else {
            preferences.evidence_limits
        }
    }

    pub(in crate::workspace) fn automation_summary_run_approval(
        &self,
        source: &AutomationSource,
        options: &session_journal::automation::Options,
        started: Timestamp,
    ) -> Option<SourceApproval> {
        if !self.journal_summaries_allowed()
            || options.closing == session_journal::automation::Closing::Off
        {
            return None;
        }
        let approval = self
            .journal_summaries
            .queue
            .preferences()
            .source_grant_at(source, started)?;
        let profile = options.summary_profile.as_ref()?;
        if profile != &approval.grant.profile {
            return None;
        }
        let transport = self.journal_summary_profile(Some(profile), false)?;
        transport.require_current_policy_authority().ok()?;
        Some(approval.clone())
    }

    fn preferred_journal_summary_cli<'a>(&self, choices: &'a [Choice]) -> Option<&'a Choice> {
        let configuration = self.ai_configuration()?;
        let id = configuration.preferred_journal_summary_cli_id()?;
        let preferred = configuration.profile_reference(id)?;
        choices.iter().find(|choice| {
            choice
                .profile
                .as_ref()
                .is_some_and(|profile| profile.cli.is_some() && profile.id == preferred)
        })
    }

    fn journal_summary_https_choices(&self) -> Vec<Choice> {
        let Some(config) = self.ai_configuration() else {
            return Vec::new();
        };
        let policy = &self.clipboard.policy.terminal_logging;
        let (Ok(root), Some(redaction_revision)) =
            (self.journal_directory(), policy.redaction_revision())
        else {
            return Vec::new();
        };
        config
            .journal_https_profiles()
            .filter_map(|(id, name, model, destination)| {
                Some(Choice {
                    label: format!("{name} · {model} · {destination}"),
                    profile: Some(ProfileChoice {
                        id: config.profile_reference(id)?,
                        model: model.into(),
                        destination,
                        policy_model_fingerprint: None,
                        https: Some(ProfileBinding {
                            root: root.clone(),
                            profile_revision: config.journal_summary_https_revision(id).ok()?,
                            redaction_revision,
                        }),
                        cli: None,
                    }),
                })
            })
            .collect()
    }

    fn journal_summary_cli_choices(&self) -> Vec<Choice> {
        let Some(config) = self.ai_configuration() else {
            return Vec::new();
        };
        if !crate::local_ai::cli::allowed(&self.clipboard.policy, false) {
            return Vec::new();
        }
        let policy = &self.clipboard.policy.terminal_logging;
        let (Ok(root), Some(redaction_revision)) =
            (self.journal_directory(), policy.redaction_revision())
        else {
            return Vec::new();
        };
        config
            .journal_summary_cli_profiles()
            .filter_map(|(id, name, model, destination)| {
                let model = crate::local_ai::cli::display_model(model);
                Some(Choice {
                    label: format!("{name} · {model} · {destination}"),
                    profile: Some(ProfileChoice {
                        id: config.profile_reference(id)?,
                        model: model.into(),
                        destination,
                        policy_model_fingerprint: None,
                        https: None,
                        cli: Some(ProfileBinding {
                            root: root.clone(),
                            profile_revision: config.journal_summary_cli_revision(id).ok()?,
                            redaction_revision,
                        }),
                    }),
                })
            })
            .collect()
    }

    fn reevaluation_profile<'a>(
        &self,
        choices: &'a [Choice],
        last_used: Option<&JournalProfileSelection>,
    ) -> Option<&'a Choice> {
        let config = self.ai_configuration();
        let matching = |id| {
            choices.iter().find(|choice| {
                choice
                    .profile
                    .as_ref()
                    .is_some_and(|profile| profile.id == id)
            })
        };
        let previous = last_used.and_then(|selection| {
            let config = config.as_ref()?;
            config.profile_reference(config.resolve_journal_profile_selection(selection)?)
        });
        if let Some(choice) = previous.and_then(matching) {
            return Some(choice);
        }

        let preferences = self.journal_summaries.queue.preferences();
        let recording = preferences.recording_profile.as_ref().or_else(|| {
            preferences
                .reviewed_grant()
                .map(|approval| &approval.profile)
        });
        if let Some(recording) = recording
            && let Some(choice) = choices
                .iter()
                .find(|choice| choice.profile.as_ref() == Some(recording))
        {
            return Some(choice);
        }
        self.preferred_journal_summary_cli(choices).or_else(|| {
            let configured = if self.uses_legacy_journal_profile() {
                Some(ProfileRef::Numeric(0))
            } else {
                config.and_then(|config| {
                    config.profile_reference(config.journal_summary_default_id()?)
                })
            };
            configured.and_then(matching)
        })
    }

    fn review_journal_summaries(&mut self, id: window::Id, catch_up: bool) {
        if !self.journal_summaries_allowed() || self.journal_edit_busy(id) {
            return;
        }
        let Some(Mode::Manager(manager)) = self.journals.get(&id).map(|surface| &surface.mode)
        else {
            return;
        };
        let Some(catalog) = &manager.catalog else {
            return;
        };
        if manager.loading || self.journal_directory().ok().as_ref() != manager.root.as_ref() {
            return;
        }
        let indices = if catch_up {
            (0..catalog.journals.len()).collect()
        } else {
            manager.tree.selected_indices(catalog)
        };
        let mut targets = indices
            .into_iter()
            .map(|index| &catalog.journals[index])
            .filter(|summary| {
                summary.ended_at.is_some()
                    && (!catch_up || summary.log_entry_count > 0 && summary.needs_summary)
                    && !self.journal_summary_pending(summary)
                    && !self.journal_metadata_is_live(manager, summary)
            })
            .filter_map(|summary| {
                Source::catalog(summary)
                    .ok()
                    .map(|source| (source, summary))
            })
            .take(MAX_PENDING)
            .peekable();
        let mut choices = self.journal_summary_choices();
        if !catch_up {
            choices.retain(|choice| {
                self.journal_summary_profile(choice.profile.as_ref(), false)
                    .is_some()
            });
        }
        let Some((_, first)) = targets.peek().filter(|_| !choices.is_empty()) else {
            self.journal_summaries.status =
                Some("Select a stopped journal and an available AI profile.".into());
            return;
        };
        let mode = if catch_up {
            PassMode::CatchUp
        } else {
            PassMode::Reevaluate
        };
        let selected = if catch_up {
            self.preferred_journal_summary_cli(&choices)
        } else {
            self.reevaluation_profile(&choices, first.usage_profile.as_ref())
        }
        .unwrap_or(&choices[0])
        .clone();
        let receipts = targets
            .map(|(source, summary)| (source, label(&summary.title, 120)))
            .collect();
        let automatic_title = selected.reviewed().is_none()
            && self.journal_rename_allowed()
            && self.journal_summaries.queue.preferences().automatic_title;
        self.journal_summaries.reviews.insert(
            id,
            Review::Run {
                receipts,
                mode,
                selected,
                choices,
                screenshots: false,
                full_output: self.journal_summaries.queue.preferences().full_output,
                automatic_title,
                evidence_limits: self.journal_summaries.queue.preferences().evidence_limits,
            },
        );
    }

    fn enqueue_reviewed_journal_summaries(&mut self, id: window::Id) -> Task<Desktop> {
        if !self.journal_summaries_allowed() || self.journal_edit_busy(id) {
            return Task::none();
        }
        if self.automation_retention_busy() {
            self.journal_summaries.status = Some(
                "Wait for automation journal maintenance to finish before starting a summary pass."
                    .into(),
            );
            return Task::none();
        }
        let Some(Review::Run {
            receipts,
            mode,
            selected,
            screenshots,
            full_output,
            automatic_title,
            evidence_limits,
            ..
        }) = self.journal_summaries.reviews.remove(&id)
        else {
            return Task::none();
        };
        let automatic_title = automatic_title && self.journal_rename_allowed();
        let title_approval =
            (automatic_title && selected.reviewed().is_some()).then(Timestamp::now);
        let profile = selected.profile;
        let Ok(root) = self.journal_directory() else {
            return Task::none();
        };
        if receipts
            .iter()
            .any(|(source, _)| source.directory().parent() != Some(root.as_path()))
        {
            self.journal_summaries.status =
                Some("The journal folder changed. Review the selection again.".into());
            return Task::none();
        }
        if self
            .journal_summary_profile(profile.as_ref(), screenshots)
            .is_none()
        {
            self.journal_summaries.status = Some(
                "The reviewed AI profile is unavailable. Choose a profile and review again.".into(),
            );
            return Task::none();
        }
        let Ok(batch) = crate::private_files::nonce() else {
            return Task::none();
        };
        let jobs: io::Result<Vec<_>> = receipts
            .into_iter()
            .map(|(source, title)| {
                Ok(Receipt {
                    generation: crate::private_files::nonce()?,
                    batch: batch.clone(),
                    source: source.summary_receipt(),
                    title,
                    mode,
                    profile: profile.clone(),
                    source_approval: None,
                    raw_only: false,
                    automatic_title,
                    title_approval,
                    screenshots,
                    automation_vision: None,
                    full_output,
                    evidence_limits,
                })
            })
            .collect();
        if let Err(error) = jobs.and_then(|jobs| self.journal_summaries.queue.enqueue(jobs)) {
            self.journal_summaries.status = Some(format!(
                "Could not queue the journal summary pass ({error})."
            ));
            return Task::none();
        }
        let progress = self.open_journal_summary_progress(batch);
        Task::batch([progress, self.start_next_journal_summary()])
    }

    fn cancel_journal_summary(&mut self) {
        if let Some(batch) = self
            .journal_summaries
            .current_receipt()
            .map(|receipt| receipt.batch.clone())
        {
            self.cancel_journal_summary_batch(&batch);
        }
    }

    fn cancel_journal_summary_batch(&mut self, batch: &str) {
        let state = &mut self.journal_summaries;
        let pending = state
            .queue
            .pending()
            .iter()
            .any(|receipt| receipt.batch == batch);
        let active = state
            .active
            .as_mut()
            .filter(|active| active.receipt.batch == batch);
        let running = active.is_some();
        if pending || active.is_some() {
            if let Some(active) = active {
                active.cancel();
            }
            if let Err(error) = state.queue.cancel_batch(batch) {
                state.pause_batch(batch);
                state.status = Some(format!(
                    "Journal summary stopped. The saved pass remains because the cancellation could not be saved ({error})."
                ));
                state.update_details(batch);
                return;
            }
            state.live.retain(|live| live.receipt.batch != batch);
            state.paused.retain(|generation| {
                state
                    .queue
                    .pending()
                    .iter()
                    .any(|receipt| &receipt.generation == generation)
            });
            state.status = Some("Journal summary cancelled. The capture is available.".into());
            if !running {
                state.batch_progress.remove(batch);
            }
            state.update_details(batch);
        }
    }

    pub(in crate::workspace) fn update_journal_summary_worker(
        &mut self,
        message: Worker,
    ) -> Task<Desktop> {
        match message {
            Worker::StartupRecovered(result) => {
                let Some((root, stop)) = self.journal_summaries.startup_recovery.take() else {
                    return Task::none();
                };
                let refresh = if stop.load(Ordering::Acquire) {
                    Task::none()
                } else {
                    match result {
                        Ok(0) => Task::none(),
                        Ok(_) => self.refresh_journal_action_managers(&root),
                        Err(error) => {
                            eprintln!("kortty: could not recover session journals ({error})");
                            Task::none()
                        }
                    }
                };
                Task::batch([refresh, self.start_next_journal_summary()])
            }
            Worker::Pulse => self.start_next_journal_summary(),
            Worker::Progress(ticket) => {
                self.revalidate_journal_summary();
                if let Some(active) = self.journal_summaries.active.as_mut().filter(|active| {
                    active.ticket == ticket && !active.stop.load(Ordering::Acquire)
                }) {
                    active.visible = active
                        .progress
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone();
                }
                Task::none()
            }
            Worker::Finished(ticket, result) => {
                if !self
                    .journal_summaries
                    .active
                    .as_ref()
                    .is_some_and(|active| active.ticket == ticket)
                {
                    return Task::none();
                }
                self.revalidate_journal_summary();
                let active = self
                    .journal_summaries
                    .active
                    .take()
                    .expect("matching summary worker");
                let root = active
                    .receipt
                    .source
                    .directory()
                    .parent()
                    .map(Path::to_owned);
                self.journal_summaries.finish_work(&active);
                if !active.stop.load(Ordering::Acquire) {
                    let completed = result
                        .as_ref()
                        .is_ok_and(|outcome| !outcome.automation_ai_unreachable);
                    match result {
                        Ok(outcome) => {
                            if !active.periodic {
                                if let Err(error) = self
                                    .journal_summaries
                                    .queue
                                    .finish(&active.receipt.generation)
                                {
                                    self.journal_summaries.status = Some(format!(
                                        "The summary was published but its receipt could not be cleared ({error})."
                                    ));
                                    self.journal_summaries
                                        .paused
                                        .insert(active.receipt.generation.clone());
                                } else {
                                    self.journal_summaries.status = Some(
                                        if outcome.automation_ai_unreachable {
                                            "AI not reachable. The automation journal was recorded without AI summaries.".into()
                                        } else {
                                            format!(
                                                "Summary finished · {} activity windows · {} raw · {} images updated · {} image failures.",
                                                outcome.windows,
                                                outcome.raw,
                                                outcome.images,
                                                outcome.image_failures
                                            )
                                        },
                                    );
                                }
                            }
                        }
                        Err(_)
                            if active.periodic
                                && self.journal_summaries.live.iter().any(|live| {
                                    live.receipt.generation == active.receipt.generation
                                        && live.capture.status()
                                            != session_journal::Status::Recording
                                }) =>
                        {
                            // The writer has stopped accepting live publications. The closing
                            // pass will resume this receipt at its last durable checkpoint.
                        }
                        Err(error) => {
                            if !active.periodic
                                || active.receipt.reviewed()
                                || pins_lm_studio_auto(&active.receipt)
                            {
                                self.journal_summaries
                                    .paused
                                    .insert(active.receipt.generation.clone());
                            }
                            self.journal_summaries.status = Some(error);
                        }
                    }
                    if !active.periodic {
                        self.journal_summaries.update_details(&active.receipt.batch);
                        if completed {
                            self.journal_summaries
                                .complete_details(&active.receipt.batch);
                        }
                    }
                }
                let refresh = root.as_deref().map_or_else(Task::none, |root| {
                    self.refresh_journal_action_managers(root)
                });
                Task::batch([refresh, self.start_next_journal_summary()])
            }
        }
    }

    fn start_next_journal_summary(&mut self) -> Task<Desktop> {
        self.start_next_journal_summary_at(Instant::now(), Timestamp::now())
    }

    fn start_next_journal_summary_at(&mut self, now: Instant, age_now: Timestamp) -> Task<Desktop> {
        if !self.journal_summaries_allowed() {
            self.journal_summaries.shutdown();
            return Task::none();
        }
        if self.journal_summaries.startup_recovery.is_some()
            || self.journal_summaries.active.is_some()
        {
            return Task::none();
        }
        let Ok(root) = self.journal_directory() else {
            return Task::none();
        };
        let enabled = self.journal_summaries.queue.preferences().enabled;
        let closed = self
            .journal_summaries
            .queue
            .pending()
            .iter()
            .find(|receipt| {
                !self.journal_summaries.paused.contains(&receipt.generation)
                    && receipt.source.directory().parent() == Some(root.as_path())
                    && (receipt.mode != PassMode::Closing || enabled)
                    && !self.journal_summary_model_pending(receipt)
                    && !self.journal_summaries.live.iter().any(|live| {
                        live.receipt.generation == receipt.generation
                            && !live.capture.done().is_closed()
                    })
            })
            .cloned();
        if let Some(receipt) = closed {
            let redactor = self
                .journal_summaries
                .live
                .iter()
                .find(|live| live.receipt.generation == receipt.generation)
                .map(|live| Arc::clone(&live.redactor));
            self.journal_summaries
                .live
                .retain(|live| live.receipt.generation != receipt.generation);
            return self.spawn_journal_summary(receipt, None, redactor);
        }
        let due = enabled
            .then(|| {
                self.journal_summaries.live.iter().position(|live| {
                    (live.force || live.next <= now)
                        && live.capture.status() == session_journal::Status::Recording
                        && live.receipt.source.directory().parent() == Some(root.as_path())
                        && !self
                            .journal_summaries
                            .paused
                            .contains(&live.receipt.generation)
                        && !self.journal_summary_model_pending(&live.receipt)
                })
            })
            .flatten()
            .map(|index| {
                let live = &mut self.journal_summaries.live[index];
                live.next = now + live.interval;
                (
                    live.receipt.clone(),
                    LivePass {
                        capture: Arc::clone(&live.capture),
                        forced: std::mem::take(&mut live.force),
                        running_preview: Arc::clone(&live.running_preview),
                        now,
                        age_now,
                    },
                    Arc::clone(&live.redactor),
                )
            });
        due.map_or_else(Task::none, |(receipt, live, redactor)| {
            self.spawn_journal_summary(receipt, Some(live), Some(redactor))
        })
    }

    fn journal_summary_model_pending(&self, receipt: &Receipt) -> bool {
        if receipt.raw_only
            || receipt.screenshots
                && (!session_journal::topic_ai_settings::vision_allowed(&self.clipboard.policy)
                    || (receipt.profile.is_none()
                        && !self.window_settings.journal_ai.vision_enabled))
        {
            return false;
        }
        let Some(config) = self.ai_configuration() else {
            return false;
        };
        let profile = match &receipt.profile {
            Some(choice) => config.resolve_profile_reference(&choice.id),
            None if self.uses_legacy_journal_profile() => None,
            None => config.journal_summary_default_id(),
        };
        profile.is_some_and(|id| self.ai_model_resolution_pending(id))
    }

    fn spawn_journal_summary(
        &mut self,
        mut receipt: Receipt,
        live_pass: Option<LivePass>,
        redactor: Option<Arc<Redactor>>,
    ) -> Task<Desktop> {
        let Some(configuration) = self.ai_configuration() else {
            return Task::none();
        };
        let settings = self.window_settings.journal_ai.clone();
        let automatic = receipt.automatic();
        if let Err(error) = self.bind_inherited_journal_model(&mut receipt) {
            self.journal_summaries
                .paused
                .insert(receipt.generation.clone());
            self.journal_summaries.status = Some(format!(
                "Could not save the journal's selected LM Studio model ({error})."
            ));
            self.journal_summaries.update_details(&receipt.batch);
            return Task::none();
        }
        let reviewed = receipt.reviewed();
        let cli = receipt.cli();
        let vision = self
            .automation_screenshot_profile(receipt.automation_vision.as_ref())
            .and_then(|profile| {
                let authority = vision::Authority::new(&configuration, &receipt, profile.clone())?;
                Some((profile, authority))
            });
        let profile = (!receipt.raw_only)
            .then(|| self.journal_summary_profile(receipt.profile.as_ref(), receipt.screenshots))
            .flatten();
        if profile.is_none() && pins_lm_studio_auto(&receipt) {
            self.journal_summaries
                .paused
                .insert(receipt.generation.clone());
            self.journal_summaries.status = Some("The LM Studio model selected for this journal changed or is unavailable. Load the selected model and retry, or cancel the saved pass.".into());
            self.journal_summaries.update_details(&receipt.batch);
            return Task::none();
        }
        let automation_check = automation_probe::Check::new(self, &receipt);
        let closing_scope = (receipt.mode == PassMode::Closing)
            .then(|| {
                Some(preflight::Scope::new(
                    receipt.source.directory().parent()?.to_owned(),
                    PassMode::Closing,
                    self.journal_summaries
                        .queue
                        .preferences()
                        .reviewed_grant()
                        .filter(|grant| Some(&grant.profile) == receipt.profile.as_ref())
                        .cloned(),
                    None,
                    self.clipboard.policy.terminal_logging.redaction_revision(),
                ))
            })
            .flatten();
        let (model, reviewed_mask) = if reviewed {
            if let Some(profile) = &profile
                && self.journal_summaries.queue.authorizes_reviewed(&receipt)
            {
                let authority = https::Authority::new(
                    Arc::clone(&configuration),
                    receipt.clone(),
                    profile.clone(),
                    self.vault.as_ref(),
                );
                let credential = authority.credential();
                let transport = profile.clone().with_output_limit(1_024);
                let authorize = move || authority.authorize();
                let model = if cli {
                    Run::begin_reviewed_cli(transport, authorize)
                } else {
                    Run::begin_reviewed_https(transport, credential.clone(), authorize)
                }
                .ok();
                let mask = https::Mask {
                    vault: self.vault.as_ref().map_or_else(
                        Default::default,
                        crate::vault_window::VaultWindow::journal_question_secrets,
                    ),
                    credential,
                    policy: self.clipboard.policy.terminal_logging.clone(),
                };
                (model, Some(mask))
            } else {
                (None, None)
            }
        } else {
            (
                profile
                    .clone()
                    .and_then(|profile| Run::begin(profile.with_output_limit(1_024)).ok()),
                None,
            )
        };
        if model.is_none()
            && profile.is_some()
            && (!reviewed && vision.is_some()
                || automation_check.is_some()
                    && (!reviewed || self.journal_summaries.queue.authorizes_reviewed(&receipt)))
        {
            // A valid automatic text route could not acquire its worker permit yet.
            return Task::none();
        }
        if model.is_none()
            && (!automatic || reviewed)
            && (live_pass.is_some()
                || !session_journal::summary_store::is_waiting_for_title(
                    &receipt.source.source(),
                    &receipt.generation,
                )
                .unwrap_or(false))
        {
            self.journal_summaries
                .paused
                .insert(receipt.generation.clone());
            self.journal_summaries.status = Some(if cli {
                https::CLI_CHANGED.into()
            } else if reviewed {
                https::CHANGED.into()
            } else {
                "The reviewed AI profile is unavailable. Retry or cancel the saved pass.".into()
            });
            return Task::none();
        }
        let (model, transport) = model.map_or((None, None), |(model, transport)| {
            (Some(model), Some(transport))
        });
        let (vision, vision_transport) = match vision {
            Some((profile, authority)) if model.is_some() => {
                // The worker uses its text permit for both clients sequentially.
                let (client, cancel) = crate::local_ai::Client::new_cancellable(profile.clone());
                (
                    Some((vision::Client::Serial(client), authority)),
                    Some((profile, cancel)),
                )
            }
            Some((profile, authority)) => {
                let Some((session, cancel)) = crate::local_ai::Session::begin(profile.clone())
                else {
                    // Leave the durable pass pending while other workflows hold both slots.
                    return Task::none();
                };
                (
                    Some((vision::Client::OwnSession(session), authority)),
                    Some((profile, cancel)),
                )
            }
            None => (None, None),
        };
        let vision_profile = vision_transport
            .as_ref()
            .map(|(vision, _)| vision)
            .filter(|vision| Some(*vision) != profile.as_ref())
            .cloned();
        let Some(redactor) = redactor.or_else(|| {
            Secrets::default().seal().ok().map(|secrets| {
                Arc::new(Redactor::for_journal(
                    secrets,
                    self.clipboard.policy.terminal_logging.clone(),
                ))
            })
        }) else {
            return Task::none();
        };
        let ticket = self.allocate();
        let started = Instant::now();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (following, plan_following) = self.journal_summaries.following_work(&receipt);
        let visible = PassProgress {
            following,
            ..PassProgress::default()
        };
        let progress = Arc::new(Mutex::new(visible.clone()));
        let worker_progress = Arc::clone(&progress);
        let job = receipt.clone();
        let automation_probes = Arc::clone(&self.journal_summaries.automation_probes);
        let title_root = self
            .journal_summaries
            .queue
            .profile_root()
            .filter(|_| job.automatic_title && self.journal_rename_allowed())
            .map(Path::to_owned);
        let periodic = live_pass.is_some();
        let (send, receive) = async_channel::bounded(1);
        let (changed, updates) = async_channel::bounded(1);
        let show_keywords = !automatic && !reviewed && redactor.allows_incremental_output();
        let worker = thread::Builder::new()
            .name("kortty-journal-summary".into())
            .spawn(move || {
                let mut live = |preview: Option<Preview>| {
                    publish_progress(&worker_progress, &changed, &worker_stop, |state| {
                        state.preview = preview;
                    });
                };
                let mut work = |work: Work| {
                    publish_progress(&worker_progress, &changed, &worker_stop, |state| {
                        state.work.update(work);
                    });
                };
                let mut reporter = Reporter::new(show_keywords, Some(&mut live), Some(&mut work));
                let authorize = || {
                    vision
                        .as_ref()
                        .map_or(Ok(()), |(_, authority)| authority.authorize())
                };
                let allow_title = || {
                    title_root
                        .as_ref()
                        .is_some_and(|root| allows_title(root, &job))
                };
                let result = run_summary(
                    &job,
                    live_pass.as_ref(),
                    SummaryModels {
                        text: model.as_ref(),
                        allow_title: &allow_title,
                        automation_check: automation_check
                            .as_ref()
                            .map(|check| (check, &*automation_probes)),
                        closing_scope: closing_scope.as_ref(),
                        plan_following: &plan_following,
                        planned_work: &mut |generation, weight| {
                            publish_progress(&worker_progress, &changed, &worker_stop, |state| {
                                state.following.insert(generation, weight);
                            });
                        },
                        vision: vision.as_ref().map(|(client, _)| {
                            session_journal::summary_job::Vision {
                                client: client.get(),
                                authorize: &authorize,
                            }
                        }),
                    },
                    &worker_stop,
                    SummaryMask {
                        capture: &redactor,
                        reviewed: reviewed_mask.as_ref(),
                    },
                    |outcome| {
                        publish_progress(&worker_progress, &changed, &worker_stop, |state| {
                            state.outcome = outcome;
                        })
                    },
                    &mut reporter,
                );
                let _ = send.try_send(
                    result.map_err(|error| format!("Journal summary stopped ({error}).")),
                );
            });
        if worker.is_err() {
            self.journal_summaries.status =
                Some("Could not start the journal summary worker.".into());
            return Task::none();
        }
        self.journal_summaries.active = Some(Active {
            ticket,
            receipt,
            periodic,
            started,
            stop,
            transport,
            vision: vision_transport,
            vision_profile,
            progress,
            visible,
            configuration,
            profile,
            settings,
            vault_revision: self
                .vault
                .as_ref()
                .and_then(crate::vault_window::VaultWindow::credential_revision),
        });
        let finish = Task::future(async move {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("The journal summary worker stopped.".into()));
            Desktop::JournalSummary(Worker::Finished(ticket, result))
        });
        let updates = Task::run(updates, move |()| {
            Desktop::JournalSummary(Worker::Progress(ticket))
        });
        Task::batch([finish, updates])
    }

    pub(super) fn journal_summary_controls(
        &self,
        manager: &Manager,
    ) -> Element<'_, JournalMessage> {
        let event = JournalMessage::Summary;
        let allowed = self.journal_summaries_allowed() && !manager.loading;
        let selected = manager
            .catalog
            .as_ref()
            .is_some_and(|catalog| !manager.tree.selected_indices(catalog).is_empty());
        let state = &self.journal_summaries;
        let mut buttons = row![
            button("Re-evaluate with AI…")
                .on_press_maybe((allowed && selected).then_some(event(Event::Review(false)))),
            button("Catch up summaries…")
                .on_press_maybe(allowed.then_some(event(Event::Review(true)))),
            button("Summary settings…").on_press_maybe(allowed.then_some(event(Event::Settings))),
        ]
        .spacing(6);
        let active = state
            .active
            .as_ref()
            .filter(|active| !active.stop.load(Ordering::Acquire));
        if state.current_receipt().is_some() {
            buttons = buttons.push(button("Show progress…").on_press(event(Event::ShowProgress)));
        }
        let status = if let Some(active) = active {
            buttons = buttons.push(button("Cancel summary").on_press(event(Event::Cancel)));
            Some(state.progress_status(active, Instant::now()))
        } else {
            if !state.paused.is_empty() {
                buttons = buttons.push(
                    button("Retry saved pass")
                        .on_press_maybe(allowed.then_some(event(Event::Retry))),
                );
            }
            if !state.queue.pending().is_empty() {
                buttons = buttons.push(button("Cancel saved pass").on_press(event(Event::Cancel)));
            }
            state.status.clone()
        };
        let live = active
            .and_then(|active| active.visible.preview.as_ref())
            .map(summary_live_status);
        column![
            buttons.wrap(),
            status.map(|status| text(status).size(13)),
            live.map(|status| text(status).size(13))
        ]
        .spacing(5)
        .into()
    }

    pub(in crate::workspace) fn pending_journal_summary_sources(&self) -> Vec<Source> {
        let state = &self.journal_summaries;
        state
            .queue
            .pending()
            .iter()
            .map(|receipt| receipt.source.source())
            .chain(
                state
                    .active
                    .iter()
                    .map(|active| active.receipt.source.source()),
            )
            .collect()
    }

    pub(in crate::workspace) fn cancelled_automation_summaries(&self) -> Vec<String> {
        self.journal_summaries.queue.cancelled_automation().to_vec()
    }

    pub(in crate::workspace) fn enqueue_automation_closing_summaries(
        &mut self,
        journals: &[ClosingSummary],
    ) -> Task<Desktop> {
        if !self.journal_summaries_allowed() {
            return Task::none();
        }
        let remaining = MAX_PENDING.saturating_sub(self.journal_summaries.queue.pending().len());
        let preferences = self.journal_summaries.queue.preferences();
        let automatic_title = preferences.automatic_title && self.journal_rename_allowed();
        let jobs: Vec<_> = journals
            .iter()
            .filter(|candidate| !self.journal_summary_pending(&candidate.journal))
            .filter_map(|candidate| {
                let journal = &candidate.journal;
                let source = Source::catalog(journal).ok()?.summary_receipt();
                let generation = journal.id.replace('-', "");
                if self
                    .journal_summaries
                    .queue
                    .cancelled_automation()
                    .contains(&generation)
                {
                    return None;
                }
                let started = journal.run_started_at.or(journal.started_at);
                let grant = started
                    .and_then(|started| preferences.automation_grant_at(started))
                    .filter(|grant| {
                        source.directory().parent()
                            == grant
                                .profile
                                .binding()
                                .map(|binding| binding.root.as_path())
                    });
                let approval = candidate.source_approval.as_ref().and_then(|receipt| {
                    let approval = preferences.source_approval(&receipt.source)?;
                    (candidate.profile.as_ref() == Some(&approval.grant.profile)
                        && source
                            .directory()
                            .parent()
                            .is_some_and(|root| approval.root_matches(root))
                        && approval.verifies_receipt(receipt, &journal.id))
                    .then_some(approval)
                });
                let (profile, raw_only) = match &candidate.profile {
                    _ if candidate.unresolved_profile => (None, true),
                    Some(profile) if approval.is_some() => (Some(profile.clone()), false),
                    _ if candidate.source_approval.is_some() => (None, true),
                    Some(profile)
                        if profile.binding().is_some()
                            && !grant.is_some_and(|grant| &grant.profile == profile) =>
                    {
                        (None, true)
                    }
                    Some(profile) => (Some(profile.clone()), false),
                    None => (
                        grant
                            .map(|grant| grant.profile.clone())
                            .or_else(|| preferences.automation_local_profile().cloned()),
                        false,
                    ),
                };
                let automatic_title = automatic_title && !raw_only;
                let profile = if raw_only {
                    profile
                } else {
                    profile.or_else(|| self.inherited_lm_studio_summary_profile())
                };
                let title_approval = approval
                    .map(|approval| &approval.grant)
                    .or(grant)
                    .filter(|grant| automatic_title && Some(&grant.profile) == profile.as_ref())
                    .and_then(|grant| grant.title_approval_at(started?));
                Some(Receipt {
                    batch: generation.clone(),
                    generation,
                    source,
                    title: label(&journal.title, 120),
                    mode: PassMode::Automation,
                    profile,
                    source_approval: approval.and(candidate.source_approval.clone()),
                    raw_only,
                    automatic_title,
                    title_approval,
                    screenshots: false,
                    automation_vision: candidate
                        .vision
                        .as_ref()
                        .filter(|vision| {
                            !raw_only && self.automation_screenshot_profile(Some(vision)).is_some()
                        })
                        .cloned(),
                    full_output: approval
                        .map_or(preferences.full_output, |approval| approval.full_output),
                    evidence_limits: approval.map_or(preferences.evidence_limits, |approval| {
                        approval.grant.evidence_limits
                    }),
                })
            })
            .take(remaining)
            .collect();
        if !jobs.is_empty()
            && let Err(error) = self.journal_summaries.queue.enqueue(jobs)
        {
            self.journal_summaries.status = Some(format!(
                "Could not queue automation closing summaries ({error})."
            ));
        }
        self.start_next_journal_summary()
    }

    pub(super) fn journal_summary_review(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let wrap = move |event| Desktop::SessionJournal(id, JournalMessage::Summary(event));
        let review = self.journal_summaries.reviews.get(&id)?;
        let content: Element<'_, Event> = match review {
            Review::Run {
                receipts,
                mode,
                choices,
                selected,
                screenshots,
                full_output,
                automatic_title,
                evidence_limits,
            } => {
                let enabled = self.journal_summaries_allowed();
                let reviewed = selected.reviewed();
                let cli = reviewed.is_some_and(|choice| choice.cli.is_some());
                let transport = self.journal_summary_profile(selected.profile.as_ref(), false);
                let command = transport
                    .as_ref()
                    .filter(|_| cli)
                    .and_then(Profile::cli_command_review);
                let preset = transport.as_ref().and_then(summary_preset);
                let vision = self
                    .journal_summary_profile(selected.profile.as_ref(), true)
                    .is_some()
                    && *mode == PassMode::Reevaluate;
                let titles = receipts
                    .iter()
                    .fold(column![].spacing(4), |body, (_, title)| {
                        body.push(text(title).size(13))
                    });
                let rename_allowed = self.journal_rename_allowed();
                column![
                    text(if *mode == PassMode::CatchUp { "Catch up missing summaries" } else { "Re-evaluate with AI" }).size(19),
                    text(if cli {
                        "Approve sending masked input, output and summary text from the listed journals to this CLI program. Run only a program you trust with this evidence. This pass can resume after restart."
                    } else if reviewed.is_some() {
                        "Approve sending masked input, output and summary text from the listed journals, including any saved scrollback, to this HTTPS provider. This pass can resume after restart."
                    } else {
                        "The selected local model receives bounded, masked terminal evidence. The original capture, notes and manual markers stay available."
                    }),
                    pick_list(choices.as_slice(), Some(selected), |choice| Event::Pick(Box::new(choice))).width(Length::Fill),
                    command.map(|command| text(command).font(iced::Font::MONOSPACE)),
                    preset.map(|preset| text(preset).size(13)),
                    evidence_limits.as_ref().map(|limits| text(evidence_scope_label(limits)).size(13)),
                    checkbox(*full_output).label("Read long command output in chunks").on_toggle(Event::FullOutput),
                    (reviewed.is_some() && *full_output).then(|| text("Approval includes additional masked chunks of long command output from these journals.")),
                    checkbox(*automatic_title && rename_allowed)
                        .label("Generate a title for journals that still have a default title")
                        .on_toggle_maybe(rename_allowed.then_some(Event::AutomaticTitle)),
                    (reviewed.is_some() && *automatic_title && rename_allowed)
                        .then(|| text(TITLE_REQUEST_SCOPE).size(13)),
                    checkbox(*screenshots && vision).label("Describe the screenshots again").on_toggle_maybe(vision.then_some(Event::Screenshots)),
                    scrollable(titles).height(Length::Fill),
                    row![button("Cancel").on_press(Event::Dismiss), button(if cli { "Approve CLI summary pass" } else if reviewed.is_some() { "Approve HTTPS summary pass" } else { "Start summary pass" }).on_press_maybe(enabled.then_some(Event::Start))].spacing(8),
                ].spacing(10).padding(14).into()
            }
            Review::Settings {
                enabled,
                interval,
                automatic_title,
                recording_title_request,
                automation_title_request,
                full_output,
                evidence_limits,
                choices,
                selected,
                automation_choices,
                automation,
                automation_source,
                source_profile,
                source_full_output,
                source_title_request,
                imported_recording,
                imported_automation,
                ..
            } => {
                let reviewed = selected.reviewed().filter(|_| *enabled);
                let cli = reviewed.is_some_and(|choice| choice.cli.is_some());
                let transport = self.journal_summary_profile(selected.profile.as_ref(), false);
                let command = transport
                    .as_ref()
                    .filter(|_| cli)
                    .and_then(Profile::cli_command_review);
                let preset = transport
                    .as_ref()
                    .filter(|_| *enabled)
                    .and_then(summary_preset);
                let automation_choice = match automation {
                    AutomationChoice::Profile(choice) => Some(choice.as_ref()),
                    _ => None,
                };
                let automation_reviewed = automation_choice.and_then(Choice::reviewed);
                let scheduled_model = automation_choice.and_then(|choice| {
                    self.journal_summary_profile(choice.profile.as_ref(), false)
                });
                let scheduled_command = scheduled_model
                    .as_ref()
                    .and_then(Profile::cli_command_review);
                let scheduled_preset = scheduled_model.as_ref().and_then(summary_preset);
                let imported_recording_needs_enable =
                    *imported_recording && !*enabled && selected.reviewed().is_some();
                let source_choices = self.automation_summary_sources();
                let outbound_choices: Vec<_> = automation_choices
                    .iter()
                    .filter_map(|choice| match choice {
                        AutomationChoice::Profile(choice) if choice.reviewed().is_some() => {
                            Some(choice.as_ref().clone())
                        }
                        _ => None,
                    })
                    .collect();
                let source_grant = automation_source.as_ref().and_then(|source| {
                    self.journal_summaries
                        .queue
                        .preferences()
                        .source_approval(&source.source)
                });
                let source_command = source_profile
                    .as_ref()
                    .and_then(|choice| self.journal_summary_profile(choice.profile.as_ref(), false))
                    .as_ref()
                    .and_then(Profile::cli_command_review);
                let saved_limits = self.journal_summaries.queue.preferences().evidence_limits;
                let limits_unchanged = evidence_limits.limits() == Ok(saved_limits);
                let imported_evidence = self.imported_summary_evidence_limits();
                let rename_allowed = self.journal_rename_allowed();
                let body = column![
                text("Journal summary settings").size(19),
                checkbox(*enabled).label("Summarize eligible journal recordings automatically").on_toggle(Event::Enable),
                text_input("Summary interval in minutes", interval).on_input(Event::Interval),
                self.ai_configuration().and_then(|config| config.imported_journal_preferences()).map(|(minutes, title)| row![
                    text(format!("Imported from Java · interval: {} · automatic title: {}",
                        minutes.map_or_else(|| "not specified".into(), |value| format!("{value} min")),
                        title.map_or("not specified", |value| if value { "on" } else { "off" }))).size(13),
                    button("Use imported preferences").on_press_maybe(
                        (!title.unwrap_or(false) || rename_allowed).then_some(Event::UseImportedPreferences)),
                ].spacing(8).wrap()),
                checkbox(*automatic_title && rename_allowed)
                    .label("Generate a title when an eligible journal closes")
                    .on_toggle_maybe(rename_allowed.then_some(Event::AutomaticTitle)),
                checkbox(*full_output).label("Read long command output in chunks").on_toggle(Event::FullOutput),
                evidence_limits.view(),
                imported_evidence.map(|(limits, chunking)| row![
                    text(format!("Imported from Java · {} · {}", evidence_scope_label(&limits),
                        if chunking { "long-output chunks on" } else { "long-output chunks off" })).size(13),
                    button("Use imported limits").on_press_maybe(
                        (evidence_limits.limits() != Ok(Some(limits)) || *full_output != chunking)
                            .then_some(Event::UseImportedEvidence),
                    ),
                ].spacing(8).wrap()),
                (!limits_unchanged
                    && !self.journal_summaries.queue.preferences().source_approvals.is_empty())
                    .then(|| text("Changing evidence limits requires new approvals for individual jobs and Swarm.").size(13)),
                text("Terminal recordings"),
                pick_list(choices.as_slice(), Some(selected), |choice| Event::Pick(Box::new(choice))).width(Length::Fill),
                self.imported_journal_profile_view(false, choices.iter(), Some(selected), *imported_recording),
                imported_recording_needs_enable.then(|| text("Enable automatic recording summaries to approve this outbound profile.").size(13)),
                command.map(|command| text(command).font(iced::Font::MONOSPACE)),
                preset.map(|preset| text(preset).size(13)),
                (reviewed.is_some() && rename_allowed).then(|| column![
                    checkbox(*recording_title_request).label("Approve a separate title request for terminal recordings")
                        .on_toggle(Event::RecordingTitleRequest),
                    (*recording_title_request).then(|| text(TITLE_REQUEST_SCOPE).size(13)),
                ].spacing(4)),
                text("These choices apply to new recordings. The connection’s journal settings can override the interval."),
                text("Automation journals"),
                pick_list(automation_choices.as_slice(), Some(automation), |choice| Event::PickAutomation(Box::new(choice))).width(Length::Fill),
                self.imported_journal_profile_view(true, automation_choices.iter().filter_map(|choice| match choice {
                    AutomationChoice::Profile(choice) => Some(choice.as_ref()),
                    _ => None,
                }), automation_choice, *imported_automation),
                text("The job or Swarm journal settings determine whether it gets a closing summary."),
                scheduled_command.map(|command| text(command).font(iced::Font::MONOSPACE)),
                scheduled_preset.map(|preset| text(preset).size(13)),
                (automation_reviewed.is_some() && rename_allowed).then(|| column![
                    checkbox(*automation_title_request).label("Approve a separate title request for automation journals")
                        .on_toggle(Event::AutomationTitleRequest),
                    (*automation_title_request).then(|| text(TITLE_REQUEST_SCOPE).size(13)),
                ].spacing(4)),
                (reviewed.is_some() || automation_reviewed.is_some()).then(|| text(
                    "Approval lets the displayed providers receive masked input, output and summary text, including saved scrollback, from new eligible journals in the selected scope. Use only providers you trust with unattended access."
                )),
                ((reviewed.is_some() || automation_reviewed.is_some()) && *full_output).then(|| text("Approval includes additional masked chunks of long command output.")),
                text("Per-source outbound approvals"),
                pick_list(source_choices, automation_source.as_ref(), Event::PickAutomationSource)
                    .placeholder("Select a job or interactive Swarm").width(Length::Fill),
                automation_source.as_ref().map(|_| pick_list(outbound_choices, source_profile.as_deref(), |choice| Event::PickSourceProfile(Box::new(choice)))
                    .placeholder("Choose an HTTPS or CLI profile").width(Length::Fill)),
                source_grant.map(|grant| text(format!("Approved for this source · {} · {}", grant.grant.profile.model, grant.grant.profile.destination))),
                source_grant.and_then(|grant| grant.grant.evidence_limits.as_ref()).map(|limits| text(evidence_scope_label(limits)).size(13)),
                source_grant.filter(|grant| grant.grant.title_since.is_some())
                    .map(|_| text("Separate title requests approved for this source").size(13)),
                source_command.map(|command| text(command).font(iced::Font::MONOSPACE)),
                source_profile.as_ref().map(|_| checkbox(*source_full_output)
                    .label("Include long output for this source").on_toggle(Event::SourceFullOutput)),
                (source_profile.is_some() && rename_allowed).then(|| column![
                    checkbox(*source_title_request).label("Approve a separate title request for this source")
                        .on_toggle(Event::SourceTitleRequest),
                    (*source_title_request).then(|| text(TITLE_REQUEST_SCOPE).size(13)),
                ].spacing(4)),
                source_profile.as_ref().map(|_| text(if *source_full_output {
                    "Approve masked terminal activity, including retained long-output chunks, for new runs of the selected source."
                } else {
                    "Approve masked terminal activity for new runs of the selected source."
                })),
                row![
                    button("Approve source").on_press_maybe((limits_unchanged && source_profile.is_some()).then_some(Event::ApproveAutomationSource)),
                    button("Revoke source approval").on_press_maybe(source_grant.map(|_| Event::RevokeAutomationSource)),
                ].spacing(8).wrap(),
                ].spacing(10).width(Length::Fill);
                column![
                    scrollable(body).height(Length::Fill),
                    row![
                        button("Cancel").on_press(Event::Dismiss),
                        button(if reviewed.is_some() || automation_reviewed.is_some() {
                            "Approve and save"
                        } else {
                            "Save"
                        })
                        .on_press_maybe(
                            (!imported_recording_needs_enable).then_some(Event::SaveSettings)
                        )
                    ]
                    .spacing(8),
                ]
                .spacing(10)
                .padding(14)
                .into()
            }
        };
        Some(content.map(wrap))
    }
}

fn summary_preset(transport: &Profile) -> Option<String> {
    let preset = transport.prompt_preset();
    Some(format!(
        "Model prompt preset · {preset}\n{}",
        preset.guidance()?
    ))
}

fn evidence_scope_label(limits: &EvidenceLimits) -> String {
    if limits.max_lines_per_direction == 0 {
        format!(
            "Summary evidence · {} context tokens",
            limits.context_tokens
        )
    } else {
        format!(
            "Summary evidence · up to {} input and output lines each · {} context tokens",
            limits.max_lines_per_direction, limits.context_tokens
        )
    }
}

fn publish_progress(
    progress: &Mutex<PassProgress>,
    changed: &async_channel::Sender<()>,
    stop: &AtomicBool,
    update: impl FnOnce(&mut PassProgress),
) {
    let mut current = progress.lock().unwrap_or_else(|error| error.into_inner());
    if !stop.load(Ordering::Acquire) {
        update(&mut current);
        let _ = changed.try_send(());
    }
}

struct SummaryMask<'a> {
    capture: &'a Redactor,
    reviewed: Option<&'a https::Mask>,
}

struct SummaryModels<'a> {
    text: Option<&'a Run>,
    allow_title: &'a dyn Fn() -> bool,
    automation_check: Option<(&'a automation_probe::Check, &'a automation_probe::Outcomes)>,
    closing_scope: Option<&'a preflight::Scope>,
    plan_following: &'a [Receipt],
    planned_work: &'a mut dyn FnMut(String, f64),
    vision: Option<session_journal::summary_job::Vision<'a>>,
}

fn allows_title(root: &Path, receipt: &Receipt) -> bool {
    let policy = crate::clipboard_policy::ClipboardPolicy::load();
    session_journal::topic_ai_settings::allowed(&policy)
        && policy.terminal_logging.journal_rename_allowed()
        && Queue::load(root).is_ok_and(|queue| queue.authorizes_title(receipt))
}

fn check_summary_model<'a>(
    model: Option<&'a Run>,
    receipt: &Receipt,
    reporter: &mut Reporter<'_>,
    closing_scope: Option<&preflight::Scope>,
) -> io::Result<Option<&'a Run>> {
    let Some(model) = model else { return Ok(None) };
    reporter.begin(Stage::Checking);
    let checked = match closing_scope {
        Some(scope) => preflight::check(model, scope),
        None => model.preflight(),
    };
    reporter.clear();
    match checked {
        Ok(()) => Ok(Some(model)),
        Err(error)
            if pins_lm_studio_auto(receipt) && error.kind() == io::ErrorKind::PermissionDenied =>
        {
            Err(error)
        }
        Err(_) if receipt.automatic() && !receipt.reviewed() => Ok(None),
        Err(error) => Err(error),
    }
}

fn run_summary(
    receipt: &Receipt,
    live_pass: Option<&LivePass>,
    models: SummaryModels<'_>,
    stop: &Arc<AtomicBool>,
    masking: SummaryMask<'_>,
    mut progress: impl FnMut(Outcome),
    reporter: &mut Reporter<'_>,
) -> io::Result<Outcome> {
    let model = models.text;
    let source = receipt.source.source();
    if receipt.mode == PassMode::CatchUp {
        let checkpoint = Progress::read(&source)?;
        if checkpoint.finished && !checkpoint.raw
            || checkpoint.generation.is_none() && checkpoint.last_ai > 0
        {
            return Ok(Outcome::default());
        }
    }
    let mask = |text: &str| {
        if let Some(mask) = masking.reviewed {
            mask.apply(text, masking.capture)
        } else {
            mask::prompt_counted(masking.capture, text)
                .map(|(text, _)| text)
                .map_err(io::Error::other)
        }
    };
    let pass = Pass {
        model,
        cancelled: stop,
        mask: &mask,
        redactor: masking.capture,
        full_output: receipt.full_output,
        evidence_limits: receipt.evidence_limits,
    };
    if let Some(live) = live_pass {
        let (checkpoint, language) = Progress::read_live(&source)?;
        let mut after = checkpoint.cursor;
        let mut outcome = Outcome::default();
        let mut checked = None;
        pass.visit_live_windows(
            &source,
            after,
            live.age_now,
            live.forced,
            |window, partial| {
                outcome.partial_capture |= partial;
                let running = (window.end == End::Running).then_some(window.first);
                if let Some(first) = running
                    && !live.forced
                    && live
                        .running_preview
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .is_some_and(|(previous, at)| {
                            previous == first
                                && live.now.saturating_duration_since(at) < RUNNING_AFTER
                        })
                {
                    return Ok(());
                }
                let model = match checked {
                    Some(checked_model) => checked_model,
                    None if window.is_skipped() => model,
                    None => {
                        let model =
                            check_summary_model(model, receipt, reporter, models.closing_scope)?;
                        checked = Some(model);
                        model
                    }
                };
                let (card, usage) =
                    Pass { model, ..pass }.window_reply(&source, &window, &language, reporter)?;
                outcome.raw += usize::from(
                    card.as_ref()
                        .is_some_and(|card| card.state == session_journal::summary_ai::State::Raw),
                );
                outcome.windows += usize::from(card.is_some());
                let written = live.capture.publish_summary(
                    after,
                    window,
                    card,
                    usage,
                    Arc::clone(stop),
                    model.and_then(Run::publication_authority),
                )?;
                after = written
                    .recv_blocking()
                    .map_err(io::Error::other)?
                    .map_err(io::Error::other)?;
                if let Some(first) = running {
                    *live
                        .running_preview
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = Some((first, Instant::now()));
                }
                progress(outcome);
                Ok(())
            },
        )?;
        Ok(outcome)
    } else {
        let title_pending =
            session_journal::summary_store::is_waiting_for_title(&source, &receipt.generation)?;
        let model = if title_pending {
            model
        } else if let Some((check, outcomes)) = models.automation_check {
            check.run(outcomes, receipt, model, stop, reporter)?
        } else {
            check_summary_model(model, receipt, reporter, models.closing_scope)?
        };
        if receipt.mode == PassMode::Closing && !title_pending {
            let authorization = model.and_then(Run::publication_authority);
            session_journal::summary_store::finish_abandoned_authorized(
                &source,
                stop,
                authorization.as_ref(),
            )?;
        }
        let target = Closed {
            source: &source,
            generation: &receipt.generation,
            mode: receipt.mode,
            automatic_title: receipt.automatic_title,
            allow_title: models.allow_title,
            screenshots: receipt.screenshots,
            automation_vision: models.vision,
        };
        let pass = Pass { model, ..pass };
        if model.is_some() {
            for receipt in models.plan_following {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let source = receipt.source.source();
                let allow_title = || {
                    receipt
                        .source
                        .directory()
                        .parent()
                        .is_some_and(|root| allows_title(root, receipt))
                };
                let target = Closed {
                    source: &source,
                    generation: &receipt.generation,
                    mode: receipt.mode,
                    automatic_title: receipt.automatic_title,
                    allow_title: &allow_title,
                    screenshots: receipt.screenshots,
                    automation_vision: None,
                };
                if let Ok(weight) = pass.plan_closed(target) {
                    (models.planned_work)(receipt.generation.clone(), weight);
                }
            }
        }
        if let Some((check, _)) = models.automation_check
            && model.is_none()
        {
            pass.run_closed_guarded(target, progress, reporter, || {
                check.require_current(receipt, stop)
            })
        } else {
            pass.run_closed(target, progress, reporter)
        }
    }
}

fn summary_live_status(preview: &Preview) -> String {
    let stage = match preview.stage {
        Stage::Checking => "Checking the AI profile",
        Stage::Window => "Summarizing a command or other activity",
        Stage::Closing => "Writing the closing summary",
        Stage::Title => "Naming the journal",
        Stage::Screenshot => "Analyzing a reviewed screenshot",
    };
    let mut status = if preview.answer_bytes == 0 {
        stage.to_owned()
    } else {
        format!("{stage} · {} answer bytes received", preview.answer_bytes)
    };
    if !preview.keywords.is_empty() {
        status.push_str("\nProvisional evidence matches · ");
        status.push_str(&preview.keywords.join(", "));
    }
    status
}
