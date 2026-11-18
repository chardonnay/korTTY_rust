//! Journal startup checks use a fixed readiness exchange and reusable, scoped successes.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use sha2::{Digest, Sha256};

use crate::ai_manager::{Configuration, ProfileRef};
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::{self, Profile};
use crate::session_journal::summary_ai::Run;
use crate::session_journal::summary_job::Mode;
use crate::session_journal::summary_queue::{
    AutomaticApproval, AutomationSource, Preferences, ProfileChoice, Queue, SourceApproval,
};
use crate::session_journal::{ConnectionConfig, topic_ai_settings};
use crate::workspace::tab_titles::WindowSettings;
use zeroize::Zeroizing;

use super::{Workspace, https, label};

const SUCCESS_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_CACHE_ENTRIES: usize = 64;
const UNAVAILABLE: &str = "No current AI profile is available for this journal.";
const CHANGED: &str = "The journal AI profile, credentials or approval changed.";
const BUSY: &str = "All AI workers are busy. Try again when a worker is available.";

static SUCCESSES: Mutex<Successes> = Mutex::new(Successes(VecDeque::new()));

#[derive(Clone, Eq, PartialEq)]
pub(in crate::workspace) struct Scope {
    journal_root: PathBuf,
    mode: Mode,
    approval: Option<AutomaticApproval>,
    source_approval: Option<(AutomationSource, SourceApproval)>,
    redaction_revision: Option<[u8; 32]>,
}

impl Scope {
    pub(in crate::workspace) fn new(
        journal_root: PathBuf,
        mode: Mode,
        approval: Option<AutomaticApproval>,
        source_approval: Option<(AutomationSource, SourceApproval)>,
        redaction_revision: Option<[u8; 32]>,
    ) -> Self {
        Self {
            journal_root,
            mode,
            approval,
            source_approval,
            redaction_revision,
        }
    }

    fn require_policy(&self, policy: &ClipboardPolicy) -> io::Result<()> {
        if !topic_ai_settings::allowed(policy)
            || self.mode == Mode::Automation && !policy.automation_journal_allowed()
            || policy.terminal_logging.redaction_revision() != self.redaction_revision
            || policy
                .terminal_logging
                .journal_directory()
                .is_some_and(|root| root != self.journal_root)
        {
            return Err(changed());
        }
        Ok(())
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(super) struct Key {
    profile: Profile,
    scope: Scope,
    credential: Option<[u8; 32]>,
}

struct Successes(VecDeque<(Key, Instant)>);

impl Successes {
    fn contains(&mut self, key: &Key, now: Instant) -> bool {
        self.0
            .retain(|(_, checked)| now.saturating_duration_since(*checked) < SUCCESS_TTL);
        self.0.iter().any(|(current, _)| current == key)
    }

    fn insert(&mut self, key: Key, now: Instant) {
        self.0.retain(|(current, checked)| {
            current != &key && now.saturating_duration_since(*checked) < SUCCESS_TTL
        });
        if self.0.len() == MAX_CACHE_ENTRIES {
            self.0.pop_front();
        }
        self.0.push_back((key, now));
    }
}

fn changed() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, CHANGED)
}

fn credential_digest(
    credential: Result<Option<Zeroizing<String>>, local_ai::Error>,
) -> io::Result<Option<[u8; 32]>> {
    credential
        .map_err(|error| {
            let kind = match error {
                local_ai::Error::Credential => io::ErrorKind::NotConnected,
                local_ai::Error::Review(_) => io::ErrorKind::PermissionDenied,
                _ => io::ErrorKind::Other,
            };
            io::Error::new(kind, CHANGED)
        })
        .map(|credential| {
            credential.map(|credential| {
                let mut digest = Sha256::new();
                digest.update(b"kortty journal startup credential v1\0");
                digest.update(credential.as_bytes());
                digest.finalize().into()
            })
        })
}

pub(super) fn require_current(run: &Run, scope: &Scope) -> io::Result<()> {
    scope.require_policy(&ClipboardPolicy::load())?;
    run.check_probe_authority()
}

pub(super) fn key(run: &Run, scope: &Scope) -> io::Result<Key> {
    require_current(run, scope)?;
    Ok(Key {
        profile: run.profile().clone(),
        scope: scope.clone(),
        credential: credential_digest(run.probe_credential())?,
    })
}

pub(super) fn check(run: &Run, scope: &Scope) -> io::Result<()> {
    check_key(run, scope).map(drop)
}

fn check_key(run: &Run, scope: &Scope) -> io::Result<Key> {
    let first = key(run, scope)?;
    let cache_hit = SUCCESSES
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contains(&first, Instant::now());
    if cache_hit {
        require_current(run, scope)?;
        return if credential_digest(run.profile().current_probe_credential())? != first.credential {
            Err(changed())
        } else {
            Ok(first)
        };
    }
    let authorize = || {
        require_current(run, scope).map_err(|_| CHANGED.to_owned())?;
        if credential_digest(run.profile().current_probe_credential())
            .is_ok_and(|credential| credential == first.credential)
        {
            Ok(())
        } else {
            Err(CHANGED.into())
        }
    };
    run.startup_preflight(&authorize)?;
    if key(run, scope)? != first {
        return Err(changed());
    }
    SUCCESSES
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(first.clone(), Instant::now());
    Ok(first)
}

pub(super) fn failure_message(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::PermissionDenied => CHANGED,
        io::ErrorKind::NotConnected => "The configured AI credential is unavailable or invalid.",
        io::ErrorKind::WouldBlock => BUSY,
        io::ErrorKind::TimedOut => "The AI connection test timed out.",
        io::ErrorKind::Interrupted => "The AI connection test was cancelled.",
        io::ErrorKind::InvalidData => {
            "The AI profile did not return the expected readiness response."
        }
        _ => "The AI profile could not complete the connection test.",
    }
}

pub(super) fn recording_route<'a>(
    preferences: &'a Preferences,
    journal_root: Option<&Path>,
    started: Timestamp,
) -> (Option<&'a AutomaticApproval>, Option<&'a ProfileChoice>) {
    let grant = preferences.reviewed_grant().filter(|grant| {
        started >= grant.since
            && grant
                .profile
                .binding()
                .is_some_and(|binding| Some(binding.root.as_path()) == journal_root)
    });
    let choice = grant
        .map(|grant| &grant.profile)
        .or(preferences.recording_profile.as_ref());
    (grant, choice)
}

#[derive(Clone)]
pub(in crate::workspace) struct Probe {
    name: String,
    target: Option<Recording>,
    checked: Arc<Mutex<Option<Checked>>>,
}

struct Checked {
    credential: Option<[u8; 32]>,
}

#[derive(Clone)]
struct Recording {
    config_root: PathBuf,
    scope: Scope,
    choice: Option<ProfileChoice>,
    profile: Profile,
    vault: Option<Weak<()>>,
    receipt_start: Timestamp,
    journal_ai: topic_ai_settings::Preferences,
    full_output: bool,
    evidence_limits: Option<crate::session_journal::summary_plan::EvidenceLimits>,
}

impl Recording {
    fn same_route(&self, other: &Self) -> bool {
        self.config_root == other.config_root
            && self.scope == other.scope
            && self.choice == other.choice
            && self.profile == other.profile
            && self.journal_ai == other.journal_ai
            && self.full_output == other.full_output
            && self.evidence_limits == other.evidence_limits
            && match (&self.vault, &other.vault) {
                (Some(previous), Some(current)) => Weak::ptr_eq(previous, current),
                (None, None) => true,
                _ => false,
            }
    }

    fn require_current(&self) -> io::Result<()> {
        if self
            .vault
            .as_ref()
            .is_some_and(|vault| vault.strong_count() == 0)
        {
            return Err(changed());
        }
        let policy = ClipboardPolicy::load();
        self.scope.require_policy(&policy)?;
        let settings = WindowSettings::read(&self.config_root.join("window-settings.json"))
            .map_err(|_| changed())?;
        if settings.journal_ai != self.journal_ai {
            return Err(changed());
        }
        let journal_root = policy
            .terminal_logging
            .journal_directory()
            .map(Path::to_owned)
            .map_or_else(|| settings.session_journal.directory(&self.config_root), Ok)
            .map_err(|_| changed())?;
        if journal_root != self.scope.journal_root {
            return Err(changed());
        }
        let queue = Queue::load(&self.config_root).map_err(|_| changed())?;
        let preferences = queue.preferences();
        let (grant, choice) = recording_route(preferences, Some(&journal_root), self.receipt_start);
        if !preferences.enabled
            || !settings.journal_ai.allows_summaries()
            || self.scope.approval.as_ref() != grant
            || self.choice.as_ref() != choice
            || self.full_output != preferences.full_output
            || self.evidence_limits != preferences.evidence_limits
        {
            return Err(changed());
        }
        let config = Configuration::load_from(&self.config_root).map_err(|_| changed())?;
        if !config.enabled()
            || recording_profile(
                &config,
                &self.journal_ai,
                choice,
                Some(&journal_root),
                &policy,
                false,
            )
            .map(|profile| profile.with_output_limit(1_024))
            .as_ref()
                != Some(&self.profile)
        {
            return Err(changed());
        }
        Ok(())
    }
}

pub(super) fn recording_profile(
    config: &Configuration,
    settings: &topic_ai_settings::Preferences,
    choice: Option<&ProfileChoice>,
    journal_root: Option<&Path>,
    policy: &ClipboardPolicy,
    vision: bool,
) -> Option<Profile> {
    let legacy = || {
        if vision {
            settings.vision_profile()
        } else {
            settings.profile()
        }
        .ok()
        .flatten()
    };
    if vision && choice.is_none() && !settings.vision_enabled {
        return None;
    }
    match choice {
        Some(choice) if choice.binding().is_some() => {
            if vision {
                return None;
            }
            https::resolve(config, choice, journal_root?, policy)
        }
        Some(choice) if choice.id == ProfileRef::Numeric(0) => (choice.model == settings.model
            && choice.destination == settings.endpoint
            && choice.policy_model_matches(None))
        .then(legacy)
        .flatten(),
        Some(choice) => {
            let id = config.resolve_profile_reference(&choice.id)?;
            if !config.local_ai_profile_matches(id, &choice.model, &choice.destination) {
                return None;
            }
            let profile = config.journal_summary_transport(id, vision).ok()?;
            choice
                .policy_model_matches(profile.reviewed_model_fingerprint())
                .then_some(profile)
        }
        None if (!settings.endpoint.is_empty() || !settings.model.is_empty())
            && (!config.journal_profile_is_explicit()
                || config.journal_profile(false).is_none()) =>
        {
            legacy()
        }
        None => config.journal_profile(vision)?.ok(),
    }
}

impl Workspace {
    pub(in crate::workspace) fn journal_start_probe(
        &self,
        config: ConnectionConfig,
        started: Option<Timestamp>,
    ) -> Option<Probe> {
        let preferences = self.journal_summaries.queue.preferences();
        if !config.ai_summaries || !self.journal_summaries_allowed() || !preferences.enabled {
            return None;
        }
        let mut probe = Probe {
            name: "Journal AI profile".into(),
            target: None,
            checked: Arc::default(),
        };
        if let Ok(journal_root) = self.journal_directory()
            && let Some(config) = self.ai_configuration()
            && let Some(profile_root) = self.journal_summaries.queue.profile_root()
        {
            let receipt_start = started.unwrap_or_else(Timestamp::now);
            let (grant, choice) = recording_route(preferences, Some(&journal_root), receipt_start);
            if let Some(profile) = self.journal_summary_profile(choice, false) {
                probe.name = label(profile.journal_name(), 100);
                let scope = Scope::new(
                    journal_root,
                    Mode::Closing,
                    grant.cloned(),
                    None,
                    self.clipboard.policy.terminal_logging.redaction_revision(),
                );
                let config_root = config.saved_root().unwrap_or(profile_root);
                if profile_root == config_root {
                    probe.target = Some(Recording {
                        config_root: config_root.to_owned(),
                        scope,
                        choice: choice.cloned(),
                        profile: profile.with_output_limit(1_024),
                        vault: self
                            .vault
                            .as_ref()
                            .and_then(crate::vault_window::VaultWindow::credential_revision),
                        receipt_start,
                        journal_ai: self.window_settings.journal_ai.clone(),
                        full_output: preferences.full_output,
                        evidence_limits: preferences.evidence_limits,
                    });
                }
            } else if let Some(choice) = choice.filter(|choice| !choice.model.is_empty()) {
                probe.name = label(&choice.model, 100);
            }
        }
        Some(probe)
    }
}

pub(in crate::workspace) struct Cancel(Arc<Control>);

#[derive(Default)]
struct Control {
    stopped: AtomicBool,
    transport: Mutex<Option<local_ai::Cancel>>,
}

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.stopped.store(true, Ordering::Release);
        self.0
            .transport
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }
}

impl Probe {
    pub(in crate::workspace) fn name(&self) -> &str {
        &self.name
    }

    pub(in crate::workspace) fn is_current(
        &self,
        workspace: &Workspace,
        config: ConnectionConfig,
        started: Option<Timestamp>,
    ) -> bool {
        let Some(current) = workspace.journal_start_probe(config, started) else {
            return false;
        };
        self.target
            .as_ref()
            .zip(current.target.as_ref())
            .is_some_and(|(previous, current)| {
                previous.same_route(current)
                    && previous
                        .vault
                        .as_ref()
                        .is_none_or(|vault| vault.strong_count() > 0)
                    && previous
                        .scope
                        .require_policy(&ClipboardPolicy::load())
                        .is_ok()
                    && credential_digest(current.profile.current_probe_credential()).is_ok_and(
                        |credential| {
                            self.checked
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .as_ref()
                                .is_some_and(|checked| checked.credential == credential)
                        },
                    )
            })
    }

    pub(in crate::workspace) fn start(
        &self,
    ) -> (Cancel, async_channel::Receiver<Result<(), String>>) {
        let control = Arc::new(Control::default());
        let (reply, receiver) = async_channel::bounded(1);
        *self
            .checked
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        let Some(target) = self.target.clone() else {
            let _ = reply.try_send(Err(UNAVAILABLE.into()));
            return (Cancel(control), receiver);
        };
        let worker_control = Arc::clone(&control);
        let checked = Arc::clone(&self.checked);
        let fallback = reply.clone();
        if thread::Builder::new()
            .name("kortty-journal-ai-preflight".into())
            .spawn(move || {
                let outcome = target
                    .run(&worker_control)
                    .map(|credential| {
                        *checked.lock().unwrap_or_else(|error| error.into_inner()) =
                            Some(Checked { credential });
                    })
                    .map_err(|error| failure_message(&error).into());
                let _ = reply.try_send(outcome);
            })
            .is_err()
        {
            let _ = fallback.try_send(Err("Could not start the AI connection test.".into()));
        }
        (Cancel(control), receiver)
    }
}

impl Recording {
    fn run(self, control: &Arc<Control>) -> io::Result<Option<[u8; 32]>> {
        self.require_current()?;
        let profile = self.profile.clone();
        let credential = profile.completion_secret();
        let scope = self.scope.clone();
        let authority = move || self.require_current().map_err(|_| CHANGED.into());
        let (run, cancel) = if profile.is_remote() {
            Run::begin_reviewed_https(profile, credential, authority)
        } else if profile.is_cli() {
            Run::begin_reviewed_cli(profile, authority)
        } else {
            Run::begin_startup_local(profile, authority)
        }?;
        {
            let mut slot = control
                .transport
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if control.stopped.load(Ordering::Acquire) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            *slot = Some(cancel);
        }
        let result = check_key(&run, &scope).map(|key| key.credential);
        control
            .transport
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_ai::test_support::{exclusive_run, fixture_response, start_test_double};

    #[test]
    fn journal_startup_cache_reuses_only_an_exact_current_success() {
        let _exclusive = exclusive_run();
        let (profile, server) =
            start_test_double(vec![fixture_response("200 OK", "", r#"{"ready":true}"#)]);
        let scratch = crate::snippets::tests::Profile::new();
        let scope = Scope::new(
            scratch.0.canonicalize().unwrap(),
            Mode::Closing,
            None,
            None,
            ClipboardPolicy::load()
                .terminal_logging
                .redaction_revision(),
        );
        let authorized = Arc::new(AtomicBool::new(true));
        let check_authorized = Arc::clone(&authorized);
        let (run, _cancel) = Run::begin_startup_local(profile, move || {
            check_authorized
                .load(Ordering::Acquire)
                .then_some(())
                .ok_or_else(|| CHANGED.into())
        })
        .unwrap();
        check(&run, &scope).unwrap();
        check(&run, &scope).unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["messages"][1]["content"], "{}");
        authorized.store(false, Ordering::Release);
        assert_eq!(
            check(&run, &scope).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
