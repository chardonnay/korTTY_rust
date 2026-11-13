//! Private resume receipts. Payload ownership is checked separately before reopening.

use std::ffi::OsString;
use std::fs::{File, TryLockError};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kortty_ssh::HostKey;
use serde::{Deserialize, Serialize};

use crate::private_files::{Directory, FileStamp, require_single_link};
use crate::workspace::SshTarget;

const FILE: &str = "index-v1.json";
const MAX_BYTES: u64 = 256 * 1024;
const MAX_RECORDS: usize = 8;
const MAX_PARTIAL_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 4 * MAX_PARTIAL_BYTES;

mod replacement;
mod upload;
pub(in crate::sftp_window) use replacement::{
    ReplacementRecord, ReplacementReview, ReviewedRemote,
};
pub(in crate::sftp_window::worker) use upload::Lease;
pub(in crate::sftp_window) use upload::{Phase, UploadRecord, remote_components};

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub(in crate::sftp_window) enum LocalPath {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}

impl LocalPath {
    pub(in crate::sftp_window) fn capture(path: &Path) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            Self::Unix(path.as_os_str().as_bytes().to_vec())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt as _;
            Self::Windows(path.as_os_str().encode_wide().collect())
        }
    }

    pub(in crate::sftp_window) fn path(&self) -> io::Result<PathBuf> {
        #[cfg(unix)]
        if let Self::Unix(bytes) = self {
            use std::os::unix::ffi::OsStringExt as _;
            if bytes.len() <= 16_384 && !bytes.contains(&0) {
                return Ok(PathBuf::from(OsString::from_vec(bytes.clone())));
            }
        }
        #[cfg(windows)]
        if let Self::Windows(units) = self {
            use std::os::windows::ffi::OsStringExt as _;
            if units.len() <= 16_384 && !units.contains(&0) {
                return Ok(PathBuf::from(OsString::from_wide(units)));
            }
        }
        Err(invalid(
            "The saved local path belongs to another platform or is invalid",
        ))
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::sftp_window) struct Identity {
    volume: u64,
    file: Vec<u8>,
}

impl Identity {
    pub(in crate::sftp_window) fn directory(
        directory: &crate::sftp_window::local_tree::Directory,
    ) -> io::Result<Self> {
        #[cfg(unix)]
        return Self::capture(directory.file());
        #[cfg(windows)]
        {
            let (volume, id) = directory.identity()?;
            Ok(Self {
                volume,
                file: id.to_vec(),
            })
        }
    }

    pub(in crate::sftp_window) fn require_directory(
        &self,
        directory: &crate::sftp_window::local_tree::Directory,
    ) -> io::Result<()> {
        if Self::directory(directory)? == *self {
            Ok(())
        } else {
            Err(invalid("The saved download folder was replaced"))
        }
    }

    pub(in crate::sftp_window) fn capture(file: &File) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = file.metadata()?;
            Ok(Self {
                volume: metadata.dev(),
                file: metadata.ino().to_be_bytes().to_vec(),
            })
        }
        #[cfg(windows)]
        {
            let (volume, id) = kortty_files::tree::file_identity(file)?;
            Ok(Self {
                volume,
                file: id.to_vec(),
            })
        }
    }

    pub(in crate::sftp_window) fn require(&self, file: &File) -> io::Result<()> {
        if Self::capture(file)? == *self {
            Ok(())
        } else {
            Err(invalid("The saved download location was replaced"))
        }
    }

    #[cfg(unix)]
    pub(in crate::sftp_window) fn matches_stat(&self, metadata: &rustix::fs::Stat) -> bool {
        i128::from(metadata.st_dev) == i128::from(self.volume)
            && u64::try_from(i128::from(metadata.st_ino))
                .is_ok_and(|inode| inode.to_be_bytes().as_slice() == self.file)
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::sftp_window) struct Payload {
    pub(in crate::sftp_window) destination: LocalPath,
    pub(in crate::sftp_window) stage_name: String,
    pub(in crate::sftp_window) parent: Identity,
    pub(in crate::sftp_window) stage: Identity,
    pub(in crate::sftp_window) file: Identity,
    pub(in crate::sftp_window) offset: u64,
    pub(in crate::sftp_window) sha256: String,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::sftp_window) struct Record {
    pub(in crate::sftp_window) id: String,
    pub(in crate::sftp_window) route: SshTarget,
    pub(in crate::sftp_window) host_key: String,
    pub(in crate::sftp_window) remote: Vec<u8>,
    pub(in crate::sftp_window) total: u64,
    pub(in crate::sftp_window) remote_modified: u32,
    pub(in crate::sftp_window) remote_permissions: u32,
    pub(in crate::sftp_window) remote_uid: Option<u32>,
    pub(in crate::sftp_window) remote_gid: Option<u32>,
    pub(in crate::sftp_window) title: String,
    pub(in crate::sftp_window) payload: Payload,
}

impl Record {
    fn validate(&self) -> io::Result<()> {
        self.route.validate()?;
        HostKey::from_openssh(&self.host_key)?;
        let name = &self.payload.stage_name;
        let stage_id = name
            .strip_prefix(".kortty-")
            .and_then(|name| name.strip_suffix(".kortty-part"));
        let destination = self.payload.destination.path()?;
        if !hex(&self.id, 32)
            || stage_id.is_none_or(|id| !hex(id, 32))
            || stage_id != Some(self.id.as_str())
            || !destination.is_absolute()
            || destination.file_name().is_none()
            || self.remote.len() > 16_384
            || !self.remote.starts_with(b"/")
            || self.remote.contains(&0)
            || self.title.len() > 1024
            || self.title.chars().any(char::is_control)
            || self.payload.offset == 0
            || self.payload.offset > MAX_PARTIAL_BYTES
            || self.payload.offset > self.total
            || self.remote_permissions & 0o170000 != 0o100000
            || !hex(&self.payload.sha256, 64)
            || [
                &self.payload.parent,
                &self.payload.stage,
                &self.payload.file,
            ]
            .into_iter()
            .any(|identity| !matches!(identity.file.len(), 8 | 16))
        {
            return Err(invalid("Invalid saved SFTP download receipt"));
        }
        Ok(())
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Index {
    version: u8,
    records: Vec<Record>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    uploads: Vec<UploadRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    replacements: Vec<ReplacementRecord>,
}

impl Index {
    fn validate(&self) -> io::Result<()> {
        if self.version != 1
            || self.records.len() + self.uploads.len() + self.replacements.len() > MAX_RECORDS
        {
            return Err(invalid("Saved SFTP transfers exceed the receipt limit"));
        }
        let mut total = 0_u64;
        for (position, record) in self.records.iter().enumerate() {
            record.validate()?;
            total = total
                .checked_add(record.payload.offset)
                .ok_or_else(|| invalid("Saved SFTP download sizes overflow"))?;
            if total > MAX_TOTAL_BYTES
                || self.records[..position]
                    .iter()
                    .any(|old| old.id == record.id)
            {
                return Err(invalid("Saved SFTP downloads exceed their bounds"));
            }
        }
        for (position, upload) in self.uploads.iter().enumerate() {
            upload.validate()?;
            total = total
                .checked_add(upload.source.size)
                .ok_or_else(|| invalid("Saved SFTP transfer sizes overflow"))?;
            if total > MAX_TOTAL_BYTES
                || self.records.iter().any(|old| old.id == upload.id)
                || self.uploads[..position]
                    .iter()
                    .any(|old| old.id == upload.id)
            {
                return Err(invalid("Saved SFTP transfers exceed their bounds"));
            }
        }
        for (position, replacement) in self.replacements.iter().enumerate() {
            replacement.validate()?;
            total = total
                .checked_add(replacement.review.source.size)
                .ok_or_else(|| invalid("Saved SFTP transfer sizes overflow"))?;
            if total > MAX_TOTAL_BYTES
                || self.records.iter().any(|old| old.id == replacement.id)
                || self.uploads.iter().any(|old| old.id == replacement.id)
                || self.replacements[..position]
                    .iter()
                    .any(|old| old.id == replacement.id || old.review == replacement.review)
            {
                return Err(invalid(
                    "Saved SFTP replacements exceed their bounds or duplicate a review",
                ));
            }
        }
        Ok(())
    }
}

pub(in crate::sftp_window) struct Store(PathBuf);

impl Store {
    pub(in crate::sftp_window) fn new(profile: &Path) -> Self {
        Self(profile.to_owned())
    }

    pub(in crate::sftp_window) fn list(&self) -> io::Result<Vec<Record>> {
        self.index().map(|index| index.records)
    }

    fn index(&self) -> io::Result<Index> {
        let root = match Directory::open(&self.0, false) {
            Ok(root) => root,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(empty_index()),
            Err(error) => return Err(error),
        };
        let directory = match root.private_child("sftp-resume", false) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(empty_index()),
            Err(error) => return Err(error),
        };
        read(&directory).map(|(index, _)| index)
    }

    #[cfg(test)]
    pub(in crate::sftp_window) fn save(&self, record: Record) -> io::Result<()> {
        self.save_guarded(record, &|| true)
    }

    pub(in crate::sftp_window) fn save_guarded(
        &self,
        record: Record,
        allowed: &impl Fn() -> bool,
    ) -> io::Result<()> {
        record.validate()?;
        self.change(allowed, |index| {
            let records = &mut index.records;
            if let Some(old) = records.iter_mut().find(|old| old.id == record.id) {
                *old = record;
            } else {
                records.push(record);
            }
        })
    }

    pub(in crate::sftp_window) fn remove(&self, id: &str) -> io::Result<()> {
        self.change(&|| true, |index| {
            index.records.retain(|record| record.id != id)
        })
    }

    fn change(
        &self,
        allowed: &impl Fn() -> bool,
        change: impl FnOnce(&mut Index),
    ) -> io::Result<()> {
        require_allowed(allowed)?;
        let directory = Directory::open(&self.0, true)?.private_child("sftp-resume", true)?;
        let lock = directory.open_lock(".index.lock")?;
        require_single_link(&lock)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            require_allowed(allowed)?;
            match lock.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "Saved transfers are being updated",
                    ));
                }
                Err(TryLockError::Error(error)) => return Err(error),
            }
        }
        let result = (|| {
            let (mut index, stamp) = read(&directory)?;
            change(&mut index);
            index.validate()?;
            require_allowed(allowed)?;
            let bytes = serde_json::to_vec(&index).map_err(io::Error::other)?;
            if bytes.len() as u64 > MAX_BYTES {
                return Err(invalid("Saved SFTP receipts exceed their size limit"));
            }
            match stamp {
                Some(stamp) => directory.replace_with(
                    FILE,
                    |file| file.write_all(&bytes),
                    || {
                        require_allowed(allowed)?;
                        stamp.require_unchanged(&directory.open_private_file(FILE)?)
                    },
                )?,
                None => {
                    require_allowed(allowed)?;
                    directory.publish_copy(FILE, &bytes)?;
                }
            }
            directory.sync()
        })();
        // A child forked during the update may still hold the descriptor until exec.
        let _ = lock.unlock();
        result
    }
}

fn read(directory: &Directory) -> io::Result<(Index, Option<FileStamp>)> {
    let file = match directory.open_private_file(FILE) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((empty_index(), None));
        }
        Err(error) => return Err(error),
    };
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Vec::new();
    (&file).take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    stamp.require_unchanged(&file)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("Saved download receipts exceed their size limit"));
    }
    let index: Index = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    index.validate()?;
    Ok((index, Some(stamp)))
}

fn empty_index() -> Index {
    Index {
        version: 1,
        ..Index::default()
    }
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn invalid(message: &str) -> io::Error {
    io::Error::other(message.to_owned())
}

fn require_allowed(allowed: &impl Fn() -> bool) -> io::Result<()> {
    if allowed() {
        Ok(())
    } else {
        Err(io::ErrorKind::Interrupted.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKuSNsxvt1EJ0ELnEMXiOgkaExGvVly+uWvDAKyIGKcX";

    fn record(root: &Path, number: u8) -> Record {
        let id = format!("{number:032x}");
        let identity = Identity {
            volume: 1,
            file: vec![1; if cfg!(windows) { 16 } else { 8 }],
        };
        Record {
            id: id.clone(),
            route: SshTarget::from_imported_route(
                crate::ssh_dialog::LoginTarget {
                    host: "sftp.example.test".into(),
                    port: 22,
                    username: "user".into(),
                },
                None,
                crate::session_security::IsolationLevel::None,
            )
            .unwrap(),
            host_key: HOST.into(),
            remote: b"/remote/file".to_vec(),
            total: 10,
            remote_modified: 24,
            remote_permissions: 0o100600,
            remote_uid: None,
            remote_gid: None,
            title: "file".into(),
            payload: Payload {
                destination: LocalPath::capture(&root.join("download")),
                stage_name: format!(".kortty-{id}.kortty-part"),
                parent: identity.clone(),
                stage: identity.clone(),
                file: identity,
                offset: 5,
                sha256: "a".repeat(64),
            },
        }
    }

    #[test]
    fn durable_download_receipts_reopen_and_reject_invalid_updates_atomically() {
        let profile = crate::snippets::tests::Profile::new();
        let store = Store::new(&profile.0);
        let first = record(&profile.0, 1);
        store.save(first.clone()).unwrap();
        let reopened = Store::new(&profile.0);
        assert!(reopened.list().unwrap() == [first.clone()]);
        let mut invalid = first.clone();
        invalid.payload.offset = invalid.total + 1;
        assert!(reopened.save(invalid).is_err());
        assert!(reopened.list().unwrap() == [first.clone()]);
        reopened.remove(&first.id).unwrap();
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn durable_paths_preserve_native_non_unicode_names() {
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStringExt as _;
            PathBuf::from(OsString::from_vec(vec![b'/', b'x', 0xff]))
        };
        #[cfg(windows)]
        let path = {
            use std::os::windows::ffi::OsStringExt as _;
            PathBuf::from(OsString::from_wide(&[
                b'C' as u16,
                b':' as u16,
                b'\\' as u16,
                0xd800,
            ]))
        };
        let bytes = serde_json::to_vec(&LocalPath::capture(&path)).unwrap();
        let restored: LocalPath = serde_json::from_slice(&bytes).unwrap();
        assert!(restored.path().unwrap() == path);
    }
}
