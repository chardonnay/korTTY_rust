//! Native encrypted credential storage for a private application profile.
//!
//! An unlocked vault keeps an exclusive operating-system file lock until it is dropped.
//! Key derivation and file operations are synchronous and should run outside the UI thread.
//!
//! The v1 file begins with the `korttvlt` magic, the version byte, three big-endian
//! Argon2id costs (64 MiB, three passes, four lanes), a 16-byte salt and a 24-byte
//! nonce. The complete header is authenticated as associated data. XChaCha20-Poly1305
//! encrypts the credential records and appends its 16-byte authentication tag.
//! Size limits are checked before key derivation. See [RFC 9106 §7.4] and [RustCrypto].
//!
//! A backup is an independent encrypted snapshot using the password that was current
//! when it was created. Authentication detects modification of a snapshot. An older
//! valid snapshot can be restored by someone with write access to the profile.
//!
//! [RFC 9106 §7.4]: https://www.rfc-editor.org/rfc/rfc9106.html#section-7.4
//! [RustCrypto]: https://docs.rs/chacha20poly1305/0.10.1/chacha20poly1305/

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

mod envelope;
mod storage;

pub const MIN_MASTER_PASSWORD_BYTES: usize = 12;
pub const MAX_MASTER_PASSWORD_BYTES: usize = 1024;
const MAX_ENTRIES: usize = 1024;
const MAX_SECRET_BYTES: usize = 65536;
pub const MAX_ARCHIVE_PASSWORD_BYTES: usize = 1024;
const ID_BYTES: usize = 32;
const MAX_LABEL_BYTES: usize = 256;
const MAX_USERNAME_BYTES: usize = 256;
const MAX_PATTERN_BYTES: usize = 253;
const MAX_ENVIRONMENT_BYTES: usize = 128;
const MAX_DESCRIPTION_BYTES: usize = 2048;

/// An owned secret that is wiped on drop.
pub type Secret = Zeroizing<String>;

/// Stretches a dedicated profile archive password before it is passed to ZIP encryption.
/// The caller generates a new random salt for each archive and persists it with the archive.
pub fn derive_backup_password(password: &Secret, salt: &[u8; 16]) -> Result<Secret, Error> {
    use std::fmt::Write as _;

    let key = envelope::derive_key(password.as_bytes(), salt)?;
    let mut encoded = Secret::new(String::with_capacity(key.len() * 2));
    for byte in key.iter() {
        write!(&mut *encoded, "{byte:02x}").map_err(|_| Error::Crypto)?;
    }
    Ok(encoded)
}

#[derive(Debug)]
pub enum Error {
    AlreadyExists,
    NotFound,
    Busy,
    Changed,
    Authentication,
    InvalidFormat,
    UnsupportedVersion,
    InvalidInput(&'static str),
    Limit,
    Crypto,
    /// The operation was published, but syncing the destination directory failed.
    Durability(io::Error),
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyExists => formatter.write_str("the destination file already exists"),
            Self::NotFound => formatter.write_str("the vault or credential does not exist"),
            Self::Busy => formatter.write_str("the vault is already open"),
            Self::Changed => formatter.write_str("the vault file changed while unlocked"),
            Self::Authentication => {
                formatter.write_str("incorrect master password or damaged vault")
            }
            Self::InvalidFormat => formatter.write_str("invalid vault file"),
            Self::UnsupportedVersion => formatter.write_str("unsupported vault file version"),
            Self::InvalidInput(message) => formatter.write_str(message),
            Self::Limit => formatter.write_str("the vault exceeds its size or resource limit"),
            Self::Crypto => formatter.write_str("could not derive or use the vault encryption key"),
            Self::Durability(error) => {
                write!(formatter, "vault saved, but directory sync failed: {error}")
            }
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) | Self::Durability(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CredentialKind {
    #[default]
    Password,
    KeyPassphrase,
    ArchivePassword,
}

/// Archive passwords are independent of SSH authentication credentials.
pub fn valid_archive_password(password: &str) -> bool {
    password.len() <= MAX_ARCHIVE_PASSWORD_BYTES && !password.trim().is_empty()
}

/// Display and matching metadata for an unlocked credential.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Credential {
    pub id: String,
    pub label: String,
    pub username: String,
    pub server_pattern: String,
    pub environment: String,
    pub description: String,
    pub kind: CredentialKind,
}

impl Credential {
    /// Matches an ASCII hostname or address against the optional case-insensitive `*` pattern.
    pub fn matches_host(&self, host: &str) -> bool {
        let host = host.strip_suffix('.').unwrap_or(host);
        if host.is_empty() || !valid_host_pattern(host, false) {
            return false;
        }
        let pattern = self
            .server_pattern
            .strip_suffix('.')
            .unwrap_or(&self.server_pattern);
        if pattern.is_empty() {
            return self.server_pattern.is_empty();
        }
        valid_host_pattern(pattern, true) && wildcard_matches(pattern.as_bytes(), host.as_bytes())
    }
}

/// Editable metadata. An absent ID creates a credential; an existing ID updates it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CredentialInput {
    pub id: Option<String>,
    pub label: String,
    pub username: String,
    pub server_pattern: String,
    pub environment: String,
    pub description: String,
    pub kind: CredentialKind,
}

struct Entry {
    metadata: Credential,
    secret: Secret,
}

/// An authenticated, unlocked vault. Dropping it wipes the key and owned secrets.
pub struct Vault {
    entries: BTreeMap<String, Entry>,
    key: envelope::Key,
    salt: envelope::Salt,
    snapshot: Vec<u8>,
    path: PathBuf,
    _lock: storage::Lock,
}

impl Vault {
    /// Checks the profile path without unlocking it or creating files.
    pub fn exists(path: impl AsRef<Path>) -> Result<bool, Error> {
        storage::exists(path.as_ref())
    }

    /// Creates a new vault without replacing a file at the destination.
    pub fn create(path: impl AsRef<Path>, master: &[u8]) -> Result<Self, Error> {
        validate_new_master(master)?;
        let path = storage::absolute_destination(path.as_ref())?;
        let lock = storage::lock(&path)?;
        if storage::exists(&path)? {
            return Err(Error::AlreadyExists);
        }
        let salt = random()?;
        let key = envelope::derive_key(master, &salt)?;
        let snapshot = envelope::seal(&key, &salt, std::iter::empty())?;
        storage::publish(&path, &snapshot, false)?;
        storage::sync_parent(&path)?;
        Ok(Self {
            entries: BTreeMap::new(),
            key,
            salt,
            snapshot,
            path,
            _lock: lock,
        })
    }

    /// Verifies the master password and authenticates the full snapshot before exposing entries.
    pub fn unlock(path: impl AsRef<Path>, master: &[u8]) -> Result<Self, Error> {
        let path = storage::absolute_destination(path.as_ref())?;
        let lock = storage::lock(&path)?;
        let snapshot = storage::read(&path)?;
        let (salt, key, entries) = envelope::authenticate(&snapshot, master)?;
        Ok(Self {
            entries,
            key,
            salt,
            snapshot,
            path,
            _lock: lock,
        })
    }

    /// Authenticates a backup and installs it as the unlocked vault of an empty profile.
    /// The destination's parent must exist. An existing vault is never replaced.
    pub fn restore_from(
        destination: impl AsRef<Path>,
        backup: impl AsRef<Path>,
        master: &[u8],
    ) -> Result<Self, Error> {
        let path = storage::absolute_destination(destination.as_ref())?;
        if storage::exists(&path)? {
            return Err(Error::AlreadyExists);
        }
        let snapshot = storage::read(backup.as_ref())?;
        let (salt, key, entries) = envelope::authenticate(&snapshot, master)?;
        let lock = storage::lock(&path)?;
        storage::publish(&path, &snapshot, false)?;
        storage::sync_parent(&path)?;
        Ok(Self {
            entries,
            key,
            salt,
            snapshot,
            path,
            _lock: lock,
        })
    }

    /// Releases the file lock and wipes the owned key and secrets.
    pub fn lock(self) {
        drop(self);
    }

    pub fn entries(&self) -> impl ExactSizeIterator<Item = &Credential> {
        self.entries.values().map(|entry| &entry.metadata)
    }

    pub fn get(&self, id: &str) -> Option<&Credential> {
        self.entries.get(id).map(|entry| &entry.metadata)
    }

    /// Borrows a secret for immediate use. The vault retains ownership of the buffer.
    pub fn secret(&self, id: &str) -> Option<&str> {
        self.entries.get(id).map(|entry| entry.secret.as_str())
    }

    /// Borrows a password explicitly stored for encrypting archives.
    pub fn archive_password(&self, id: &str) -> Option<&str> {
        self.entries.get(id).and_then(|entry| {
            (entry.metadata.kind == CredentialKind::ArchivePassword)
                .then_some(entry.secret.as_str())
        })
    }

    /// Stores a credential and returns its stable ID after publication.
    pub fn save(&mut self, input: CredentialInput, secret: Secret) -> Result<String, Error> {
        let id = match input.id {
            Some(id) => {
                let existing = self.entries.get(&id).ok_or(Error::NotFound)?;
                let archive = CredentialKind::ArchivePassword;
                if input.kind != existing.metadata.kind
                    && (input.kind == archive || existing.metadata.kind == archive)
                {
                    return Err(Error::InvalidInput(
                        "create a separate credential to change the archive password type",
                    ));
                }
                id
            }
            None if self.entries.len() >= MAX_ENTRIES => return Err(Error::Limit),
            None => self.new_id()?,
        };
        let entry = Entry {
            metadata: Credential {
                id: id.clone(),
                label: input.label,
                username: input.username,
                server_pattern: input.server_pattern,
                environment: input.environment,
                description: input.description,
                kind: input.kind,
            },
            secret,
        };
        validate_entry(&entry)?;
        let updated = self
            .entries
            .values()
            .filter(|existing| existing.metadata.id != id);
        let snapshot = envelope::seal(&self.key, &self.salt, updated.chain([&entry]))?;
        self.publish(snapshot)?;
        self.entries.insert(id.clone(), entry);
        storage::sync_parent(&self.path)?;
        Ok(id)
    }

    pub fn delete(&mut self, id: &str) -> Result<bool, Error> {
        if !self.entries.contains_key(id) {
            return Ok(false);
        }
        let retained = self
            .entries
            .values()
            .filter(|entry| entry.metadata.id != id);
        let snapshot = envelope::seal(&self.key, &self.salt, retained)?;
        self.publish(snapshot)?;
        self.entries.remove(id);
        storage::sync_parent(&self.path)?;
        Ok(true)
    }

    /// Verifies the old password and replaces the envelope under a newly salted key.
    pub fn change_master_password(&mut self, old: &[u8], new: &[u8]) -> Result<(), Error> {
        validate_new_master(new)?;
        self.ensure_current()?;
        let old_key = envelope::derive_key(old, &self.salt)?;
        envelope::open(&self.snapshot, &old_key)?;
        let salt = random()?;
        let key = envelope::derive_key(new, &salt)?;
        let snapshot = envelope::seal(&key, &salt, self.entries.values())?;
        self.publish(snapshot)?;
        self.salt = salt;
        self.key = key;
        storage::sync_parent(&self.path)
    }

    /// Copies the current encrypted envelope to a new file. It remains independently unlockable.
    pub fn backup_to(&self, path: impl AsRef<Path>) -> Result<(), Error> {
        self.ensure_current()?;
        let path = storage::absolute_destination(path.as_ref())?;
        storage::publish(&path, &self.snapshot, false)?;
        storage::sync_parent(&path)
    }

    /// Checks the format and bounded parameters of an encrypted vault snapshot.
    /// Authentication and decryption still require its master password.
    pub fn validate_backup_envelope(bytes: &[u8]) -> Result<(), Error> {
        envelope::salt(bytes).map(|_| ())
    }

    fn new_id(&self) -> Result<String, Error> {
        for _ in 0..8 {
            let id = hex(random::<16>()?);
            if !self.entries.contains_key(&id) {
                return Ok(id);
            }
        }
        Err(Error::Crypto)
    }

    fn publish(&mut self, snapshot: Vec<u8>) -> Result<(), Error> {
        self.ensure_current()?;
        storage::publish(&self.path, &snapshot, true)?;
        self.snapshot = snapshot;
        Ok(())
    }

    fn ensure_current(&self) -> Result<(), Error> {
        match storage::read(&self.path) {
            Ok(snapshot) if snapshot == self.snapshot => Ok(()),
            Ok(_) | Err(Error::NotFound | Error::InvalidFormat | Error::Limit) => {
                Err(Error::Changed)
            }
            Err(error) => Err(error),
        }
    }
}

fn validate_new_master(master: &[u8]) -> Result<(), Error> {
    if !(MIN_MASTER_PASSWORD_BYTES..=MAX_MASTER_PASSWORD_BYTES).contains(&master.len()) {
        return Err(Error::InvalidInput(
            "master password must contain 12 to 1024 bytes",
        ));
    }
    Ok(())
}

fn validate_entry(entry: &Entry) -> Result<(), Error> {
    let metadata = &entry.metadata;
    if metadata.id.len() != ID_BYTES
        || !metadata
            .id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Error::InvalidInput("invalid credential ID"));
    }
    if metadata.label.trim().is_empty() {
        return Err(Error::InvalidInput("credential label is required"));
    }
    for (value, limit) in [
        (&metadata.label, MAX_LABEL_BYTES),
        (&metadata.username, MAX_USERNAME_BYTES),
        (&metadata.environment, MAX_ENVIRONMENT_BYTES),
    ] {
        if value.len() > limit || value.chars().any(char::is_control) {
            return Err(Error::InvalidInput(
                "credential metadata is too long or contains control characters",
            ));
        }
    }
    if metadata.description.len() > MAX_DESCRIPTION_BYTES
        || metadata
            .description
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(Error::InvalidInput("invalid credential description"));
    }
    if !valid_host_pattern(&metadata.server_pattern, true) {
        return Err(Error::InvalidInput("invalid credential server pattern"));
    }
    if metadata.kind == CredentialKind::ArchivePassword
        && (!metadata.username.is_empty()
            || !metadata.server_pattern.is_empty()
            || !valid_archive_password(&entry.secret))
    {
        return Err(Error::InvalidInput(
            "archive passwords require a non-blank value of at most 1024 bytes and no SSH scope",
        ));
    }
    if entry.secret.len() > MAX_SECRET_BYTES {
        return Err(Error::Limit);
    }
    Ok(())
}

fn valid_host_pattern(pattern: &str, wildcard: bool) -> bool {
    pattern.len() <= MAX_PATTERN_BYTES
        && pattern.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'-' | b':' | b'_')
                || (wildcard && byte == b'*')
        })
}

fn wildcard_matches(pattern: &[u8], host: &[u8]) -> bool {
    let (mut expected, mut actual) = (0, 0);
    let mut star = None;
    while let Some(&byte) = host.get(actual) {
        match pattern.get(expected) {
            Some(b'*') => {
                star = Some((expected + 1, actual));
                expected += 1;
            }
            Some(literal) if literal.eq_ignore_ascii_case(&byte) => {
                expected += 1;
                actual += 1;
            }
            _ => {
                let Some((after_star, consumed)) = &mut star else {
                    return false;
                };
                *consumed += 1;
                actual = *consumed;
                expected = *after_star;
            }
        }
    }
    pattern[expected..].iter().all(|&byte| byte == b'*')
}

fn random<const N: usize>() -> Result<[u8; N], Error> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|_| Error::Crypto)?;
    Ok(bytes)
}

fn hex<const N: usize>(bytes: [u8; N]) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(N * 2);
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}
