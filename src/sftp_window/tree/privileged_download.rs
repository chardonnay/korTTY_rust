use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use crate::sftp_window::files;
use crate::sftp_window::worker::resume::persist::Identity;

use super::MAX_DEPTH;
use super::local::{self, Directory};

pub(in crate::sftp_window) fn validate_depth(parts: usize, directory: bool) -> io::Result<()> {
    // Cleanup starts at the stage root and spends depth only on child directories.
    let depth = parts.checked_sub(usize::from(!directory));
    if parts == 0 || depth.is_none_or(|depth| depth > MAX_DEPTH) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The privileged download exceeds the local directory depth limit",
        ));
    }
    Ok(())
}

pub(in crate::sftp_window) struct Destination {
    path: PathBuf,
    parent: Directory,
    identity: Identity,
    #[cfg(windows)]
    _ancestors: Vec<File>,
}

impl Destination {
    pub(in crate::sftp_window) fn inspect(path: &Path) -> io::Result<Self> {
        let parent_path = path.parent().ok_or_else(changed)?;
        let name = path.file_name().ok_or_else(changed)?;
        files::local_name(name.as_encoded_bytes())?;
        #[cfg(windows)]
        let ancestors = crate::private_files::executable::lock_windows_ancestors(path)?;
        let parent = checked_parent(parent_path)?;
        let identity = Identity::directory(&parent)?;
        require_absent(path)?;
        Ok(Self {
            path: path.to_owned(),
            parent,
            identity,
            #[cfg(windows)]
            _ancestors: ancestors,
        })
    }

    pub(in crate::sftp_window) fn witness(&self) -> io::Result<Vec<u8>> {
        serde_json::to_vec(&self.identity).map_err(io::Error::other)
    }

    fn verify(&self) -> io::Result<()> {
        let current = checked_parent(self.path.parent().ok_or_else(changed)?)?;
        self.identity.require_directory(&current)
    }

    pub(in crate::sftp_window) fn stage(self) -> io::Result<Stage> {
        self.verify()?;
        require_absent(&self.path)?;
        let name = OsString::from(files::nonce()?);
        let root = self.parent.create_child(&name)?;
        let stage = Stage {
            destination: self,
            name,
            root: Some(root),
            folders: BTreeMap::new(),
        };
        private_directory(stage.root()?)?;
        Ok(stage)
    }
}

pub(in crate::sftp_window) struct Stage {
    destination: Destination,
    name: OsString,
    root: Option<Directory>,
    folders: BTreeMap<Vec<OsString>, Directory>,
}

#[derive(Debug)]
pub(in crate::sftp_window) enum PublishError {
    Denied,
    Refused,
    Uncertain,
}

impl Stage {
    fn root(&self) -> io::Result<&Directory> {
        self.root.as_ref().ok_or_else(changed)
    }

    fn parent<'a>(&self, path: &'a [OsString]) -> io::Result<(&Directory, &'a OsStr)> {
        let (name, parts) = path.split_last().ok_or_else(changed)?;
        let directory = if parts.is_empty() {
            self.root()?
        } else {
            self.folders.get(parts).ok_or_else(changed)?
        };
        Ok((directory, name))
    }

    pub(in crate::sftp_window) fn directory(&mut self, path: &[OsString]) -> io::Result<()> {
        validate_depth(path.len(), true)?;
        let (parent, name) = self.parent(path)?;
        let directory = parent.create_child(name)?;
        private_directory(&directory)?;
        self.folders.insert(path.to_vec(), directory);
        Ok(())
    }

    pub(in crate::sftp_window) fn file(&self, path: &[OsString]) -> io::Result<File> {
        validate_depth(path.len(), false)?;
        let (parent, name) = self.parent(path)?;
        let file = parent.create_file(name)?;
        #[cfg(windows)]
        Directory::require_owner_only_file(&file)?;
        #[cfg(unix)]
        private_file(&file, 0o600)?;
        Ok(file)
    }

    pub(in crate::sftp_window) fn publish(
        mut self,
        mut permitted: impl FnMut() -> bool,
    ) -> Result<(), PublishError> {
        self.destination
            .verify()
            .map_err(|_| PublishError::Refused)?;
        let root = self.root().map_err(|_| PublishError::Refused)?;
        private_directory(root).map_err(|_| PublishError::Refused)?;
        #[cfg(unix)]
        {
            for directory in self.folders.values() {
                directory
                    .file()
                    .sync_all()
                    .map_err(|_| PublishError::Refused)?;
            }
            root.file().sync_all().map_err(|_| PublishError::Refused)?;
        }
        self.folders.clear();
        let root = self.root().map_err(|_| PublishError::Refused)?;
        let leaf = self
            .destination
            .path
            .file_name()
            .ok_or(PublishError::Refused)?;
        let identity = Identity::directory(root).map_err(|_| PublishError::Refused)?;
        #[cfg(windows)]
        self.publish_windows(
            root,
            &mut permitted,
            || self.destination.parent.publish_directory_once(root, leaf),
            std::thread::sleep,
        )?;
        #[cfg(unix)]
        {
            self.destination
                .verify()
                .map_err(|_| PublishError::Refused)?;
            if !permitted() {
                return Err(PublishError::Denied);
            }
            if local::publish(&self.destination.parent, &self.name, root, leaf).is_err() {
                return Err(if self.stage_still_named(root) {
                    PublishError::Refused
                } else {
                    PublishError::Uncertain
                });
            }
        }
        self.root.take();
        let published =
            Directory::open_path(&self.destination.path).map_err(|_| PublishError::Uncertain)?;
        identity
            .require_directory(&published)
            .map_err(|_| PublishError::Uncertain)?;
        #[cfg(unix)]
        self.destination
            .parent
            .file()
            .sync_all()
            .map_err(|_| PublishError::Uncertain)?;
        Ok(())
    }

    #[cfg(windows)]
    fn publish_windows(
        &self,
        root: &Directory,
        mut permitted: impl FnMut() -> bool,
        mut rename_once: impl FnMut() -> io::Result<()>,
        mut pause: impl FnMut(std::time::Duration),
    ) -> Result<(), PublishError> {
        let mut failed_attempt = 0;
        loop {
            if !self.stage_still_named(root) {
                return Err(if failed_attempt == 0 {
                    PublishError::Refused
                } else {
                    PublishError::Uncertain
                });
            }
            self.destination
                .verify()
                .map_err(|_| PublishError::Refused)?;
            private_directory(root).map_err(|_| PublishError::Refused)?;
            require_absent(&self.destination.path).map_err(|_| PublishError::Refused)?;
            if !permitted() {
                return Err(PublishError::Denied);
            }
            let Err(error) = rename_once() else {
                return Ok(());
            };
            if !self.stage_still_named(root) {
                return Err(PublishError::Uncertain);
            }
            let delay = Directory::publish_directory_retry_delay(&error, failed_attempt)
                .ok_or(PublishError::Refused)?;
            failed_attempt += 1;
            pause(delay);
        }
    }

    fn stage_still_named(&self, root: &Directory) -> bool {
        matches!(
            local::entry(&self.destination.parent, &self.name),
            Ok(local::Entry::Directory(current)) if root.same_as(&current).unwrap_or(false)
        )
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        self.folders.clear();
        if let Some(root) = self.root.take()
            && self.stage_still_named(&root)
        {
            let _ = local::discard(&self.destination.parent, &self.name, root);
        }
    }
}

fn checked_parent(path: &Path) -> io::Result<Directory> {
    let directory = Directory::open_path(path)?;
    #[cfg(unix)]
    if !crate::private_files::Directory::open_owner_controlled(path)?
        .same_as_file(directory.file())?
    {
        return Err(changed());
    }
    Ok(directory)
}

fn require_absent(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Local destination exists",
        )),
    }
}

fn private_directory(directory: &Directory) -> io::Result<()> {
    #[cfg(windows)]
    return directory.require_owner_only();
    #[cfg(unix)]
    private_file(directory.file(), 0o700)
}

#[cfg(unix)]
fn private_file(file: &File, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let info = file.metadata()?;
    if info.uid() != rustix::process::geteuid().as_raw() || info.mode() & 0o077 != 0 {
        return Err(changed());
    }
    if info.mode() & 0o7777 != mode {
        file.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    #[cfg(target_os = "macos")]
    kortty_files::require_no_extended_acl(file)?;
    Ok(())
}

fn changed() -> io::Error {
    io::Error::other("The private local destination or stage changed")
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn privileged_tree_download_stage_refuses_late_target_and_cancellation() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let target = root.join("download");
        let stage = || {
            let mut stage = Destination::inspect(&target).unwrap().stage().unwrap();
            stage.directory(&["nested".into()]).unwrap();
            stage.directory(&["nested".into(), "empty".into()]).unwrap();
            let mut file = stage.file(&["nested".into(), "bytes".into()]).unwrap();
            file.write_all(b"reviewed content").unwrap();
            file.sync_all().unwrap();
            stage
        };
        let cancelled = stage();
        assert!(matches!(
            cancelled.publish(|| false),
            Err(PublishError::Denied)
        ));
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        let raced = stage();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("sentinel"), b"preserve").unwrap();
        assert!(matches!(raced.publish(|| true), Err(PublishError::Refused)));
        assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"preserve");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_file(target.join("sentinel")).unwrap();
        fs::remove_dir(&target).unwrap();
        stage().publish(|| true).unwrap();
        assert_eq!(
            fs::read(target.join("nested/bytes")).unwrap(),
            b"reviewed content"
        );
        assert!(target.join("nested/empty").is_dir());
    }

    #[cfg(windows)]
    #[test]
    fn windows_privileged_tree_retry_rechecks_authority_and_late_destination() {
        let profile = crate::snippets::tests::Profile::new();
        let parent = profile.0.canonicalize().unwrap();
        let target = parent.join("download");
        let stage = || Destination::inspect(&target).unwrap().stage().unwrap();

        let revoked = stage();
        let allowed = std::cell::Cell::new(true);
        let mut authorizations = 0;
        let mut attempts = 0;
        let result = revoked.publish_windows(
            revoked.root().unwrap(),
            || {
                authorizations += 1;
                allowed.get()
            },
            || {
                attempts += 1;
                Err(io::Error::from_raw_os_error(32))
            },
            |_| allowed.set(false),
        );
        assert!(matches!(result, Err(PublishError::Denied)));
        assert_eq!((authorizations, attempts), (2, 1));
        drop(revoked);
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);

        let raced = stage();
        let mut attempts = 0;
        let result = raced.publish_windows(
            raced.root().unwrap(),
            || true,
            || {
                attempts += 1;
                Err(io::Error::from_raw_os_error(5))
            },
            |_| {
                fs::create_dir(&target).unwrap();
                fs::write(target.join("sentinel"), b"preserve").unwrap();
            },
        );
        assert!(matches!(result, Err(PublishError::Refused)));
        assert_eq!(attempts, 1);
        drop(raced);
        assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"preserve");
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn windows_privileged_tree_retry_is_bounded_and_stops_after_uncertain_rename() {
        let profile = crate::snippets::tests::Profile::new();
        let parent = profile.0.canonicalize().unwrap();
        let target = parent.join("download");
        let stage = Destination::inspect(&target).unwrap().stage().unwrap();
        let root = stage.root().unwrap();
        let mut authorizations = 0;
        let mut attempts = 0;
        let mut waits = Vec::new();
        let result = stage.publish_windows(
            root,
            || {
                authorizations += 1;
                true
            },
            || {
                attempts += 1;
                Err(io::Error::from_raw_os_error(32))
            },
            |delay| waits.push(delay.as_millis()),
        );
        assert!(matches!(result, Err(PublishError::Refused)));
        assert_eq!((authorizations, attempts), (6, 6));
        assert_eq!(waits, [10, 25, 50, 100, 200]);
        assert!(stage.stage_still_named(root) && !target.exists());

        let result = stage.publish_windows(
            root,
            || true,
            || {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "not a Win32 rename error",
                ))
            },
            |_| panic!("Only recognized Win32 errors may be retried"),
        );
        assert!(matches!(result, Err(PublishError::Refused)));

        let identity = Identity::directory(root).unwrap();
        let result = stage.publish_windows(
            root,
            || true,
            || {
                stage
                    .destination
                    .parent
                    .publish(root, target.file_name().unwrap())
                    .expect("fixture rename before reporting an ambiguous result");
                Err(io::Error::from_raw_os_error(32))
            },
            |_| panic!("An ambiguous rename must never be retried"),
        );
        assert!(matches!(result, Err(PublishError::Uncertain)));
        drop(stage);
        identity
            .require_directory(&Directory::open_path(&target).unwrap())
            .unwrap();
    }

    #[test]
    fn privileged_tree_download_depth_boundary_is_fully_cleaned_after_denial() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let mut stage = Destination::inspect(&root.join("download"))
            .unwrap()
            .stage()
            .unwrap();
        let mut parts = Vec::new();
        for _ in 0..MAX_DEPTH {
            parts.push("d".into());
            stage.directory(&parts).unwrap();
        }
        parts.push("f".into());
        let mut file = stage.file(&parts).unwrap();
        file.write_all(b"private depth boundary").unwrap();
        drop(file);
        assert_eq!(
            stage.directory(&parts).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        parts.push("too-deep".into());
        assert_eq!(
            stage.file(&parts).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(matches!(stage.publish(|| false), Err(PublishError::Denied)));
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }
}
