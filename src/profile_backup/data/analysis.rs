//! Portable analysis histories attached to their reviewed snippet catalogue.

use std::cell::Cell;
use std::io::{self, Write as _};

use zeroize::Zeroizing;

use super::{MAX_ENTRIES, Snapshot, include, invalid, optional, read_file};
use crate::clipboard_policy::ClipboardPolicy;
use crate::private_files::{Directory, FileStamp, require_single_link};
use crate::snippet_analysis::store::java;
use crate::snippets::{self, Library};

const DIRECTORY: &str = "snippet-analyses";

pub(in crate::profile_backup) fn filename(path: &str) -> Option<&str> {
    let name = path.strip_prefix("snippet-analyses/")?;
    let stem = name.strip_suffix(".json")?;
    let id = stem.strip_prefix("folder-").unwrap_or(stem);
    (id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')))
    .then_some(name)
}

pub(in crate::profile_backup) fn has_histories(snapshot: &Snapshot) -> bool {
    snapshot.keys().any(|path| filename(path).is_some())
}

pub(in crate::profile_backup) fn allowed() -> bool {
    managed_limit().is_some()
}

pub(in crate::profile_backup) fn managed_limit() -> Option<usize> {
    let policy = ClipboardPolicy::load();
    (policy.problem.is_none() && policy.ai_allowed).then(|| java::content_limit(&policy))
}

pub(super) fn collect(
    root: &Directory,
    snapshot: &mut Snapshot,
    total: &mut usize,
) -> io::Result<()> {
    let Some(library) = snapshot.get(snippets::FILE_NAME) else {
        return Ok(());
    };
    let Some(directory) = optional(root.private_child(DIRECTORY, false))? else {
        return Ok(());
    };
    let library = Library::analysis_backup_catalogue(library)?;
    for item in directory.names(MAX_ENTRIES)? {
        let Ok(name) = item.into_string() else {
            continue;
        };
        let path = format!("{DIRECTORY}/{name}");
        if filename(&path).is_none() {
            continue;
        }
        let Some(file) = optional(directory.open_private_file(&name))? else {
            continue;
        };
        require_single_link(&file)?;
        let stamp = FileStamp::read(&file)?;
        let bytes = read_file(&file, java::MAX_BACKUP_BYTES)?;
        stamp.require_unchanged(&file)?;
        if library.has_analysis_subject(java::binding(&name, &bytes)?) {
            let portable = java::portable_backup(&name, &bytes)?;
            include(snapshot, total, path, Zeroizing::new(portable))?;
        }
    }
    Ok(())
}

pub(super) fn validate(snapshot: &Snapshot) -> io::Result<()> {
    if !has_histories(snapshot) {
        return Ok(());
    }
    let library = source_library(snapshot)?;
    for (path, bytes) in snapshot {
        if let Some(name) = filename(path)
            && !library.has_analysis_subject(java::binding(name, bytes)?)
        {
            return Err(invalid(
                "A snippet analysis history is missing its backup subject",
            ));
        }
    }
    Ok(())
}

fn source_library(snapshot: &Snapshot) -> io::Result<Library> {
    let bytes = snapshot
        .get(snippets::FILE_NAME)
        .ok_or_else(|| invalid("Snippet analysis histories require a snippet backup catalogue"))?;
    Library::analysis_backup_catalogue(bytes)
}

pub(in crate::profile_backup) fn install(
    root: &Directory,
    snapshot: &Snapshot,
    managed_limit: impl Fn() -> Option<usize>,
) -> io::Result<()> {
    if !has_histories(snapshot) || managed_limit().is_none() {
        return Ok(());
    }
    let source = source_library(snapshot)?;
    let Some(file) = optional(root.open_file(snippets::FILE_NAME))? else {
        return Ok(());
    };
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let current = read_file(&file, super::file_limit(snippets::FILE_NAME))?;
    stamp.require_unchanged(&file)?;
    let Ok(current) = Library::analysis_backup_catalogue(&current) else {
        // Keep the user's local catalogue and omit histories without a verified subject.
        return Ok(());
    };
    let mut directory = optional(root.private_child(DIRECTORY, false))?;
    for (path, bytes) in snapshot {
        let Some(name) = filename(path) else {
            continue;
        };
        if directory
            .as_ref()
            .map(|directory| directory.has_entry(name))
            .transpose()?
            .unwrap_or(false)
            || !source.matches_analysis_subject(&current, java::binding(name, bytes)?)
        {
            continue;
        }
        let Some(limit) = managed_limit() else {
            break;
        };
        let portable = Zeroizing::new(java::portable_backup_with_limit(name, bytes, limit)?);
        if directory.is_none() {
            directory = Some(root.private_child(DIRECTORY, true)?);
        }
        let target = directory
            .as_ref()
            .expect("a private analysis directory is available");
        let denied = Cell::new(false);
        let result = target.publish_checked(
            name,
            |file| file.write_all(&portable),
            || {
                let Some(current_limit) = managed_limit() else {
                    denied.set(true);
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "Managed policy prevents snippet analysis restore",
                    ));
                };
                if current_limit < limit && !java::fits_retention(name, &portable, current_limit)? {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "Managed snippet analysis retention changed during restore",
                    ));
                }
                let file = root.open_file(snippets::FILE_NAME)?;
                require_single_link(&file)?;
                stamp.require_unchanged(&file)
            },
        );
        if denied.get() {
            break;
        }
        match result {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    if let Some(directory) = directory {
        directory.sync()?;
    }
    root.sync()
}
