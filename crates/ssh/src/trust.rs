use std::borrow::Cow;
use std::cmp::Ordering as Comparison;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ssh_key::{Algorithm, HashAlg, PublicKey};

mod import;
pub use import::{ImportConflict, ImportReport, ImportReview, ImportSkipped, KnownHostsFile};

const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_KEY_LINE_BYTES: usize = 8192;
const DEFAULT_PORT: u16 = 22;
const TRUSTED_SINCE: &str = "kortty-trusted-at=";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// A user-visible SSH host and port. This identity is independent of DNS resolution.
#[derive(Clone, Debug)]
pub struct Endpoint {
    host: String,
    port: u16,
    absolute_dns: bool,
}

impl Endpoint {
    /// Normalizes DNS case, trailing dots, and bracketed numeric addresses.
    pub fn new(host: &str, port: u16) -> io::Result<Self> {
        let host = host.trim();
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        let absolute_dns = host.ends_with('.');
        let host = host.trim_end_matches('.');
        if host.is_empty() || host.len() > 253 || port == 0 {
            return Err(invalid("invalid SSH endpoint"));
        }
        let (host, absolute_dns) = if let Ok(address) = host.parse::<IpAddr>() {
            (address.to_string(), false)
        } else if host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
        {
            (host.to_ascii_lowercase(), absolute_dns)
        } else {
            return Err(invalid("SSH host must be an ASCII DNS name or IP address"));
        };
        Ok(Self {
            host,
            port,
            absolute_dns,
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// Host to send to the remote TCP resolver, preserving the DNS root dot.
    pub fn wire_host(&self) -> Cow<'_, str> {
        if self.absolute_dns {
            Cow::Owned(format!("{}.", self.host))
        } else {
            Cow::Borrowed(&self.host)
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    fn parse_known_host(value: &str) -> io::Result<Self> {
        if let Some((host, port)) = value
            .strip_prefix('[')
            .and_then(|host| host.split_once("]:"))
        {
            if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid("invalid known_hosts port"));
            }
            return Self::new(host, port.parse().map_err(invalid)?);
        }
        Self::new(value, DEFAULT_PORT)
    }
}

impl PartialEq for Endpoint {
    fn eq(&self, other: &Self) -> bool {
        (self.host(), self.port()) == (other.host(), other.port())
    }
}

impl Eq for Endpoint {}

impl PartialOrd for Endpoint {
    fn partial_cmp(&self, other: &Self) -> Option<Comparison> {
        Some(self.cmp(other))
    }
}

impl Ord for Endpoint {
    fn cmp(&self, other: &Self) -> Comparison {
        (self.host(), self.port()).cmp(&(other.host(), other.port()))
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.port == DEFAULT_PORT {
            self.host.fmt(f)
        } else {
            write!(f, "[{}]:{}", self.host, self.port)
        }
    }
}

/// A canonical OpenSSH public host key and its SHA-256 fingerprint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostKey(Arc<PublicKey>);

impl HostKey {
    /// Parses an OpenSSH public key, optionally followed by a comment.
    pub fn from_openssh(line: &str) -> io::Result<Self> {
        if line.len() > MAX_KEY_LINE_BYTES || line.contains(['\r', '\n', '\0']) {
            return Err(invalid("invalid SSH public key length or characters"));
        }
        let mut key = PublicKey::from_openssh(line).map_err(invalid)?;
        if !matches!(
            key.algorithm(),
            Algorithm::Ed25519 | Algorithm::Ecdsa { .. } | Algorithm::Rsa { .. }
        ) {
            return Err(invalid("unsupported SSH host key algorithm"));
        }
        key.set_comment("");
        Ok(Self(Arc::new(key)))
    }

    pub fn fingerprint(&self) -> String {
        self.0.fingerprint(HashAlg::Sha256).to_string()
    }

    /// The canonical public-key algorithm name.
    pub fn algorithm(&self) -> String {
        self.0.algorithm().as_str().to_owned()
    }

    pub fn to_openssh(&self) -> io::Result<String> {
        self.0.to_openssh().map_err(invalid)
    }
}

impl TryFrom<&sunset::PubKey<'_>> for HostKey {
    type Error = sunset::Error;

    fn try_from(key: &sunset::PubKey<'_>) -> Result<Self, Self::Error> {
        let mut wire = Vec::new();
        sunset::sshwire::ssh_push_vec(&mut wire, key)?;
        let key = PublicKey::from_bytes(&wire).map_err(|_| sunset::Error::BadKey)?;
        Ok(Self(Arc::new(key)))
    }
}

/// First-use policy. A changed key is rejected in either mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TrustMode {
    /// A new key requires an explicit pin before retrying the connection.
    #[default]
    Strict,
    /// An explicitly selected policy that pins a new key after proof of possession.
    AcceptNew,
}

/// Comparison against the pin for exactly one host and port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostKeyStatus {
    Known,
    Unknown,
    Changed { expected: HostKey },
}

/// A single-key-per-endpoint trust store.
///
/// File-backed stores use OpenSSH text in an existing private directory.
/// Cooperating processes lock the same sidecar. A snapshot can be used without
/// giving its consumer access to that directory.
#[derive(Clone, Debug)]
pub struct TrustStore {
    source: TrustSource,
    mode: TrustMode,
}

/// A trusted endpoint and its key. Older stores may lack a trust timestamp.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustedHost {
    pub endpoint: Endpoint,
    pub key: HostKey,
    /// Seconds since the Unix epoch, if recorded by kortty when the pin was approved.
    pub trusted_since: Option<u64>,
}

#[derive(Clone, Debug)]
struct Pin {
    key: HostKey,
    since: Option<u64>,
}

impl Pin {
    fn now(key: HostKey) -> Self {
        Self {
            key,
            since: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|time| time.as_secs()),
        }
    }

    fn at(&self, endpoint: &Endpoint) -> TrustedHost {
        TrustedHost {
            endpoint: endpoint.clone(),
            key: self.key.clone(),
            trusted_since: self.since,
        }
    }
}

type Pins = BTreeMap<Endpoint, Pin>;

#[derive(Clone, Debug)]
enum TrustSource {
    File(PathBuf),
    Pins(Arc<Pins>),
}

impl TrustStore {
    pub fn new(path: PathBuf, mode: TrustMode) -> Self {
        Self {
            source: TrustSource::File(path),
            mode,
        }
    }

    /// Uses an in-memory read-only snapshot under strict first-use policy.
    pub fn from_pins(pins: Vec<(Endpoint, HostKey)>) -> io::Result<Self> {
        let mut snapshot = BTreeMap::new();
        for (endpoint, key) in pins {
            if snapshot
                .insert(endpoint, Pin { key, since: None })
                .is_some()
            {
                return Err(invalid("duplicate endpoint in SSH trust snapshot"));
            }
        }
        Ok(Self {
            source: TrustSource::Pins(Arc::new(snapshot)),
            mode: TrustMode::Strict,
        })
    }

    /// Copies the available pins for these endpoints in canonical endpoint order.
    /// Reads a file-backed store under a single lock.
    pub fn selected_pins(&self, endpoints: &[Endpoint]) -> io::Result<Vec<(Endpoint, HostKey)>> {
        let pins = self.read_pins()?;
        let selected: BTreeMap<_, _> = endpoints
            .iter()
            .filter_map(|endpoint| Some((endpoint.clone(), pins.get(endpoint)?.key.clone())))
            .collect();
        Ok(selected.into_iter().collect())
    }

    /// Lists all trusted hosts in endpoint order. A damaged store returns an error.
    pub fn list(&self) -> io::Result<Vec<TrustedHost>> {
        Ok(self
            .read_pins()?
            .iter()
            .map(|(endpoint, pin)| pin.at(endpoint))
            .collect())
    }

    pub fn mode(&self) -> TrustMode {
        self.mode
    }

    pub(crate) fn strict(&self) -> Self {
        Self {
            source: self.source.clone(),
            mode: TrustMode::Strict,
        }
    }

    pub(crate) fn pinned_algorithm(&self, endpoint: &Endpoint) -> io::Result<Option<String>> {
        Ok(self
            .read_pins()?
            .get(endpoint)
            .map(|pin| pin.key.algorithm()))
    }

    /// Checks a public key without modifying its pin.
    pub fn check(&self, endpoint: &Endpoint, offered: &HostKey) -> io::Result<HostKeyStatus> {
        Ok(compare(self.read_pins()?.as_ref(), endpoint, offered))
    }

    /// Records a manually approved public key for a new endpoint.
    ///
    /// If a pin already exists it must match exactly. This makes simultaneous
    /// first-use decisions safe and never replaces an existing pin.
    pub fn remember(&self, endpoint: &Endpoint, offered: &HostKey) -> io::Result<HostKeyStatus> {
        let path = self.writable_path()?;
        let _lock = Self::lock(path)?;
        let mut pins = Self::load(path)?;
        let status = compare(&pins, endpoint, offered);
        if status == HostKeyStatus::Unknown {
            pins.insert(endpoint.clone(), Pin::now(offered.clone()));
            Self::write(path, &pins)?;
            return Ok(HostKeyStatus::Known);
        }
        Ok(status)
    }

    /// Removes an explicitly reviewed pin only if the same public key is still trusted.
    pub fn remove_pin(&self, endpoint: &Endpoint, expected: &HostKey) -> io::Result<bool> {
        self.change_pin(endpoint, expected, None)
    }

    /// Replaces a pin after explicit verification, if the reviewed old key is still trusted.
    pub fn replace_pin(
        &self,
        endpoint: &Endpoint,
        expected: &HostKey,
        verified: &HostKey,
    ) -> io::Result<bool> {
        self.change_pin(endpoint, expected, Some(verified))
    }

    fn change_pin(
        &self,
        endpoint: &Endpoint,
        expected: &HostKey,
        verified: Option<&HostKey>,
    ) -> io::Result<bool> {
        let path = self.writable_path()?;
        let _lock = Self::lock(path)?;
        let mut pins = Self::load(path)?;
        if pins.get(endpoint).is_none_or(|pin| &pin.key != expected) {
            return Ok(false);
        }
        if let Some(verified) = verified {
            pins.insert(endpoint.clone(), Pin::now(verified.clone()));
        } else {
            pins.remove(endpoint);
        }
        Self::write(path, &pins)?;
        Ok(true)
    }

    fn writable_path(&self) -> io::Result<&Path> {
        match &self.source {
            TrustSource::File(path) => Ok(path),
            TrustSource::Pins(_) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "SSH trust snapshot is read-only",
            )),
        }
    }

    fn read_pins(&self) -> io::Result<Cow<'_, Pins>> {
        match &self.source {
            TrustSource::File(path) => {
                let _lock = Self::lock(path)?;
                Ok(Cow::Owned(Self::load(path)?))
            }
            TrustSource::Pins(pins) => Ok(Cow::Borrowed(pins)),
        }
    }

    fn lock(path: &Path) -> io::Result<File> {
        let path = sidecar(path, ".lock")?;
        ensure_regular_if_present(&path)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        restrict_permissions(&mut options);
        let file = options.open(&path)?;
        if !file.metadata()?.is_file() {
            return Err(invalid("SSH trust lock must be a regular file"));
        }
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "SSH trust store is busy",
                ));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
        Ok(file)
    }

    fn load(path: &Path) -> io::Result<Pins> {
        if !ensure_regular_if_present(path)? {
            return Ok(BTreeMap::new());
        }
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            return Err(invalid(
                "SSH trust store must be a regular file up to 1 MiB",
            ));
        }
        let mut input = String::new();
        file.take(MAX_FILE_BYTES + 1).read_to_string(&mut input)?;
        Self::decode(&input)
    }

    /// Validates the native trust-store format without touching a profile or its locks.
    pub fn validate_backup(bytes: &[u8]) -> io::Result<()> {
        let input = std::str::from_utf8(bytes).map_err(invalid)?;
        Self::decode(input).map(|_| ())
    }

    fn decode(input: &str) -> io::Result<Pins> {
        if input.len() as u64 > MAX_FILE_BYTES {
            return Err(invalid("SSH trust store exceeds 1 MiB"));
        }
        let mut pins = BTreeMap::new();
        for line in input.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (host, key) = line
                .split_once(char::is_whitespace)
                .ok_or_else(|| invalid("malformed SSH trust store entry"))?;
            let endpoint = Endpoint::parse_known_host(host)?;
            let mut fields = key.split_ascii_whitespace();
            let (Some(algorithm), Some(encoded)) = (fields.next(), fields.next()) else {
                return Err(invalid("malformed SSH trust store entry"));
            };
            let since = fields
                .next()
                .and_then(|comment| comment.strip_prefix(TRUSTED_SINCE))
                .and_then(|seconds| seconds.parse().ok());
            let key = HostKey::from_openssh(&format!("{algorithm} {encoded}"))?;
            if pins.insert(endpoint, Pin { key, since }).is_some() {
                return Err(invalid("duplicate endpoint in SSH trust store"));
            }
        }
        Ok(pins)
    }

    fn encode(pins: &Pins) -> io::Result<Vec<u8>> {
        let mut contents = Vec::new();
        for (endpoint, pin) in pins {
            write!(&mut contents, "{endpoint} {}", pin.key.to_openssh()?)?;
            if let Some(since) = pin.since {
                write!(&mut contents, " {TRUSTED_SINCE}{since}")?;
            }
            contents.push(b'\n');
            if contents.len() as u64 > MAX_FILE_BYTES {
                return Err(invalid("SSH trust store exceeds 1 MiB"));
            }
        }
        Ok(contents)
    }

    fn write(path: &Path, pins: &Pins) -> io::Result<()> {
        let contents = Self::encode(pins)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        restrict_permissions(&mut options);
        let (temp_path, mut temp) = loop {
            let suffix = format!(
                ".tmp-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            );
            let path = sidecar(path, &suffix)?;
            match options.open(&path) {
                Ok(file) => break (path, file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        };
        let result = (|| {
            temp.write_all(&contents)?;
            temp.sync_all()?;
            drop(temp);
            fs::rename(&temp_path, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp_path);
        }
        result
    }
}

fn compare(pins: &Pins, endpoint: &Endpoint, offered: &HostKey) -> HostKeyStatus {
    match pins.get(endpoint) {
        None => HostKeyStatus::Unknown,
        Some(expected) if &expected.key == offered => HostKeyStatus::Known,
        Some(expected) => HostKeyStatus::Changed {
            expected: expected.key.clone(),
        },
    }
}

fn sidecar(path: &Path, suffix: &str) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| invalid("SSH trust store path requires a file name"))?;
    let mut name = OsString::from(name);
    name.push(suffix);
    Ok(path.with_file_name(name))
}

fn ensure_regular_if_present(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(invalid("SSH trust path must be a regular file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn restrict_permissions(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = options;
}

fn invalid(error: impl fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use sunset::ed25519_dalek::SigningKey;

    use super::*;

    #[test]
    fn pins_match_normalized_endpoint_and_changed_key_cannot_replace() {
        let directory = TempDir::new();
        let path = directory.0.join("pins");
        let initial = TrustStore::new(path.clone(), TrustMode::Strict);
        let endpoint = Endpoint::new(" [Server.Example.] ", 22).unwrap();
        let alias = Endpoint::new("server.example", 22).unwrap();
        assert_eq!(endpoint, alias);
        assert_eq!(endpoint.wire_host(), "server.example.");
        assert_eq!(alias.wire_host(), "server.example");
        let other_port = Endpoint::new("server.example", 2222).unwrap();
        let first = key(1);
        let changed = key(2);
        assert_ne!(first.fingerprint(), changed.fingerprint());
        assert!(first.fingerprint().starts_with("SHA256:"));
        assert_eq!(
            initial.check(&endpoint, &first).unwrap(),
            HostKeyStatus::Unknown
        );
        assert!(!path.exists());
        assert_eq!(
            initial.remember(&endpoint, &first).unwrap(),
            HostKeyStatus::Known
        );

        let reopened = TrustStore::new(path, TrustMode::AcceptNew);
        assert_eq!(
            reopened.check(&alias, &first).unwrap(),
            HostKeyStatus::Known
        );
        assert_eq!(
            reopened.check(&other_port, &first).unwrap(),
            HostKeyStatus::Unknown
        );
        assert_eq!(
            reopened.remember(&alias, &changed).unwrap(),
            HostKeyStatus::Changed {
                expected: first.clone()
            }
        );
        assert_eq!(
            reopened.check(&endpoint, &first).unwrap(),
            HostKeyStatus::Known
        );
    }

    #[test]
    fn invalid_trust_store_fails_closed_and_busy_store_cannot_write() {
        let directory = TempDir::new();
        let path = directory.0.join("pins");
        let store = TrustStore::new(path.clone(), TrustMode::AcceptNew);
        let endpoint = Endpoint::new("localhost", 2022).unwrap();
        let key = key(3);
        let lock = TrustStore::lock(&path).unwrap();
        let error = store.remember(&endpoint, &key).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        let error = store
            .selected_pins(std::slice::from_ref(&endpoint))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        drop(lock);
        let malformed = "localhost ssh-ed25519 definitely-not-a-public-key\n";
        fs::write(&path, malformed).unwrap();
        assert!(store.check(&endpoint, &key).is_err());
        assert!(store.remember(&endpoint, &key).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), malformed);
    }

    #[test]
    fn selected_pins_make_a_strict_read_only_snapshot() {
        let directory = TempDir::new();
        let path = directory.0.join("pins");
        let store = TrustStore::new(path.clone(), TrustMode::AcceptNew);
        let endpoint = Endpoint::new("server.example", 2022).unwrap();
        let alias = Endpoint::new("SERVER.EXAMPLE.", 2022).unwrap();
        let unselected = Endpoint::new("unselected.example", 2022).unwrap();
        let missing = Endpoint::new("server.example", 22).unwrap();
        let first = key(5);
        let other = key(6);
        store.remember(&endpoint, &first).unwrap();
        store.remember(&unselected, &other).unwrap();
        let pins = store
            .selected_pins(&[alias.clone(), endpoint.clone(), missing.clone()])
            .unwrap();
        assert_eq!(pins, vec![(endpoint.clone(), first.clone())]);
        let snapshot = TrustStore::from_pins(pins).unwrap().strict();
        fs::remove_file(path).unwrap();

        assert_eq!(snapshot.mode(), TrustMode::Strict);
        assert_eq!(
            snapshot.pinned_algorithm(&alias).unwrap().as_deref(),
            Some("ssh-ed25519")
        );
        assert_eq!(
            snapshot.check(&alias, &first).unwrap(),
            HostKeyStatus::Known
        );
        assert_eq!(
            snapshot.check(&endpoint, &other).unwrap(),
            HostKeyStatus::Changed {
                expected: first.clone()
            }
        );
        assert_eq!(
            snapshot.check(&unselected, &other).unwrap(),
            HostKeyStatus::Unknown
        );
        assert_eq!(
            snapshot.check(&missing, &first).unwrap(),
            HostKeyStatus::Unknown
        );
        assert_eq!(
            snapshot.remember(&missing, &first).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(TrustStore::from_pins(vec![(endpoint, first), (alias, other)]).is_err());
    }

    #[test]
    fn canonical_ssh_public_key_ignores_comment_and_requires_matching_algorithm() {
        let key = key(4);
        let line = key.to_openssh().unwrap();
        let annotated = format!("{line} this comment is ignored");
        assert_eq!(HostKey::from_openssh(&annotated).unwrap(), key);
        let mismatched = line.replacen("ssh-ed25519", "ssh-rsa", 1);
        assert!(HostKey::from_openssh(&mismatched).is_err());
        assert!(HostKey::from_openssh(&format!("{line}\n{line}")).is_err());
    }

    #[test]
    fn reviewed_import_skips_revoked_and_negated_hosts_and_keeps_existing_pins() {
        let directory = TempDir::new();
        let path = directory.0.join("pins");
        let store = TrustStore::new(path.clone(), TrustMode::Strict);
        let existing = Endpoint::new("same.example", 22).unwrap();
        let conflict = Endpoint::new("conflict.example", 22).unwrap();
        let revoked = Endpoint::new("revoked.example", 22).unwrap();
        let public = key(10);
        let replacement = key(11);
        let revoked_key = key(12);
        store.remember(&existing, &public).unwrap();
        store.remember(&conflict, &public).unwrap();
        store.remember(&revoked, &revoked_key).unwrap();

        let first = public.to_openssh().unwrap();
        let second = replacement.to_openssh().unwrap();
        let revoked_line = revoked_key.to_openssh().unwrap();
        let contents = format!(
            "\u{feff}# source\r\n\
             keep.example,deny.example,!d?ny.example,[2001:db8::1]:2200 {first}\r\n\
             same.example {first}\nconflict.example {second}\n\
             alias.example {revoked_line}\n@revoked * {revoked_line}\n\
             @cert-authority *.example {first}\n|1|hidden {first}\n\
             unsupported.example ssh-dss invalid\ninvalid.example ssh-ed25519 malformed\n",
        );
        let source = KnownHostsFile::parse(&contents).unwrap();
        let original = fs::read(&path).unwrap();
        let review = store.preview_import(source).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        let proposed = review.report();
        assert_eq!(proposed.added.len(), 2);
        assert_eq!(proposed.already_trusted, 1);
        assert_eq!(proposed.conflicts[0].endpoint, conflict);
        assert_eq!(proposed.trusted_but_revoked[0].endpoint, revoked);
        assert_eq!(proposed.skipped.patterns, 2);
        assert_eq!(proposed.skipped.malformed_lines, vec![10]);
        assert_eq!(proposed.skipped.revoked_candidates, 1);
        assert_eq!(proposed.skipped.authorities, 1);
        assert_eq!(proposed.skipped.hashed, 1);
        assert_eq!(proposed.skipped.unsupported, 1);

        let report = store.import_reviewed(&review).unwrap();
        assert_eq!(report.added.len(), 2);
        assert!(report.added.iter().all(|key| key.trusted_since.is_some()));
        assert_eq!(store.list().unwrap().len(), 5);
        assert_eq!(
            store.check(&conflict, &public).unwrap(),
            HostKeyStatus::Known
        );
        assert_eq!(
            store
                .check(&Endpoint::new("deny.example", 22).unwrap(), &public)
                .unwrap(),
            HostKeyStatus::Unknown
        );
        assert_eq!(
            store
                .check(&Endpoint::new("alias.example", 22).unwrap(), &revoked_key)
                .unwrap(),
            HostKeyStatus::Unknown
        );
    }

    #[test]
    fn reviewed_import_does_not_add_an_endpoint_that_became_available_later() {
        let directory = TempDir::new();
        let path = directory.0.join("pins");
        let store = TrustStore::new(path.clone(), TrustMode::Strict);
        let gone = Endpoint::new("gone.example", 22).unwrap();
        let new = Endpoint::new("new.example", 22).unwrap();
        let old = key(13);
        let imported = key(14);
        let concurrent = key(15);
        store.remember(&gone, &old).unwrap();
        let public = imported.to_openssh().unwrap();
        let review = store
            .preview_import(
                KnownHostsFile::parse(&format!("gone.example {public}\nnew.example {public}\n"))
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(review.report().added.len(), 1);
        assert!(store.remove_pin(&gone, &old).unwrap());
        store.remember(&new, &concurrent).unwrap();
        let unchanged = fs::read(&path).unwrap();
        let result = store.import_reviewed(&review).unwrap();
        assert!(result.added.is_empty());
        assert_eq!(result.conflicts[0].endpoint, new);
        assert_eq!(result.needs_review, 1);
        assert_eq!(fs::read(path).unwrap(), unchanged);
    }

    #[test]
    fn selected_import_preserves_unselected_new_keys_and_concurrent_local_pins() {
        let directory = TempDir::new();
        let store = TrustStore::new(directory.0.join("pins"), TrustMode::Strict);
        let endpoint = |name| Endpoint::new(name, 22).unwrap();
        let imported = key(14);
        let local = key(15);
        let formerly_conflicting = endpoint("a-conflict.example");
        let selected = endpoint("b-selected.example");
        let unselected = endpoint("c-unselected.example");
        let changed = endpoint("d-changed.example");
        store.remember(&formerly_conflicting, &local).unwrap();
        let public = imported.to_openssh().unwrap();
        let source = KnownHostsFile::parse(&format!(
            "{formerly_conflicting} {public}\n{selected} {public}\n{unselected} {public}\n{changed} {public}\n"
        )).unwrap();
        let review = store.preview_import(source).unwrap();
        assert_eq!(review.report().added[0].endpoint, selected);
        assert_eq!(review.report().added[1].endpoint, unselected);
        assert_eq!(review.report().added[2].endpoint, changed);
        assert!(store.remove_pin(&formerly_conflicting, &local).unwrap());
        store.remember(&changed, &local).unwrap();
        assert!(
            store
                .import_reviewed_selected(&review, &std::collections::BTreeSet::from([3]))
                .is_err()
        );
        let result = store
            .import_reviewed_selected(&review, &std::collections::BTreeSet::from([0, 2]))
            .unwrap();
        assert_eq!(result.added[0].endpoint, selected);
        assert_eq!(result.added.len(), 1);
        assert_eq!(result.conflicts[0].endpoint, changed);
        assert_eq!(result.needs_review, 1);
        assert_eq!(
            store.check(&unselected, &imported).unwrap(),
            HostKeyStatus::Unknown
        );
        assert_eq!(
            store.check(&formerly_conflicting, &imported).unwrap(),
            HostKeyStatus::Unknown
        );
        assert_eq!(store.check(&changed, &local).unwrap(), HostKeyStatus::Known);
    }

    #[test]
    fn stale_remove_and_replace_cannot_change_the_current_key() {
        let directory = TempDir::new();
        let path = directory.0.join("pins");
        let store = TrustStore::new(path.clone(), TrustMode::Strict);
        let host = Endpoint::new("review.example", 22).unwrap();
        let first = key(16);
        let next = key(17);
        let other = key(18);
        fs::write(
            &path,
            format!("{host} {} user comment\n", first.to_openssh().unwrap()),
        )
        .unwrap();
        assert_eq!(store.list().unwrap()[0].trusted_since, None);
        assert!(store.replace_pin(&host, &first, &next).unwrap());
        assert!(!store.remove_pin(&host, &first).unwrap());
        assert!(!store.replace_pin(&host, &first, &other).unwrap());
        assert_eq!(store.check(&host, &next).unwrap(), HostKeyStatus::Known);
        assert!(store.list().unwrap()[0].trusted_since.is_some());
        assert!(store.remove_pin(&host, &next).unwrap());
        assert_eq!(store.check(&host, &next).unwrap(), HostKeyStatus::Unknown);
    }

    fn key(test_byte: u8) -> HostKey {
        let signing_key = SigningKey::from_bytes(&[test_byte; 32]);
        let public = ssh_key::public::Ed25519PublicKey(*signing_key.verifying_key().as_bytes());
        let key = PublicKey::from(public);
        HostKey::from_openssh(&key.to_openssh().unwrap()).unwrap()
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let tick = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "kortty-ssh-trust-{}-{tick}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
