//! Transactional rotation of native encrypted profile backups.

use std::fs::{File, FileTimes, TryLockError};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use jiff::{Zoned, civil::DateTime};

use crate::private_files::{Directory, FileStamp};

use super::{archive, openpgp, preferences::EncryptionMode};

#[cfg(test)]
mod tests;

pub(crate) const CURRENT_NAME: &str = "kortty-backup.zip";
const OPENPGP_CURRENT_NAME: &str = "kortty-backup.zip.gpg";
const HISTORY: &str = "old-backups";
const LOCK: &str = ".kortty-backup.lock";
const PREFIX: &str = "kortty-backup_";
const TIMESTAMP_FORMAT: &str = "%Y-%m-%d_%H-%M-%S";
const MAX_RETENTION: u8 = 100;
const MAX_ARCHIVE_ENTRIES: usize = 4096;

#[derive(Debug)]
pub(crate) struct Report {
    pub path: PathBuf,
    pub warning: Option<String>,
}

pub(crate) fn current_name(mode: EncryptionMode) -> &'static str {
    match mode {
        EncryptionMode::Password => CURRENT_NAME,
        EncryptionMode::OpenPgp => OPENPGP_CURRENT_NAME,
    }
}

pub(super) fn write(
    directory: &Path,
    snapshot: &super::Snapshot,
    password: &kortty_vault::Secret,
    retention: u8,
    now: Zoned,
) -> io::Result<Report> {
    write_with(
        directory,
        EncryptionMode::Password,
        retention,
        now,
        |file| archive::write_to(file, snapshot, password),
    )
}

pub(super) fn write_with(
    directory: &Path,
    mode: EncryptionMode,
    retention: u8,
    now: Zoned,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<Report> {
    if retention > MAX_RETENTION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Backup retention must be between 0 and 100.",
        ));
    }
    let target = Directory::open(directory, false)?;
    let _lock = Lock::acquire(target.open_lock(LOCK)?)?;
    let requested = Previous::open(&target, mode)?;
    let alternate = match mode {
        EncryptionMode::Password => EncryptionMode::OpenPgp,
        EncryptionMode::OpenPgp => EncryptionMode::Password,
    };
    let inactive = match Previous::open(&target, alternate) {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::InvalidData
                    | io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::PermissionDenied
            ) =>
        {
            None
        }
        result => result?,
    };
    let name = current_name(mode);
    let mut history = None;
    let mut preserve = || {
        let timestamp = now.strftime(TIMESTAMP_FORMAT).to_string();
        if !valid_timestamp(&timestamp) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid backup timestamp.",
            ));
        }
        let folder = target.subdirectory(HISTORY, true)?;
        for previous in requested.iter().chain(inactive.iter()) {
            previous.preserve(&target, &folder, &timestamp)?;
        }
        folder.sync()?;
        history = Some(folder);
        if let Some(previous) = &requested {
            previous.stamp.require_unchanged(&target.open_file(name)?)?;
        }
        Ok(())
    };
    if requested.is_some() {
        target.replace_with(name, write, preserve)?;
    } else if inactive.is_some() {
        target.publish_with(name, |file| {
            write(file)?;
            // The old current may also be the last successful backup. Stage the new one first.
            file.sync_all()?;
            preserve()
        })?;
    } else {
        target.publish_with(name, write)?;
    }

    // Publication is the commit point. Keep extra copies if durability or cleanup fails.
    let warning = if let Err(error) = target.sync() {
        Some(format!(
            "Backup saved, but directory synchronization could not be confirmed ({error})."
        ))
    } else {
        finish_rotation(&target, inactive, history, usize::from(retention))
            .err()
            .map(|error| {
                format!("Backup saved, but older backups could not be cleaned up ({error}).")
            })
    };
    Ok(Report {
        path: directory.join(name),
        warning,
    })
}

struct Previous {
    mode: EncryptionMode,
    stamp: FileStamp,
    length: u64,
}

impl Previous {
    fn open(directory: &Directory, mode: EncryptionMode) -> io::Result<Option<Self>> {
        let name = current_name(mode);
        let Some(mut file) = optional(directory.open_file(name))? else {
            return Ok(None);
        };
        let stamp = FileStamp::read(&file)?;
        let length = file.metadata()?.len();
        verify_ciphertext(&mut file, mode).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("Existing {name} cannot be rotated ({error})."),
            )
        })?;
        stamp.require_unchanged(&file)?;
        if file.metadata()?.len() != length {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Current backup changed while being verified.",
            ));
        }
        Ok(Some(Self {
            mode,
            stamp,
            length,
        }))
    }

    fn preserve(&self, target: &Directory, history: &Directory, timestamp: &str) -> io::Result<()> {
        let base = format!("{PREFIX}{timestamp}");
        let extension = match self.mode {
            EncryptionMode::Password => ".zip",
            EncryptionMode::OpenPgp => ".zip.gpg",
        };
        for suffix in 0..=MAX_ARCHIVE_ENTRIES {
            let name = if suffix == 0 {
                format!("{base}{extension}")
            } else {
                format!("{base}-{suffix}{extension}")
            };
            if history.has_entry(&name)? {
                continue;
            }
            let result = history.publish_with(&name, |output| {
                let source = target.open_file(current_name(self.mode))?;
                self.stamp.require_unchanged(&source)?;
                let copied = io::copy(&mut (&source).take(self.length.saturating_add(1)), output)?;
                if copied != self.length {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Current backup changed while being archived.",
                    ));
                }
                self.stamp.require_unchanged(&source)?;
                output.set_times(FileTimes::new().set_modified(self.stamp.modified))
            });
            match result {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                result => return result,
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "No free rotated backup filename.",
        ))
    }
}

fn finish_rotation(
    target: &Directory,
    inactive: Option<Previous>,
    history: Option<Directory>,
    retention: usize,
) -> io::Result<()> {
    if let Some(inactive) = inactive {
        optional(target.remove_unchanged(current_name(inactive.mode), &inactive.stamp))?;
        target.sync()?;
    }
    if retention != 0 {
        prune(target, history, retention)?;
    }
    Ok(())
}

fn prune(directory: &Directory, history: Option<Directory>, retention: usize) -> io::Result<()> {
    let history = match history {
        Some(history) => history,
        None => {
            let Some(history) = optional(directory.subdirectory(HISTORY, false))? else {
                return Ok(());
            };
            history
        }
    };
    let mut backups = Vec::new();
    for name in history.names(MAX_ARCHIVE_ENTRIES)? {
        let Ok(name) = name.into_string() else {
            continue;
        };
        let Some(mode) = rotated_mode(&name) else {
            continue;
        };
        let Some(stamp) = verified_backup(&history, &name, mode)? else {
            continue;
        };
        backups.push((name, stamp));
    }
    let count = backups.len().saturating_sub(retention);
    if count == 0 {
        return Ok(());
    }
    backups.sort_unstable_by(|(left_name, left), (right_name, right)| {
        left.modified
            .cmp(&right.modified)
            .then_with(|| left_name.cmp(right_name))
    });
    for (name, stamp) in backups.into_iter().take(count) {
        optional(history.remove_unchanged(&name, &stamp))?;
    }
    history.sync()
}

fn verified_backup(
    directory: &Directory,
    name: &str,
    mode: EncryptionMode,
) -> io::Result<Option<FileStamp>> {
    let mut file = match directory.open_file(name) {
        Ok(file) => file,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidData
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let stamp = FileStamp::read(&file)?;
    match verify_ciphertext(&mut file, mode) {
        Ok(()) => stamp.require_unchanged(&file)?,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    }
    Ok(Some(stamp))
}

fn verify_ciphertext(file: &mut File, mode: EncryptionMode) -> io::Result<()> {
    match mode {
        EncryptionMode::Password => archive::verify_rotation_candidate(file),
        EncryptionMode::OpenPgp => openpgp::validate_ciphertext(file),
    }
}

fn rotated_mode(name: &str) -> Option<EncryptionMode> {
    let stem = name.strip_prefix(PREFIX)?;
    let (stem, mode) = if let Some(stem) = stem.strip_suffix(".zip.gpg") {
        (stem, EncryptionMode::OpenPgp)
    } else {
        (stem.strip_suffix(".zip")?, EncryptionMode::Password)
    };
    let (timestamp, suffix) = stem.split_at_checked(19)?;
    let numbered = suffix.strip_prefix('-').is_some_and(|suffix| {
        suffix.bytes().all(|byte| byte.is_ascii_digit())
            && !suffix.starts_with('0')
            && suffix.parse::<u64>().is_ok()
    });
    (valid_timestamp(timestamp) && (suffix.is_empty() || numbered)).then_some(mode)
}

fn valid_timestamp(timestamp: &str) -> bool {
    timestamp.len() == 19
        && timestamp
            .bytes()
            .enumerate()
            .all(|(index, byte)| match index {
                4 | 7 | 13 | 16 => byte == b'-',
                10 => byte == b'_',
                _ => byte.is_ascii_digit(),
            })
        && DateTime::strptime(TIMESTAMP_FORMAT, timestamp)
            .is_ok_and(|parsed| parsed.strftime(TIMESTAMP_FORMAT).to_string() == timestamp)
}

struct Lock(File);

impl Lock {
    fn acquire(file: File) -> io::Result<Self> {
        file.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                "Another instance is writing a backup in this directory.",
            ),
            TryLockError::Error(error) => error,
        })?;
        Ok(Self(file))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn optional<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        result => result.map(Some),
    }
}
