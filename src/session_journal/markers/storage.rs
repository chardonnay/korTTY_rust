//! The marker registry has its own bounded portable profile file.

use std::fs::{File, TryLockError};
use std::io::{self, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::private_files::{Directory, FileStamp};

use super::{Compiled, Settings};
use crate::session_journal::metadata::require_single_link;

pub(crate) const FILE: &str = "journal-markers.json";
pub(crate) const MAX_FILE_BYTES: usize = 1024 * 1024;
pub(super) const LOCK: &str = "journal-markers.lock";
const VERSION: u8 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u8,
    settings: Settings,
}

#[derive(Serialize)]
struct Stored<'a> {
    version: u8,
    settings: &'a Settings,
}

impl Settings {
    pub(crate) fn load(root: &Path) -> io::Result<(Self, Compiled)> {
        let Some(directory) = optional(Directory::open(root, false))? else {
            return Ok(Default::default());
        };
        let (settings, _) = read_stored(&directory)?;
        require_directory(root, &directory)?;
        let compiled = Compiled::new(&settings)?;
        Ok((settings, compiled))
    }

    pub(crate) fn save(&self, root: &Path, baseline: &Self) -> io::Result<Compiled> {
        let compiled = Compiled::new(self)?;
        let bytes = Zeroizing::new(serde_json::to_vec(&Stored {
            version: VERSION,
            settings: self,
        })?);
        check_size(&bytes)?;
        let directory = Directory::open(root, true)?;
        let lock = directory.open_lock(LOCK)?;
        require_single_link(&lock)?;
        lock.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                "Another kortty instance is saving journal markers. Try again.",
            ),
            TryLockError::Error(error) => error,
        })?;
        let lock_stamp = FileStamp::read(&lock)?;
        let (current, previous) = read_stored(&directory)?;
        if &current != baseline {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "The journal marker settings changed in another instance. Restart kortty to load the latest settings.",
            ));
        }
        let check = || {
            require_directory(root, &directory)?;
            let current_lock = directory.open_private_file(LOCK)?;
            require_single_link(&current_lock)?;
            lock_stamp.require_unchanged(&current_lock)?;
            if current_stamp(&directory)? != previous {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "The journal marker settings changed while being saved.",
                ));
            }
            Ok(())
        };
        if previous.is_some() {
            directory.replace_with(FILE, |file| file.write_all(&bytes), check)?;
        } else {
            directory.publish_checked(FILE, |file| file.write_all(&bytes), check)?;
        }
        Ok(compiled)
    }
}

fn read_stored(directory: &Directory) -> io::Result<(Settings, Option<FileStamp>)> {
    let Some(file) = optional(open(directory))? else {
        return Ok((Settings::default(), None));
    };
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Zeroizing::new(Vec::new());
    (&file)
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    stamp.require_unchanged(&file)?;
    stamp.require_unchanged(&open(directory)?)?;
    Ok((decode(&bytes)?, Some(stamp)))
}

pub(crate) fn validate_backup(bytes: &[u8]) -> io::Result<()> {
    decode(bytes)?.validate()
}

fn decode(bytes: &[u8]) -> io::Result<Settings> {
    check_size(bytes)?;
    let document: Document = serde_json::from_slice(bytes)
        .map_err(|_| invalid("Invalid journal marker settings file."))?;
    if document.version != VERSION {
        return Err(invalid("Unsupported journal marker settings version."));
    }
    Ok(document.settings)
}

fn check_size(bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(invalid(
            "Journal marker settings exceed the profile file limit.",
        ));
    }
    Ok(())
}

fn open(directory: &Directory) -> io::Result<File> {
    let file = directory.open_private_file(FILE)?;
    require_single_link(&file)?;
    Ok(file)
}

fn current_stamp(directory: &Directory) -> io::Result<Option<FileStamp>> {
    optional(open(directory))?
        .as_ref()
        .map(FileStamp::read)
        .transpose()
}

fn require_directory(root: &Path, directory: &Directory) -> io::Result<()> {
    if !directory.same_as(&Directory::open(root, false)?)? {
        return Err(invalid("The journal marker profile folder changed."));
    }
    Ok(())
}

fn optional<T>(value: io::Result<T>) -> io::Result<Option<T>> {
    match value {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
