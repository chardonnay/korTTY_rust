//! Durable upload receipts share the private download index and its atomic updates.

use kortty_ssh::sftp::UploadOwner;

use crate::sftp_window::files;
use crate::sftp_window::tree::single_file::UploadSource;

use super::*;

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::sftp_window) enum Phase {
    Writing,
    Ready,
    Discard,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::sftp_window) struct UploadRecord {
    pub(in crate::sftp_window) id: String,
    pub(in crate::sftp_window) route: SshTarget,
    pub(in crate::sftp_window) host_key: String,
    pub(in crate::sftp_window) remote: Vec<u8>,
    pub(in crate::sftp_window) source: UploadSource,
    pub(in crate::sftp_window) uid: u32,
    pub(in crate::sftp_window) gid: u32,
    pub(in crate::sftp_window) title: String,
    pub(in crate::sftp_window) phase: Phase,
}

impl UploadRecord {
    pub(super) fn validate(&self) -> io::Result<()> {
        self.route.validate()?;
        HostKey::from_openssh(&self.host_key)?;
        validate_source(&self.source)?;
        let Some((_, name)) = remote_components(&self.remote) else {
            return Err(invalid("Invalid saved upload destination"));
        };
        if !hex(&self.id, 32)
            || self.remote.len() > 16_384
            || files::validate_transfer_name(name).is_err()
            || self.title.len() > 1024
            || self.title.chars().any(char::is_control)
        {
            return Err(invalid("Invalid saved SFTP upload receipt"));
        }
        Ok(())
    }

    pub(in crate::sftp_window) fn owner(&self) -> UploadOwner {
        UploadOwner {
            uid: self.uid,
            gid: self.gid,
        }
    }

    pub(in crate::sftp_window) fn parent(&self) -> io::Result<&[u8]> {
        remote_components(&self.remote)
            .map(|(parent, _)| parent)
            .ok_or_else(|| invalid("Invalid saved upload destination"))
    }

    pub(in crate::sftp_window) fn stage(&self) -> io::Result<Vec<u8>> {
        if !hex(&self.id, 32) {
            return Err(invalid("Invalid saved upload stage"));
        }
        Ok(files::remote_join(
            self.parent()?,
            format!(".kortty-upload-{}.part", self.id).as_bytes(),
        ))
    }
}

pub(super) fn validate_source(saved: &UploadSource) -> io::Result<()> {
    let source = saved.path.path()?;
    if !source.is_absolute()
        || source.file_name().is_none()
        || source
            .components()
            .any(|part| part == std::path::Component::ParentDir)
        || saved.size > MAX_PARTIAL_BYTES
        || !hex(&saved.sha256, 64)
        || [&saved.parent, &saved.file]
            .into_iter()
            .any(|identity| identity.file.len() != if cfg!(windows) { 16 } else { 8 })
    {
        return Err(invalid("Invalid saved SFTP upload source"));
    }
    Ok(())
}

pub(in crate::sftp_window) fn remote_components(path: &[u8]) -> Option<(&[u8], &[u8])> {
    if !path.starts_with(b"/") || path.starts_with(b"//") || path.contains(&0) {
        return None;
    }
    let split = path.iter().rposition(|&byte| byte == b'/')?;
    let parent = &path[..split.max(1)];
    let name = &path[split + 1..];
    if files::validate_transfer_name(name).is_err()
        || (parent != b"/"
            && parent[1..]
                .split(|&byte| byte == b'/')
                .any(|part| files::validate_remote_name(part).is_err()))
    {
        return None;
    }
    Some((parent, name))
}

/// The exact saved transfer is owned by one worker through prefix checks, writes and rename.
pub(in crate::sftp_window) struct Lease {
    directory: Directory,
    name: String,
    stamp: FileStamp,
    file: File,
    retained: bool,
}

impl Lease {
    pub(in crate::sftp_window) fn retain(&mut self, retained: bool) {
        self.retained = retained;
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if !self.retained {
            let _ = self.directory.remove_unchanged(&self.name, &self.stamp);
        }
        let _ = self.file.unlock();
    }
}

impl Store {
    pub(in crate::sftp_window) fn uploads(&self) -> io::Result<Vec<UploadRecord>> {
        self.index().map(|index| index.uploads)
    }

    pub(in crate::sftp_window) fn save_upload(
        &self,
        record: UploadRecord,
        allowed: &impl Fn() -> bool,
    ) -> io::Result<()> {
        record.validate()?;
        self.change(allowed, |index| {
            if let Some(old) = index.uploads.iter_mut().find(|old| old.id == record.id) {
                *old = record;
            } else {
                index.uploads.push(record);
            }
        })
    }

    pub(in crate::sftp_window) fn remove_upload(
        &self,
        id: &str,
        allowed: &impl Fn() -> bool,
    ) -> io::Result<()> {
        self.change(allowed, |index| {
            index.uploads.retain(|record| record.id != id)
        })
    }

    pub(in crate::sftp_window) fn lease_upload(
        &self,
        id: &str,
        retained: bool,
    ) -> io::Result<Lease> {
        if !hex(id, 32) {
            return Err(invalid("Invalid saved upload stage"));
        }
        self.lease(
            format!(".upload-{id}.lock"),
            retained,
            "Another transfer owns this saved upload",
        )
    }

    pub(in crate::sftp_window) fn lease_replacement_target(
        &self,
        key: &[u8; 32],
    ) -> io::Result<Lease> {
        let key: String = key.iter().map(|byte| format!("{byte:02x}")).collect();
        self.lease(
            format!(".replacement-target-{key}.lock"),
            false,
            "Another reviewed replacement owns this server destination",
        )
    }

    fn lease(&self, name: String, retained: bool, busy: &'static str) -> io::Result<Lease> {
        let directory = Directory::open(&self.0, true)?.private_child("sftp-resume", true)?;
        let file = directory.open_lock(&name)?;
        require_single_link(&file)?;
        file.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => io::Error::new(io::ErrorKind::WouldBlock, busy),
            TryLockError::Error(error) => error,
        })?;
        let stamp = FileStamp::read(&file)?;
        // A waiter can have opened the old inode before a previous owner unlinked it.
        stamp.require_unchanged(&directory.open_private_file(&name)?)?;
        Ok(Lease {
            directory,
            name,
            stamp,
            file,
            retained,
        })
    }
}

#[cfg(all(test, unix))]
#[test]
fn released_upload_lease_unlocks_a_cloned_file_description() {
    let profile = crate::snippets::tests::Profile::new();
    let store = Store::new(&profile.0);
    let id = "a".repeat(32);
    let lease = store.lease_upload(&id, true).unwrap();
    let inherited = lease.file.try_clone().unwrap();
    drop(lease);
    let reopened = store.lease_upload(&id, false).unwrap();
    drop(inherited);
    drop(reopened);
}
