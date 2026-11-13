//! Typed, per-target metadata and capture lifecycle for unattended and swarm runs.

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use jiff::{Timestamp, civil::Date, tz::TimeZone};
use pgp::crypto::hash::HashAlgorithm;
use rsa::signature::digest::DynDigest;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::terminal_logging::Redactor;

use super::entry_edit::{apply_patches, lock_source, patch_scalars};
use super::markers::Compiled;
use super::tree::RunStatus;
use super::viewer::Source;
use super::{
    Capture, ConnectionConfig, LogFormat, MAX_DOCUMENT_BYTES, Metadata, RecordKind, Sink, Status,
    child, document_id, field, invalid, number, parse_document,
};

pub(crate) const MAX_DAYS: u16 = 3_650;
pub(crate) const MAX_RUNS: u16 = 1_000;
pub(crate) const MAX_STORAGE_MIB: u32 = 1_048_576;
pub(super) const OPTIONS: &str = "nativeAutomationOptions";
pub(super) const TARGET: &str = "nativeAutomationTarget";
pub(super) const COMPLETE: &str = "nativeAutomationComplete";
pub(super) const COMMAND_DIGEST: &str = "nativeAutomationCommandDigest";
pub(super) const DECISION: &str = "nativeAutomationDecision";
pub(super) const DIGEST: &str = "automationDigest";
pub(super) const DUPLICATE_OF: &str = "nativeAutomationDuplicateOf";
pub(super) const DUPLICATE_RECEIPTS: &str = "nativeAutomationDuplicateReceipts";
const ORDERED_COMMAND_DOMAIN: &[u8] = b"kortty-automation-command-sequence-v1\0";

pub(crate) mod retention;
pub(crate) mod run_links;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Keep {
    #[default]
    Always,
    Failures,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Closing {
    #[default]
    Off,
    Always,
    Failures,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Retention {
    Days { days: u16 },
    Date { date: String },
    Forever,
}

impl Default for Retention {
    fn default() -> Self {
        Self::Days { days: 14 }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Options {
    pub(crate) enabled: bool,
    pub(crate) capture_input: bool,
    pub(crate) keep: Keep,
    pub(crate) closing: Closing,
    pub(crate) retention: Retention,
    pub(crate) max_runs: u16,
    pub(crate) max_storage_mib: u32,
    pub(crate) deduplicate: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            enabled: false,
            capture_input: false,
            keep: Keep::Always,
            closing: Closing::Off,
            retention: Retention::default(),
            max_runs: 0,
            max_storage_mib: 0,
            deduplicate: true,
        }
    }
}

impl Options {
    pub(crate) fn validate(&self) -> io::Result<()> {
        let valid_retention = match &self.retention {
            Retention::Days { days } => (1..=MAX_DAYS).contains(days),
            Retention::Date { date } => {
                date.len() == 10
                    && date
                        .parse::<Date>()
                        .is_ok_and(|date| date.tomorrow().is_ok())
            }
            Retention::Forever => true,
        };
        if valid_retention && self.max_runs <= MAX_RUNS && self.max_storage_mib <= MAX_STORAGE_MIB {
            Ok(())
        } else {
            Err(invalid(
                "Automation journal retention exceeds the allowed limits",
            ))
        }
    }

    pub(crate) fn keep(&self, outcome: RunStatus) -> bool {
        self.keep == Keep::Always || outcome.is_failure()
    }

    pub(crate) fn summarize(&self, outcome: RunStatus) -> bool {
        self.closing == Closing::Always || self.closing == Closing::Failures && outcome.is_failure()
    }

    pub(crate) fn expiry(&self, ended: Timestamp, cap: Caps) -> io::Result<Option<Timestamp>> {
        self.validate()?;
        let days = |days| {
            ended
                .checked_add(Duration::from_secs(u64::from(days) * 86_400))
                .map_err(io::Error::other)
        };
        let expires = match &self.retention {
            Retention::Days { days: count } => Some(days(*count)?),
            Retention::Date { date } => {
                let date: Date = date.parse().map_err(io::Error::other)?;
                let next = date.tomorrow().map_err(io::Error::other)?;
                Some(
                    next.at(0, 0, 0, 0)
                        .to_zoned(TimeZone::system())
                        .map_err(io::Error::other)?
                        .timestamp(),
                )
            }
            Retention::Forever => None,
        };
        let limit = cap.days.map(days).transpose()?;
        Ok(match (expires, limit) {
            (Some(expires), Some(limit)) => Some(expires.min(limit)),
            (expires, limit) => expires.or(limit),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Caps {
    pub(crate) days: Option<u16>,
    pub(crate) runs: Option<u16>,
    pub(crate) storage_mib: Option<u32>,
}

impl Caps {
    pub(crate) fn intersect(self, other: Self) -> Self {
        Self {
            days: minimum(self.days, other.days),
            runs: minimum(self.runs, other.runs),
            storage_mib: minimum(self.storage_mib, other.storage_mib),
        }
    }

    pub(crate) fn maximum_runs(self, options: &Options, user_delete: bool) -> Option<usize> {
        minimum(
            self.runs,
            (user_delete && options.max_runs != 0).then_some(options.max_runs),
        )
        .map(usize::from)
    }

    pub(crate) fn maximum_bytes(self, options: &Options, user_delete: bool) -> Option<u64> {
        minimum(
            self.storage_mib,
            (user_delete && options.max_storage_mib != 0).then_some(options.max_storage_mib),
        )
        .map(|value| u64::from(value) * 1024 * 1024)
    }
}

fn minimum<T: Ord>(left: Option<T>, right: Option<T>) -> Option<T> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Kind {
    Job,
    Swarm,
}

impl Kind {
    fn xml(self) -> &'static str {
        match self {
            Self::Job => "JOB",
            Self::Swarm => "SWARM",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Run(Arc<Identity>);

#[derive(Debug)]
struct Identity {
    kind: Kind,
    source_id: String,
    source_name: String,
    action: &'static str,
    run_id: String,
    started: Timestamp,
    options: Options,
    caps: Caps,
}

impl Run {
    pub(crate) fn new(
        kind: Kind,
        source_id: String,
        source_name: String,
        action: &'static str,
        started: Timestamp,
        options: Options,
        caps: Caps,
    ) -> io::Result<Self> {
        options.validate()?;
        if !options.enabled
            || !valid_label(&source_id)
            || !valid_label(&source_name)
            || !valid_label(action)
        {
            return Err(invalid("Automation journal source is unavailable"));
        }
        Ok(Self(Arc::new(Identity {
            kind,
            source_id,
            source_name,
            action,
            run_id: super::writer::uuid()?,
            started,
            options,
            caps,
        })))
    }

    pub(crate) fn options(&self) -> &Options {
        &self.0.options
    }
}

#[derive(Clone, Debug)]
pub(super) struct TargetMetadata {
    run: Run,
    target: String,
    source_name: String,
    result: Arc<OnceLock<RunStatus>>,
    complete: Arc<OnceLock<bool>>,
    command_digest: Arc<OnceLock<String>>,
}

impl TargetMetadata {
    pub(super) fn complete(&self, complete: bool) {
        let _ = self.complete.set(complete);
    }

    pub(super) fn write(
        &self,
        ended: Option<Timestamp>,
        mut field: impl FnMut(&str, &str),
    ) -> io::Result<()> {
        let run = &self.run.0;
        field("sourceKind", run.kind.xml());
        field("sourceId", &run.source_id);
        field("sourceName", &self.source_name);
        field("automationAction", run.action);
        field("runId", &run.run_id);
        field("runStartedAt", &run.started.to_string());
        field(TARGET, &self.target);
        field(OPTIONS, &serde_json::to_string(&run.options)?);
        if let Some(ended) = ended {
            if let Some(digest) = self.command_digest.get() {
                field(COMMAND_DIGEST, digest);
            }
            if let Some(complete) = self.complete.get().copied() {
                field(COMPLETE, if complete { "true" } else { "false" });
            }
            if let Some(status) = self.result.get().copied() {
                field("runStatus", outcome_key(status));
            }
            if let Some(expiry) = run.options.expiry(ended, run.caps)? {
                field("expiresAt", &expiry.to_string());
            }
        }
        Ok(())
    }
}

pub(crate) fn outcome_key(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Success => "SUCCESS",
        RunStatus::Failed => "FAILED",
        RunStatus::Blocked => "BLOCKED",
        RunStatus::Cancelled => "CANCELLED",
    }
}

impl From<kortty_jobs::RunOutcome> for RunStatus {
    fn from(value: kortty_jobs::RunOutcome) -> Self {
        match value {
            kortty_jobs::RunOutcome::Success => Self::Success,
            kortty_jobs::RunOutcome::Failed { .. } => Self::Failed,
            kortty_jobs::RunOutcome::Blocked => Self::Blocked,
            kortty_jobs::RunOutcome::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Destination {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) host: String,
    pub(crate) port: Option<u16>,
    pub(crate) username: String,
}

pub(crate) struct Recorder {
    capture: Capture,
    metadata: TargetMetadata,
    redactor: Arc<Redactor>,
    stopped: Arc<AtomicBool>,
    commands: Mutex<CommandDigests>,
}

#[derive(Default)]
struct CommandDigests {
    ordered: Option<OrderedDigest>,
    incomplete: bool,
}

struct OrderedDigest {
    hash: Option<Box<dyn DynDigest + Send>>,
    started: bool,
}

#[derive(Clone)]
pub(crate) struct CaptureSettings {
    pub(crate) root: PathBuf,
    pub(crate) format: LogFormat,
    pub(crate) max_parts: Option<u16>,
    pub(crate) markers: Arc<Compiled>,
    pub(crate) stopped: Arc<AtomicBool>,
}

#[derive(Clone, Debug)]
pub(crate) struct Artifact {
    pub(crate) source: Source,
    pub(crate) outcome: RunStatus,
    pub(crate) complete: bool,
}

pub(crate) struct PendingArtifact {
    capture: Capture,
    done: async_channel::Receiver<()>,
    outcome: RunStatus,
    ordered_metadata: Option<TargetMetadata>,
}

impl PendingArtifact {
    pub(crate) async fn complete(self) -> Artifact {
        let _ = self.done.recv().await;
        self.artifact()
    }

    fn complete_blocking(self) -> Artifact {
        let _ = self.done.recv_blocking();
        self.artifact()
    }

    fn artifact(self) -> Artifact {
        let source = Source::capture(&self.capture);
        let complete = self.capture.status() == Status::Complete;
        if !complete && let Some(metadata) = &self.ordered_metadata {
            // The writer may have sealed a truncated journal before we knew
            // its outcome. An unsafe edit leaves it for normal recovery.
            let _ = reconcile_terminal_metadata(&source, metadata, self.outcome);
        }
        Artifact {
            source,
            outcome: self.outcome,
            complete,
        }
    }
}

fn reconcile_terminal_metadata(
    source: &Source,
    metadata: &TargetMetadata,
    outcome: RunStatus,
) -> io::Result<()> {
    let changed = || invalid("The finished automation journal identity changed");
    if !super::summary_store::is_native_source(source.directory(), source.id()) {
        return Err(changed());
    }
    let lock = lock_source(source)?;
    let (stamp, xml) = lock.read_document()?;
    let document = parse_document(&xml)?;
    let meta = child(document.root_element(), "meta").ok_or_else(changed)?;
    let run = &metadata.run.0;
    if document_id(&document)? != source.id()
        || field(meta, "sourceKind")?.as_deref() != Some(run.kind.xml())
        || field(meta, "sourceId")?.as_deref() != Some(run.source_id.as_str())
        || field(meta, "automationAction")?.as_deref() != Some(run.action)
        || field(meta, "runId")?.as_deref() != Some(run.run_id.as_str())
        || number::<Timestamp>(meta, "runStartedAt")? != Some(run.started)
        || field(meta, TARGET)?.as_deref() != Some(metadata.target.as_str())
    {
        return Err(changed());
    }
    let started = number::<Timestamp>(meta, "startedAt")?.ok_or_else(changed)?;
    let ended = number::<Timestamp>(meta, "endedAt")?
        .filter(|&ended| ended >= started)
        .ok_or_else(changed)?;
    if field(meta, "runStatus")?.is_some() {
        return Ok(());
    }
    let expiry = run
        .options
        .expiry(ended, run.caps)?
        .map(|value| value.to_string());
    let mut patches = Vec::new();
    patch_scalars(
        &xml,
        meta,
        [
            ("runStatus", Some(outcome_key(outcome))),
            (COMPLETE, Some("false")),
            ("expiresAt", expiry.as_deref()),
        ],
        &mut patches,
    )?;
    let next = apply_patches(&xml, patches)?;
    if next.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(invalid("Automation journal metadata exceeds its limit"));
    }
    parse_document(&next)?;
    lock.publish_document(&stamp, Some(&next), |opened| {
        source.require_location(source.directory(), opened)
    })?;
    lock.directory().sync()
}

impl Recorder {
    pub(crate) fn start(
        settings: &CaptureSettings,
        run: Run,
        target: Destination,
        redactor: Arc<Redactor>,
    ) -> io::Result<Self> {
        if settings.stopped.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if !valid_label(&target.key) || !valid_label(&target.label) {
            return Err(invalid("Automation journal target identity is unavailable"));
        }
        let mask = |value: &str| {
            redactor
                .journal_text(value, Instant::now() + Duration::from_millis(100))
                .map(|text| text.to_string())
                .ok_or_else(|| invalid("Automation journal metadata could not be masked"))
        };
        let config = ConnectionConfig {
            max_log_parts: settings
                .max_parts
                .unwrap_or(ConnectionConfig::default().max_log_parts),
            ai_summaries: run.options().closing != Closing::Off,
            ..ConnectionConfig::default()
        };
        let source_name = mask(&run.0.source_name)?;
        let metadata = TargetMetadata {
            run,
            target: target.key,
            source_name,
            result: Arc::new(OnceLock::new()),
            complete: Arc::new(OnceLock::new()),
            command_digest: Arc::new(OnceLock::new()),
        };
        let capture = Capture::start_configured_automation(
            settings.root.clone(),
            Metadata {
                title: metadata.source_name.clone(),
                connection_name: mask(&target.label)?,
                host: mask(&target.host)?,
                port: target.port,
                username: mask(&target.username)?,
                log_format: settings.format,
                connection_config: Some(config),
                ..Metadata::default()
            },
            Arc::clone(&settings.markers),
            metadata.clone(),
        )?;
        Ok(Self {
            capture,
            metadata,
            redactor,
            stopped: Arc::clone(&settings.stopped),
            commands: Mutex::default(),
        })
    }

    /// Track every started command in order without retaining the masked text.
    /// Enable before starting any commands.
    pub(crate) fn ordered_commands(mut self) -> Self {
        let commands = self
            .commands
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        if commands.ordered.is_none() {
            let hash = HashAlgorithm::Sha256.new_hasher().ok().map(|mut hash| {
                hash.update(ORDERED_COMMAND_DOMAIN);
                hash
            });
            commands.incomplete |= hash.is_none() || self.metadata.command_digest.get().is_some();
            commands.ordered = Some(OrderedDigest {
                hash,
                started: false,
            });
        }
        self
    }

    pub(crate) fn command(&self, command: &str) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        let mut commands = self
            .commands
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(text) = self
            .redactor
            .journal_text(command, Instant::now() + Duration::from_millis(100))
        else {
            commands.incomplete = true;
            return;
        };
        if let Some(ordered) = &mut commands.ordered {
            if let Some(hash) = ordered.hash.as_mut() {
                hash.update(&(text.len() as u64).to_le_bytes());
                hash.update(text.as_bytes());
                ordered.started = true;
            }
        } else if let Ok(hex) = identity_digest(text.as_bytes()) {
            let _ = self.metadata.command_digest.set(hex);
        } else {
            commands.incomplete = true;
        }
        if self.metadata.run.options().capture_input && text.len() <= super::MAX_RECORD_BYTES {
            let _ = self
                .capture
                .sink()
                .try_write(RecordKind::Input, text, false);
        }
    }

    pub(crate) fn output(&self, text: &str, partial: bool) {
        self.write(RecordKind::Output, text, partial);
    }

    /// Records one masked automation outcome in the capture log and timeline.
    pub(crate) fn system_note(&self, text: &str) {
        self.write(RecordKind::SystemNote, text, false);
    }

    pub(crate) fn observer(&self) -> (Sink, Arc<Redactor>, Arc<AtomicBool>) {
        (
            self.capture.sink(),
            Arc::clone(&self.redactor),
            Arc::clone(&self.stopped),
        )
    }

    pub(crate) fn finish(self, outcome: RunStatus, exit_code: Option<i32>) -> Artifact {
        self.finish_with_truncation(outcome, exit_code, false)
    }

    pub(crate) fn finish_with_truncation(
        self,
        outcome: RunStatus,
        exit_code: Option<i32>,
        truncated: bool,
    ) -> Artifact {
        self.begin_finish(outcome, exit_code, truncated)
            .complete_blocking()
    }

    pub(crate) fn finish_status(self, outcome: RunStatus, truncated: bool) -> Artifact {
        self.begin_finish_status(outcome, truncated)
            .complete_blocking()
    }

    pub(crate) fn begin_finish_status(
        self,
        outcome: RunStatus,
        truncated: bool,
    ) -> PendingArtifact {
        self.begin_finish_inner(outcome, None, truncated)
    }

    /// Finalize metadata and signal the writer. Await the returned receipt to
    /// obtain the artifact after queued records have reached disk.
    pub(crate) fn begin_finish(
        self,
        outcome: RunStatus,
        exit_code: Option<i32>,
        truncated: bool,
    ) -> PendingArtifact {
        let ended = exit_code.map_or_else(
            || "shell integration: command finished".to_owned(),
            |code| format!("shell integration: command finished (exit {code})"),
        );
        self.begin_finish_inner(outcome, Some(&ended), truncated)
    }

    fn begin_finish_inner(
        mut self,
        outcome: RunStatus,
        command_end: Option<&str>,
        truncated: bool,
    ) -> PendingArtifact {
        let commands = self
            .commands
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        let ordered = commands.ordered.is_some();
        if let Some(OrderedDigest {
            hash: Some(hash),
            started: true,
        }) = commands.ordered.take()
        {
            commands.incomplete |= self
                .metadata
                .command_digest
                .set(digest_hex(&hash.finalize()))
                .is_err();
        }
        let incomplete = commands.incomplete;
        let _ = self.metadata.result.set(outcome);
        if truncated || incomplete || self.stopped.load(Ordering::Acquire) {
            self.capture.sink().mark_truncated();
        } else if let Some(ended) = command_end {
            self.write(RecordKind::CommandEnd, ended, false);
        }
        let done = self.capture.finish();
        PendingArtifact {
            capture: self.capture,
            done,
            outcome,
            ordered_metadata: ordered.then_some(self.metadata),
        }
    }

    fn write(&self, kind: RecordKind, text: &str, partial: bool) {
        if self.stopped.load(Ordering::Acquire) || text.trim().is_empty() {
            return;
        }
        let sink = self.capture.sink();
        // One outcome belongs to one timeline entry, within the record limit.
        let system_note = kind == RecordKind::SystemNote;
        if system_note && text.len() > super::MAX_RECORD_BYTES {
            sink.mark_truncated();
            return;
        }
        let deadline = Instant::now() + Duration::from_millis(100);
        let masked = if kind == RecordKind::Output {
            self.redactor
                .outbound_text(text, deadline)
                .map(|(text, _)| text)
        } else {
            self.redactor.journal_text(text, deadline)
        };
        let Some(text) = masked else {
            sink.mark_truncated();
            return;
        };
        if system_note || text.len() <= super::MAX_RECORD_BYTES {
            let _ = sink.try_write(kind, text, partial);
        } else {
            let mut rest = text.as_str();
            while !rest.is_empty() {
                let size = rest.floor_char_boundary(rest.len().min(super::MAX_RECORD_BYTES));
                let (chunk, remaining) = rest.split_at(size);
                if sink
                    .try_write(
                        kind,
                        Zeroizing::new(chunk.to_owned()),
                        partial || !remaining.is_empty(),
                    )
                    .is_err()
                {
                    break;
                }
                rest = remaining;
            }
        }
    }
}

pub(crate) fn identity_digest(bytes: &[u8]) -> io::Result<String> {
    let bytes = HashAlgorithm::Sha256
        .digest(bytes)
        .map_err(io::Error::other)?;
    Ok(digest_hex(&bytes))
}

fn digest_hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn valid_label(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= 1_024 && !text.chars().any(char::is_control)
}

pub(super) fn is_native_path(root: &Path, source: &Source) -> bool {
    source.directory().parent() == Some(root)
        && super::summary_store::is_native_source(source.directory(), source.id())
}
