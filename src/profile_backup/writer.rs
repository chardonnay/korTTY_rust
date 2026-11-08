//! Manual and automatic exports use the same snapshot and success accounting.

use std::io;
use std::path::{Path, PathBuf};

use jiff::{Timestamp, Zoned, tz::TimeZone};
use kortty_vault::Secret;

use super::preferences::{self, EncryptionMode, Interval, Operation, Preferences, Progress};
use super::{Snapshot, archive, check_password, check_snapshot, data, openpgp, rotation, schedule};
use crate::clipboard_policy::ClipboardPolicy;
use crate::private_files::Directory;

#[cfg(test)]
mod tests;

pub(crate) struct Exported {
    pub count: usize,
    pub path: PathBuf,
    pub progress: Progress,
    pub warning: Option<String>,
}

pub(crate) enum AutoOutcome {
    Written(Exported),
    Waiting(String),
}

pub(crate) fn export_profile(
    destination: &Path,
    password: &Secret,
    credential: Option<&str>,
) -> io::Result<Exported> {
    let root = crate::profile::root()?;
    export_for(&root, destination, password, credential, zoned_now)
}

pub(crate) fn export_openpgp(destination: &Path, recipient: &str) -> io::Result<Exported> {
    let root = crate::profile::root()?;
    export_using(
        &root,
        destination,
        Protection::OpenPgp(recipient),
        None,
        zoned_now,
    )
}

enum Protection<'a> {
    Password(&'a Secret),
    OpenPgp(&'a str),
}

impl Protection<'_> {
    fn mode(&self) -> EncryptionMode {
        match self {
            Self::Password(_) => EncryptionMode::Password,
            Self::OpenPgp(_) => EncryptionMode::OpenPgp,
        }
    }

    fn rotate(
        &self,
        root: &Path,
        directory: &Path,
        snapshot: &Snapshot,
        retention: u8,
        now: Zoned,
    ) -> io::Result<rotation::Report> {
        match self {
            Self::Password(password) => {
                rotation::write(directory, snapshot, password, retention, now)
            }
            Self::OpenPgp(recipient) => {
                rotation::write_with(directory, self.mode(), retention, now, |file| {
                    openpgp::write_to(file, snapshot, root, recipient)
                })
            }
        }
    }

    fn export(&self, root: &Path, destination: &Path, snapshot: &Snapshot) -> io::Result<()> {
        match self {
            Self::Password(password) => archive::write(destination, snapshot, password),
            Self::OpenPgp(recipient) => crate::profile::create_new_atomic(destination, |file| {
                openpgp::write_to(file, snapshot, root, recipient)
            }),
        }
    }
}

fn export_for(
    root: &Path,
    destination: &Path,
    password: &Secret,
    credential: Option<&str>,
    now: impl FnOnce() -> io::Result<Zoned>,
) -> io::Result<Exported> {
    export_using(
        root,
        destination,
        Protection::Password(password),
        credential,
        now,
    )
}

fn export_using(
    root: &Path,
    destination: &Path,
    protection: Protection<'_>,
    credential: Option<&str>,
    now: impl FnOnce() -> io::Result<Zoned>,
) -> io::Result<Exported> {
    let operation = Operation::acquire(root)?;
    let preferences = operation.preferences()?;
    match protection {
        Protection::Password(password) => {
            check_password(password)?;
            if credential.is_some()
                && (preferences.encryption != EncryptionMode::Password
                    || credential != preferences.credential.as_deref())
            {
                return Err(io::Error::other(
                    "The saved backup password selection changed.",
                ));
            }
        }
        Protection::OpenPgp(recipient) => {
            if preferences.encryption != EncryptionMode::OpenPgp
                || preferences.recipient.as_deref() != Some(recipient)
            {
                return Err(io::Error::other(
                    "The selected OpenPGP backup recipient changed.",
                ));
            }
        }
    }
    let progress = operation.progress()?;
    let snapshot = collect(root)?;
    let now = now()?;
    let directory = destination
        .parent()
        .filter(|_| destination.file_name().is_some())
        .ok_or_else(|| io::Error::other("Enter an absolute backup file path."))?;
    preferences::validate_directory(directory)?;
    let report = if destination
        .file_name()
        .is_some_and(|name| name == rotation::current_name(protection.mode()))
    {
        protection.rotate(
            root,
            directory,
            &snapshot,
            preferences.retention,
            now.clone(),
        )?
    } else {
        protection.export(root, destination, &snapshot)?;
        rotation::Report {
            path: destination.into(),
            warning: None,
        }
    };
    Ok(record_success(
        &operation,
        progress,
        report,
        snapshot.len(),
        now.timestamp(),
        false,
    ))
}

pub(crate) fn run_automatic(
    expected: Preferences,
    password: Option<Result<Secret, &'static str>>,
    recent_success: Option<Timestamp>,
) -> io::Result<AutoOutcome> {
    let root = crate::profile::root()?;
    run_automatic_for(
        &root,
        expected,
        password,
        recent_success,
        ClipboardPolicy::load,
        zoned_now,
    )
}

fn run_automatic_for(
    root: &Path,
    expected: Preferences,
    password: Option<Result<Secret, &'static str>>,
    recent_success: Option<Timestamp>,
    load_policy: impl Fn() -> ClipboardPolicy,
    now: impl FnOnce() -> io::Result<Zoned>,
) -> io::Result<AutoOutcome> {
    let operation = Operation::acquire(root)?;
    let preferences = operation.preferences()?;
    if preferences != expected {
        return Err(io::Error::other(
            "Backup settings changed. Reopen Settings to refresh them.",
        ));
    }
    let policy = load_policy();
    let (interval, directory) = effective_schedule(&preferences, &policy);
    if interval == Interval::Off {
        return Ok(AutoOutcome::Waiting("Automatic backups are off.".into()));
    }
    let Some(directory) = directory else {
        return Ok(AutoOutcome::Waiting(
            "Choose a destination folder for automatic backups.".into(),
        ));
    };
    preferences::validate_directory(directory)?;
    Directory::open(directory, false).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("The automatic backup folder is unavailable ({error})."),
        )
    })?;
    let protection = match preferences.encryption {
        EncryptionMode::Password => match password.as_ref() {
            Some(Ok(password)) => {
                check_password(password)?;
                Protection::Password(password)
            }
            Some(Err(error)) => return Ok(AutoOutcome::Waiting((*error).into())),
            None => {
                return Ok(AutoOutcome::Waiting(
                    "Select an archive password in Backup Settings.".into(),
                ));
            }
        },
        EncryptionMode::OpenPgp => match preferences.recipient.as_deref() {
            Some(recipient) => Protection::OpenPgp(recipient),
            None => {
                return Ok(AutoOutcome::Waiting(
                    "Select an OpenPGP recipient in Backup Settings.".into(),
                ));
            }
        },
    };
    let now = now()?;
    let progress = operation.progress()?;
    if !schedule::due(interval, progress.last_auto.max(recent_success), &now)
        .map_err(io::Error::other)?
    {
        return Ok(AutoOutcome::Waiting(
            "The next automatic backup is scheduled.".into(),
        ));
    }
    let snapshot = collect(root)?;
    let latest_policy = load_policy();
    if effective_schedule(&preferences, &latest_policy) != (interval, Some(directory)) {
        return Ok(AutoOutcome::Waiting(
            "The managed backup schedule changed. A later check will use the new settings.".into(),
        ));
    }
    let report = protection.rotate(
        root,
        directory,
        &snapshot,
        preferences.retention,
        now.clone(),
    )?;
    Ok(AutoOutcome::Written(record_success(
        &operation,
        progress,
        report,
        snapshot.len(),
        now.timestamp(),
        true,
    )))
}

pub(crate) fn effective_schedule<'a>(
    preferences: &'a Preferences,
    policy: &'a ClipboardPolicy,
) -> (Interval, Option<&'a Path>) {
    if policy.problem.is_some() {
        return (Interval::Off, None);
    }
    (
        policy.backup.interval.unwrap_or(preferences.interval),
        policy
            .backup
            .directory
            .as_deref()
            .or(preferences.directory.as_deref()),
    )
}

fn collect(root: &Path) -> io::Result<Snapshot> {
    let snapshot = data::collect(root)?;
    check_snapshot(&snapshot)?;
    Ok(snapshot)
}

fn zoned_now() -> io::Result<Zoned> {
    Ok(Timestamp::now().to_zoned(TimeZone::try_system().map_err(io::Error::other)?))
}

fn record_success(
    operation: &Operation,
    mut progress: Progress,
    report: rotation::Report,
    count: usize,
    now: Timestamp,
    automatic: bool,
) -> Exported {
    progress.last_backup = Some(now);
    if automatic {
        progress.last_auto = Some(now);
    }
    progress.last_directory = report.path.parent().map(Path::to_path_buf);
    let mut warning = report.warning;
    if !operation
        .store_progress(&progress)
        .is_ok_and(|synced| synced)
    {
        let text = "The archive was saved, but the backup history could not be synchronized.";
        warning = Some(warning.map_or_else(|| text.into(), |warning| format!("{warning} {text}")));
    }
    Exported {
        count,
        path: report.path,
        progress,
        warning,
    }
}
