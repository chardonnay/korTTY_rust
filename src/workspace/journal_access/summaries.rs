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
use crate::session_journal::summary_ai::{Preview, Reporter, Run, Stage};
use crate::session_journal::summary_job::{self, Closed, Mode as PassMode, Outcome, Pass};
use crate::session_journal::summary_queue::{
    AutomaticApproval, MAX_PENDING, ProfileBinding, ProfileChoice, Queue, Receipt,
};
use crate::session_journal::summary_store::Progress;
use crate::session_journal::viewer::Source;
use crate::session_journal::{self, Capture, Catalog, ConnectionConfig, Summary};
use crate::terminal_logging::{Redactor, Secrets};
use crate::workspace::terminal_ai::mask;

use super::{Desktop, Manager, Message as JournalMessage, Mode, Workspace, label};

const CLOCK: Duration = Duration::from_secs(1);
pub(in crate::workspace) const PROGRESS_TITLE: &str = "Journal Summary Progress";

mod https;
mod progress;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub enum Event {
    Review(bool),
    Pick(Box<Choice>),
    Screenshots(bool),
    FullOutput(bool),
    Start,
    Dismiss,
    Settings,
    Enable(bool),
    Interval(String),
    AutomaticTitle(bool),
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
    startup_recovery: Option<(PathBuf, Arc<AtomicBool>)>,
    active: Option<Active>,
    live: Vec<Live>,
    paused: HashSet<String>,
    reviews: HashMap<window::Id, Review>,
    status: Option<String>,
    details: Option<progress::Details>,
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
}

struct Active {
    ticket: u64,
    receipt: Receipt,
    periodic: bool,
    started: Instant,
    stop: Arc<AtomicBool>,
    transport: Option<Cancel>,
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
}

impl Active {
    fn cancel(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.transport.take();
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
    },
    Settings {
        enabled: bool,
        interval: String,
        automatic_title: bool,
        full_output: bool,
        choices: Vec<Choice>,
        selected: Choice,
    },
}

impl State {
    pub(in crate::workspace) fn load(&mut self, profile: &Path) {
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
        let valid = self.journal_summaries_allowed()
            && self
                .ai_configuration()
                .is_some_and(|config| Arc::ptr_eq(&config, &active.configuration))
            && self.window_settings.journal_ai == active.settings
            && self.journal_directory().ok().as_deref()
                == active.receipt.source.directory().parent()
            && (!active.receipt.screenshots
                || session_journal::topic_ai_settings::vision_allowed(&self.clipboard.policy))
            && (!active.receipt.reviewed()
                || self
                    .journal_summaries
                    .queue
                    .authorizes_reviewed(&active.receipt)
                    && https::vault_matches(active.vault_revision.as_ref(), self.vault.as_ref())
                    && self.journal_summary_profile(active.receipt.profile.as_ref(), false)
                        == active.profile);
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
        }
    }

    pub(in crate::workspace) fn journal_summaries_allowed(&self) -> bool {
        session_journal::topic_ai_settings::allowed(&self.clipboard.policy)
            && self.window_settings.journal_ai.enabled
            && self.ai_features_enabled()
            && !self.exiting
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
        let receipt = Receipt {
            batch: generation.clone(),
            generation,
            source: Source::capture(&capture).summary_receipt(),
            title: label(&title, 120),
            mode: PassMode::Closing,
            profile: preferences.reviewed_grant().and_then(|grant| {
                let source = Source::capture(&capture);
                (capture.started_at() >= grant.since
                    && source.directory().parent()
                        == grant
                            .profile
                            .binding()
                            .map(|binding| binding.root.as_path()))
                .then(|| grant.profile.clone())
            }),
            automatic_title: preferences.automatic_title,
            screenshots: false,
            full_output: preferences.full_output,
        };
        match self.journal_summaries.queue.enqueue([receipt.clone()]) {
            Ok(()) => self.journal_summaries.live.push(Live {
                capture,
                receipt,
                interval,
                next: Instant::now() + interval,
                redactor,
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
                    ..
                }) if choices.contains(choice.as_ref()) => {
                    *selected = *choice;
                    *screenshots = false;
                }
                Some(Review::Settings {
                    choices, selected, ..
                }) if choices.contains(choice.as_ref()) => *selected = *choice,
                _ => {}
            },
            Event::Screenshots(value) => {
                if let Some(Review::Run { screenshots, .. }) =
                    self.journal_summaries.reviews.get_mut(&id)
                {
                    *screenshots = value;
                }
            }
            Event::FullOutput(value) => match self.journal_summaries.reviews.get_mut(&id) {
                Some(Review::Run { full_output, .. } | Review::Settings { full_output, .. }) => {
                    *full_output = value;
                }
                None => {}
            },
            Event::Start => return self.enqueue_reviewed_journal_summaries(id),
            Event::Dismiss => self.journal_summaries.close_review(id),
            Event::Settings => {
                let settings = self.journal_summaries.queue.preferences();
                let mut choices = vec![Choice {
                    profile: None,
                    label: "Use local journal profile".into(),
                }];
                choices.extend(self.journal_summary_https_choices());
                choices.extend(self.journal_summary_cli_choices());
                let selected = if let Some(grant) = settings.reviewed_grant() {
                    choices
                        .iter()
                        .find(|choice| choice.profile.as_ref() == Some(&grant.profile))
                } else if !settings.enabled {
                    self.preferred_journal_summary_cli(&choices)
                } else {
                    None
                }
                .unwrap_or(&choices[0])
                .clone();
                self.journal_summaries.reviews.insert(
                    id,
                    Review::Settings {
                        enabled: settings.enabled,
                        interval: settings.interval_minutes.to_string(),
                        automatic_title: settings.automatic_title,
                        full_output: settings.full_output,
                        choices,
                        selected,
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
            Event::AutomaticTitle(value) => {
                if let Some(Review::Settings {
                    automatic_title, ..
                }) = self.journal_summaries.reviews.get_mut(&id)
                {
                    *automatic_title = value;
                }
            }
            Event::SaveSettings if self.journal_summaries_allowed() => {
                if let Some(Review::Settings {
                    enabled,
                    interval,
                    automatic_title,
                    full_output,
                    selected,
                    ..
                }) = self.journal_summaries.reviews.get(&id)
                    && let Ok(interval_minutes) = interval.parse()
                {
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
                            let previous = preferences
                                .reviewed_grant()
                                .filter(|_| preferences.full_output == *full_output);
                            Some(
                                previous
                                    .filter(|grant| &grant.profile == choice)
                                    .cloned()
                                    .unwrap_or_else(|| AutomaticApproval {
                                        profile: choice.clone(),
                                        since: Timestamp::now(),
                                    }),
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
                    let settings = session_journal::summary_queue::Preferences {
                        enabled: *enabled,
                        interval_minutes,
                        automatic_title: *automatic_title,
                        full_output: *full_output,
                        https,
                        cli,
                    };
                    match self.journal_summaries.queue.set_preferences(settings) {
                        Ok(()) => {
                            self.journal_summaries.close_review(id);
                            let queue = &self.journal_summaries.queue;
                            self.journal_summaries.live.retain(|live| {
                                !live.receipt.reviewed() || queue.authorizes_reviewed(&live.receipt)
                            });
                            self.revalidate_journal_summary();
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
            Event::SaveSettings => {}
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
        let Some(choice) = choice else {
            let profile = self.configured_journal_ai(vision)?;
            profile.require_current_policy_authority().ok()?;
            return Some(profile);
        };
        if choice.id == ProfileRef::Numeric(0) {
            let preferences = &self.window_settings.journal_ai;
            if !choice.policy_model_matches(None)
                || preferences.model != choice.model
                || preferences.endpoint != choice.destination
            {
                return None;
            }
            return if vision {
                preferences.vision_profile()
            } else {
                preferences.profile()
            }
            .ok()
            .flatten();
        }
        let config = self.ai_configuration()?;
        if choice.binding().is_some() {
            return (!vision)
                .then(|| {
                    https::resolve(
                        &config,
                        choice,
                        &self.journal_directory().ok()?,
                        &self.clipboard.policy,
                    )
                })
                .flatten();
        }
        let id = config.resolve_profile_reference(&choice.id)?;
        if !config.local_ai_profile_matches(id, &choice.model, &choice.destination) {
            return None;
        }
        let profile = config.journal_summary_transport(id, vision).ok()?;
        profile.require_current_policy_authority().ok()?;
        choice
            .policy_model_matches(profile.policy_authority_fingerprint())
            .then_some(profile)
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
            .journal_question_https_profiles()
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
        let receipts: Vec<_> = indices
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
                    .map(|source| (source, label(&summary.title, 120)))
            })
            .take(MAX_PENDING)
            .collect();
        let choices = self.journal_summary_choices();
        if receipts.is_empty() || choices.is_empty() {
            self.journal_summaries.status =
                Some("Select a stopped journal and an available AI profile.".into());
            return;
        }
        let mode = if catch_up {
            PassMode::CatchUp
        } else {
            PassMode::Reevaluate
        };
        let selected = self
            .preferred_journal_summary_cli(&choices)
            .unwrap_or(&choices[0])
            .clone();
        self.journal_summaries.reviews.insert(
            id,
            Review::Run {
                receipts,
                mode,
                selected,
                choices,
                screenshots: false,
                full_output: self.journal_summaries.queue.preferences().full_output,
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
            ..
        }) = self.journal_summaries.reviews.remove(&id)
        else {
            return Task::none();
        };
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
                    automatic_title: false,
                    screenshots,
                    full_output,
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
                if !active.stop.load(Ordering::Acquire) {
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
                                    self.journal_summaries.status = Some(format!(
                                        "Summary finished · {} activity windows · {} raw · {} images updated · {} image failures.",
                                        outcome.windows,
                                        outcome.raw,
                                        outcome.images,
                                        outcome.image_failures
                                    ));
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
                            if !active.periodic || active.receipt.reviewed() {
                                self.journal_summaries
                                    .paused
                                    .insert(active.receipt.generation.clone());
                            }
                            self.journal_summaries.status = Some(error);
                        }
                    }
                    if !active.periodic {
                        self.journal_summaries.update_details(&active.receipt.batch);
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
        let now = Instant::now();
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
                    live.next <= now
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
                    Arc::clone(&live.capture),
                    Arc::clone(&live.redactor),
                )
            });
        due.map_or_else(Task::none, |(receipt, capture, redactor)| {
            self.spawn_journal_summary(receipt, Some(capture), Some(redactor))
        })
    }

    fn journal_summary_model_pending(&self, receipt: &Receipt) -> bool {
        if receipt.screenshots
            && (!session_journal::topic_ai_settings::vision_allowed(&self.clipboard.policy)
                || (receipt.profile.is_none() && !self.window_settings.journal_ai.vision_enabled))
        {
            return false;
        }
        let Some(config) = self.ai_configuration() else {
            return false;
        };
        let profile = match &receipt.profile {
            Some(choice) => config.resolve_profile_reference(&choice.id),
            None => config.journal_summary_default_id(),
        };
        profile.is_some_and(|id| self.policy_auto_model_pending(id))
    }

    fn spawn_journal_summary(
        &mut self,
        receipt: Receipt,
        capture: Option<Arc<Capture>>,
        redactor: Option<Arc<Redactor>>,
    ) -> Task<Desktop> {
        let Some(configuration) = self.ai_configuration() else {
            return Task::none();
        };
        let settings = self.window_settings.journal_ai.clone();
        let automatic = receipt.automatic();
        let reviewed = receipt.reviewed();
        let cli = receipt.cli();
        let profile = self.journal_summary_profile(receipt.profile.as_ref(), receipt.screenshots);
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
                    Run::begin_reviewed_https(transport, authorize)
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
        if model.is_none() && (!automatic || reviewed) {
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
        let progress = Arc::new(Mutex::new(PassProgress::default()));
        let worker_progress = Arc::clone(&progress);
        let job = receipt.clone();
        let periodic = capture.is_some();
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
                let mut reporter = Reporter {
                    show_keywords,
                    update: Some(&mut live),
                };
                let result = run_summary(
                    &job,
                    capture.as_deref(),
                    model.as_ref(),
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
            progress,
            visible: PassProgress::default(),
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
            Some(format!(
                "{} · {} activity windows processed",
                active.receipt.title, active.visible.outcome.windows
            ))
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
        journals: &[Summary],
    ) -> Task<Desktop> {
        if !self.journal_summaries_allowed() {
            return Task::none();
        }
        let remaining = MAX_PENDING.saturating_sub(self.journal_summaries.queue.pending().len());
        let preferences = self.journal_summaries.queue.preferences();
        let grant = preferences.reviewed_grant();
        let jobs: Vec<_> = journals
            .iter()
            .filter(|journal| !self.journal_summary_pending(journal))
            .filter_map(|journal| {
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
                let profile = grant
                    .filter(|grant| {
                        journal
                            .started_at
                            .is_some_and(|started| started >= grant.since)
                            && source.directory().parent()
                                == grant
                                    .profile
                                    .binding()
                                    .map(|binding| binding.root.as_path())
                    })
                    .map(|grant| grant.profile.clone());
                Some(Receipt {
                    batch: generation.clone(),
                    generation,
                    source,
                    title: label(&journal.title, 120),
                    mode: PassMode::Automation,
                    profile,
                    automatic_title: false,
                    screenshots: false,
                    full_output: preferences.full_output,
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
                    checkbox(*full_output).label("Read long command output in chunks").on_toggle(Event::FullOutput),
                    (reviewed.is_some() && *full_output).then(|| text("Approval includes additional masked chunks of long command output from these journals.")),
                    checkbox(*screenshots && vision).label("Describe the screenshots again").on_toggle_maybe(vision.then_some(Event::Screenshots)),
                    scrollable(titles).height(Length::Fill),
                    row![button("Cancel").on_press(Event::Dismiss), button(if cli { "Approve CLI summary pass" } else if reviewed.is_some() { "Approve HTTPS summary pass" } else { "Start summary pass" }).on_press_maybe(enabled.then_some(Event::Start))].spacing(8),
                ].spacing(10).padding(14).into()
            }
            Review::Settings {
                enabled,
                interval,
                automatic_title,
                full_output,
                choices,
                selected,
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
                column![
                text("Journal summary settings").size(19),
                checkbox(*enabled).label("Summarize eligible journal recordings automatically").on_toggle(Event::Enable),
                text_input("Summary interval in minutes", interval).on_input(Event::Interval),
                checkbox(*automatic_title).label("Allow an automatic title when the journal closes").on_toggle(Event::AutomaticTitle),
                checkbox(*full_output).label("Read long command output in chunks").on_toggle(Event::FullOutput),
                pick_list(choices.as_slice(), Some(selected), |choice| Event::Pick(Box::new(choice))).width(Length::Fill),
                command.map(|command| text(command).font(iced::Font::MONOSPACE)),
                preset.map(|preset| text(preset).size(13)),
                text("These choices apply to new recordings. The connection’s journal settings can override the interval."),
                text(if cli {
                    "Approve sending masked input, output and summary text from new eligible recordings and scheduled journals to this CLI program while korTTY runs. Use a program you trust with unattended access to this evidence. Choose the local profile or disable automatic summaries here to revoke approval."
                } else if reviewed.is_some() {
                    "Approve sending masked input, output and summary text from new eligible recordings and scheduled journals, including any saved scrollback, to this HTTPS provider. Choose the local profile or disable automatic summaries here to revoke approval."
                } else {
                    "Local summaries use the configured journal profile."
                }),
                (reviewed.is_some() && *full_output).then(|| text("Approval includes additional masked chunks of long command output from new eligible recordings.")),
                row![button("Cancel").on_press(Event::Dismiss), button(if cli { "Approve automatic CLI summaries" } else if reviewed.is_some() { "Approve automatic HTTPS summaries" } else { "Save" }).on_press(Event::SaveSettings)].spacing(8),
            ].spacing(10).padding(14).into()
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

fn run_summary(
    receipt: &Receipt,
    capture: Option<&Capture>,
    model: Option<&Run>,
    stop: &Arc<AtomicBool>,
    masking: SummaryMask<'_>,
    mut progress: impl FnMut(Outcome),
    reporter: &mut Reporter<'_>,
) -> io::Result<Outcome> {
    let source = receipt.source.source();
    if receipt.mode == PassMode::CatchUp {
        let checkpoint = Progress::read(&source)?;
        if checkpoint.finished && !checkpoint.raw
            || checkpoint.generation.is_none() && checkpoint.last_ai > 0
        {
            return Ok(Outcome::default());
        }
    }
    let checked = model
        .map(|model| {
            reporter.begin(Stage::Checking);
            let result = model.preflight();
            reporter.clear();
            result.map(|()| model)
        })
        .transpose();
    let model = match checked {
        Ok(model) => model,
        Err(_) if receipt.automatic() && !receipt.reviewed() => None,
        Err(error) => return Err(error),
    };
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
    };
    if let Some(capture) = capture {
        let mut after = Progress::read(&source)?.cursor;
        let mut outcome = Outcome::default();
        summary_job::visit_windows(
            &source,
            after,
            false,
            receipt.full_output,
            Timestamp::now(),
            stop,
            |window, partial| {
                let card = pass.window(&source, &window, reporter)?;
                outcome.raw += usize::from(
                    card.as_ref()
                        .is_some_and(|card| card.state == session_journal::summary_ai::State::Raw),
                );
                outcome.windows += usize::from(card.is_some());
                let written = capture.publish_summary(
                    after,
                    window,
                    card,
                    Arc::clone(stop),
                    model.and_then(Run::publication_authority),
                )?;
                after = written
                    .recv_blocking()
                    .map_err(io::Error::other)?
                    .map_err(io::Error::other)?;
                outcome.partial_capture |= partial;
                progress(outcome);
                Ok(())
            },
        )?;
        Ok(outcome)
    } else {
        if receipt.mode == PassMode::Closing {
            let authorization = model.and_then(Run::publication_authority);
            session_journal::summary_store::finish_abandoned_authorized(
                &source,
                stop,
                authorization.as_ref(),
            )?;
        }
        pass.run_closed(
            Closed {
                source: &source,
                generation: &receipt.generation,
                mode: receipt.mode,
                automatic_title: receipt.automatic_title,
                screenshots: receipt.screenshots,
            },
            progress,
            reporter,
        )
    }
}

fn summary_live_status(preview: &Preview) -> String {
    let stage = match preview.stage {
        Stage::Checking => "Checking the AI profile",
        Stage::Window => "Summarizing a command or other activity",
        Stage::Closing => "Writing the closing summary",
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
