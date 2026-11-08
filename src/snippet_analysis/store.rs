//! Private, revisioned histories with one atomic file per analysis subject.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::model::{
    History, MAX_CONTENT_BYTES, Purpose, Record, Subject, content_hash, now_millis,
};
use crate::private_files::{Directory, FileStamp, require_single_link};
use crate::snippets::SnippetId;

const DIRECTORY_NAME: &str = "snippet-analyses";
const LOCK_NAME: &str = ".history.lock";
const SCHEMA_VERSION: u32 = 1;
const RETENTION: usize = 5;
const MAX_RECORDS: usize = 128;
const MAX_FILE_BYTES: usize = 12 * MAX_CONTENT_BYTES;
const CONTENT_BUDGET: usize = MAX_FILE_BYTES / 2;
// Keep some space for finding and provenance metadata even with heavy JSON escaping.
const ENCODED_CONTENT_BUDGET: usize = MAX_FILE_BYTES - 8 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct Store {
    profile: PathBuf,
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
        Self { profile }
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
        still_current: impl FnOnce() -> bool,
    ) -> io::Result<History> {
        self.update(subject, |history| {
            if !still_current() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "The analysis is no longer current",
                ));
            }
            if history
                .records
                .iter()
                .any(|existing| existing.id == record.id)
            {
                return Ok(false);
            }
            validate_source(&record)?;
            let protected = (record.purpose == Purpose::Verify)
                .then_some(record.previous_record_id)
                .flatten();
            history.records.insert(0, record);
            let mut index = history.records.len();
            while history.records.len() > RETENTION && index > 1 {
                index -= 1;
                let old = &history.records[index];
                if !old.pinned && Some(old.id) != protected {
                    history.records.remove(index);
                }
            }
            if history.records.len() > MAX_RECORDS {
                return Err(invalid("The analysis history is full of protected records"));
            }
            Ok(true)
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

    fn update(
        &self,
        subject: Subject,
        change: impl FnOnce(&mut History) -> io::Result<bool>,
    ) -> io::Result<History> {
        let name = file_name(subject)?;
        let directory = self.directory(true)?;
        let _lock = lock_history(&directory)?;
        let (mut history, before) = read(&directory, &name, subject)?;
        if !change(&mut history)? {
            return Ok(history);
        }
        history.revision = history
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("The analysis history revision is exhausted"))?;
        history.updated_at = now_millis().max(history.updated_at);
        let document = Document::capture(history);
        let write = |file: &mut File| {
            let mut output = BoundedWriter {
                output: io::BufWriter::new(file),
                remaining: MAX_FILE_BYTES,
            };
            serde_json::to_writer(&mut output, &document).map_err(io::Error::from)?;
            output.flush()
        };
        if let Some(before) = before {
            directory.replace_with(&name, write, || {
                let current = directory.open_private_file(&name)?;
                require_single_link(&current)?;
                before.require_unchanged(&current)
            })?;
        } else {
            directory.publish_with(&name, write)?;
        }
        directory.sync()?;
        Ok(document.history)
    }
}

fn lock_history(directory: &Directory) -> io::Result<File> {
    let lock = directory.open_lock(LOCK_NAME)?;
    require_single_link(&lock)?;
    lock.lock()?;
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
    fn capture(mut history: History) -> Self {
        let mut raw_remaining = CONTENT_BUDGET;
        let mut encoded_remaining = ENCODED_CONTENT_BUDGET;
        let mut blobs = BTreeMap::new();
        for record in &mut history.records {
            let source = &mut record.source;
            let Some(content) = &source.content else {
                continue;
            };
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
        Self {
            schema_version: SCHEMA_VERSION,
            history,
            blobs,
        }
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
        if self.blobs.len() > self.history.records.len() {
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
                used.insert(record.source.sha256.as_str());
            }
        }
        if used.len() != self.blobs.len() {
            return Err(invalid(
                "The analysis history has an unreferenced source blob",
            ));
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
