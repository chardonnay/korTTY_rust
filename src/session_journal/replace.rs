//! Verified preview and staged replacement of a stopped journal and its capture parts.

use std::ffi::OsStr;
use std::fmt;
use std::io;

use crate::private_files::{Directory, EntryIdentity, FileStamp, nonce};

use super::capture::replacement::{Mode, inventory};
use super::entry_edit::lock_source;
use super::metadata::require_single_link;
use super::viewer::Source;
use super::{DOCUMENT, child, document_id, invalid, number, parse_document};

pub(in crate::session_journal) mod rule;
mod xml;

pub(crate) use rule::{MAX_RULE_BYTES, Rule};

const MAX_CAPTURE_IO: u64 = 512 * 1024 * 1024;
const MAX_STAGED_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Counts {
    pub(crate) fields: usize,
    pub(crate) records: usize,
}

impl Counts {
    fn add(&mut self, other: Self) {
        self.fields += other.fields;
        self.records += other.records;
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Outcome {
    pub(crate) counts: Counts,
    pub(crate) published: usize,
    pub(crate) pending: usize,
    pub(crate) durability_warning: bool,
    pub(crate) cleanup_warning: bool,
}

pub(crate) struct Preview {
    source: Source,
    rule: Rule,
    document: FileStamp,
    capture: Option<CaptureReview>,
    counts: Counts,
}

impl Preview {
    pub(crate) fn counts(&self) -> Counts {
        self.counts
    }

    pub(crate) fn source(&self) -> &Source {
        &self.source
    }
}

impl fmt::Debug for Preview {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JournalReplacePreview")
            .field("counts", &self.counts)
            .finish_non_exhaustive()
    }
}

struct CaptureReview {
    declared: u32,
    parts: Vec<PartReview>,
}

struct PartReview {
    name: String,
    stamp: FileStamp,
}

impl CaptureReview {
    fn require_inventory(&self, directory: &Directory) -> io::Result<()> {
        let parts = inventory(directory, self.declared)?;
        if !parts
            .iter()
            .map(|part| part.name())
            .eq(self.parts.iter().map(|part| part.name.as_str()))
        {
            return Err(changed());
        }
        Ok(())
    }

    fn require_current(&self, directory: &Directory) -> io::Result<()> {
        self.require_inventory(directory)?;
        for part in &self.parts {
            require_file(directory, &part.name, &part.stamp)?;
        }
        Ok(())
    }
}

/// Counts changes against a verified snapshot without acquiring a lock or staging files.
pub(crate) fn preview(
    source: &Source,
    rule: Rule,
    include_capture: bool,
    live: bool,
    cancelled: impl Fn() -> bool,
) -> io::Result<Preview> {
    check_cancelled(&cancelled)?;
    let directory = Directory::open(source.directory(), false)?;
    source.require_location(source.directory(), &directory)?;
    require_member(&directory, DOCUMENT)?;
    let (document, capture, counts) = source.with_document(|opened, document| {
        check_cancelled(&cancelled)?;
        let stamp = FileStamp::read(opened.file())?;
        require_file(&directory, DOCUMENT, &stamp)?;
        let (fields, _) = xml::rewrite(document.input_text(), &rule)?;
        check_cancelled(&cancelled)?;
        let mut counts = Counts { fields, records: 0 };
        let capture = if include_capture {
            let meta = child(document.root_element(), "meta")
                .ok_or_else(|| invalid("Missing session journal metadata"))?;
            let declared = number(meta, "logParts")?.unwrap_or(0);
            let parts = inventory(&directory, declared)?;
            let mut budget = MAX_CAPTURE_IO;
            let mut reviewed = Vec::with_capacity(parts.len());
            for (index, part) in parts.iter().enumerate() {
                check_cancelled(&cancelled)?;
                let (stamp, raw) = part.read(&directory, &mut budget)?;
                let mode = Mode::Preview {
                    allow_open: live && index + 1 == parts.len(),
                };
                let (records, _) =
                    part.rewrite(&raw, source.id(), &rule, mode, &mut budget, &cancelled)?;
                counts.records += record_count(records)?;
                reviewed.push(PartReview {
                    name: part.name().into(),
                    stamp,
                });
            }
            let capture = CaptureReview {
                declared,
                parts: reviewed,
            };
            check_cancelled(&cancelled)?;
            capture.require_current(&directory)?;
            Some(capture)
        } else {
            None
        };
        Ok((stamp, capture, counts))
    })?;
    check_cancelled(&cancelled)?;
    Ok(Preview {
        source: source.clone(),
        rule,
        document,
        capture,
        counts,
    })
}

fn check_cancelled(cancelled: &impl Fn() -> bool) -> io::Result<()> {
    if cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    Ok(())
}

/// Revalidates the reviewed source, stages every changed file, then publishes one at a time.
pub(crate) fn apply(preview: &Preview) -> io::Result<Outcome> {
    let source = preview.source();
    let lock = lock_source(source).map_err(|error| {
        if error.kind() == io::ErrorKind::WouldBlock {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "A session journal writer is still running. Stop it before applying replacements.",
            )
        } else {
            error
        }
    })?;
    let directory = lock.directory();
    require_file(directory, DOCUMENT, &preview.document)?;
    let (stamp, original) = lock.read_document()?;
    if stamp != preview.document || document_id(&parse_document(&original)?)? != source.id() {
        return Err(changed());
    }
    if let Some(capture) = &preview.capture {
        capture.require_current(directory)?;
    }
    if preview.counts() == Counts::default() {
        return Ok(Outcome::default());
    }

    let (fields, updated) = xml::rewrite(&original, &preview.rule)?;
    drop(original);
    let mut stage = Stage::create(directory)?;
    if let Some(capture) = &preview.capture {
        stage_capture(&mut stage, capture, source, &preview.rule)?;
    }
    if let Some(updated) = updated {
        // Publish the XML last so its reviewed stamp remains a guard for the capture files.
        stage.write(
            DOCUMENT,
            updated.as_bytes(),
            preview.document.clone(),
            Counts { fields, records: 0 },
        )?;
    }
    if stage.counts() != preview.counts() {
        return Err(changed());
    }

    let require_source = || {
        source.require_location(source.directory(), directory)?;
        lock.require_current(source.directory())?;
        require_file(directory, DOCUMENT, &preview.document)
    };
    stage.require_ready()?;
    require_source()?;
    if let Some(capture) = &preview.capture {
        capture.require_current(directory)?;
    }
    stage.publish(require_source)
}

fn stage_capture(
    stage: &mut Stage<'_>,
    capture: &CaptureReview,
    source: &Source,
    rule: &Rule,
) -> io::Result<()> {
    let parts = inventory(stage.journal, capture.declared)?;
    if !parts
        .iter()
        .map(|part| part.name())
        .eq(capture.parts.iter().map(|part| part.name.as_str()))
    {
        return Err(changed());
    }
    let mut budget = MAX_CAPTURE_IO;
    for (part, reviewed) in parts.into_iter().zip(&capture.parts) {
        let (stamp, raw) = part.read(stage.journal, &mut budget)?;
        if stamp != reviewed.stamp {
            return Err(changed());
        }
        let (records, rewritten) =
            part.rewrite(&raw, source.id(), rule, Mode::Rewrite, &mut budget, &|| {
                false
            })?;
        if let Some(rewritten) = rewritten {
            stage.write(
                part.name(),
                &rewritten,
                stamp,
                Counts {
                    fields: 0,
                    records: record_count(records)?,
                },
            )?;
        }
    }
    Ok(())
}

fn record_count(count: u64) -> io::Result<usize> {
    usize::try_from(count)
        .map_err(|_| invalid("Session journal replacement count exceeds its limit"))
}

fn require_member(directory: &Directory, name: &str) -> io::Result<()> {
    #[cfg(unix)]
    if directory.deletion_entry(OsStr::new(name))?.1 {
        return Err(invalid(
            "A session journal file was replaced by a directory",
        ));
    }
    #[cfg(windows)]
    let _ = (directory, name);
    Ok(())
}

fn require_file(directory: &Directory, name: &str, stamp: &FileStamp) -> io::Result<()> {
    require_member(directory, name)?;
    let file = directory.open_file(name)?;
    require_single_link(&file)?;
    stamp.require_unchanged(&file).map_err(|_| changed())
}

fn changed() -> io::Error {
    invalid("The session journal changed. Count the replacements again.")
}

struct StagedFile {
    name: String,
    original: FileStamp,
    staged: FileStamp,
    counts: Counts,
}

struct Stage<'a> {
    journal: &'a Directory,
    directory: Directory,
    identity: EntryIdentity,
    name: String,
    files: Vec<StagedFile>,
    bytes_left: u64,
    published: usize,
    cleaned: bool,
}

impl<'a> Stage<'a> {
    fn create(journal: &'a Directory) -> io::Result<Self> {
        let name = format!(".kortty-journal-replace-{}", nonce()?);
        let directory = journal.create_private_subdirectory(OsStr::new(&name))?;
        Ok(Self {
            journal,
            identity: directory.identity()?,
            directory,
            name,
            files: Vec::new(),
            bytes_left: MAX_STAGED_BYTES,
            published: 0,
            cleaned: false,
        })
    }

    fn write(
        &mut self,
        name: &str,
        bytes: &[u8],
        original: FileStamp,
        counts: Counts,
    ) -> io::Result<()> {
        self.bytes_left = self
            .bytes_left
            .checked_sub(bytes.len() as u64)
            .ok_or_else(|| invalid("Session journal replacement exceeds its staging size limit"))?;
        self.directory.publish_copy(name, bytes)?;
        let staged = self
            .directory
            .open_private_file(name)
            .and_then(|file| FileStamp::read(&file));
        let staged = match staged {
            Ok(staged) => staged,
            Err(error) => {
                let _ = self.directory.remove_copy(name);
                return Err(error);
            }
        };
        self.files.push(StagedFile {
            name: name.into(),
            original,
            staged,
            counts,
        });
        Ok(())
    }

    fn counts(&self) -> Counts {
        let mut counts = Counts::default();
        for file in &self.files {
            counts.add(file.counts);
        }
        counts
    }

    fn require_current(&self) -> io::Result<()> {
        if self
            .journal
            .deletion_child(OsStr::new(&self.name))?
            .identity()?
            != self.identity
        {
            return Err(invalid("The session journal replacement stage changed"));
        }
        Ok(())
    }

    fn require_ready(&self) -> io::Result<()> {
        self.require_current()?;
        for file in &self.files {
            file.staged
                .require_unchanged(&self.directory.open_private_file(&file.name)?)?;
        }
        self.directory.sync()
    }

    fn publish(mut self, require_source: impl Fn() -> io::Result<()>) -> io::Result<Outcome> {
        let mut outcome = Outcome::default();
        for file in &self.files {
            let result =
                self.journal
                    .replace_staged(&self.directory, &file.name, &file.staged, || {
                        require_source()?;
                        self.require_current()?;
                        require_file(self.journal, &file.name, &file.original)
                    });
            match result {
                Ok(()) => {
                    self.published += 1;
                    outcome.counts.add(file.counts);
                }
                Err(error) if self.published == 0 => return Err(error),
                Err(_) => break,
            }
        }
        outcome.published = self.published;
        outcome.pending = self.files.len() - self.published;
        outcome.cleanup_warning = self.cleanup().is_err();
        outcome.durability_warning = self.journal.sync().is_err();
        Ok(outcome)
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        let mut error = None;
        for file in &self.files[self.published..] {
            if let Err(problem) = self.directory.remove_unchanged(&file.name, &file.staged)
                && problem.kind() != io::ErrorKind::NotFound
            {
                error.get_or_insert(problem);
            }
        }
        if let Err(problem) =
            self.journal
                .remove_deletion_entry(OsStr::new(&self.name), self.identity, true)
        {
            error.get_or_insert(problem);
        } else {
            self.cleaned = true;
        }
        error.map_or(Ok(()), Err)
    }
}

impl Drop for Stage<'_> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[cfg(test)]
mod tests;
