//! Device-local backup configuration and successful-run history.

use std::fmt;
use std::fs::{File, TryLockError};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::private_files::Directory;

const SETTINGS: &str = "backup-settings.json";
const PROGRESS: &str = "backup-progress.json";
const OPERATION_LOCK: &str = ".profile-backup.lock";
const VERSION: u8 = 1;
const MAX_DOCUMENT_BYTES: u64 = 16 * 1024;
const MAX_PATH_BYTES: usize = 4096;

pub(crate) const MAX_RETENTION: u8 = 100;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Interval {
    Off,
    Daily,
    #[default]
    Weekly,
    Monthly,
}

impl Interval {
    pub(crate) const ALL: [Self; 4] = [Self::Off, Self::Daily, Self::Weekly, Self::Monthly];

    pub(crate) fn from_id(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            "monthly" => Some(Self::Monthly),
            _ => None,
        }
    }
}

impl fmt::Display for Interval {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Off => "Off",
            Self::Daily => "Daily",
            Self::Weekly => "Weekly",
            Self::Monthly => "Monthly",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum EncryptionMode {
    #[default]
    Password,
    OpenPgp,
}

impl EncryptionMode {
    pub(crate) const ALL: [Self; 2] = [Self::Password, Self::OpenPgp];
}

impl fmt::Display for EncryptionMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Password => "ZIP with Password · AES-256",
            Self::OpenPgp => "OpenPGP",
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Preferences {
    pub retention: u8,
    pub credential: Option<String>,
    #[serde(default)]
    pub encryption: EncryptionMode,
    #[serde(default)]
    pub recipient: Option<String>,
    pub interval: Interval,
    pub directory: Option<PathBuf>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            retention: 10,
            credential: None,
            encryption: EncryptionMode::Password,
            recipient: None,
            interval: Interval::Weekly,
            directory: None,
        }
    }
}

impl Preferences {
    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.retention > MAX_RETENTION {
            return Err(invalid("Keep between 0 and 100 older backups."));
        }
        if self.credential.as_ref().is_some_and(|id| {
            id.len() != 32
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            return Err(invalid("Invalid backup archive password reference."));
        }
        if let Some(directory) = &self.directory {
            validate_directory(directory)?;
        }
        if self.recipient.as_ref().is_some_and(|fingerprint| {
            !matches!(fingerprint.len(), 40 | 64)
                || !fingerprint
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
        }) {
            return Err(invalid("Invalid backup OpenPGP recipient fingerprint."));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Progress {
    pub last_backup: Option<Timestamp>,
    pub last_auto: Option<Timestamp>,
    pub last_directory: Option<PathBuf>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SettingsDocument {
    version: u8,
    settings: Preferences,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProgressDocument {
    version: u8,
    last_backup: Option<String>,
    last_auto: Option<String>,
    last_directory: Option<PathBuf>,
}

pub(crate) fn load(root: &Path) -> io::Result<(Preferences, Progress)> {
    let directory = Directory::open(root, false)?;
    Ok((read_preferences(&directory)?, read_progress(&directory)?))
}

pub(crate) fn save(root: &Path, current: &Preferences, draft: Preferences) -> io::Result<bool> {
    draft.validate()?;
    let operation = Operation::acquire(root)?;
    if operation.preferences()? != *current {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "The backup settings changed in another instance. Reopen Settings and try again.",
        ));
    }
    publish(
        &operation.directory,
        SETTINGS,
        &SettingsDocument {
            version: VERSION,
            settings: draft,
        },
    )
}

/// Serializes configured backups, configuration changes, and success stamps across instances.
pub(super) struct Operation {
    directory: Directory,
    lock: File,
}

impl Operation {
    pub(super) fn acquire(root: &Path) -> io::Result<Self> {
        let directory = Directory::open(root, false)?;
        let lock = directory.open_lock(OPERATION_LOCK)?;
        lock.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                "Another kortty instance is writing a backup or its settings.",
            ),
            TryLockError::Error(error) => error,
        })?;
        Ok(Self { directory, lock })
    }

    pub(super) fn preferences(&self) -> io::Result<Preferences> {
        read_preferences(&self.directory)
    }

    pub(super) fn progress(&self) -> io::Result<Progress> {
        read_progress(&self.directory)
    }

    pub(super) fn store_progress(&self, progress: &Progress) -> io::Result<bool> {
        publish(
            &self.directory,
            PROGRESS,
            &ProgressDocument {
                version: VERSION,
                last_backup: progress.last_backup.map(|time| time.to_string()),
                last_auto: progress.last_auto.map(|time| time.to_string()),
                last_directory: progress.last_directory.clone(),
            },
        )
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        let _ = self.lock.unlock();
    }
}

fn read_preferences(directory: &Directory) -> io::Result<Preferences> {
    let Some(document) = read::<SettingsDocument>(directory, SETTINGS)? else {
        return Ok(Preferences::default());
    };
    require_version(document.version)?;
    document.settings.validate()?;
    Ok(document.settings)
}

fn read_progress(directory: &Directory) -> io::Result<Progress> {
    let Some(document) = read::<ProgressDocument>(directory, PROGRESS)? else {
        return Ok(Progress::default());
    };
    require_version(document.version)?;
    if let Some(path) = &document.last_directory {
        validate_directory(path)?;
    }
    let parse = |time: Option<String>| {
        time.map(|time| time.parse::<Timestamp>().map_err(invalid))
            .transpose()
    };
    Ok(Progress {
        last_backup: parse(document.last_backup)?,
        last_auto: parse(document.last_auto)?,
        last_directory: document.last_directory,
    })
}

pub(crate) fn validate_directory(directory: &Path) -> io::Result<()> {
    if !directory.is_absolute()
        || directory
            .to_str()
            .is_none_or(|path| path.len() > MAX_PATH_BYTES || path.chars().any(char::is_control))
        || directory
            .components()
            .any(|part| part == Component::ParentDir)
    {
        return Err(invalid(
            "Enter an absolute backup folder without parent path components.",
        ));
    }
    Ok(())
}

fn read<T: serde::de::DeserializeOwned>(
    directory: &Directory,
    name: &str,
) -> io::Result<Option<T>> {
    let file = match directory.open_private_file(name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(MAX_DOCUMENT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(invalid("Backup preferences exceed their size limit."));
    }
    serde_json::from_slice(&bytes).map(Some).map_err(invalid)
}

fn publish(directory: &Directory, name: &str, document: &impl Serialize) -> io::Result<bool> {
    let bytes = serde_json::to_vec_pretty(document).map_err(invalid)?;
    directory.replace_file(name, &bytes)?;
    Ok(directory.sync().is_ok())
}

fn require_version(version: u8) -> io::Result<()> {
    if version != VERSION {
        return Err(invalid("Unsupported backup preferences version."));
    }
    Ok(())
}

fn invalid(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
