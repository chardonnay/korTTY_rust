//! Profile-scoped SSH identity metadata and private managed copies.

use std::collections::HashSet;
use std::fs::{File, TryLockError};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

#[cfg(test)]
mod tests;

use crate::private_files::{Directory, nonce};

pub(crate) const CATALOG: &str = "ssh-keys.json";
const LOCK: &str = "ssh-keys.json.lock";
pub(crate) const MANAGED: &str = "ssh-keys";
const MAX_RECORDS: usize = 512;
const MAX_CATALOG_BYTES: usize = 1024 * 1024;
const MAX_KEY_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum KeySource {
    External(PathBuf),
    Managed(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct KeyRecord {
    pub id: u64,
    pub name: String,
    pub description: String,
    pub source: KeySource,
    pub passphrase: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum KeyInput {
    Keep,
    External(PathBuf),
    Copy(PathBuf),
}

#[derive(Clone, Debug)]
pub(crate) struct KeyDraft {
    pub name: String,
    pub description: String,
    pub input: KeyInput,
    pub passphrase: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum Edit {
    Add(KeyDraft),
    Update(u64, KeyDraft),
    Remove(u64),
}

#[derive(Debug, Default)]
pub(crate) struct CommitReport {
    pub added: Vec<u64>,
    pub retained_copies: usize,
    pub durability_warning: bool,
}

/// Metadata for an already validated managed private key.
pub(crate) struct ManagedAddition {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) passphrase: Option<String>,
}

/// Native identity allocated before a managed key is published.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparedManagedIdentity {
    pub(crate) id: u64,
    pub(crate) token: String,
}

/// A frozen additive catalogue and the identities for its new managed files.
pub(crate) struct PreparedManagedAdditions {
    pub(crate) before: Option<[u8; 32]>,
    pub(crate) contents: Zeroizing<Vec<u8>>,
    pub(crate) additions: Vec<PreparedManagedIdentity>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    next_id: u64,
    records: Vec<KeyRecord>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: 1,
            next_id: 1,
            records: Vec::new(),
        }
    }
}

pub(crate) struct Catalog {
    root: PathBuf,
    document: Document,
}

impl Catalog {
    pub fn open(root: PathBuf) -> io::Result<Self> {
        let document = match Directory::open(&root, false) {
            Ok(directory) => read_document(&directory)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Document::default(),
            Err(error) => return Err(error),
        };
        Ok(Self { root, document })
    }

    pub fn open_profile() -> io::Result<Self> {
        Self::open(crate::profile::root()?)
    }

    pub fn records(&self) -> &[KeyRecord] {
        &self.document.records
    }

    pub fn get(&self, id: u64) -> Option<&KeyRecord> {
        self.records().iter().find(|record| record.id == id)
    }

    /// Path label from the open snapshot. Use `read_key` for authentication.
    pub fn display_path(&self, id: u64) -> Option<PathBuf> {
        self.get(id).map(|record| match &record.source {
            KeySource::External(path) => path.clone(),
            KeySource::Managed(token) => self.root.join(MANAGED).join(token),
        })
    }

    pub fn read_key(&self, expected: &KeyRecord) -> io::Result<Zeroizing<Vec<u8>>> {
        if self.get(expected.id) != Some(expected) {
            return Err(stale());
        }
        let directory = Directory::open(&self.root, false)?;
        require_current(&directory, expected)?;
        let file = match &expected.source {
            KeySource::External(path) => open_external(path)?,
            KeySource::Managed(token) => {
                let managed = directory.private_child(MANAGED, false)?;
                managed.open_private_file(token)?
            }
        };
        let key = read_key_file(file)?;
        require_current(&directory, expected)?;
        Ok(key)
    }

    /// Plans managed additions without creating a profile, files or locks.
    pub(crate) fn prepare_managed_additions(
        &self,
        additions: Vec<ManagedAddition>,
    ) -> io::Result<PreparedManagedAdditions> {
        let directory = match Directory::open(&self.root, false) {
            Ok(directory) => Some(directory),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let before = directory
            .as_ref()
            .map(read_catalog_for_restore)
            .transpose()?
            .flatten();
        let mut document = before
            .as_deref()
            .map(|bytes| decode_document(bytes))
            .transpose()?
            .unwrap_or_default();
        if document != self.document {
            return Err(stale());
        }
        if additions.is_empty() || additions.len() > MAX_RECORDS - document.records.len() {
            return Err(invalid("invalid number of managed SSH key additions"));
        }
        let mut planned = Vec::with_capacity(additions.len());
        for addition in additions {
            validate_metadata(
                &addition.name,
                &addition.description,
                addition.passphrase.as_deref(),
            )?;
            let id = document.next_id;
            document.next_id = id
                .checked_add(1)
                .ok_or_else(|| invalid("SSH key IDs are exhausted"))?;
            let token = nonce()?;
            document.records.push(KeyRecord {
                id,
                name: addition.name,
                description: addition.description,
                source: KeySource::Managed(token.clone()),
                passphrase: addition.passphrase,
            });
            planned.push(PreparedManagedIdentity { id, token });
        }
        let contents = Zeroizing::new(serde_json::to_vec(&document).map_err(io::Error::other)?);
        validate_prepared_managed_additions(
            before.as_deref().map(Vec::as_slice),
            &contents,
            &planned,
        )?;
        Ok(PreparedManagedAdditions {
            before: before.as_deref().map(|bytes| Sha256::digest(bytes).into()),
            contents,
            additions: planned,
        })
    }

    /// Errors leave the previous catalog live. Publication is the commit point.
    pub fn apply(&mut self, edits: Vec<Edit>) -> io::Result<CommitReport> {
        if edits.is_empty() {
            return Ok(CommitReport::default());
        }
        if edits.len() > MAX_RECORDS * 2 {
            return Err(invalid("too many SSH key edits"));
        }

        let directory = Directory::open(&self.root, true)?;
        let _lock = lock_for_restore(&directory)?;
        if read_document(&directory)? != self.document {
            return Err(stale());
        }

        let mut changes = Changes::new(&directory, self.document.clone());
        for edit in edits {
            changes.apply(edit)?;
        }
        let bytes = serde_json::to_vec(&changes.document).map_err(io::Error::other)?;
        if bytes.len() > MAX_CATALOG_BYTES {
            return Err(invalid("SSH key catalog exceeds the profile size limit"));
        }
        if let Some(managed) = &changes.managed {
            managed.sync()?;
        }
        directory.replace_file(CATALOG, &bytes)?;
        let (document, report) = changes.commit();
        self.document = document;
        Ok(report)
    }
}

struct Changes<'a> {
    directory: &'a Directory,
    document: Document,
    managed: Option<Directory>,
    touched: HashSet<u64>,
    added: Vec<u64>,
    staged: Vec<String>,
    retired: Vec<String>,
}

impl<'a> Changes<'a> {
    fn new(directory: &'a Directory, document: Document) -> Self {
        Self {
            directory,
            document,
            managed: None,
            touched: HashSet::new(),
            added: Vec::new(),
            staged: Vec::new(),
            retired: Vec::new(),
        }
    }

    fn apply(&mut self, edit: Edit) -> io::Result<()> {
        match edit {
            Edit::Add(draft) => {
                if self.document.records.len() == MAX_RECORDS {
                    return Err(invalid("SSH key catalog is full"));
                }
                let id = self.document.next_id;
                self.document.next_id = id
                    .checked_add(1)
                    .ok_or_else(|| invalid("SSH key IDs are exhausted"))?;
                let record = self.resolve(id, draft, None)?;
                self.document.records.push(record);
                self.added.push(id);
            }
            Edit::Update(id, draft) => {
                let index = self.existing_index(id)?;
                let previous = self.document.records[index].source.clone();
                let record = self.resolve(id, draft, Some(&previous))?;
                if record.source != previous {
                    self.retire(previous);
                }
                self.document.records[index] = record;
            }
            Edit::Remove(id) => {
                let index = self.existing_index(id)?;
                let record = self.document.records.remove(index);
                self.retire(record.source);
            }
        }
        Ok(())
    }

    fn existing_index(&mut self, id: u64) -> io::Result<usize> {
        let first_new_id = self.added.first().copied().unwrap_or(self.document.next_id);
        if id >= first_new_id || !self.touched.insert(id) {
            return Err(invalid("SSH key changes must target each existing ID once"));
        }
        self.document
            .records
            .iter()
            .position(|record| record.id == id)
            .ok_or_else(missing_record)
    }

    fn resolve(
        &mut self,
        id: u64,
        draft: KeyDraft,
        previous: Option<&KeySource>,
    ) -> io::Result<KeyRecord> {
        validate_metadata(&draft.name, &draft.description, draft.passphrase.as_deref())?;
        let source = match draft.input {
            KeyInput::Keep => previous
                .cloned()
                .ok_or_else(|| invalid("an SSH key file is required"))?,
            KeyInput::External(path) => {
                let file = open_external(&path)?;
                check_key_size(&file, 1)?;
                KeySource::External(path)
            }
            KeyInput::Copy(path) => {
                let (source, name) = external_parent(&path)?;
                let key = read_key_file(source.open_file(name)?)?;
                let public_key = match source.open_file(&public_name(name)) {
                    Ok(file) => Some(read_bounded_key_file(file, 0)?),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error),
                };
                let token = nonce()?;
                let directory = match &mut self.managed {
                    Some(directory) => directory,
                    slot => slot.insert(self.directory.private_child(MANAGED, true)?),
                };
                let public_token = public_name(&token);
                match directory.open_file(&public_token) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            "managed SSH public key already exists",
                        ));
                    }
                }
                directory.publish_copy(&token, &key)?;
                self.staged.push(token.clone());
                if let Some(public_key) = public_key {
                    directory.publish_copy(&public_token, &public_key)?;
                    self.staged.push(public_token);
                }
                KeySource::Managed(token)
            }
        };
        Ok(KeyRecord {
            id,
            name: draft.name,
            description: draft.description,
            source,
            passphrase: draft.passphrase,
        })
    }

    fn retire(&mut self, source: KeySource) {
        if let KeySource::Managed(token) = source {
            self.retired.push(token);
        }
    }

    fn commit(mut self) -> (Document, CommitReport) {
        self.staged.clear();
        let mut report = CommitReport {
            added: std::mem::take(&mut self.added),
            durability_warning: self.directory.sync().is_err(),
            ..CommitReport::default()
        };
        if report.durability_warning {
            // A crash could still restore the prior catalog, which references these copies.
            report.retained_copies = self.retired.len();
        } else if !self.retired.is_empty() {
            match self.directory.private_child(MANAGED, false) {
                Ok(managed) => {
                    for token in &self.retired {
                        for name in [token.as_str(), &public_name(token)] {
                            match managed.remove_copy(name) {
                                Ok(()) => {}
                                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                                Err(_) => report.retained_copies += 1,
                            }
                        }
                    }
                    report.durability_warning |= managed.sync().is_err();
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => report.retained_copies = self.retired.len(),
            }
        }
        (std::mem::take(&mut self.document), report)
    }
}

impl Drop for Changes<'_> {
    fn drop(&mut self) {
        if let Some(directory) = &self.managed {
            for token in &self.staged {
                let _ = directory.remove_copy(token);
            }
        }
    }
}

struct StoreLock(File);

impl StoreLock {
    fn acquire(file: File) -> io::Result<Self> {
        file.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                "SSH key catalog is being saved by another instance",
            ),
            TryLockError::Error(error) => error,
        })?;
        Ok(Self(file))
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub(crate) fn collect_backup(
    root: &Path,
    mut include: impl FnMut(String, Zeroizing<Vec<u8>>) -> io::Result<()>,
) -> io::Result<()> {
    let directory = match Directory::open(root, false) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let Some(document) = read_document_if_present(&directory)? else {
        return Ok(());
    };
    let mut managed = None;
    for record in &document.records {
        let KeySource::Managed(name) = &record.source else {
            continue;
        };
        let managed = match &mut managed {
            Some(managed) => managed,
            slot => slot.insert(directory.private_child(MANAGED, false)?),
        };
        for (name, optional) in [(name.clone(), false), (public_name(name), true)] {
            let file = match managed.open_private_file(&name) {
                Ok(file) => file,
                Err(error) if optional && error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let bytes = read_bounded_key_file(file, usize::from(!optional))?;
            include(format!("{MANAGED}/{name}"), bytes)?;
        }
    }
    if read_document_if_present(&directory)?.as_ref() != Some(&document) {
        return Err(stale());
    }
    let bytes = serde_json::to_vec(&document).map_err(io::Error::other)?;
    include(CATALOG.into(), Zeroizing::new(bytes))
}

pub(crate) fn backup_managed_names(bytes: &[u8]) -> io::Result<Vec<String>> {
    let document = decode_document(bytes)?;
    Ok(document
        .records
        .into_iter()
        .filter_map(|record| match record.source {
            KeySource::Managed(name) => Some(name),
            KeySource::External(_) => None,
        })
        .collect())
}

pub(crate) fn is_backup_managed_name(name: &str) -> bool {
    valid_token(name.strip_suffix(".pub").unwrap_or(name))
}

fn read_document(directory: &Directory) -> io::Result<Document> {
    Ok(read_document_if_present(directory)?.unwrap_or_default())
}

fn read_document_if_present(directory: &Directory) -> io::Result<Option<Document>> {
    read_catalog_for_restore(directory)?
        .map(|bytes| decode_document(&bytes))
        .transpose()
}

/// Reads the exact bounded bytes used to compare catalogue revisions.
pub(crate) fn read_catalog_for_restore(
    directory: &Directory,
) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    let file = match directory.open_private_file(CATALOG) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_CATALOG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err(invalid("SSH key catalog exceeds the profile size limit"));
    }
    Ok(Some(bytes))
}

/// Uses the same lock as ordinary key catalogue edits.
pub(crate) fn lock_for_restore(directory: &Directory) -> io::Result<impl Drop> {
    StoreLock::acquire(directory.open_lock(LOCK)?)
}

/// Validates the candidate and its exact appended managed identity sequence.
/// Use this after a committing plan has already installed the catalogue.
pub(crate) fn validate_prepared_managed_paths(
    contents: &[u8],
    additions: &[PreparedManagedIdentity],
    credential_ids: &[String],
) -> io::Result<()> {
    let document = decode_document(contents)?;
    let (existing, _) = require_prepared_managed_tail(&document, additions)?;
    if document.records[existing.len()..].iter().any(|record| {
        record
            .passphrase
            .as_ref()
            .is_some_and(|id| !credential_ids.contains(id))
    }) {
        return Err(invalid(
            "prepared SSH key passphrase is missing from the vault additions",
        ));
    }
    Ok(())
}

/// Validates a prepared catalogue against the exact original document.
pub(crate) fn validate_prepared_managed_additions(
    before: Option<&[u8]>,
    contents: &[u8],
    additions: &[PreparedManagedIdentity],
) -> io::Result<()> {
    let current = before.map(decode_document).transpose()?.unwrap_or_default();
    let candidate = decode_document(contents)?;
    let (existing, first_id) = require_prepared_managed_tail(&candidate, additions)?;
    if candidate.version != current.version
        || existing != current.records
        || first_id != current.next_id
    {
        return Err(invalid(
            "prepared SSH key catalog does not preserve its original records",
        ));
    }
    Ok(())
}

fn require_prepared_managed_tail<'a>(
    document: &'a Document,
    additions: &[PreparedManagedIdentity],
) -> io::Result<(&'a [KeyRecord], u64)> {
    if additions.is_empty() {
        return Err(invalid("prepared SSH key catalog has no additions"));
    }
    let prefix_len = document
        .records
        .len()
        .checked_sub(additions.len())
        .ok_or_else(|| invalid("prepared SSH key catalog is missing additions"))?;
    let (existing, appended) = document.records.split_at(prefix_len);
    let first_id = document
        .next_id
        .checked_sub(additions.len() as u64)
        .filter(|id| *id != 0)
        .ok_or_else(|| invalid("prepared SSH key IDs are invalid"))?;
    for (offset, (record, addition)) in appended.iter().zip(additions).enumerate() {
        if record.id != first_id + offset as u64
            || record.id != addition.id
            || !matches!(&record.source, KeySource::Managed(token) if token == &addition.token)
        {
            return Err(invalid(
                "prepared SSH key does not match its managed identity",
            ));
        }
    }
    Ok((existing, first_id))
}

fn decode_document(bytes: &[u8]) -> io::Result<Document> {
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err(invalid("SSH key catalog exceeds the profile size limit"));
    }
    let document: Document = serde_json::from_slice(bytes).map_err(invalid)?;
    if document.version != 1 || document.next_id == 0 || document.records.len() > MAX_RECORDS {
        return Err(invalid("unsupported or invalid SSH key catalog"));
    }
    let mut ids = HashSet::new();
    let mut tokens = HashSet::new();
    for record in &document.records {
        if record.id == 0 || record.id >= document.next_id || !ids.insert(record.id) {
            return Err(invalid("invalid or duplicate SSH key ID"));
        }
        validate_metadata(
            &record.name,
            &record.description,
            record.passphrase.as_deref(),
        )?;
        match &record.source {
            KeySource::External(path) => validate_path(path)?,
            KeySource::Managed(token) if valid_token(token) && tokens.insert(token) => {}
            KeySource::Managed(_) => return Err(invalid("invalid or duplicate managed SSH key")),
        }
    }
    Ok(document)
}

fn require_current(directory: &Directory, expected: &KeyRecord) -> io::Result<()> {
    if read_document(directory)?
        .records
        .iter()
        .any(|record| record == expected)
    {
        Ok(())
    } else {
        Err(stale())
    }
}

fn validate_metadata(name: &str, description: &str, passphrase: Option<&str>) -> io::Result<()> {
    if name.trim().is_empty()
        || name.len() > 256
        || description.len() > 4096
        || name.chars().any(char::is_control)
        || description
            .chars()
            .any(|value| value.is_control() && !matches!(value, '\n' | '\r' | '\t'))
    {
        return Err(invalid("invalid SSH key name or description"));
    }
    if passphrase.is_some_and(|id| !valid_token(id)) {
        return Err(invalid("invalid SSH key passphrase credential ID"));
    }
    Ok(())
}

fn validate_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || path.file_name().is_none()
        || path
            .to_str()
            .is_none_or(|path| path.len() > 4096 || path.chars().any(char::is_control))
        || path.components().any(|part| part == Component::ParentDir)
    {
        return Err(invalid("enter an absolute, ordinary SSH key file path"));
    }
    Ok(())
}

fn open_external(path: &Path) -> io::Result<File> {
    let (parent, name) = external_parent(path)?;
    parent.open_file(name)
}

fn external_parent(path: &Path) -> io::Result<(Directory, &str)> {
    validate_path(path)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("invalid SSH key filename"))?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid("SSH key has no directory"))?;
    Ok((Directory::open(parent, false)?, name))
}

fn check_key_size(file: &File, minimum: usize) -> io::Result<usize> {
    let size = file.metadata()?.len();
    if !(minimum as u64..=MAX_KEY_BYTES as u64).contains(&size) {
        return Err(invalid("SSH key file has an invalid size"));
    }
    Ok(size as usize)
}

fn read_key_file(file: File) -> io::Result<Zeroizing<Vec<u8>>> {
    read_bounded_key_file(file, 1)
}

fn read_bounded_key_file(mut file: File, minimum: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(vec![0; check_key_size(&file, minimum)?]);
    file.read_exact(&mut bytes)?;
    // Read without reallocating the key buffer if the file grew after inspection.
    let mut trailing = Zeroizing::new([0; 1]);
    if file.read(&mut *trailing)? != 0 {
        return Err(invalid("SSH key file changed size while being read"));
    }
    Ok(bytes)
}

fn public_name(name: &str) -> String {
    format!("{name}.pub")
}

fn valid_token(token: &str) -> bool {
    token.len() == 32
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn missing_record() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "SSH key was removed from the catalog",
    )
}

fn stale() -> io::Error {
    io::Error::other("SSH key catalog changed; reopen it and try again")
}

fn invalid(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
