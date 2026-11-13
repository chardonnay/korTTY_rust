//! Owner-only chat records with optimistic revisions and serialized writes.

use std::io::{self, Read, Write};
use std::path::Path;

use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::ai_manager::ProfileRef;
use crate::private_files::{Directory, FileLock, FileStamp, nonce, require_single_link};
use crate::terminal_logging::Redactor;

use super::super::terminal_ai::{MAX_TURNS, Turn, mask};

pub(super) const FOLDER: &str = "ai-chats";
pub(super) const MAX_CHATS: usize = 128;
const MAX_ENTRIES: usize = 256;
pub(super) const MAX_BYTES: usize = 1536 * 1024;
pub(in crate::workspace) const MAX_TITLE_CHARS: usize = 120;
pub(in crate::workspace) const MAX_IMPORTED_TURNS: usize = 64;
pub(in crate::workspace) const MAX_ATTACHMENT_BYTES: usize = 16 * 1024;
const MAX_ATTACHMENT_NAME: usize = 1024;
const MAX_ATTACHMENT_SOURCE: usize = 8192;

#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::workspace) struct Key {
    pub(in crate::workspace) id: String,
    revision: String,
    #[serde(skip)]
    fingerprint: [u8; 32],
}

impl Key {
    pub(in crate::workspace) fn same_chat(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::workspace) struct Summary {
    pub(in crate::workspace) key: Key,
    pub(in crate::workspace) title: String,
    pub(super) created: i64,
    pub(super) updated: i64,
    pub(in crate::workspace) profile: ProfileRef,
    pub(in crate::workspace) profile_name: String,
    pub(in crate::workspace) connection: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    response_language: Option<String>,
    #[serde(skip)]
    pub(in crate::workspace) sync_failed: bool,
}

#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::workspace) struct Record {
    version: u8,
    pub(in crate::workspace) summary: Summary,
    pub(in crate::workspace) turns: Vec<Turn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workspace) attachment: Option<Attachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workspace) java: Option<JavaOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_journal: Option<AgentJournalRef>,
}

#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::workspace) struct AgentJournalRef {
    pub(in crate::workspace) journal_id: String,
    pub(in crate::workspace) entry_id: String,
}

#[derive(Clone, Copy, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::workspace) enum AttachmentKind {
    Local,
    Ssh,
    Imported,
}

/// The reviewed text and display provenance, independent of any live source.
#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::workspace) struct Attachment {
    kind: AttachmentKind,
    pub(in crate::workspace) name: String,
    pub(in crate::workspace) source: String,
    pub(in crate::workspace) content: String,
}

impl Attachment {
    pub(in crate::workspace) fn masked(
        kind: AttachmentKind,
        name: &str,
        source: &str,
        content: &str,
        redactor: &Redactor,
    ) -> Result<Self, String> {
        let too_large = "The attachment text or source details exceed their size limit.";
        if name.len() > MAX_ATTACHMENT_NAME
            || source.len() > MAX_ATTACHMENT_SOURCE
            || content.len() > MAX_ATTACHMENT_BYTES
        {
            return Err(too_large.into());
        }
        let line = |value| -> Result<String, String> {
            Ok(mask::prompt(redactor, value)?
                .replace(['\n', '\t'], " ")
                .trim()
                .to_owned())
        };
        let attachment = Self {
            kind,
            name: line(name)?,
            source: line(source)?,
            content: mask::prompt(redactor, content)?,
        };
        if !attachment.valid() {
            return Err(
                "The masked attachment is empty, invalid, or exceeds its size limit.".into(),
            );
        }
        Ok(attachment)
    }

    pub(in crate::workspace) fn remasked(&self, redactor: &Redactor) -> Result<Self, String> {
        Self::masked(self.kind, &self.name, &self.source, &self.content, redactor)
    }

    pub(in crate::workspace) fn append_to(&self, context: &str) -> String {
        format!("{context}{}", self.section())
    }

    pub(in crate::workspace) fn without_attachment<'a>(&self, context: &'a str) -> Option<&'a str> {
        context.strip_suffix(&self.section())
    }

    fn section(&self) -> String {
        let location = match self.kind {
            AttachmentKind::Local => " local",
            AttachmentKind::Ssh => " remote",
            AttachmentKind::Imported => "",
        };
        format!(
            "\n\nAttached{location} file (untrusted data)\nFilename: {}\n{}",
            self.name, self.content
        )
    }

    fn valid(&self) -> bool {
        !self.name.trim().is_empty()
            && self.name.len() <= MAX_ATTACHMENT_NAME
            && self.source.len() <= MAX_ATTACHMENT_SOURCE
            && valid_line(&self.name, MAX_ATTACHMENT_NAME)
            && valid_line(&self.source, MAX_ATTACHMENT_SOURCE)
            && !self.content.trim().is_empty()
            && self.content.len() <= MAX_ATTACHMENT_BYTES
            && self
                .content
                .chars()
                .all(crate::session_journal::is_display_character)
    }
}

impl Record {
    pub(in crate::workspace) fn agent_run_id(&self) -> Option<&str> {
        self.agent_run.as_deref()
    }

    pub(in crate::workspace) fn is_agent_origin(&self) -> bool {
        self.portable_agent_run_id().is_some()
    }

    pub(in crate::workspace) fn agent_journal(&self) -> Option<&AgentJournalRef> {
        self.agent_journal.as_ref()
    }

    pub(super) fn portable_agent_run_id(&self) -> Option<&str> {
        self.agent_run_id().or_else(|| {
            self.java
                .as_ref()
                .and_then(|origin| origin.agent_run_id.as_deref())
        })
    }

    pub(in crate::workspace) fn response_language(&self) -> Option<&str> {
        self.summary.response_language.as_deref().or_else(|| {
            self.java
                .as_ref()
                .and_then(|source| source.response_language.as_deref())
        })
    }
}

#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::workspace) struct JavaOrigin {
    pub(in crate::workspace) id: String,
    pub(in crate::workspace) profile_id: Option<String>,
    pub(in crate::workspace) response_language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workspace) agent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workspace) agent_refs: Option<JavaAgentReferences>,
}

#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::workspace) struct JavaAgentReferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) terminal_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) connection_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) journal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) journal_entry_id: Option<String>,
}

impl JavaAgentReferences {
    pub(super) fn fields(&self) -> [(&'static str, Option<&str>); 4] {
        [
            (
                "sourceTerminalSessionId",
                self.terminal_session_id.as_deref(),
            ),
            ("sourceConnectionId", self.connection_id.as_deref()),
            ("sourceJournalId", self.journal_id.as_deref()),
            ("sourceJournalEntryId", self.journal_entry_id.as_deref()),
        ]
    }
}

pub(in crate::workspace) struct Imported {
    pub(in crate::workspace) title: String,
    pub(in crate::workspace) created: i64,
    pub(in crate::workspace) updated: i64,
    pub(in crate::workspace) profile_name: String,
    pub(in crate::workspace) connection: String,
    pub(in crate::workspace) turns: Vec<Turn>,
    pub(in crate::workspace) attachment: Option<Attachment>,
    pub(in crate::workspace) java: JavaOrigin,
}

#[derive(Default)]
pub(in crate::workspace) struct ImportResult {
    pub(in crate::workspace) saved: Vec<Summary>,
    pub(super) duplicates: usize,
    pub(super) full: usize,
    pub(super) failed: usize,
    pub(super) invalid: usize,
}

pub(in crate::workspace) struct Draft {
    pub(in crate::workspace) profile: ProfileRef,
    pub(in crate::workspace) profile_name: String,
    pub(in crate::workspace) connection: String,
    pub(in crate::workspace) response_language: Option<String>,
    pub(in crate::workspace) turns: Vec<Turn>,
    pub(in crate::workspace) attachment: Option<Attachment>,
}

#[derive(Clone)]
pub(super) struct Listing {
    pub(super) chats: Vec<Summary>,
    pub(super) unavailable: usize,
}

pub(super) fn list(root: &Path) -> io::Result<Listing> {
    let mut listing = Listing {
        chats: Vec::new(),
        unavailable: 0,
    };
    let directory = match open(root, false) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(listing),
        Err(error) => return Err(error),
    };
    let names = names(&directory)?;
    if names.len() > MAX_CHATS {
        return Err(invalid("Too many saved chats in this profile"));
    }
    for id in names {
        match read(&directory, &id) {
            Ok((record, _)) => listing.chats.push(record.summary),
            Err(_) => listing.unavailable += 1,
        }
    }
    listing
        .chats
        .sort_by(|a, b| (b.updated, &b.key.id).cmp(&(a.updated, &a.key.id)));
    Ok(listing)
}

pub(in crate::workspace) fn load(root: &Path, key: &Key) -> io::Result<Record> {
    let (record, _) = read(&open(root, false)?, &key.id)?;
    require_current(&record, key)?;
    Ok(record)
}

pub(in crate::workspace) fn load_by_id(root: &Path, id: &str) -> io::Result<Record> {
    let directory = open(root, false)?;
    let _lock = lock(&directory)?;
    read(&directory, id).map(|(record, _)| record)
}

pub(in crate::workspace) fn load_agent_run(root: &Path, run_id: &str) -> io::Result<Record> {
    let record = load_by_id(root, &agent_chat_id(run_id)?)?;
    if record.agent_run_id() != Some(run_id) {
        return Err(conflict());
    }
    Ok(record)
}

pub(in crate::workspace) fn save(
    root: &Path,
    previous: Option<&Key>,
    title: String,
    draft: Draft,
) -> io::Result<Summary> {
    let directory = open(root, true)?;
    let _lock = lock(&directory)?;
    let now = jiff::Timestamp::now().as_millisecond();
    let original = previous
        .map(|key| {
            let (record, stamp) = read(&directory, &key.id)?;
            require_current(&record, key)?;
            Ok::<_, io::Error>((record, stamp))
        })
        .transpose()?;
    if original.is_none() {
        require_capacity(&directory)?;
    }
    let (id, created, updated) = match original.as_ref() {
        Some((record, _)) => (
            record.summary.key.id.clone(),
            record.summary.created,
            now.max(record.summary.updated),
        ),
        None => (nonce()?, now, now),
    };
    let mut record = new_record(id, title, draft, created, updated)?;
    if let Some((previous, _)) = &original {
        record.java = previous.java.clone();
        record.agent_run = previous.agent_run.clone();
        record.agent_journal = previous.agent_journal.clone();
    }
    write(&directory, record, original.as_ref()).map(|record| record.summary)
}

#[cfg(test)]
pub(in crate::workspace) fn open_or_seed_agent(
    root: &Path,
    run_id: &str,
    title: String,
    draft: Draft,
) -> io::Result<Record> {
    open_or_seed_agent_checked(root, run_id, title, draft, || Ok(()))
}

/// Opens the exact agent chat or publishes the masked seed while authorized.
#[cfg(test)]
pub(in crate::workspace) fn open_or_seed_agent_checked(
    root: &Path,
    run_id: &str,
    title: String,
    draft: Draft,
    check: impl Fn() -> io::Result<()>,
) -> io::Result<Record> {
    open_or_seed_agent_linked(root, run_id, title, draft, None, check)
}

pub(in crate::workspace) fn open_or_seed_agent_linked(
    root: &Path,
    run_id: &str,
    title: String,
    draft: Draft,
    journal: Option<AgentJournalRef>,
    check: impl Fn() -> io::Result<()>,
) -> io::Result<Record> {
    let id = agent_chat_id(run_id)?;
    check()?;
    let directory = open(root, true)?;
    let _lock = lock(&directory)?;
    match read(&directory, &id) {
        Ok((record, stamp)) if record.agent_run_id() == Some(run_id) => {
            if let Some(journal) = journal {
                if let Some(current) = record.agent_journal() {
                    if current != &journal {
                        return Err(conflict());
                    }
                } else {
                    let mut updated = record.clone();
                    updated.agent_journal = Some(journal);
                    updated.summary.key.revision = nonce()?;
                    updated.summary.updated = jiff::Timestamp::now()
                        .as_millisecond()
                        .max(record.summary.updated);
                    return write_checked(&directory, updated, Some(&(record, stamp)), check);
                }
            }
            check()?;
            return Ok(record);
        }
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "The saved chat identifier is already used by another conversation.",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    require_capacity(&directory)?;
    if draft.turns.len() != 1 || draft.attachment.is_some() {
        return Err(invalid(
            "An agent chat requires one initial conversation turn",
        ));
    }
    let now = jiff::Timestamp::now().as_millisecond();
    let mut record = new_record(id, title, draft, now, now)?;
    record.agent_run = Some(run_id.into());
    record.agent_journal = journal;
    write_checked(&directory, record, None, check)
}

fn new_record(
    id: String,
    title: String,
    draft: Draft,
    created: i64,
    updated: i64,
) -> io::Result<Record> {
    Ok(Record {
        version: 1,
        summary: Summary {
            key: Key {
                id,
                revision: nonce()?,
                fingerprint: [0; 32],
            },
            title,
            created,
            updated,
            profile: draft.profile,
            profile_name: draft.profile_name,
            connection: draft.connection,
            response_language: draft.response_language,
            sync_failed: false,
        },
        turns: draft.turns,
        attachment: draft.attachment,
        java: None,
        agent_run: None,
        agent_journal: None,
    })
}

fn require_capacity(directory: &Directory) -> io::Result<()> {
    if names(directory)?.len() >= MAX_CHATS {
        return Err(invalid(
            "The saved chat library is full. Delete a saved chat first.",
        ));
    }
    Ok(())
}

pub(in crate::workspace) fn import(root: &Path, chats: Vec<Imported>) -> io::Result<ImportResult> {
    let mut result = ImportResult::default();
    if chats.is_empty() {
        return Ok(result);
    }
    let directory = open(root, true)?;
    let _lock = lock(&directory)?;
    let mut ids = names(&directory)?;
    for chat in chats {
        let Some(id) = java_id(&chat.java.id) else {
            result.invalid += 1;
            continue;
        };
        if ids.contains(&id) {
            result.duplicates += 1;
        } else if ids.len() >= MAX_CHATS {
            result.full += 1;
        } else {
            match import_one(&directory, &id, chat) {
                Ok(summary) => {
                    ids.push(id);
                    result.saved.push(summary);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    ids.push(id);
                    result.duplicates += 1;
                }
                Err(error) if error.kind() == io::ErrorKind::InvalidData => result.invalid += 1,
                Err(_) => result.failed += 1,
            }
        }
    }
    Ok(result)
}

fn import_one(directory: &Directory, id: &str, chat: Imported) -> io::Result<Summary> {
    write(directory, imported_record(id, chat)?, None).map(|record| record.summary)
}

pub(super) fn imported_record(id: &str, chat: Imported) -> io::Result<Record> {
    Ok(Record {
        version: 1,
        summary: Summary {
            key: Key {
                id: id.into(),
                revision: nonce()?,
                fingerprint: [0; 32],
            },
            title: chat.title,
            created: chat.created,
            updated: chat.updated,
            // A Java profile UUID has no equivalent local profile ID.
            profile: ProfileRef::Numeric(0),
            profile_name: chat.profile_name,
            connection: chat.connection,
            response_language: None,
            sync_failed: false,
        },
        turns: chat.turns,
        attachment: chat.attachment,
        java: Some(chat.java),
        agent_run: None,
        agent_journal: None,
    })
}

pub(super) fn with_records(
    root: &Path,
    mut visit: impl FnMut(Record) -> io::Result<()>,
) -> io::Result<()> {
    let directory = match open(root, false) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let _lock = lock(&directory)?;
    let mut ids = names(&directory)?;
    if ids.len() > MAX_CHATS {
        return Err(invalid("Too many saved chats in this profile"));
    }
    ids.sort_unstable();
    for id in ids {
        let (record, _) = read(&directory, &id)?;
        visit(record)?;
    }
    Ok(())
}

pub(super) fn rename(root: &Path, key: &Key, title: String) -> io::Result<Summary> {
    let directory = open(root, false)?;
    let _lock = lock(&directory)?;
    let (record, stamp) = read(&directory, &key.id)?;
    require_current(&record, key)?;
    let mut updated = record.clone();
    updated.summary.key.revision = nonce()?;
    updated.summary.title = title;
    updated.summary.updated = jiff::Timestamp::now()
        .as_millisecond()
        .max(record.summary.updated);
    write(&directory, updated, Some(&(record, stamp))).map(|record| record.summary)
}

pub(super) fn delete(root: &Path, key: &Key) -> io::Result<bool> {
    let directory = open(root, false)?;
    let _lock = lock(&directory)?;
    let (record, stamp) = read(&directory, &key.id)?;
    require_current(&record, key)?;
    directory.remove_unchanged(&filename(&key.id)?, &stamp)?;
    Ok(directory.sync().is_err())
}

pub(in crate::workspace) fn valid_title(title: &str) -> bool {
    title == title.trim() && valid_line(title, MAX_TITLE_CHARS) && !title.is_empty()
}

fn valid_line(text: &str, limit: usize) -> bool {
    text.chars().count() <= limit
        && text
            .chars()
            .all(|ch| !ch.is_control() && crate::session_journal::is_display_character(ch))
}

pub(super) fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn agent_chat_id(run_id: &str) -> io::Result<String> {
    if !valid_id(run_id) {
        return Err(invalid("Invalid terminal agent run identifier"));
    }
    let digest = fingerprint(format!("kortty saved terminal agent chat v1:{run_id}").as_bytes())?;
    let mut id = [0; 16];
    id.copy_from_slice(&digest[..16]);
    Ok(format!("{:032x}", u128::from_be_bytes(id)))
}

pub(super) fn java_id(id: &str) -> Option<String> {
    (id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        }))
    .then(|| id.replace('-', "").to_ascii_lowercase())
}

pub(super) fn valid_agent_source_run_id(id: &str) -> bool {
    valid_id(id) || java_id(id).is_some()
}

pub(super) fn valid_agent_reference(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(in crate::workspace) fn valid_response_language(code: &str) -> bool {
    let mut parts = code.split(['-', '_']);
    code.len() <= 35
        && parts.next().is_some_and(|primary| {
            (2..=8).contains(&primary.len())
                && primary.bytes().all(|byte| byte.is_ascii_alphabetic())
        })
        && parts.all(|part| {
            (1..=8).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

pub(super) fn valid_java_profile_id(id: &str) -> bool {
    java_id(id).is_some()
        || crate::clipboard_policy::valid_policy_ai_profile_id(id)
        || id.strip_prefix("profile-").is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix.len() <= 20
                && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn filename(id: &str) -> io::Result<String> {
    if !valid_id(id) {
        return Err(invalid("Invalid saved chat identifier"));
    }
    Ok(format!("{id}.json"))
}

pub(super) fn names(directory: &Directory) -> io::Result<Vec<String>> {
    Ok(directory
        .names(MAX_ENTRIES)?
        .into_iter()
        .filter_map(|name| name.into_string().ok())
        .filter_map(|name| {
            name.strip_suffix(".json")
                .filter(|id| valid_id(id))
                .map(str::to_owned)
        })
        .collect())
}

fn open(root: &Path, create: bool) -> io::Result<Directory> {
    Directory::open(root, create)?.private_child(FOLDER, create)
}

pub(super) fn lock(directory: &Directory) -> io::Result<FileLock> {
    let busy = || {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "Another window is changing saved chats. Try again.",
        )
    };
    let opened = directory.open_lock(".chats.lock");
    #[cfg(windows)]
    let opened = opened.map_err(|error| {
        const SHARING_VIOLATION: i32 = 32;
        // Private creation briefly holds an exclusive handle before the shared reopen.
        if error.raw_os_error() == Some(SHARING_VIOLATION) {
            busy()
        } else {
            error
        }
    });
    let file = opened?;
    require_single_link(&file)?;
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => busy(),
        std::fs::TryLockError::Error(error) => error,
    })?;
    Ok(FileLock::new(file))
}

fn read(directory: &Directory, id: &str) -> io::Result<(Record, FileStamp)> {
    let file = directory.open_private_file(&filename(id)?)?;
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Zeroizing::new(Vec::new());
    (&file).take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    stamp.require_unchanged(&file)?;
    let mut record = decode(&bytes, id)?;
    record.summary.key.fingerprint = fingerprint(&bytes)?;
    Ok((record, stamp))
}

pub(super) fn decode(bytes: &[u8], id: &str) -> io::Result<Record> {
    if bytes.len() > MAX_BYTES {
        return Err(invalid("Saved chat exceeds the file limit"));
    }
    let record =
        serde_json::from_slice(bytes).map_err(|_| invalid("Saved chat has an invalid format"))?;
    validate(&record, id)?;
    Ok(record)
}

fn write(
    directory: &Directory,
    record: Record,
    previous: Option<&(Record, FileStamp)>,
) -> io::Result<Record> {
    write_checked(directory, record, previous, || Ok(()))
}

pub(super) fn encode(record: &Record) -> io::Result<Zeroizing<Vec<u8>>> {
    validate(record, &record.summary.key.id)?;
    let bytes = Zeroizing::new(
        serde_json::to_vec(record).map_err(|_| invalid("Could not encode saved chat"))?,
    );
    if bytes.len() > MAX_BYTES {
        return Err(invalid("Saved chat exceeds the file limit"));
    }
    Ok(bytes)
}

fn write_checked(
    directory: &Directory,
    mut record: Record,
    previous: Option<&(Record, FileStamp)>,
    check: impl FnOnce() -> io::Result<()>,
) -> io::Result<Record> {
    let bytes = encode(&record)?;
    let name = filename(&record.summary.key.id)?;
    let fingerprint = fingerprint(&bytes)?;
    if let Some(previous) = previous {
        directory.replace_with(
            &name,
            |file| file.write_all(&bytes),
            || {
                if &read(directory, &record.summary.key.id)? != previous {
                    return Err(conflict());
                }
                check()
            },
        )?;
    } else {
        directory.publish_checked(&name, |file| file.write_all(&bytes), check)?;
    }
    // Publication already succeeded. Keep the identity so a retry updates it.
    record.summary.key.fingerprint = fingerprint;
    record.summary.sync_failed = directory.sync().is_err();
    Ok(record)
}

pub(super) fn fingerprint(bytes: &[u8]) -> io::Result<[u8; 32]> {
    HashAlgorithm::Sha256
        .digest(bytes)
        .ok()
        .and_then(|digest| digest.try_into().ok())
        .ok_or_else(|| io::Error::other("Could not fingerprint saved chat"))
}

fn validate(record: &Record, id: &str) -> io::Result<()> {
    let row = &record.summary;
    let turns = record.turns.split_first();
    if let Some(run_id) = record.agent_run_id()
        && (record.java.is_some() || agent_chat_id(run_id)? != id)
    {
        return Err(invalid("Saved chat has an invalid terminal agent origin"));
    }
    if record.agent_journal().is_some_and(|reference| {
        record.agent_run_id().is_none()
            || !valid_agent_reference(&reference.journal_id)
            || !valid_agent_reference(&reference.entry_id)
    }) {
        return Err(invalid(
            "Saved chat has an invalid terminal agent journal link",
        ));
    }
    let valid_turns = match &record.java {
        Some(origin) => {
            java_id(&origin.id).as_deref() == Some(id)
                && origin
                    .profile_id
                    .as_deref()
                    .is_none_or(valid_java_profile_id)
                && origin
                    .response_language
                    .as_deref()
                    .is_none_or(valid_response_language)
                && origin
                    .agent_run_id
                    .as_deref()
                    .is_none_or(valid_agent_source_run_id)
                && origin.agent_refs.as_ref().is_none_or(|references| {
                    origin.agent_run_id.is_some()
                        && references
                            .fields()
                            .into_iter()
                            .all(|(_, reference)| reference.is_none_or(valid_agent_reference))
                })
                && record.turns.len() <= MAX_IMPORTED_TURNS
                && turns.is_some_and(|(first, rest)| {
                    first.valid_initial()
                        && (!(origin.agent_run_id.is_some() || !first.agent_steps().is_empty())
                            || (first.is_native_pair() && record.attachment.is_none()))
                        && rest.iter().all(Turn::valid_follow_up)
                })
        }
        None => {
            row.profile != ProfileRef::Numeric(0)
                && record.turns.len() <= MAX_TURNS
                && turns.is_some_and(|(first, rest)| {
                    first.is_native_pair()
                        && first.context().is_some()
                        && first.valid_initial()
                        && rest
                            .iter()
                            .all(|turn| turn.is_native_pair() && turn.valid_follow_up())
                })
        }
    };
    if record.version != 1
        || row.key.id != id
        || !valid_id(&row.key.revision)
        || !valid_title(&row.title)
        || row.created < 0
        || row.updated < row.created
        || !valid_line(&row.profile_name, 256)
        || !valid_line(&row.connection, 120)
        || !row
            .response_language
            .as_deref()
            .is_none_or(valid_response_language)
        || !valid_turns
        || record.attachment.as_ref().is_some_and(|attachment| {
            !attachment.valid()
                || turns
                    .and_then(|(first, _)| first.context())
                    .and_then(|context| attachment.without_attachment(context))
                    .is_none()
        })
    {
        return Err(invalid(
            "Saved chat has invalid metadata or conversation text",
        ));
    }
    Ok(())
}

fn require_current(record: &Record, key: &Key) -> io::Result<()> {
    if &record.summary.key == key {
        Ok(())
    } else {
        Err(conflict())
    }
}

fn conflict() -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        "The saved chat changed. Refresh and reopen it before saving again.",
    )
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN_ID: &str = "03684aee2b4c4bcfbe6dcfe70e0d025d";

    fn agent_draft() -> Draft {
        Draft {
            profile: ProfileRef::Numeric(7),
            profile_name: "Test profile".into(),
            connection: "Test connection".into(),
            response_language: None,
            turns: vec![Turn::from_parts(
                Some("Completed agent activity"),
                "Inspect the build".into(),
                "The build passed.".into(),
            )],
            attachment: None,
        }
    }

    #[test]
    fn concurrent_agent_chat_seed_opens_one_durable_record() -> io::Result<()> {
        use std::sync::Barrier;

        let profile = crate::snippets::tests::Profile::new();
        let root = &profile.0;
        // Create the private library before racing the store lock.
        open(root, true)?;
        let barrier = Barrier::new(2);
        let seed = |title: &str| {
            barrier.wait();
            open_or_seed_agent(root, RUN_ID, title.into(), agent_draft())
        };
        let attempts = std::thread::scope(|scope| {
            let first = scope.spawn(|| seed("First seed"));
            let second = scope.spawn(|| seed("Second seed"));
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert!(attempts.iter().any(Result::is_ok));
        let current = open_or_seed_agent(root, RUN_ID, "Third seed".into(), agent_draft())?;
        for attempt in attempts {
            match attempt {
                Ok(record) => {
                    assert!(record.summary.key == current.summary.key);
                    assert_eq!(record.agent_run_id(), current.agent_run_id());
                    assert_eq!(record.summary.title, current.summary.title);
                }
                Err(error) => assert_eq!(error.kind(), io::ErrorKind::WouldBlock, "{error:?}"),
            }
        }
        assert_eq!(current.agent_run_id(), Some(RUN_ID));
        assert_ne!(current.summary.key.id, RUN_ID);
        assert_eq!(list(root)?.chats.len(), 1);
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn shared_lock_creation_reports_contention_until_the_creator_handle_closes() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let directory = open(&profile.0, true)?;
        let handle = kortty_files::tree::Directory::open_path(&profile.0.join(FOLDER))?;
        let creator = handle.create_read_write_file(std::ffi::OsStr::new(".chats.lock"))?;
        let Err(error) = lock(&directory) else {
            panic!("the creator denies a shared reopen");
        };
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock, "{error:?}");
        drop(creator);
        let _lock = lock(&directory)?;
        Ok(())
    }

    #[test]
    fn agent_chat_seed_reopens_current_revision_after_rename_and_follow_up() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = &profile.0;
        let original = open_or_seed_agent(root, RUN_ID, "Agent chat".into(), agent_draft())?;
        let renamed = rename(root, &original.summary.key, "Renamed".into())?;
        assert_eq!(load(root, &renamed.key)?.agent_run_id(), Some(RUN_ID));
        let mut draft = agent_draft();
        draft.turns.push(Turn::from_parts(
            None,
            "Which checks ran?".into(),
            "The full suite ran.".into(),
        ));
        let saved = save(root, Some(&renamed.key), renamed.title, draft)?;
        let latest = open_or_seed_agent(root, RUN_ID, "Old seed".into(), agent_draft())?;
        assert_eq!(latest.agent_run_id(), Some(RUN_ID));
        assert_eq!(latest.summary.title, "Renamed");
        assert_eq!(latest.turns.len(), 2);
        assert!(latest.summary.key == saved.key);
        assert!(latest.summary.key.same_chat(&original.summary.key));
        assert!(
            matches!(load(root, &original.summary.key), Err(error) if error.kind() == io::ErrorKind::AlreadyExists)
        );
        Ok(())
    }

    #[test]
    fn agent_chat_seed_refuses_an_existing_chat_without_the_matching_origin() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = &profile.0;
        let id = agent_chat_id(RUN_ID)?;
        let directory = open(root, true)?;
        let now = jiff::Timestamp::now().as_millisecond();
        let unrelated = write(
            &directory,
            new_record(id, "Independent chat".into(), agent_draft(), now, now)?,
            None,
        )?;
        assert!(
            matches!(open_or_seed_agent(root, RUN_ID, "Agent seed".into(), agent_draft()), Err(error) if error.kind() == io::ErrorKind::AlreadyExists)
        );
        assert!(
            matches!(open_or_seed_agent(root, "../outside", "Invalid seed".into(), agent_draft()), Err(error) if error.kind() == io::ErrorKind::InvalidData)
        );
        let current = load(root, &unrelated.summary.key)?;
        assert_eq!(current.agent_run_id(), None);
        assert_eq!(current.summary.title, unrelated.summary.title);
        Ok(())
    }

    #[test]
    fn agent_chat_seed_rechecks_authority_at_publication_and_existing_open() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = &profile.0;
        let checks = std::cell::Cell::new(0);
        let permitted = || {
            let prior = checks.get();
            checks.set(prior + 1);
            if prior == 0 {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Handoff revoked",
                ))
            }
        };
        let open_checked = || {
            open_or_seed_agent_checked(root, RUN_ID, "Agent chat".into(), agent_draft(), permitted)
        };
        assert!(matches!(open_checked(), Err(error) if error.kind() == io::ErrorKind::Interrupted));
        assert_eq!(checks.get(), 2);
        assert!(list(root)?.chats.is_empty());
        assert_eq!(
            open(root, false)?.names(MAX_ENTRIES)?,
            [std::ffi::OsString::from(".chats.lock")]
        );

        let saved = open_or_seed_agent(root, RUN_ID, "Agent chat".into(), agent_draft())?;
        checks.set(0);
        assert!(matches!(open_checked(), Err(error) if error.kind() == io::ErrorKind::Interrupted));
        assert_eq!(checks.get(), 2);
        assert_eq!(load(root, &saved.summary.key)?.agent_run_id(), Some(RUN_ID));
        Ok(())
    }

    #[test]
    fn agent_chat_link_updates_exact_revision_and_preserves_existing_chat() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = &profile.0;
        let original =
            open_or_seed_agent(root, RUN_ID, "Existing Agent chat".into(), agent_draft())?;
        let reference = AgentJournalRef {
            journal_id: "journal-fixture".into(),
            entry_id: "agent-entry".into(),
        };
        let open_linked = |reference, check: fn() -> io::Result<()>| {
            open_or_seed_agent_linked(
                root,
                RUN_ID,
                "Ignored seed".into(),
                agent_draft(),
                Some(reference),
                check,
            )
        };
        let linked = open_linked(reference.clone(), || Ok(()))?;
        assert!(linked.summary.key.same_chat(&original.summary.key));
        assert!(linked.summary.key != original.summary.key);
        assert_eq!(linked.summary.title, original.summary.title);
        assert!(linked.turns == original.turns);
        assert!(linked.agent_journal() == Some(&reference));

        let renamed = rename(root, &linked.summary.key, "Renamed Agent chat".into())?;
        let current = load_by_id(root, &renamed.key.id)?;
        assert!(current.summary.key == renamed.key);
        assert!(load_agent_run(root, RUN_ID)?.summary.key == renamed.key);
        assert!(current.agent_journal() == Some(&reference));
        let repeated = open_linked(reference.clone(), || Ok(()))?;
        assert!(repeated.summary.key == current.summary.key);

        let conflicting = AgentJournalRef {
            entry_id: "other-entry".into(),
            ..reference
        };
        assert!(
            matches!(open_linked(conflicting, || Ok(())), Err(error) if error.kind() == io::ErrorKind::AlreadyExists)
        );
        assert!(load(root, &renamed.key).is_ok());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn saved_chat_lock_releases_with_a_duplicated_descriptor() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let directory = open(&profile.0, true)?;
        let guard = lock(&directory)?;
        let inherited = guard.try_clone()?;
        assert!(
            matches!(lock(&directory), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );

        drop(guard);
        let _next = lock(&directory)?;
        drop(inherited);
        Ok(())
    }

    #[test]
    fn java_chat_import_keeps_local_changes_and_original_identity() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = &profile.0;
        let incoming = || Imported {
            title: "Java conversation".into(),
            created: 1_700_000_000_000,
            updated: 1_700_000_000_100,
            profile_name: "Java profile".into(),
            connection: "Test connection".into(),
            turns: vec![Turn::from_parts(
                None,
                String::new(),
                "Initial summary".into(),
            )],
            attachment: None,
            java: JavaOrigin {
                id: "20f88b8a-4070-47fd-a4a4-73b27b5cc3a2".into(),
                profile_id: Some("profile-42".into()),
                response_language: Some("de-DE".into()),
                agent_run_id: None,
                agent_refs: None,
            },
        };

        let added = import(root, vec![incoming(), incoming()])?;
        assert_eq!(added.duplicates, 1);
        assert_eq!(added.saved.len(), 1);
        let original = load(root, &added.saved[0].key)?;
        assert_eq!(original.summary.profile, ProfileRef::Numeric(0));
        assert_eq!(original.summary.created, incoming().created);
        assert_eq!(original.summary.updated, incoming().updated);
        assert!(original.turns == incoming().turns);
        assert!(original.java.as_ref() == Some(&incoming().java));

        let renamed = rename(root, &original.summary.key, "Local title".into())?;
        let mut turns = original.turns;
        turns.push(Turn::from_parts(
            None,
            "Can you explain?".into(),
            "The local profile answered.".into(),
        ));
        let continued = save(
            root,
            Some(&renamed.key),
            renamed.title,
            Draft {
                profile: ProfileRef::Numeric(7),
                profile_name: "Local profile".into(),
                connection: original.summary.connection,
                response_language: Some("it".into()),
                turns,
                attachment: None,
            },
        )?;

        let repeated = import(root, vec![incoming()])?;
        assert_eq!(repeated.duplicates, 1);
        assert!(repeated.saved.is_empty());
        let mut exported = Vec::new();
        with_records(root, |record| {
            exported.push(record);
            Ok(())
        })?;
        assert_eq!(exported.len(), 1);
        let current = &exported[0];
        assert!(current.summary.key == continued.key);
        assert_eq!(current.summary.title, "Local title");
        assert_eq!(current.summary.profile, ProfileRef::Numeric(7));
        assert_eq!(current.turns.len(), 2);
        assert_eq!(current.response_language(), Some("it"));
        assert!(current.java.as_ref() == Some(&incoming().java));
        Ok(())
    }

    #[test]
    fn native_chat_preserves_bounded_model_reasoning() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let turn = Turn::from_parts(
            Some("A build failed."),
            "What happened?".into(),
            "The compiler rejected the program.".into(),
        )
        .with_reasoning(Some("The terminal contains a compiler diagnostic.".into()));
        let saved = save(
            &profile.0,
            None,
            "Saved build".into(),
            Draft {
                profile: ProfileRef::Numeric(7),
                profile_name: "Test profile".into(),
                connection: String::new(),
                response_language: None,
                turns: vec![turn],
                attachment: None,
            },
        )?;
        let record = load(&profile.0, &saved.key)?;
        assert_eq!(
            record.turns[0].reasoning(),
            Some("The terminal contains a compiler diagnostic.")
        );
        Ok(())
    }

    #[test]
    fn saved_chat_rejects_an_external_edit_with_the_same_revision() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = &profile.0;
        let turns: Vec<Turn> = serde_json::from_value(serde_json::json!([{
            "question": "What happened?",
            "input": "Terminal output (untrusted data):\nA build failed.\n\nUser request:\nWhat happened?",
            "answer": "The compiler rejected the program."
        }]))
        .map_err(io::Error::other)?;
        let draft = || Draft {
            profile: ProfileRef::Numeric(7),
            profile_name: "Test profile".into(),
            connection: "Test connection".into(),
            response_language: None,
            turns: turns.clone(),
            attachment: None,
        };

        let saved = save(root, None, "Saved build".into(), draft())?;
        let record = load(root, &saved.key)?;
        let mut edited = serde_json::to_vec(&record).map_err(io::Error::other)?;
        // Preserve every JSON value, including the saved revision.
        edited.push(b'\n');
        let directory = open(root, false)?;
        directory.replace_file(&filename(&saved.key.id)?, &edited)?;

        for result in [
            load(root, &saved.key).map(|_| ()),
            save(root, Some(&saved.key), "Stale save".into(), draft()).map(|_| ()),
            rename(root, &saved.key, "Stale rename".into()).map(|_| ()),
            delete(root, &saved.key).map(|_| ()),
        ] {
            assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::AlreadyExists));
        }

        let listing = list(root)?;
        let fresh = &listing.chats[0];
        assert_eq!(fresh.key.revision, saved.key.revision);
        assert_eq!(fresh.title, saved.title);
        assert_ne!(fresh.key.fingerprint, saved.key.fingerprint);
        let renamed = rename(root, &fresh.key, "Fresh rename".into())?;
        assert_eq!(load(root, &renamed.key)?.summary.title, "Fresh rename");
        delete(root, &renamed.key)?;
        assert!(list(root)?.chats.is_empty());
        Ok(())
    }
}
