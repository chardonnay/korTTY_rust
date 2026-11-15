use std::collections::VecDeque;

use crate::clipboard_policy::ClipboardPolicy;
use crate::session_journal::summary_store;
use crate::session_journal::tree::SourceKind;
use crate::workspace::tab_titles::WindowSettings;

use super::{
    AtomicBool, Mutex, Ordering, PassMode, Path, PathBuf, Queue, Receipt, Reporter, Run, Stage,
    Timestamp, Workspace, io, preflight, session_journal,
};

const RETAINED_RUNS: usize = 128;

/// In-process outcomes for sibling journals of the same run. A failed target also has a
/// durable notice so its own replay never contacts the provider or adds the notice again.
#[derive(Default)]
pub(super) struct Outcomes(Mutex<VecDeque<(Identity, Result<(), &'static str>)>>);

impl Outcomes {
    fn get(&self, identity: &Identity) -> Option<Result<(), &'static str>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .rev()
            .find_map(|(key, result)| (key == identity).then_some(*result))
    }

    fn remember(&self, identity: Identity, result: Result<(), &'static str>) {
        let mut outcomes = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if outcomes.len() >= RETAINED_RUNS {
            outcomes.pop_front();
        }
        outcomes.push_back((identity, result));
    }
}

#[derive(PartialEq, Eq)]
struct Identity {
    route: preflight::Key,
    run: String,
    kind: SourceKind,
    source: String,
    started: Timestamp,
}

pub(super) struct Check {
    scope: preflight::Scope,
    profile_root: PathBuf,
    journal_root: PathBuf,
    redaction_revision: Option<[u8; 32]>,
}

impl Check {
    pub(super) fn new(workspace: &Workspace, receipt: &Receipt) -> Option<Self> {
        if receipt.mode != PassMode::Automation || receipt.raw_only {
            return None;
        }
        let queue = &workspace.journal_summaries.queue;
        let profile_root = queue.profile_root()?.to_owned();
        let journal_root = receipt.source.directory().parent()?.to_owned();
        let policy = &workspace.clipboard.policy;
        let redaction_revision = policy.terminal_logging.redaction_revision();
        let prefs = queue.preferences();
        let source_approval = receipt.source_approval.as_ref().and_then(|proof| {
            Some((
                proof.source.clone(),
                prefs.source_approval(&proof.source)?.clone(),
            ))
        });
        let approval = receipt
            .reviewed()
            .then(|| {
                source_approval
                    .as_ref()
                    .map(|(_, source)| &source.grant)
                    .or_else(|| prefs.automation_grant())
                    .cloned()
            })
            .flatten();
        Some(Self {
            scope: preflight::Scope::new(
                journal_root.clone(),
                PassMode::Automation,
                approval,
                source_approval,
                redaction_revision,
            ),
            profile_root,
            journal_root,
            redaction_revision,
        })
    }

    pub(super) fn run<'a>(
        &self,
        outcomes: &Outcomes,
        receipt: &Receipt,
        model: Option<&'a Run>,
        stop: &AtomicBool,
        reporter: &mut Reporter<'_>,
    ) -> io::Result<Option<&'a Run>> {
        self.require_current(receipt, stop)?;
        let source = receipt.source.source();
        let checkpoint = summary_store::Progress::read(&source)?;
        if checkpoint.finished
            && checkpoint.generation.as_deref() == Some(receipt.generation.as_str())
        {
            return Ok(None);
        }
        if summary_store::automation_probe_failed(&source, &receipt.generation)? {
            return Ok(None);
        }
        let Some(model) = model else {
            self.record_failure(
                receipt,
                stop,
                "No AI profile is available for this journal.",
            )?;
            return Ok(None);
        };
        let key = match preflight::key(model, &self.scope) {
            Ok(key) => key,
            Err(error) if error.kind() == io::ErrorKind::NotConnected => {
                self.record_failure(receipt, stop, preflight::failure_message(&error))?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let journal = source
            .read(None)?
            .ok_or_else(|| io::Error::other("The selected automation journal changed"))?;
        let summary = journal.summary;
        let identity = summary
            .run_id
            .filter(|id| kortty_jobs::valid_session_journal_id(id))
            .zip(summary.source_id)
            .zip(summary.run_started_at)
            .filter(|_| summary.source_kind != SourceKind::Interactive)
            .map(|((run, source), started)| Identity {
                route: key.clone(),
                run,
                kind: summary.source_kind,
                source,
                started,
            });
        if let Some((identity, result)) = identity
            .as_ref()
            .and_then(|identity| Some((identity, outcomes.get(identity)?)))
        {
            self.require_route(model, &identity.route)?;
            return match result {
                Ok(()) => Ok(Some(model)),
                Err(reason) => {
                    self.record_failure(receipt, stop, reason)?;
                    Ok(None)
                }
            };
        }

        reporter.begin(Stage::Checking);
        let result = preflight::check(model, &self.scope);
        reporter.clear();
        self.require_current(receipt, stop)?;
        self.require_route(model, &key)?;
        let result = match result {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::PermissionDenied
                        | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(error);
            }
            Err(error) => Err(preflight::failure_message(&error)),
        };
        if let Some(identity) = identity {
            outcomes.remember(identity, result);
        }
        match result {
            Ok(()) => Ok(Some(model)),
            Err(reason) => {
                self.record_failure(receipt, stop, reason)?;
                Ok(None)
            }
        }
    }

    fn require_route(&self, model: &Run, key: &preflight::Key) -> io::Result<()> {
        if preflight::key(model, &self.scope)? == *key {
            Ok(())
        } else {
            Err(authority_changed())
        }
    }

    fn record_failure(&self, receipt: &Receipt, stop: &AtomicBool, reason: &str) -> io::Result<()> {
        let note = format!(
            "The AI connection test failed. {reason} The raw capture is available for re-evaluation in the journal manager."
        );
        summary_store::record_automation_probe_failure(
            &receipt.source.source(),
            &receipt.generation,
            &note,
            stop,
            || self.require_current(receipt, stop),
        )
    }

    pub(super) fn require_current(&self, receipt: &Receipt, stop: &AtomicBool) -> io::Result<()> {
        if stop.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let policy = ClipboardPolicy::load();
        if !session_journal::topic_ai_settings::allowed(&policy)
            || !policy.automation_journal_allowed()
            || policy.terminal_logging.redaction_revision() != self.redaction_revision
            || receipt.source.directory().parent() != Some(self.journal_root.as_path())
            || !self
                .current_journal_root(&policy)
                .is_some_and(|root| root == self.journal_root)
        {
            return Err(authority_changed());
        }
        let queue = Queue::load(&self.profile_root).map_err(|_| authority_changed())?;
        if !queue.pending().contains(receipt)
            || receipt.reviewed() && !queue.authorizes_reviewed(receipt)
        {
            return Err(authority_changed());
        }
        receipt
            .source
            .source()
            .require_summary_location()
            .map_err(|_| authority_changed())
    }

    fn current_journal_root(&self, policy: &ClipboardPolicy) -> Option<PathBuf> {
        let settings =
            WindowSettings::read(&self.profile_root.join("window-settings.json")).ok()?;
        if !settings.journal_ai.enabled {
            return None;
        }
        policy
            .terminal_logging
            .journal_directory()
            .map(Path::to_owned)
            .or_else(|| settings.session_journal.directory(&self.profile_root).ok())
    }
}

fn authority_changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Automation summary authority changed",
    )
}
