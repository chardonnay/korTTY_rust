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

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

mod envelope;
mod storage;

pub const MIN_MASTER_PASSWORD_BYTES: usize = 12;
pub const MAX_MASTER_PASSWORD_BYTES: usize = 1024;
const MAX_ENTRIES: usize = 1024;
const MAX_SECRET_BYTES: usize = 65536;
pub const MAX_ARCHIVE_PASSWORD_BYTES: usize = 1024;
pub const MAX_QDRANT_API_KEY_BYTES: usize = 4096;
pub const MAX_RECOVERY_OUTPUT_BYTES: usize = 3 * 1024 * 1024;
pub const MAX_SEALED_RECOVERY_OUTPUT_BYTES: usize =
    MAX_RECOVERY_OUTPUT_BYTES + envelope::RECOVERY_OVERHEAD_BYTES;
pub const MAX_SUDO_SCOPE_BYTES: usize = 261;
pub const MAX_SUDO_GROUP_BYTES: usize = 512;
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
    HuggingFaceToken,
    SudoPassword,
    JobWebhookUrl,
    SudoGroupPassword,
    QdrantApiKey,
    SshPasswordCommand,
}

impl CredentialKind {
    /// Whether this credential is available for SSH authentication.
    pub fn is_ssh(self) -> bool {
        matches!(self, Self::Password | Self::KeyPassphrase)
    }
}

/// Message format selected for a saved scheduler webhook.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WebhookFormat {
    /// Keeps the format in scheduler jobs created before central target settings.
    #[default]
    JobDefault,
    GenericJson,
    Slack,
    Teams,
}

impl fmt::Display for WebhookFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::JobDefault => "Use job format",
            Self::GenericJson => "Generic JSON",
            Self::Slack => "Slack",
            Self::Teams => "Microsoft Teams",
        })
    }
}

/// Delivery options authenticated alongside the destination URL.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WebhookSettings {
    pub enabled: bool,
    pub format: WebhookFormat,
    pub include_summary: bool,
}

impl Default for WebhookSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            format: WebhookFormat::JobDefault,
            include_summary: false,
        }
    }
}

/// Archive passwords are independent of SSH authentication credentials.
pub fn valid_archive_password(password: &str) -> bool {
    password.len() <= MAX_ARCHIVE_PASSWORD_BYTES && !password.trim().is_empty()
}

/// A Hub access token is bounded and safe to place in a bearer header.
pub fn valid_huggingface_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_SECRET_BYTES
        && token.bytes().all(|byte| byte.is_ascii_graphic())
}

/// A Qdrant API key must fit in its HTTP header without whitespace or control characters.
pub fn valid_qdrant_api_key(key: &str) -> bool {
    (1..=MAX_QDRANT_API_KEY_BYTES).contains(&key.len())
        && key.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Whether this value has the format of a stable credential identifier.
pub fn valid_credential_id(id: &str) -> bool {
    id.len() == ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A sudo password is sent as one line through an SSH exec channel.
pub fn valid_sudo_password(password: &str) -> bool {
    !password.is_empty() && password.len() <= 1024 && !password.chars().any(char::is_control)
}

/// Group scopes name one saved connection folder by its full path.
pub fn valid_sudo_group(group: &str) -> bool {
    group.len() <= MAX_SUDO_GROUP_BYTES
        && group.split('/').count() <= 8
        && group.split('/').all(|part| {
            (1..=96).contains(&part.len())
                && part.trim() == part
                && !matches!(part, "." | "..")
                && !part.chars().any(|character| {
                    character.is_control()
                        || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                })
        })
}

/// Splits the exact host and nonzero port used by a sudo credential.
pub fn sudo_scope(value: &str) -> Option<(&str, u16)> {
    if value.len() > MAX_SUDO_SCOPE_BYTES {
        return None;
    }
    let (host, port) = value.rsplit_once(':')?;
    let host = if let Some(ip) = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
    {
        ip.parse::<std::net::Ipv6Addr>().ok()?;
        ip
    } else {
        let name = host.trim_end_matches('.');
        if name.is_empty() || name.contains(':') || !valid_host_pattern(name, false) {
            return None;
        }
        host
    };
    if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let port = port.parse::<u16>().ok()?;
    (port != 0).then_some((host, port))
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
    webhook: WebhookSettings,
}

impl Credential {
    pub fn webhook_settings(&self) -> Option<WebhookSettings> {
        (self.kind == CredentialKind::JobWebhookUrl).then_some(self.webhook)
    }

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

/// A new credential for a prepared batch. The input ID must be absent.
pub struct CredentialAddition {
    pub input: CredentialInput,
    pub secret: Secret,
    pub webhook: Option<WebhookSettings>,
}

/// A native vault replacement prepared against one exact live vault revision.
pub struct PreparedAdditions {
    /// Generated credential IDs in input order.
    pub ids: Vec<String>,
    /// SHA-256 of the live encrypted snapshot used for preparation.
    pub before: [u8; 32],
    /// SHA-256 of the prepared encrypted snapshot.
    pub after: [u8; 32],
    /// A sealed native snapshot containing both the live and added credentials.
    pub snapshot: Zeroizing<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparedState {
    Before,
    After,
}

struct Entry {
    metadata: Credential,
    secret: Secret,
}

impl Entry {
    fn from_input(
        id: String,
        input: CredentialInput,
        secret: Secret,
        webhook: WebhookSettings,
    ) -> Result<Self, Error> {
        let entry = Self {
            metadata: Credential {
                id,
                label: input.label,
                username: input.username,
                server_pattern: input.server_pattern,
                environment: input.environment,
                description: input.description,
                kind: input.kind,
                webhook,
            },
            secret,
        };
        validate_entry(&entry)?;
        Ok(entry)
    }
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

    /// Seals one recovery payload under the unlocked vault key and its exact layout.
    pub fn seal_recovery_output(&self, layout: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, Error> {
        envelope::seal_recovery_output(&self.key, layout, plaintext)
    }

    /// Authenticates recovery output and its exact layout before returning plaintext.
    pub fn open_recovery_output(
        &self,
        layout: &[u8],
        sealed: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        envelope::open_recovery_output(&self.key, layout, sealed)
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

    /// Borrows a token explicitly stored for access to huggingface.co.
    pub fn huggingface_token(&self, id: &str) -> Option<&str> {
        self.entries.get(id).and_then(|entry| {
            (entry.metadata.kind == CredentialKind::HuggingFaceToken)
                .then_some(entry.secret.as_str())
        })
    }

    /// Borrows an API key explicitly stored for Qdrant.
    pub fn qdrant_api_key(&self, id: &str) -> Option<&str> {
        self.entries.get(id).and_then(|entry| {
            (entry.metadata.kind == CredentialKind::QdrantApiKey).then_some(entry.secret.as_str())
        })
    }

    /// Borrows a typed Qdrant key and the authenticated ciphertext that contained it.
    /// A caller retaining a proof must compare it with the current on-disk envelope.
    pub fn qdrant_api_key_with_envelope(&self, id: &str) -> Option<(&str, &[u8])> {
        self.qdrant_api_key(id)
            .map(|key| (key, self.snapshot.as_slice()))
    }

    /// Reads the bounded encrypted envelope without unlocking or decrypting it.
    pub fn encrypted_envelope(path: impl AsRef<Path>) -> Result<Vec<u8>, Error> {
        let bytes = storage::read(path.as_ref())?;
        Self::validate_backup_envelope(&bytes)?;
        Ok(bytes)
    }

    /// Borrows the destination explicitly stored for scheduler notifications.
    pub fn job_webhook_url(&self, id: &str) -> Option<&str> {
        self.entries.get(id).and_then(|entry| {
            (entry.metadata.kind == CredentialKind::JobWebhookUrl).then_some(entry.secret.as_str())
        })
    }

    /// Validates and seals one additive batch without changing the open vault or its file.
    pub fn prepare_additions(
        &self,
        additions: Vec<CredentialAddition>,
    ) -> Result<PreparedAdditions, Error> {
        if additions.is_empty() {
            return Err(Error::InvalidInput(
                "at least one vault addition is required",
            ));
        }
        if additions.len() > MAX_ENTRIES - self.entries.len() {
            return Err(Error::Limit);
        }
        self.ensure_current()?;
        let mut staged = BTreeMap::new();
        let mut ids = Vec::with_capacity(additions.len());
        for CredentialAddition {
            input,
            secret,
            webhook,
        } in additions
        {
            if input.id.is_some() {
                return Err(Error::InvalidInput("a vault addition cannot specify an ID"));
            }
            if webhook.is_some() && input.kind != CredentialKind::JobWebhookUrl {
                return Err(Error::InvalidInput(
                    "webhook options require a webhook URL credential",
                ));
            }
            let id = self.new_id(|id| staged.contains_key(id))?;
            let entry = Entry::from_input(id.clone(), input, secret, webhook.unwrap_or_default())?;
            ids.push(id.clone());
            staged.insert(id, entry);
        }
        let snapshot = Zeroizing::new(envelope::seal(
            &self.key,
            &self.salt,
            self.entries.values().chain(staged.values()),
        )?);
        Ok(PreparedAdditions {
            ids,
            before: Sha256::digest(&self.snapshot).into(),
            after: Sha256::digest(snapshot.as_slice()).into(),
            snapshot,
        })
    }

    /// Checks which revision is installed without unlocking or changing the vault.
    ///
    /// The revisions must come from a trusted record written by the preparing caller.
    /// This checks the native envelope parameters and matching salt, and compares
    /// ciphertext digests. Authenticating ciphertext still requires the vault key.
    /// A publisher must hold the same vault lock throughout its own check and write.
    pub fn verify_prepared_additions(
        path: impl AsRef<Path>,
        before: &[u8; 32],
        after: &[u8; 32],
        snapshot: &[u8],
    ) -> Result<PreparedState, Error> {
        let salt = check_prepared_snapshot(before, after, snapshot)?;
        let path = storage::absolute_destination(path.as_ref())?;
        let _lock = storage::lock(&path)?;
        prepared_state(&path, salt, before, after)
    }

    /// Publishes a sealed batch made by an unlocked vault against its exact prior revision.
    ///
    /// The caller must supply ciphertext and revisions from a trusted preparation record.
    /// An already installed snapshot is accepted on retry. The returned state describes
    /// what was installed before this call. On a durability error, publication may have
    /// succeeded and the same inputs can be retried.
    pub fn apply_prepared_additions(
        path: impl AsRef<Path>,
        before: &[u8; 32],
        after: &[u8; 32],
        snapshot: &[u8],
    ) -> Result<PreparedState, Error> {
        let salt = check_prepared_snapshot(before, after, snapshot)?;
        let path = storage::absolute_destination(path.as_ref())?;
        if !storage::exists(&path)? {
            return Err(Error::NotFound);
        }
        let _lock = storage::lock(&path)?;
        let state = prepared_state(&path, salt, before, after)?;
        if state == PreparedState::Before {
            storage::publish_checked(&path, snapshot, true, || {
                if prepared_state(&path, salt, before, after)? != PreparedState::Before {
                    return Err(Error::Changed);
                }
                Ok(())
            })?;
        }
        storage::sync_parent(&path)?;
        Ok(state)
    }

    /// Stores a credential and returns its stable ID after publication.
    pub fn save(&mut self, input: CredentialInput, secret: Secret) -> Result<String, Error> {
        let webhook = input
            .id
            .as_deref()
            .and_then(|id| self.get(id))
            .and_then(Credential::webhook_settings)
            .unwrap_or_default();
        self.save_entry(input, secret, webhook)
    }

    /// Stores webhook delivery options with the immutable destination.
    pub fn save_webhook(
        &mut self,
        input: CredentialInput,
        secret: Secret,
        webhook: WebhookSettings,
    ) -> Result<String, Error> {
        if input.kind != CredentialKind::JobWebhookUrl {
            return Err(Error::InvalidInput(
                "webhook options require a webhook URL credential",
            ));
        }
        self.save_entry(input, secret, webhook)
    }

    fn save_entry(
        &mut self,
        mut input: CredentialInput,
        secret: Secret,
        webhook: WebhookSettings,
    ) -> Result<String, Error> {
        let id = match input.id.take() {
            Some(id) => {
                let existing = self.entries.get(&id).ok_or(Error::NotFound)?;
                if input.kind != existing.metadata.kind
                    && (!input.kind.is_ssh() || !existing.metadata.kind.is_ssh())
                {
                    return Err(Error::InvalidInput(
                        "create a separate credential to change its dedicated purpose",
                    ));
                }
                if input.kind == CredentialKind::JobWebhookUrl && *secret != *existing.secret {
                    return Err(Error::InvalidInput(
                        "create a new webhook credential to change its destination",
                    ));
                }
                id
            }
            None if self.entries.len() >= MAX_ENTRIES => return Err(Error::Limit),
            None => self.new_id(|_| false)?,
        };
        let entry = Entry::from_input(id.clone(), input, secret, webhook)?;
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

    fn new_id(&self, reserved: impl Fn(&str) -> bool) -> Result<String, Error> {
        for _ in 0..8 {
            let id = hex(random::<16>()?);
            if !self.entries.contains_key(&id) && !reserved(&id) {
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

fn check_prepared_snapshot(
    before: &[u8; 32],
    after: &[u8; 32],
    snapshot: &[u8],
) -> Result<envelope::Salt, Error> {
    let salt = envelope::salt(snapshot)?;
    let prepared_revision: [u8; 32] = Sha256::digest(snapshot).into();
    if before == after || prepared_revision != *after {
        return Err(Error::InvalidFormat);
    }
    Ok(salt)
}

fn prepared_state(
    path: &Path,
    salt: envelope::Salt,
    before: &[u8; 32],
    after: &[u8; 32],
) -> Result<PreparedState, Error> {
    let current = Zeroizing::new(storage::read(path)?);
    if envelope::salt(&current)? != salt {
        return Err(Error::Changed);
    }
    let revision: [u8; 32] = Sha256::digest(&current).into();
    if revision == *before {
        Ok(PreparedState::Before)
    } else if revision == *after {
        Ok(PreparedState::After)
    } else {
        Err(Error::Changed)
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
    if !valid_credential_id(&metadata.id) {
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
    if matches!(
        metadata.kind,
        CredentialKind::SudoPassword | CredentialKind::SudoGroupPassword
    ) {
        let valid_scope = match metadata.kind {
            CredentialKind::SudoGroupPassword => valid_sudo_group(&metadata.server_pattern),
            _ => sudo_scope(&metadata.server_pattern).is_some(),
        };
        if metadata.username.is_empty()
            || metadata.username.trim() != metadata.username
            || !metadata.environment.is_empty()
            || !valid_scope
            || !valid_sudo_password(&entry.secret)
        {
            return Err(Error::InvalidInput(
                "sudo passwords require an exact account and endpoint or group, and a single-line secret of at most 1024 bytes",
            ));
        }
    } else if metadata.kind == CredentialKind::SshPasswordCommand {
        if metadata.username.is_empty()
            || metadata.username.trim() != metadata.username
            || sudo_scope(&metadata.server_pattern).is_none()
            || entry.secret.is_empty()
        {
            return Err(Error::InvalidInput(
                "SSH password commands require an exact account, endpoint and reviewed program",
            ));
        }
    } else if !valid_host_pattern(&metadata.server_pattern, true) {
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
    if metadata.kind == CredentialKind::HuggingFaceToken
        && (!metadata.username.is_empty()
            || !metadata.server_pattern.is_empty()
            || !metadata.environment.is_empty()
            || !valid_huggingface_token(&entry.secret))
    {
        return Err(Error::InvalidInput(
            "Hugging Face tokens require a non-blank ASCII value and no SSH scope",
        ));
    }
    if metadata.kind == CredentialKind::QdrantApiKey
        && (!metadata.username.is_empty()
            || !metadata.server_pattern.is_empty()
            || !metadata.environment.is_empty()
            || !valid_qdrant_api_key(&entry.secret))
    {
        return Err(Error::InvalidInput(
            "Qdrant API keys require 1 to 4096 printable ASCII bytes without spaces and no SSH scope",
        ));
    }
    if metadata.kind == CredentialKind::JobWebhookUrl
        && (!metadata.username.is_empty()
            || !metadata.server_pattern.is_empty()
            || !metadata.environment.is_empty()
            || entry.secret.len() > 4096
            || !entry.secret.starts_with("https://")
            || !entry.secret.bytes().all(|byte| byte.is_ascii_graphic()))
    {
        return Err(Error::InvalidInput(
            "webhook destinations require an HTTPS URL and no SSH scope",
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
