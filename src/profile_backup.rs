//! Encrypted portable profile backups and startup-only restore.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::Path;

use kortty_vault::{MAX_ARCHIVE_PASSWORD_BYTES, Secret};
use zeroize::Zeroizing;

use crate::profile;

mod archive;
mod data;
mod openpgp;
pub(crate) mod preferences;
mod restore;
mod rotation;
pub(crate) mod schedule;
mod writer;

pub(crate) use openpgp::decrypt_openpgp_document;
pub(crate) use restore::{ProfileLease, RestoreMode};
pub(crate) use rotation::current_name;
pub(crate) use writer::{
    AutoOutcome, Exported, effective_schedule, export_openpgp, export_profile, run_automatic,
};

pub(super) type Snapshot = BTreeMap<String, Zeroizing<Vec<u8>>>;

pub(crate) const MIN_PASSWORD_BYTES: usize = 12;

/// An integrity-checked and validated backup, held until the user confirms import.
pub(crate) struct Preview(Snapshot);

impl Preview {
    pub(crate) fn count(&self) -> usize {
        self.0.len()
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }
}

/// Identifies the supported envelope before asking for credentials.
pub(crate) fn detect_format(source: &Path) -> io::Result<preferences::EncryptionMode> {
    use preferences::EncryptionMode;

    let file = profile::open_preference(source)?;
    if file.metadata()?.is_file() {
        let mut prefix = Vec::with_capacity(32);
        (&file).take(32).read_to_end(&mut prefix)?;
        if prefix.starts_with(b"PK\x03\x04") {
            archive::verify_rotation_candidate(&file)?;
            return Ok(EncryptionMode::Password);
        }
        if openpgp::recognizes_header(&prefix) {
            return Ok(EncryptionMode::OpenPgp);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "The file does not have a supported kortty backup format.",
    ))
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
    validate_preview(openpgp::read(source, private_key, password)?)
}

fn validate_preview(snapshot: Snapshot) -> io::Result<Preview> {
    data::validate(&snapshot)?;
    check_snapshot(&snapshot)?;
    Ok(Preview(snapshot))
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

/// Queues the reviewed snapshot. The result indicates whether publication
/// could also be synchronized to the destination directory.
pub(crate) fn queue_restore(preview: Preview, mode: RestoreMode) -> io::Result<bool> {
    restore::queue_with_mode(&profile::root()?, preview.0, mode, host_pins_locked())
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
