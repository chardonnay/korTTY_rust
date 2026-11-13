//! Encrypted portable profile backups and startup-only restore.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read};
use std::path::Path;

use kortty_vault::{MAX_ARCHIVE_PASSWORD_BYTES, Secret};
use zeroize::Zeroizing;

use crate::profile;

mod archive;
mod data;
mod java_credential_restore;
mod java_host_pins;
mod java_migration;
mod java_scheduler;
mod java_secrets;
mod openpgp;
pub(crate) mod preferences;
mod restore;
mod rotation;
pub(crate) mod schedule;
mod writer;

pub(crate) use openpgp::decrypt_openpgp_document;
pub(crate) use restore::{
    PendingJavaCancellation, discard_pending_java, pending_java_cancellation,
};
pub(crate) use restore::{ProfileLease, RestoreMode};
pub(crate) use rotation::current_name;
pub(crate) use writer::{
    AutoOutcome, Exported, effective_schedule, export_openpgp, export_profile, run_automatic,
};

pub(super) type Snapshot = BTreeMap<String, Zeroizing<Vec<u8>>>;

pub(crate) const MIN_PASSWORD_BYTES: usize = 12;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BackupFormat {
    Password,
    OpenPgp,
    JavaPassword,
}

impl fmt::Display for BackupFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password => preferences::EncryptionMode::Password.fmt(formatter),
            Self::OpenPgp => preferences::EncryptionMode::OpenPgp.fmt(formatter),
            Self::JavaPassword => formatter.write_str("Java password ZIP"),
        }
    }
}

/// An integrity-checked and validated backup, held until the user confirms import.
pub(crate) struct Preview {
    snapshot: Snapshot,
    java: Option<Box<java_migration::Report>>,
    java_credentials: Option<Box<java_credential_restore::Prepared>>,
}

impl Preview {
    pub(crate) fn count(&self) -> usize {
        self.snapshot.len()
            + usize::from(self.java_credentials.is_some())
            + usize::from(
                self.java
                    .as_ref()
                    .is_some_and(|report| report.scheduler.is_some()),
            )
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = &str> {
        self.snapshot.keys().map(String::as_str)
    }

    pub(crate) fn java_review(&self) -> Option<(&'static str, &[String], &[String])> {
        let report = self.java.as_ref()?;
        let format = match report.protection {
            archive::java::Protection::Aes256 => "Java password backup (AES-256)",
            archive::java::Protection::LegacyZipCrypto => "Java password backup (legacy ZipCrypto)",
            archive::java::Protection::OpenPgp => "Integrity-checked Java OpenPGP backup",
        };
        Some((format, &report.entries, &report.omissions))
    }

    pub(crate) fn has_java_passwords(&self) -> bool {
        self.java
            .as_ref()
            .and_then(|report| report.ssh.as_ref())
            .is_some_and(|source| source.has_passwords)
    }

    pub(crate) fn java_password_summary(&self) -> Option<(usize, usize)> {
        self.java.as_ref()?.password_summary
    }

    pub(crate) fn has_java_managed_keys(&self) -> bool {
        self.java
            .as_ref()
            .and_then(|report| report.ssh.as_ref())
            .is_some_and(|source| source.key_catalog.is_some())
    }

    pub(crate) fn java_key_summary(&self) -> Option<(usize, usize)> {
        self.java.as_ref()?.key_summary
    }

    pub(crate) fn java_host_pins(&self) -> Option<std::sync::Arc<kortty_ssh::KnownHostsFile>> {
        self.java.as_ref()?.host_pins.clone()
    }
}

/// Identifies the supported envelope before asking for credentials.
pub(crate) fn detect_format(source: &Path) -> io::Result<BackupFormat> {
    let file = profile::open_preference(source)?;
    if file.metadata()?.is_file() {
        let mut prefix = Vec::with_capacity(32);
        (&file).take(32).read_to_end(&mut prefix)?;
        if prefix.starts_with(b"PK\x03\x04") {
            if archive::verify_rotation_candidate(&file).is_ok() {
                return Ok(BackupFormat::Password);
            }
            if archive::java::recognizes(&file)? {
                return Ok(BackupFormat::JavaPassword);
            }
        }
        if openpgp::recognizes_header(&prefix) {
            return Ok(BackupFormat::OpenPgp);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "The file does not have a supported kortty backup format.",
    ))
}

pub(crate) fn inspect_java_password(source: &Path, password: &Secret) -> io::Result<Preview> {
    if !(1..=MAX_ARCHIVE_PASSWORD_BYTES).contains(&password.len()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The Java backup password must contain 1 to 1024 UTF-8 bytes.",
        ));
    }
    validate_java_preview(java_migration::inspect(source, password)?)
}

pub(crate) fn inspect_backup(source: &Path, password: &Secret) -> io::Result<Preview> {
    check_password(password)?;
    validate_preview(archive::read(source, password)?)
}

pub(crate) fn inspect_openpgp(
    source: &Path,
    private_key: &Path,
    password: &Secret,
) -> io::Result<Preview> {
    if password.len() > MAX_ARCHIVE_PASSWORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The OpenPGP passphrase exceeds 1024 UTF-8 bytes.",
        ));
    }
    match openpgp::read_profile(source, private_key, password)? {
        openpgp::ProfileBackup::Native(snapshot) => validate_preview(snapshot),
        openpgp::ProfileBackup::Java(members) => {
            validate_java_preview(java_migration::convert(members)?)
        }
    }
}

fn validate_java_preview(
    (snapshot, report): (Snapshot, java_migration::Report),
) -> io::Result<Preview> {
    data::validate(&snapshot)?;
    Ok(Preview {
        snapshot,
        java: Some(Box::new(report)),
        java_credentials: None,
    })
}

fn validate_preview(mut snapshot: Snapshot) -> io::Result<Preview> {
    data::validate(&snapshot)?;
    data::ai::prepare(&mut snapshot)?;
    check_snapshot(&snapshot)?;
    Ok(Preview {
        snapshot,
        java: None,
        java_credentials: None,
    })
}

fn check_snapshot(snapshot: &Snapshot) -> io::Result<()> {
    if snapshot.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The backup contains no portable profile data.",
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct QueuedRestore {
    pub(crate) durable: bool,
    pub(crate) scheduler_notes: Vec<String>,
}

/// Queues the reviewed snapshot and reports any destination-specific job omissions.
pub(crate) fn queue_restore(preview: Preview, mode: RestoreMode) -> io::Result<QueuedRestore> {
    let root = profile::root()?;
    queue_restore_at(&root, preview, mode)
}

fn queue_restore_at(root: &Path, preview: Preview, mode: RestoreMode) -> io::Result<QueuedRestore> {
    let is_java = preview.java.is_some();
    let scheduler = preview.java.and_then(|report| report.scheduler);
    if preview.java_credentials.is_some() || scheduler.is_some() {
        return restore::queue_java_transaction(
            root,
            preview.snapshot,
            preview.java_credentials.map(|prepared| *prepared),
            scheduler,
        );
    }
    check_snapshot(&preview.snapshot)?;
    let mode = if is_java {
        RestoreMode::JavaAdditive
    } else {
        mode
    };
    let durable = restore::queue_with_mode(root, preview.snapshot, mode, host_pins_locked())?;
    Ok(QueuedRestore {
        durable,
        scheduler_notes: Vec::new(),
    })
}

pub(crate) fn prepare_java_credentials(
    preview: &mut Preview,
    source_password: Option<&Secret>,
    vault: Option<&kortty_vault::Vault>,
    root: &Path,
) -> io::Result<()> {
    let report = preview
        .java
        .as_mut()
        .ok_or_else(|| io::Error::other("Expected a Java profile backup."))?;
    let source = report
        .ssh
        .as_ref()
        .ok_or_else(|| io::Error::other("No Java SSH credentials are available for review."))?;
    let review = java_credential_restore::prepare(
        root,
        source,
        source_password,
        vault,
        report.scheduler.is_some(),
    )?;
    if source.has_passwords && review.passwords.is_none() {
        report
            .omissions
            .push("Java SSH passwords were skipped.".into());
    }
    report.ssh = None;
    report.password_summary = review.passwords;
    report.key_summary = review.keys;
    for (label, summary) in [
        ("SSH passwords", review.passwords),
        ("Managed SSH keys", review.keys),
    ] {
        if let Some((count, _)) = summary.filter(|(count, _)| *count > 0) {
            report.entries.push(format!("{label} ({count})"));
        }
    }
    report.note_count(
        review.omitted_key_routes,
        "Java SSH key assignments omitted",
    );
    report.note_count(
        review.unused_key_files,
        "Unmatched Java SSH key files omitted",
    );
    preview.java_credentials = review.prepared.map(Box::new);
    Ok(())
}

pub(crate) fn host_pins_locked() -> bool {
    let policy = crate::clipboard_policy::ClipboardPolicy::load();
    policy.host_pins_locked || policy.problem.is_some()
}

fn check_password(password: &Secret) -> io::Result<()> {
    if !(MIN_PASSWORD_BYTES..=MAX_ARCHIVE_PASSWORD_BYTES).contains(&password.len()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The backup password must contain 12 to 1024 UTF-8 bytes.",
        ));
    }
    Ok(())
}
