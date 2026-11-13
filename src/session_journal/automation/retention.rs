//! Recover, classify, deduplicate, and expire native automation journals by exact identity.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::Timestamp;
use pgp::crypto::hash::HashAlgorithm;
use roxmltree::Node;

use crate::private_files::Directory;
use crate::session_journal::capture::retrieval;
use crate::session_journal::deletion::Deletion;
use crate::session_journal::entry_edit::{apply_patches, lock_source, patch_scalars};
use crate::session_journal::summary_store::{self, Progress};
use crate::session_journal::tree::{RunStatus, SourceKind};
use crate::session_journal::{
    CaptureKind, MAX_DOCUMENT_BYTES, Summary, boolean, catalog, child, document_id, field, invalid,
    number, parse_document, viewer::Source,
};

use super::{
    Artifact, COMMAND_DIGEST, COMPLETE, Caps, DECISION, DIGEST, DUPLICATE_OF, DUPLICATE_RECEIPTS,
    OPTIONS, Options, TARGET, is_native_path, outcome_key,
};

const MAX_HASH_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FILES: usize = 2_048;
const MAX_RECEIPTS: usize = 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Decision {
    Pending,
    Keep,
    Discard,
    Duplicate,
}

#[derive(Debug)]
struct Stored {
    source: Source,
    row: Summary,
    options: Options,
    target: String,
    complete: bool,
    command_digest: Option<String>,
    decision: Decision,
    digest: Option<String>,
    duplicate_of: Option<String>,
    receipts: Vec<String>,
}

pub(crate) struct Sweep<'a> {
    pub(crate) root: &'a Path,
    pub(crate) caps: Caps,
    pub(crate) user_delete: bool,
    pub(crate) summarize: bool,
    pub(crate) cancelled_closing: &'a [String],
    pub(crate) protected: &'a [Source],
    pub(crate) completed: &'a [Artifact],
    /// A startup pass can finalize captures whose writer lock was released by a crashed process.
    pub(crate) recover: bool,
    pub(crate) now: Timestamp,
    pub(crate) cancelled: &'a AtomicBool,
}

#[derive(Default, Debug)]
pub(crate) struct ResultSet {
    pub(crate) closing: Vec<Summary>,
    pub(crate) removed: usize,
    pub(crate) warnings: usize,
}

impl Sweep<'_> {
    pub(crate) fn run(&self) -> io::Result<ResultSet> {
        self.check()?;
        let directory = Arc::new(Directory::open(self.root, false)?);
        let snapshot = catalog(self.root)?;
        let mut report = ResultSet::default();
        let mut records = Vec::new();
        for row in snapshot.journals {
            self.check()?;
            if row.source_kind == SourceKind::Interactive
                || row.started_at.is_some_and(|started| started >= self.now)
            {
                continue;
            }
            match Stored::read(self.root, row) {
                Ok(Some(record)) => records.push(record),
                Ok(None) => {}
                Err(_) => report.warnings += 1,
            }
        }
        records.sort_by(|left, right| order(&left.row).cmp(&order(&right.row)));
        let mut retained = Vec::<Stored>::new();
        for mut record in records {
            self.check()?;
            match self.classify(&mut record, &mut retained, &directory) {
                Ok(true) => retained.push(record),
                Ok(false) => report.removed += 1,
                Err(_) => {
                    report.warnings += 1;
                    retained.push(record);
                }
            }
        }

        // A retained journal selected for closing work is protected from this pass's retention.
        let mut closing = HashSet::new();
        for item in &retained {
            self.check()?;
            if self.summarize
                && item.decision == Decision::Keep
                && !self
                    .cancelled_closing
                    .contains(&item.row.id.replace('-', ""))
                && item.row.log_entry_count > 0
                && item
                    .row
                    .run_status
                    .is_some_and(|status| item.options.summarize(status))
                && !self.protected(&item.source)
                && !expired(item, self.now, self.caps, self.user_delete)
                && Progress::read(&item.source).is_ok_and(|progress| !progress.finished)
            {
                closing.insert(item.row.id.clone());
                report.closing.push(item.row.clone());
            }
        }
        self.expire(retained, &closing, &directory, &mut report)?;
        self.check()?;
        Ok(report)
    }

    fn classify(
        &self,
        item: &mut Stored,
        previous: &mut [Stored],
        directory: &Arc<Directory>,
    ) -> io::Result<bool> {
        if self.protected(&item.source) {
            return Ok(true);
        }
        let completed = self
            .completed
            .iter()
            .find(|artifact| item.source.same_journal(&artifact.source));
        if item.row.run_status.is_none() && (self.recover || completed.is_some()) {
            if item.row.ended_at.is_none() {
                summary_store::finish_abandoned(&item.source, self.cancelled)?;
                item.refresh(self.root)?;
            }
            if let Some(ended) = item.row.ended_at {
                let outcome = completed.map_or(RunStatus::Cancelled, |artifact| artifact.outcome);
                let expiry = item
                    .options
                    .expiry(ended, self.caps)?
                    .map(|value| value.to_string());
                self.update(
                    item,
                    [
                        ("runStatus", Some(outcome_key(outcome))),
                        ("expiresAt", expiry.as_deref()),
                        (
                            COMPLETE,
                            Some(if completed.is_some_and(|artifact| artifact.complete) {
                                "true"
                            } else {
                                "false"
                            }),
                        ),
                    ],
                )?;
            }
        }
        let Some(outcome) = item.row.run_status.filter(|_| item.row.ended_at.is_some()) else {
            return Ok(true);
        };
        if item.decision == Decision::Pending {
            let digest = if item.complete
                && item.options.deduplicate
                && item.command_digest.is_some()
                && item.row.log_entry_count != 0
            {
                capture_digest(&item.source, self.cancelled)?
            } else {
                None
            };
            let bytes = storage_bytes(&item.source)?.to_string();
            let duplicate = digest
                .as_deref()
                .filter(|_| self.user_delete && !item.row.pinned)
                .and_then(|digest| {
                    previous.iter().rev().find(|candidate| {
                        self.matches(item, candidate, digest)
                            && candidate.receipts.len() < MAX_RECEIPTS
                            && !expired(candidate, self.now, self.caps, self.user_delete)
                    })
                });
            let (decision, duplicate_id) =
                if self.user_delete && !item.row.pinned && !item.options.keep(outcome) {
                    ("discard", None)
                } else if let Some(duplicate) = duplicate {
                    ("duplicate", Some(duplicate.row.id.as_str()))
                } else {
                    ("keep", None)
                };
            self.update(
                item,
                [
                    (DECISION, Some(decision)),
                    (DIGEST, digest.as_deref()),
                    (DUPLICATE_OF, duplicate_id),
                    ("storageBytes", Some(bytes.as_str())),
                ],
            )?;
        }
        if item.row.pinned || !self.user_delete || item.decision == Decision::Keep {
            return Ok(true);
        }
        if item.decision == Decision::Duplicate {
            let Some(original) = previous.iter_mut().find(|original| {
                item.duplicate_of.as_ref() == Some(&original.row.id)
                    && item
                        .digest
                        .as_deref()
                        .is_some_and(|digest| self.matches(item, original, digest))
            }) else {
                self.update(item, [(DECISION, Some("keep")), (DUPLICATE_OF, None)])?;
                return Ok(true);
            };
            let receipt = item.row.id.clone();
            if !original.receipts.contains(&receipt) {
                if original.receipts.len() >= MAX_RECEIPTS {
                    self.update(item, [(DECISION, Some("keep")), (DUPLICATE_OF, None)])?;
                    return Ok(true);
                }
                let mut receipts = original.receipts.clone();
                receipts.push(receipt);
                let receipts = serde_json::to_string(&receipts)?;
                let count = original
                    .row
                    .duplicate_run_count
                    .saturating_add(1)
                    .to_string();
                self.update(
                    original,
                    [
                        (DUPLICATE_RECEIPTS, Some(receipts.as_str())),
                        ("duplicateRunCount", Some(count.as_str())),
                    ],
                )?;
            }
        }
        self.remove(item, directory)?;
        Ok(false)
    }

    fn matches(&self, item: &Stored, original: &Stored, digest: &str) -> bool {
        original.decision == Decision::Keep
            && !self.protected(&original.source)
            && original.row.source_kind == item.row.source_kind
            && original.row.source_id == item.row.source_id
            && original.row.automation_action == item.row.automation_action
            && original.row.run_status == item.row.run_status
            && original.row.run_id != item.row.run_id
            && original.target == item.target
            && original.command_digest == item.command_digest
            && original.digest.as_deref() == Some(digest)
    }

    fn expire(
        &self,
        retained: Vec<Stored>,
        closing: &HashSet<String>,
        directory: &Arc<Directory>,
        report: &mut ResultSet,
    ) -> io::Result<()> {
        let mut by_source = HashMap::<_, BTreeMap<_, Vec<Stored>>>::new();
        for item in retained {
            let Some(source_id) = item.row.source_id.clone() else {
                continue;
            };
            let Some(run_id) = item.row.run_id.clone() else {
                continue;
            };
            let started = item.row.run_started_at.or(item.row.started_at);
            by_source
                .entry((item.row.source_kind, source_id))
                .or_default()
                .entry((started, run_id))
                .or_default()
                .push(item);
        }
        for runs in by_source.into_values() {
            self.check()?;
            let Some((_, newest)) = runs.last_key_value() else {
                continue;
            };
            let options = &newest[0].options;
            let max_runs = self.caps.maximum_runs(options, self.user_delete);
            let max_bytes = self.caps.maximum_bytes(options, self.user_delete);
            let mut count = runs.len();
            let mut bytes: u64 = runs
                .values()
                .flatten()
                .map(|item| item.row.storage_bytes)
                .fold(0, u64::saturating_add);
            for group in runs.values() {
                self.check()?;
                if group.iter().any(|item| {
                    item.row.ended_at.is_none()
                        || item.row.pinned
                        || item.decision != Decision::Keep
                        || closing.contains(&item.row.id)
                        || self.protected(&item.source)
                }) {
                    continue;
                }
                let all_expired = group
                    .iter()
                    .all(|item| expired(item, self.now, self.caps, self.user_delete));
                let over = !std::ptr::eq(group, newest)
                    && (max_runs.is_some_and(|limit| count > limit)
                        || max_bytes.is_some_and(|limit| bytes > limit));
                if !all_expired && !over {
                    continue;
                }
                let mut all_removed = true;
                for item in group {
                    match self.remove(item, directory) {
                        Ok(()) => {
                            report.removed += 1;
                            bytes = bytes.saturating_sub(item.row.storage_bytes);
                        }
                        Err(_) => {
                            report.warnings += 1;
                            all_removed = false;
                        }
                    }
                }
                if all_removed {
                    count -= 1;
                }
            }
        }
        Ok(())
    }

    fn update<'a>(
        &self,
        item: &mut Stored,
        fields: impl IntoIterator<Item = (&'static str, Option<&'a str>)>,
    ) -> io::Result<()> {
        self.check()?;
        let lock = lock_source(&item.source)?;
        let (stamp, xml) = lock.read_document()?;
        let document = parse_document(&xml)?;
        if document_id(&document)? != item.source.id() {
            return Err(changed());
        }
        let expected = item.row.deletion_identity.as_ref().ok_or_else(changed)?;
        expected.require_document(&lock.directory().open_file("journal.xml")?)?;
        let mut patches = Vec::new();
        patch_scalars(&xml, meta(&document)?, fields, &mut patches)?;
        let next = apply_patches(&xml, patches)?;
        if next.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err(invalid("Automation journal metadata exceeds its limit"));
        }
        parse_document(&next)?;
        lock.publish_document(&stamp, Some(&next), |opened| {
            self.check()?;
            item.source
                .require_location(item.source.directory(), opened)
        })?;
        lock.directory().sync()?;
        drop(lock);
        item.refresh(self.root)
    }

    fn remove(&self, item: &Stored, directory: &Arc<Directory>) -> io::Result<()> {
        self.check()?;
        if item.row.pinned || self.protected(&item.source) || item.row.ended_at.is_none() {
            return Err(changed());
        }
        let expected = item.row.deletion_identity.as_ref().ok_or_else(changed)?;
        Deletion::prepare(
            Arc::clone(directory),
            self.root,
            item.source.directory(),
            item.source.id(),
            expected,
        )?
        .delete()
    }

    fn check(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(io::ErrorKind::Interrupted.into())
        } else {
            Ok(())
        }
    }

    fn protected(&self, source: &Source) -> bool {
        self.protected
            .iter()
            .any(|pending| source.same_journal(pending))
    }
}

impl Stored {
    fn read(root: &Path, row: Summary) -> io::Result<Option<Self>> {
        let source = Source::catalog(&row)?;
        if !is_native_path(root, &source) {
            return Ok(None);
        }
        source.clone().with_document(|_, document| {
            let meta = meta(document)?;
            let Some(options) = field(meta, OPTIONS)? else {
                return Ok(None);
            };
            let options: Options = serde_json::from_str(&options)?;
            options.validate()?;
            let target = field(meta, TARGET)?.ok_or_else(changed)?.into_owned();
            if target.is_empty() || row.run_id.as_ref().is_none_or(|value| value.is_empty()) {
                return Err(changed());
            }
            let decision = match field(meta, DECISION)?.as_deref() {
                None => Decision::Pending,
                Some("keep") => Decision::Keep,
                Some("discard") => Decision::Discard,
                Some("duplicate") => Decision::Duplicate,
                Some(_) => return Err(changed()),
            };
            let receipts = duplicate_receipts(meta)?;
            Ok(Some(Self {
                source,
                row,
                options,
                target,
                complete: boolean(meta, COMPLETE)?,
                command_digest: field(meta, COMMAND_DIGEST)?.map(|value| value.into_owned()),
                decision,
                digest: field(meta, DIGEST)?.map(|value| value.into_owned()),
                duplicate_of: field(meta, DUPLICATE_OF)?.map(|value| value.into_owned()),
                receipts,
            }))
        })
    }

    fn refresh(&mut self, root: &Path) -> io::Result<()> {
        let snapshot = self.source.read(None)?.ok_or_else(changed)?;
        *self = Self::read(root, snapshot.summary)?.ok_or_else(changed)?;
        Ok(())
    }
}

pub(super) fn duplicate_receipts(meta: Node<'_, '_>) -> io::Result<Vec<String>> {
    let receipts: Vec<String> = field(meta, DUPLICATE_RECEIPTS)?
        .map(|value| serde_json::from_str(&value))
        .transpose()?
        .unwrap_or_default();
    if receipts.len() > MAX_RECEIPTS
        || !receipts
            .iter()
            .all(|id| kortty_jobs::valid_session_journal_id(id))
    {
        return Err(changed());
    }
    Ok(receipts)
}

fn order(row: &Summary) -> (Option<Timestamp>, &str) {
    (row.run_started_at.or(row.started_at), row.id.as_str())
}

fn expired(item: &Stored, now: Timestamp, caps: Caps, user_delete: bool) -> bool {
    let Some(ended) = item.row.ended_at else {
        return false;
    };
    let managed = Options {
        retention: super::Retention::Forever,
        ..Options::default()
    };
    let limit = managed.expiry(ended, caps).ok().flatten();
    user_delete && item.row.expires_at.is_some_and(|expiry| expiry <= now)
        || limit.is_some_and(|expiry| expiry <= now)
}

fn storage_bytes(source: &Source) -> io::Result<u64> {
    let root = Directory::open(source.directory(), false)?;
    source.require_location(source.directory(), &root)?;
    let bytes = measure(&root)?;
    let screenshots = match root.deletion_child(std::ffi::OsStr::new("screenshots")) {
        Ok(directory) => measure(&directory)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error),
    };
    source.require_location(source.directory(), &root)?;
    Ok(bytes.saturating_add(screenshots))
}

fn measure(directory: &Directory) -> io::Result<u64> {
    let names = directory.names(MAX_FILES + 1)?;
    if names.len() > MAX_FILES {
        return Err(invalid("Automation journal exceeds its file limit"));
    }
    let mut total = 0_u64;
    for name in names {
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == "journal.xml" || name.starts_with("session-log") || name.ends_with(".png") {
            let file = directory.open_file(name)?;
            crate::private_files::require_single_link(&file)?;
            total = total.saturating_add(file.metadata()?.len());
        }
    }
    Ok(total)
}

fn capture_digest(source: &Source, cancelled: &AtomicBool) -> io::Result<Option<String>> {
    source.with_document(|file, document| {
        let count = number::<u32>(meta(document)?, "logParts")?.unwrap_or(1);
        let mut budget = MAX_HASH_BYTES;
        let mut digest = HashAlgorithm::Sha256
            .new_hasher()
            .map_err(io::Error::other)?;
        let mut image = false;
        let status = retrieval::visit(
            file,
            source.id(),
            count,
            None,
            &mut budget,
            &|| cancelled.load(Ordering::Acquire),
            |row| {
                let record = row.record;
                let kind = match record.kind {
                    CaptureKind::Seed => 0,
                    CaptureKind::Input => 1,
                    CaptureKind::Output => 2,
                    CaptureKind::Note => 3,
                    CaptureKind::Screenshot => {
                        image = true;
                        return false;
                    }
                };
                digest.update(&[kind, u8::from(record.partial), u8::from(record.redacted)]);
                digest.update(&record.repeat.to_le_bytes());
                digest.update(&(record.text.len() as u64).to_le_bytes());
                digest.update(record.text.as_bytes());
                true
            },
        )?;
        if image || status.notice().is_some() {
            return Ok(None);
        }
        let mut output = String::with_capacity(64);
        for byte in digest.finalize() {
            let _ = write!(output, "{byte:02x}");
        }
        Ok(Some(output))
    })
}

fn meta<'a, 'input>(document: &'a roxmltree::Document<'input>) -> io::Result<Node<'a, 'input>> {
    child(document.root_element(), "meta").ok_or_else(changed)
}

fn changed() -> io::Error {
    invalid("The automation journal changed during retention review")
}
