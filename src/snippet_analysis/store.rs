//! Private, revisioned histories with one atomic file per analysis subject.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::diagram::AnalysisDiagram;
use super::model::{
    ApplyRecovery, ApplyRun, ApplyStatus, ExportEntry, ExportPhase, History, MAX_CONTENT_BYTES,
    Purpose, Record, Subject, Verification, content_hash, now_millis, source_fits,
};
use crate::clipboard_policy::ClipboardPolicy;
use crate::private_files::{Directory, FileStamp, require_single_link};
use crate::session_journal::export_output::valid_component;
use crate::snippets::SnippetId;

const DIRECTORY_NAME: &str = "snippet-analyses";
const LOCK_NAME: &str = ".history.lock";
const SCHEMA_VERSION: u32 = 1;
const RETENTION: usize = 5;
const MAX_RECORDS: usize = 128;
const MAX_APPLY_RUNS: usize = 8;
const MAX_APPLY_RECOVERIES: usize = 4;
const MAX_EXPORT_ENTRIES: usize = 32;
const MAX_FILE_BYTES: usize = 12 * MAX_CONTENT_BYTES;
const CONTENT_BUDGET: usize = MAX_FILE_BYTES / 2;
// Keep some space for finding and provenance metadata even with heavy JSON escaping.
const ENCODED_CONTENT_BUDGET: usize = MAX_FILE_BYTES - 8 * 1024 * 1024;

mod modularization;
pub(crate) use modularization::ReviewedSplitRevision;

#[cfg(test)]
type RetentionOverride = (PathBuf, std::sync::Weak<std::sync::atomic::AtomicUsize>);

#[cfg(test)]
thread_local! {
    static TEST_RETENTION_LIMIT: std::cell::RefCell<Option<RetentionOverride>> = const { std::cell::RefCell::new(None) };
}

#[derive(Clone)]
pub(crate) struct Store {
    profile: PathBuf,
    #[cfg(test)]
    retention_limit: Option<Arc<std::sync::atomic::AtomicUsize>>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Document {
    schema_version: u32,
    history: History,
    blobs: BTreeMap<String, Arc<str>>,
}

impl Store {
    pub(crate) fn new(profile: PathBuf) -> Self {
        Self {
            #[cfg(test)]
            retention_limit: TEST_RETENTION_LIMIT.with(|override_limit| {
                override_limit
                    .borrow()
                    .as_ref()
                    .filter(|(path, _)| path == &profile)
                    .and_then(|(_, limit)| limit.upgrade())
            }),
            profile,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_retention_limit(
        profile: PathBuf,
        limit: usize,
    ) -> Arc<std::sync::atomic::AtomicUsize> {
        let limit = Arc::new(std::sync::atomic::AtomicUsize::new(limit));
        TEST_RETENTION_LIMIT.with(|override_limit| {
            override_limit.replace(Some((profile, Arc::downgrade(&limit))));
        });
        limit
    }

    /// Reads an immutable snapshot. Invalid or unsupported files remain untouched.
    pub(crate) fn load(&self, subject: Subject) -> io::Result<History> {
        let directory = match self.directory(false) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(History::empty(subject));
            }
            Err(error) => return Err(error),
        };
        read(&directory, &file_name(subject)?, subject).map(|(history, _)| history)
    }

    #[cfg(test)]
    pub(crate) fn append(&self, subject: Subject, record: Record) -> io::Result<History> {
        self.append_while(subject, record, || true)
    }

    /// Checks whether the run may still save while holding the same lock as `clear`.
    pub(crate) fn append_while(
        &self,
        subject: Subject,
        record: Record,
        still_current: impl Fn() -> bool,
    ) -> io::Result<History> {
        self.update_while(subject, still_current, |history| {
            if !record.apply_runs.is_empty()
                || record.apply_recovery.is_some()
                || record.modularization.is_some()
                || record.verification.is_some()
                || !record.exports.is_empty()
            {
                return Err(invalid(
                    "Append apply, verification and export data through their own workflows",
                ));
            }
            insert_record(history, record)
        })
    }

    #[cfg(test)]
    pub(crate) fn append_apply_while(
        &self,
        subject: Subject,
        pre_id: SnippetId,
        expected_pre_sha256: &str,
        run: ApplyRun,
        still_current: impl Fn() -> bool,
    ) -> io::Result<History> {
        self.append_apply(
            subject,
            pre_id,
            expected_pre_sha256,
            None,
            run,
            still_current,
        )
    }

    pub(crate) fn append_apply_recovery_while(
        &self,
        subject: Subject,
        pre_id: SnippetId,
        expected_pre_sha256: &str,
        expected_revision: SnippetId,
        run: ApplyRun,
        still_current: impl Fn() -> bool,
    ) -> io::Result<History> {
        self.append_apply(
            subject,
            pre_id,
            expected_pre_sha256,
            Some(expected_revision),
            run,
            still_current,
        )
    }

    fn append_apply(
        &self,
        subject: Subject,
        pre_id: SnippetId,
        expected_pre_sha256: &str,
        expected_revision: Option<SnippetId>,
        mut run: ApplyRun,
        still_current: impl Fn() -> bool,
    ) -> io::Result<History> {
        require_snippet(subject)?;
        if !run.has_review_source() {
            return Err(invalid(
                "The apply proposal requires its complete reviewed source",
            ));
        }
        self.update_while(subject, still_current, |history| {
            if history.records.iter().any(|record| {
                record.apply_recovery.as_ref().is_some_and(|recovery| {
                    recovery.revision == run.id
                        && (record.id != pre_id || expected_revision != Some(run.id))
                })
            }) {
                return Err(invalid("The apply run identity already exists"));
            }
            if history
                .records
                .iter()
                .filter(|record| record.id != pre_id)
                .any(|record| {
                    record
                        .apply_runs
                        .iter()
                        .any(|previous| previous.id == run.id)
                })
            {
                return Err(invalid("The apply run identity already exists"));
            }
            let pre = record_mut(history, pre_id)?;
            if pre.source.sha256 != expected_pre_sha256 || run.status != ApplyStatus::Proposed {
                return Err(invalid(
                    "The proposal no longer matches the reviewed analysis",
                ));
            }
            if run.provenance == Default::default() {
                run.provenance = pre.provenance.clone();
            }
            if run.provenance.profile_id != pre.provenance.profile_id {
                return Err(invalid("The proposal must use the analysis profile"));
            }
            run.validate(&pre.analysis)?;
            if let Some(previous) = pre.apply_runs.iter().find(|previous| previous.id == run.id) {
                return if previous.matches_live(&run) {
                    Ok(false)
                } else {
                    Err(invalid("The apply run identity already exists"))
                };
            }
            if pre.apply_runs.len() >= MAX_APPLY_RUNS {
                return Err(invalid("This analysis has reached its apply run limit"));
            }
            if let Some(revision) = expected_revision {
                if !pre.apply_recovery.as_ref().is_some_and(|recovery| {
                    recovery.revision == revision && recovery.matches_proposal(&run)
                }) {
                    return Err(recovery_changed());
                }
                pre.apply_recovery = None;
            }
            pre.apply_runs.insert(0, run);
            pre.updated_at = now_millis().max(pre.updated_at);
            Ok(true)
        })
    }

    /// Claims or advances an owned live apply. An omitted checkpoint remains a
    /// progress receipt, while the exact source stays in the calling worker.
    pub(crate) fn save_apply_recovery_while(
        &self,
        subject: Subject,
        pre_id: SnippetId,
        expected_pre_sha256: &str,
        expected_revision: Option<SnippetId>,
        recovery: ApplyRecovery,
        still_current: impl Fn() -> bool,
    ) -> io::Result<History> {
        require_snippet(subject)?;
        if !recovery.request.selection.has_required_source()
            || recovery
                .checkpoint
                .as_ref()
                .is_some_and(|point| !point.has_source())
        {
            return Err(retention_changed());
        }
        let result = self.update_while(subject, &still_current, |history| {
            let recoveries = history
                .records
                .iter()
                .filter(|record| record.apply_recovery.is_some())
                .count();
            if history.records.iter().any(|record| {
                (record.id != pre_id
                    && record
                        .apply_recovery
                        .as_ref()
                        .is_some_and(|other| other.revision == recovery.revision))
                    || record
                        .apply_runs
                        .iter()
                        .any(|run| run.id == recovery.revision)
            }) {
                return Err(invalid("The apply recovery identity already exists"));
            }
            let pre = record_mut(history, pre_id)?;
            if pre.source.sha256 != expected_pre_sha256 {
                return Err(recovery_changed());
            }
            let previous = pre.apply_recovery.as_ref();
            if previous.is_some_and(|saved| saved.matches_live(&recovery)) {
                return Ok(false);
            }
            if expected_revision == Some(recovery.revision)
                || previous.map(|previous| previous.revision) != expected_revision
                || previous.is_some_and(|previous| !recovery.continues(previous))
            {
                return Err(recovery_changed());
            }
            if previous.is_none() && recoveries >= MAX_APPLY_RECOVERIES {
                return Err(invalid("This snippet has reached its apply recovery limit"));
            }
            if pre.apply_runs.len() >= MAX_APPLY_RUNS {
                return Err(invalid("This analysis has reached its apply run limit"));
            }
            recovery.validate(&pre.analysis)?;
            pre.apply_recovery = Some(recovery.clone());
            pre.updated_at = now_millis().max(pre.updated_at);
            Ok(true)
        });
        // A directory sync can fail after the replacement is already visible.
        // Require a successful sync before the worker can send the next stage.
        result.or_else(|error| {
            if still_current()
                && let Ok(history) = self.load(subject)
                && history.records.iter().any(|record| {
                    record.id == pre_id
                        && record
                            .apply_recovery
                            .as_ref()
                            .is_some_and(|saved| saved.matches_live(&recovery))
                })
            {
                self.directory(false)?.sync()?;
                Ok(history)
            } else {
                Err(error)
            }
        })
    }

    pub(crate) fn remove_apply_recovery(
        &self,
        subject: Subject,
        pre_id: SnippetId,
        expected_revision: SnippetId,
    ) -> io::Result<History> {
        require_snippet(subject)?;
        self.update(subject, |history| {
            let pre = record_mut(history, pre_id)?;
            if !pre
                .apply_recovery
                .as_ref()
                .is_some_and(|recovery| recovery.revision == expected_revision)
            {
                return Err(recovery_changed());
            }
            pre.apply_recovery = None;
            pre.updated_at = now_millis().max(pre.updated_at);
            Ok(true)
        })
    }

    #[cfg(test)]
    pub(crate) fn decide_apply_while(
        &self,
        subject: Subject,
        pre_id: SnippetId,
        run_id: SnippetId,
        expected_after_sha256: &str,
        status: ApplyStatus,
        still_current: impl Fn() -> bool,
    ) -> io::Result<History> {
        self.decide_apply(
            subject,
            pre_id,
            (run_id, expected_after_sha256),
            None,
            status,
            still_current,
        )
    }

    pub(crate) fn decide_reviewed_apply_while(
        &self,
        subject: Subject,
        pre_id: SnippetId,
        reviewed: &ApplyRun,
        status: ApplyStatus,
        still_current: impl Fn() -> bool,
    ) -> io::Result<History> {
        self.decide_apply(
            subject,
            pre_id,
            (reviewed.id, &reviewed.after_sha256),
            Some(reviewed),
            status,
            still_current,
        )
    }

    fn decide_apply(
        &self,
        subject: Subject,
        pre_id: SnippetId,
        (run_id, expected_after_sha256): (SnippetId, &str),
        reviewed: Option<&ApplyRun>,
        status: ApplyStatus,
        still_current: impl Fn() -> bool,
    ) -> io::Result<History> {
        require_snippet(subject)?;
        if status == ApplyStatus::Proposed {
            return Err(invalid("Choose an apply review decision"));
        }
        self.update_with_lock(subject, still_current, lock_history, |history, limit| {
            let pre = record_mut(history, pre_id)?;
            let run = pre
                .apply_runs
                .iter_mut()
                .find(|run| run.id == run_id)
                .ok_or_else(|| invalid("The apply proposal no longer exists"))?;
            if run.after_sha256 != expected_after_sha256 {
                return Err(invalid("The apply proposal changed during review"));
            }
            if status == ApplyStatus::Accepted {
                if let Some(reviewed) = reviewed {
                    reviewed.validate(&pre.analysis)?;
                    let matches = if run.status == status {
                        let mut reviewed = reviewed.clone();
                        reviewed.status = run.status;
                        reviewed.decided_at = run.decided_at;
                        run.matches_live(&reviewed)
                    } else {
                        run.matches_live(reviewed)
                    };
                    if !matches {
                        return Err(invalid(
                            "The live apply review no longer matches the saved proposal",
                        ));
                    }
                } else if !run.has_review_source() || !run.fits_retention(limit) {
                    return Err(retention_changed());
                }
            }
            if run.status == status {
                return Ok(false);
            }
            if run.status != ApplyStatus::Proposed {
                return Err(invalid(
                    "The apply proposal already has a different decision",
                ));
            }
            let decided = now_millis().max(run.started_at);
            run.decided_at = Some(decided);
            run.status = status;
            pre.updated_at = decided.max(pre.updated_at);
            Ok(true)
        })
    }

    pub(crate) fn append_verification_while(
        &self,
        subject: Subject,
        pre_id: SnippetId,
        run_id: SnippetId,
        mut record: Record,
        still_current: impl Fn() -> bool,
    ) -> io::Result<History> {
        require_snippet(subject)?;
        self.update_while(subject, still_current, |history| {
            if record.purpose != Purpose::Verify
                || !record.apply_runs.is_empty()
                || record.apply_recovery.is_some()
                || record.modularization.is_some()
                || record.verification.is_some()
            {
                return Err(invalid("The follow-up must be a fresh verification"));
            }
            let pre = history
                .records
                .iter()
                .find(|record| record.id == pre_id)
                .ok_or_else(|| invalid("The analysis record no longer exists"))?;
            record.previous_record_id = Some(pre_id);
            record.verification = Some(Verification::compare(pre, run_id, &record)?);
            insert_record(history, record)
        })
    }

    pub(crate) fn set_pinned(
        &self,
        subject: Subject,
        id: SnippetId,
        pinned: bool,
    ) -> io::Result<History> {
        self.update(subject, |history| {
            let record = history
                .records
                .iter_mut()
                .find(|record| record.id == id)
                .ok_or_else(|| invalid("The analysis record no longer exists"))?;
            if record.pinned == pinned {
                return Ok(false);
            }
            record.pinned = pinned;
            record.updated_at = now_millis().max(record.updated_at);
            Ok(true)
        })
    }

    pub(crate) fn set_diagram_while(
        &self,
        subject: Subject,
        id: SnippetId,
        source_hash: &str,
        expected: Option<&AnalysisDiagram>,
        diagram: AnalysisDiagram,
        still_current: impl FnOnce() -> bool,
    ) -> io::Result<History> {
        self.update(subject, |history| {
            if !still_current() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "The flowchart request is no longer current",
                ));
            }
            let record = history
                .records
                .iter_mut()
                .find(|record| record.id == id)
                .ok_or_else(|| invalid("The analysis record no longer exists"))?;
            if record.source.sha256 != source_hash {
                return Err(invalid(
                    "The analysis source changed during flowchart generation",
                ));
            }
            if record.diagram.as_ref() != expected {
                return Err(invalid("The saved flowchart changed during generation"));
            }
            diagram
                .validate(record.source.line_count)
                .map_err(io::Error::other)?;
            record.diagram = Some(diagram);
            record.updated_at = now_millis().max(record.updated_at);
            Ok(true)
        })
    }

    pub(crate) fn record_export(
        &self,
        subject: Subject,
        id: SnippetId,
        source_hash: &str,
        export: ExportEntry,
    ) -> io::Result<()> {
        require_snippet(subject)?;
        self.update_with_lock(
            subject,
            || true,
            try_lock_history,
            |history, _| {
                let record = record_mut(history, id)?;
                if record.source.sha256 != source_hash {
                    return Err(invalid("The exported analysis source changed"));
                }
                if !valid_export(&export, record) {
                    return Err(invalid(
                        "The export receipt does not match the saved analysis",
                    ));
                }
                if record.exports.len() == MAX_EXPORT_ENTRIES {
                    record.exports.remove(0);
                }
                record.exports.push(export);
                Ok(true)
            },
        )
        .map(|_| ())
    }

    pub(crate) fn remove(&self, subject: Subject, id: SnippetId) -> io::Result<History> {
        self.update(subject, |history| {
            let count = history.records.len();
            history.records.retain(|record| record.id != id);
            Ok(history.records.len() != count)
        })
    }

    /// Removes the private history file, including every source blob owned by the subject.
    pub(crate) fn clear(&self, subject: Subject) -> io::Result<()> {
        let directory = match self.directory(false) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let name = file_name(subject)?;
        // A writer cannot have passed its guard unless the persistent lock exists.
        if !directory.has_entry(&name)? && !directory.has_entry(LOCK_NAME)? {
            return Ok(());
        }
        let _lock = lock_history(&directory)?;
        let file = match directory.open_private_file(&name) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        require_single_link(&file)?;
        let stamp = FileStamp::read(&file)?;
        drop(file);
        directory.remove_unchanged(&name, &stamp)?;
        directory.sync()
    }

    fn directory(&self, create: bool) -> io::Result<Directory> {
        Directory::open(&self.profile, create)?.private_child(DIRECTORY_NAME, create)
    }

    fn retained_content_limit(&self) -> usize {
        #[cfg(test)]
        if let Some(limit) = &self.retention_limit {
            return limit.load(std::sync::atomic::Ordering::Acquire);
        }
        ClipboardPolicy::load()
            .snippets
            .analysis_max_stored_content_bytes
            .unwrap_or(MAX_CONTENT_BYTES)
            .min(MAX_CONTENT_BYTES)
    }

    fn update(
        &self,
        subject: Subject,
        change: impl FnOnce(&mut History) -> io::Result<bool>,
    ) -> io::Result<History> {
        self.update_while(subject, || true, change)
    }

    fn update_while(
        &self,
        subject: Subject,
        still_current: impl Fn() -> bool,
        change: impl FnOnce(&mut History) -> io::Result<bool>,
    ) -> io::Result<History> {
        self.update_with_lock(subject, still_current, lock_history, |history, _| {
            change(history)
        })
    }

    fn update_with_lock(
        &self,
        subject: Subject,
        still_current: impl Fn() -> bool,
        lock: fn(&Directory) -> io::Result<File>,
        change: impl FnOnce(&mut History, usize) -> io::Result<bool>,
    ) -> io::Result<History> {
        let name = file_name(subject)?;
        let directory = self.directory(true)?;
        let _lock = lock(&directory)?;
        require_current(&still_current)?;
        let (mut history, before) = read(&directory, &name, subject)?;
        let retention_limit = self.retained_content_limit();
        if !change(&mut history, retention_limit)? {
            return Ok(history);
        }
        history.revision = history
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("The analysis history revision is exhausted"))?;
        history.updated_at = now_millis().max(history.updated_at);
        let document = Document::capture(history, retention_limit)?;
        let write = |file: &mut File| {
            let mut output = BoundedWriter {
                output: io::BufWriter::new(file),
                remaining: MAX_FILE_BYTES,
            };
            serde_json::to_writer(&mut output, &document).map_err(io::Error::from)?;
            output.flush()
        };
        let publishable = || {
            require_current(&still_current)?;
            if !document.fits_retention(self.retained_content_limit()) {
                return Err(retention_changed());
            }
            Ok(())
        };
        if let Some(before) = before {
            directory.replace_with(&name, write, || {
                let current = directory.open_private_file(&name)?;
                require_single_link(&current)?;
                before.require_unchanged(&current)?;
                publishable()
            })?;
        } else {
            directory.publish_checked(&name, write, publishable)?;
        }
        directory.sync()?;
        Ok(document.history)
    }
}

fn require_snippet(subject: Subject) -> io::Result<()> {
    if matches!(subject, Subject::Snippet(_)) {
        Ok(())
    } else {
        Err(invalid("This review workflow requires a single snippet"))
    }
}

fn retention_changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Managed policy prevents retaining the required analysis source text.",
    )
}

fn require_current(still_current: impl FnOnce() -> bool) -> io::Result<()> {
    if still_current() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "The analysis operation is no longer current",
        ))
    }
}

fn recovery_changed() -> io::Error {
    invalid("The saved apply recovery changed. Reopen the analysis to continue.")
}

fn record_mut(history: &mut History, id: SnippetId) -> io::Result<&mut Record> {
    history
        .records
        .iter_mut()
        .find(|record| record.id == id)
        .ok_or_else(|| invalid("The analysis record no longer exists"))
}

fn valid_export(export: &ExportEntry, record: &Record) -> bool {
    export.exported_at > 0
        && valid_component(&export.file_name)
        && match (export.phase, export.run_id) {
            (ExportPhase::BeforeApply, None) => true,
            (ExportPhase::AfterApply, Some(id)) => record.apply_runs.iter().any(|run| run.id == id),
            _ => false,
        }
}

fn insert_record(history: &mut History, record: Record) -> io::Result<bool> {
    if let Some(existing) = history
        .records
        .iter()
        .find(|existing| existing.id == record.id)
    {
        if existing.source.sha256 != record.source.sha256
            || existing.verification != record.verification
        {
            return Err(invalid("The analysis record identity already exists"));
        }
        return Ok(false);
    }
    validate_source(&record)?;
    let protected = (record.purpose == Purpose::Verify)
        .then_some(record.previous_record_id)
        .flatten();
    history.records.insert(0, record);
    let accepted: HashSet<_> = history
        .records
        .iter()
        .flat_map(|record| &record.apply_runs)
        .filter(|run| run.status == ApplyStatus::Accepted)
        .map(|run| run.id)
        .collect();
    let mut verified = HashSet::new();
    let evidence: HashSet<_> = history
        .records
        .iter()
        .filter(|record| {
            record.verification.as_ref().is_some_and(|verification| {
                verification.matches_applied_revision
                    && accepted.contains(&verification.run_id)
                    && verified.insert(verification.run_id)
            })
        })
        .map(|record| record.id)
        .collect();
    let mut index = history.records.len();
    while history.records.len() > RETENTION && index > 1 {
        index -= 1;
        let old = &history.records[index];
        if !old.pinned
            && Some(old.id) != protected
            && !evidence.contains(&old.id)
            && old.apply_recovery.is_none()
            && !modularization::retained(old)
            && old
                .apply_runs
                .iter()
                .all(|run| run.status == ApplyStatus::Rejected)
        {
            history.records.remove(index);
        }
    }
    if history.records.len() > MAX_RECORDS {
        return Err(invalid("The analysis history is full of protected records"));
    }
    Ok(true)
}

fn lock_history(directory: &Directory) -> io::Result<File> {
    let lock = open_history_lock(directory)?;
    lock.lock()?;
    Ok(lock)
}

fn try_lock_history(directory: &Directory) -> io::Result<File> {
    let lock = open_history_lock(directory)?;
    lock.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => {
            io::Error::new(io::ErrorKind::WouldBlock, "The analysis history is busy")
        }
        std::fs::TryLockError::Error(error) => error,
    })?;
    Ok(lock)
}

fn open_history_lock(directory: &Directory) -> io::Result<File> {
    let lock = directory.open_lock(LOCK_NAME)?;
    require_single_link(&lock)?;
    Ok(lock)
}

fn read(
    directory: &Directory,
    name: &str,
    subject: Subject,
) -> io::Result<(History, Option<FileStamp>)> {
    let file = match directory.open_private_file(name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((History::empty(subject), None));
        }
        Err(error) => return Err(error),
    };
    require_single_link(&file)?;
    let before = FileStamp::read(&file)?;
    if file.metadata()?.len() > MAX_FILE_BYTES as u64 {
        return Err(invalid("The analysis history exceeds its storage limit"));
    }
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    before.require_unchanged(&file)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(invalid("The analysis history exceeds its storage limit"));
    }
    let document: Document = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("The analysis history is corrupt or uses an unsupported schema"))?;
    let history = document.decode(subject)?;
    Ok((history, Some(before)))
}

impl Document {
    fn capture(mut history: History, retention_limit: usize) -> io::Result<Self> {
        for record in &mut history.records {
            for run in &mut record.apply_runs {
                run.apply_retention(retention_limit)?;
            }
            if let Some(recovery) = &mut record.apply_recovery {
                recovery.apply_retention(retention_limit)?;
            }
        }
        let mut raw_remaining = CONTENT_BUDGET;
        let mut encoded_remaining = ENCODED_CONTENT_BUDGET;
        let mut blobs = BTreeMap::new();
        // Recovery checkpoints must survive compaction. Their total size is bounded
        // separately, so reserve their space before retaining ordinary report sources.
        for checkpoint in history
            .records
            .iter()
            .filter_map(|record| record.apply_recovery.as_ref()?.checkpoint.as_deref())
            .filter(|checkpoint| checkpoint.has_source())
        {
            if !blobs.contains_key(&checkpoint.code_sha256) {
                raw_remaining -= checkpoint.code.len();
                encoded_remaining -= encoded_cost(&checkpoint.code);
                blobs.insert(checkpoint.code_sha256.clone(), Arc::clone(&checkpoint.code));
            }
        }
        for record in &mut history.records {
            let source = &mut record.source;
            let Some(content) = &source.content else {
                continue;
            };
            if !source_fits(content, retention_limit) {
                source.content = None;
                source.content_truncated = true;
                continue;
            }
            if let Some(stored) = blobs.get(&source.sha256) {
                source.content = Some(Arc::clone(stored));
                continue;
            }
            let raw = content.len();
            let encoded = encoded_cost(content);
            if raw > raw_remaining || encoded > encoded_remaining {
                source.content = None;
                source.content_truncated = true;
            } else {
                raw_remaining -= raw;
                encoded_remaining -= encoded;
                blobs.insert(source.sha256.clone(), Arc::clone(content));
            }
        }
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            history,
            blobs,
        })
    }

    fn fits_retention(&self, limit: usize) -> bool {
        self.blobs
            .values()
            .all(|content| source_fits(content, limit))
            && self.history.records.iter().all(|record| {
                record
                    .apply_runs
                    .iter()
                    .all(|run| run.fits_retention(limit))
                    && record
                        .apply_recovery
                        .as_ref()
                        .is_none_or(|recovery| recovery.fits_retention(limit))
            })
    }

    fn decode(mut self, subject: Subject) -> io::Result<History> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(invalid("The analysis history uses an unsupported schema"));
        }
        if self.history.subject != subject || self.history.records.len() > MAX_RECORDS {
            return Err(invalid(
                "The analysis history has an invalid subject or record count",
            ));
        }
        let recovery_count = self
            .history
            .records
            .iter()
            .filter(|record| record.apply_recovery.is_some())
            .count();
        if recovery_count > MAX_APPLY_RECOVERIES
            || self.blobs.len() > self.history.records.len() + recovery_count
        {
            return Err(invalid("The analysis history has too many source blobs"));
        }
        let mut total = 0;
        for (hash, blob) in &self.blobs {
            total += blob.len();
            if total > CONTENT_BUDGET
                || blob.len() > MAX_CONTENT_BYTES
                || content_hash(blob)? != *hash
            {
                return Err(invalid("The analysis history has an invalid source blob"));
            }
        }
        let mut ids = HashSet::new();
        let mut run_ids = HashSet::new();
        let active_modularizations = self
            .history
            .records
            .iter()
            .filter(|record| modularization::retained(record))
            .count();
        if active_modularizations > modularization::MAX_ACTIVE_JOBS {
            return Err(invalid(
                "The analysis history has too many active modularizations",
            ));
        }
        let mut used = HashSet::new();
        for record in &mut self.history.records {
            if !ids.insert(record.id) || !valid_hash(&record.source.sha256) {
                return Err(invalid(
                    "The analysis history has an invalid record identity",
                ));
            }
            if !record.source.content_truncated {
                let content = self
                    .blobs
                    .get(&record.source.sha256)
                    .ok_or_else(|| invalid("The analysis history is missing a source blob"))?;
                record.source.content = Some(Arc::clone(content));
                used.insert(record.source.sha256.clone());
            }
            if record.apply_runs.len() > MAX_APPLY_RUNS {
                return Err(invalid("The analysis history has too many apply runs"));
            }
            if !record.apply_runs.is_empty()
                || record.apply_recovery.is_some()
                || record.verification.is_some()
                || !record.exports.is_empty()
            {
                require_snippet(subject)?;
            }
            if let Some(recovery) = &mut record.apply_recovery {
                if !run_ids.insert(recovery.revision) {
                    return Err(invalid(
                        "The analysis history has an invalid apply recovery identity",
                    ));
                }
                if let Some(checkpoint) = &mut recovery.checkpoint
                    && checkpoint.has_source()
                {
                    let checkpoint = Arc::make_mut(checkpoint);
                    checkpoint.code = self
                        .blobs
                        .get(&checkpoint.code_sha256)
                        .cloned()
                        .ok_or_else(|| {
                            invalid("The analysis history is missing an apply checkpoint blob")
                        })?;
                    used.insert(checkpoint.code_sha256.clone());
                }
                recovery.validate(&record.analysis)?;
            }
            if let Some(job) = &record.modularization {
                modularization::validate_job(job, record, subject)?;
            }
            for run in &record.apply_runs {
                if !run_ids.insert(run.id)
                    || run.provenance.profile_id != record.provenance.profile_id
                {
                    return Err(invalid(
                        "The analysis history has an invalid apply run identity",
                    ));
                }
                run.validate(&record.analysis)?;
            }
            if record.exports.len() > MAX_EXPORT_ENTRIES
                || record
                    .exports
                    .iter()
                    .any(|export| !valid_export(export, record))
            {
                return Err(invalid(
                    "The analysis history has an invalid export receipt",
                ));
            }
        }
        if used.len() != self.blobs.len() {
            return Err(invalid(
                "The analysis history has an unreferenced source blob",
            ));
        }
        for record in &self.history.records {
            if let Some(verification) = &record.verification {
                let pre = self
                    .history
                    .records
                    .iter()
                    .find(|pre| pre.id == verification.previous_record_id);
                verification.validate(record, pre)?;
            }
        }
        Ok(self.history)
    }
}

fn validate_source(record: &Record) -> io::Result<()> {
    let source = &record.source;
    if !valid_hash(&source.sha256) || source.content_truncated == source.content.is_some() {
        return Err(invalid(
            "The analysis source hash or retention state is invalid",
        ));
    }
    if let Some(content) = &source.content
        && (content.len() > MAX_CONTENT_BYTES || content_hash(content)? != source.sha256)
    {
        return Err(invalid("The analysis source does not match its hash"));
    }
    Ok(())
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(is_lower_hex)
}

fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

fn file_name(subject: Subject) -> io::Result<String> {
    match subject {
        Subject::Snippet(id) => Ok(format!("{}.json", String::from(id))),
        Subject::Folder(id) => {
            let encoded = serde_json::to_string(&id).map_err(io::Error::from)?;
            let id = encoded
                .strip_prefix('"')
                .and_then(|text| text.strip_suffix('"'))
                .filter(|text| text.len() == 32 && text.bytes().all(is_lower_hex))
                .ok_or_else(|| invalid("The analysis folder identity is invalid"))?;
            Ok(format!("folder-{id}.json"))
        }
    }
}

fn encoded_cost(content: &str) -> usize {
    content
        .bytes()
        .map(|byte| match byte {
            b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 0x08 | 0x0c => 2,
            0..=0x1f => 6,
            _ => 1,
        })
        .sum()
}

struct BoundedWriter<W> {
    output: W,
    remaining: usize,
}

impl<W: Write> Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(invalid("The analysis history exceeds its storage limit"));
        }
        let written = self.output.write(bytes)?;
        self.remaining -= written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
