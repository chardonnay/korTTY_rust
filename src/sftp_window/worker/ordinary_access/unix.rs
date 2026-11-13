use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt as _;

use rustix::fs::{
    AtFlags, FileType, Mode, OFlags, RenameFlags, fchmod, fstat, openat, renameat_with, statat,
    unlinkat,
};

use crate::sftp_window::local_tree::Directory;

#[cfg(target_os = "linux")]
mod acl;
#[cfg(target_os = "linux")]
use acl::Acl;
#[cfg(target_os = "macos")]
use kortty_files::ExtendedAcl as Acl;

pub(super) struct NativeAccess {
    parent: ParentAccess,
    file: FileAccess,
}

impl NativeAccess {
    pub(super) fn capture(parent: &Directory) -> io::Result<Self> {
        let parent_access = ParentAccess::capture(parent)?;
        let mut probe = Probe::create(parent)?;
        let result = (|| {
            let file = FileAccess::capture(&probe.file)?;
            parent_access.require_parent(parent)?;
            Ok(Self {
                parent: parent_access,
                file,
            })
        })();
        // Anonymous probes have no directory entry. Named fallbacks are moved
        // into a private directory before identity verification and removal.
        probe.remove()?;
        result
    }

    pub(super) fn capture_directory(parent: &Directory) -> io::Result<Self> {
        let parent_access = ParentAccess::capture(parent)?;
        let mut probe = Probe::create_directory(parent)?;
        let result = (|| {
            let file = FileAccess::capture_directory(&probe.file)?;
            parent_access.require_parent(parent)?;
            Ok(Self {
                parent: parent_access,
                file,
            })
        })();
        probe.remove()?;
        result
    }

    pub(super) fn require_parent(&self, parent: &Directory) -> io::Result<()> {
        self.parent.require_parent(parent)
    }

    pub(super) fn apply_to(&self, file: &File) -> io::Result<()> {
        if self.file.directory {
            return Err(io::Error::other(
                "Directory creation rights cannot finalize a file",
            ));
        }
        self.file.apply_to(file)
    }

    pub(super) fn apply_to_directory(&self, file: &File) -> io::Result<()> {
        if !self.file.directory {
            return Err(io::Error::other(
                "File creation rights cannot finalize a directory",
            ));
        }
        self.file.apply_to(file)
    }
}

#[derive(Eq, PartialEq)]
struct FileAccess {
    directory: bool,
    mode: u32,
    uid: u32,
    gid: u32,
    acl: Acl,
    #[cfg(target_os = "linux")]
    defaults: Option<Acl>,
}

impl FileAccess {
    fn capture(file: &File) -> io::Result<Self> {
        Self::capture_kind(file, false)
    }

    fn capture_directory(file: &File) -> io::Result<Self> {
        Self::capture_kind(file, true)
    }

    fn capture_kind(file: &File, directory: bool) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file()
        } {
            return Err(io::Error::other(
                "The ordinary creation sample has a different entry type",
            ));
        }
        Ok(Self {
            directory,
            mode: metadata.mode() & 0o7777,
            uid: metadata.uid(),
            gid: metadata.gid(),
            acl: Acl::capture(file)?,
            #[cfg(target_os = "linux")]
            defaults: directory.then(|| Acl::capture_default(file)).transpose()?,
        })
    }

    fn apply_to(&self, file: &File) -> io::Result<()> {
        let metadata = file.metadata()?;
        if (if self.directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file()
        }) || metadata.uid() != self.uid
            || metadata.gid() != self.gid
        {
            return Err(io::Error::other(
                "The private file's creation ownership changed",
            ));
        }
        // An ordinary payload root behind a private ancestor may already have
        // the exact inherited state. Rewriting an ACL or mode can clear an
        // inherited SGID bit when its group is not in the caller's group list.
        if Self::capture_kind(file, self.directory)? == *self {
            return Ok(());
        }
        self.acl.apply_to(file)?;
        #[cfg(target_os = "linux")]
        if let Some(defaults) = &self.defaults {
            defaults.apply_default_to(file)?;
        }
        if file.metadata()?.mode() & 0o7777 != self.mode {
            fchmod(file, Mode::from_raw_mode(self.mode as _))?;
        }
        if Self::capture_kind(file, self.directory)? == *self {
            Ok(())
        } else {
            Err(io::Error::other(
                "Ordinary file creation permissions could not be verified",
            ))
        }
    }
}

#[derive(Eq, PartialEq)]
struct ParentAccess {
    mode: u32,
    uid: u32,
    gid: u32,
    access: Acl,
    #[cfg(target_os = "linux")]
    defaults: Acl,
}

impl ParentAccess {
    fn capture(parent: &Directory) -> io::Result<Self> {
        let file = parent.file();
        let metadata = file.metadata()?;
        Ok(Self {
            mode: metadata.mode() & 0o7777,
            uid: metadata.uid(),
            gid: metadata.gid(),
            access: Acl::capture(file)?,
            #[cfg(target_os = "linux")]
            defaults: Acl::capture_default(file)?,
        })
    }

    fn require_parent(&self, parent: &Directory) -> io::Result<()> {
        if Self::capture(parent)? == *self {
            Ok(())
        } else {
            Err(io::Error::other(
                "The destination directory permissions changed during the transfer",
            ))
        }
    }
}

struct Probe<'a> {
    parent: &'a Directory,
    name: Option<OsString>,
    file: File,
    removed: bool,
}

impl<'a> Probe<'a> {
    fn create_directory(parent: &'a Directory) -> io::Result<Self> {
        let name = OsString::from(format!(
            ".kortty-creation-directory-{}",
            crate::private_files::nonce()?
        ));
        rustix::fs::mkdirat(parent.file(), name.as_os_str(), Mode::from_raw_mode(0o777))?;
        let file = openat(parent.file(), name.as_os_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty()).map_err(|error| io::Error::other(format!(
                "Could not sample ordinary directory permissions; an empty creation probe may remain at {} ({error})",
                name.to_string_lossy())))?;
        Ok(Self {
            parent,
            name: Some(name),
            file: File::from(file),
            removed: false,
        })
    }

    fn create(parent: &'a Directory) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            use rustix::io::Errno;
            match openat(
                parent.file(),
                ".",
                OFlags::TMPFILE | OFlags::WRONLY | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o666),
            ) {
                Ok(file) => {
                    return Ok(Self {
                        parent,
                        name: None,
                        file: File::from(file),
                        removed: false,
                    });
                }
                // These are the documented old-kernel/filesystem indications
                // that this known-valid O_TMPFILE request is unsupported.
                Err(
                    Errno::OPNOTSUPP | Errno::INVAL | Errno::ISDIR | Errno::NOENT | Errno::NOSYS,
                ) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Self::create_named(parent)
    }

    fn create_named(parent: &'a Directory) -> io::Result<Self> {
        let name = OsString::from(format!(
            ".kortty-creation-{}",
            crate::private_files::nonce()?
        ));
        // The original descriptor remains usable for metadata even if the
        // native umask or inherited ACL gives the new file no read access.
        let file = File::from(openat(
            parent.file(),
            name.as_os_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o666),
        )?);
        Ok(Self {
            parent,
            name: Some(name),
            file,
            removed: false,
        })
    }

    fn remove(&mut self) -> io::Result<()> {
        if self.removed {
            return Ok(());
        }
        let expected = fstat(&self.file)?;
        if let Some(name) = &self.name {
            let mut quarantine = Quarantine::create(self.parent)?;
            renameat_with(
                self.parent.file(),
                name.as_os_str(),
                quarantine.directory.file(),
                OsStr::new("probe"),
                RenameFlags::NOREPLACE,
            )?;
            // The public name is no longer ours. No later error or destructor
            // may retry deleting it, including after a foreign entry is restored.
            self.removed = true;
            let result = quarantine.dispose(name, &expected);
            let cleanup = quarantine.remove_empty();
            result.and(cleanup)
        } else {
            self.removed = true;
            require_empty(&expected)
        }
    }
}

struct Quarantine<'a> {
    parent: &'a Directory,
    name: OsString,
    directory: Directory,
    removed: bool,
}

impl<'a> Quarantine<'a> {
    fn create(parent: &'a Directory) -> io::Result<Self> {
        let name = OsString::from(format!(
            ".kortty-creation-cleanup-{}",
            crate::private_files::nonce()?
        ));
        let directory = parent.create_child(&name)?;
        Ok(Self {
            parent,
            name,
            directory,
            removed: false,
        })
    }

    fn dispose(&self, original_name: &OsStr, expected: &rustix::fs::Stat) -> io::Result<()> {
        let actual = statat(self.directory.file(), "probe", AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|error| io::Error::other(format!("Could not verify the creation probe in destination recovery folder {}/probe ({error})", self.name.to_string_lossy())))?;
        let kind = FileType::from_raw_mode(expected.st_mode);
        if (actual.st_dev, actual.st_ino) != (expected.st_dev, expected.st_ino)
            || FileType::from_raw_mode(actual.st_mode) != kind
        {
            return match renameat_with(
                self.directory.file(),
                "probe",
                self.parent.file(),
                original_name,
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) => Err(io::Error::other(
                    "The creation-permission probe name changed; the unrelated entry was restored",
                )),
                Err(error) => Err(io::Error::other(format!(
                    "The creation-permission probe name changed; preserved the unrelated entry in destination recovery folder {}/probe ({error})",
                    self.name.to_string_lossy()
                ))),
            };
        }
        // Only this user can modify names inside the held private quarantine.
        // The public destination name is never unlinked after an identity check.
        let flags = if kind == FileType::Directory {
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        };
        unlinkat(self.directory.file(), "probe", flags).map_err(|error| io::Error::other(format!("Could not remove the creation probe in destination recovery folder {}/probe ({error})", self.name.to_string_lossy())))?;
        if kind == FileType::Directory {
            Ok(())
        } else {
            require_empty(&actual)
        }
    }

    fn remove_empty(&mut self) -> io::Result<()> {
        if self.removed {
            return Ok(());
        }
        let actual = statat(
            self.parent.file(),
            self.name.as_os_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )?;
        let expected = fstat(self.directory.file())?;
        if (actual.st_dev, actual.st_ino) != (expected.st_dev, expected.st_ino) {
            return Err(io::Error::other(format!(
                "The creation-probe cleanup folder name changed: {}",
                self.name.to_string_lossy()
            )));
        }
        // RMDIR cannot remove an entry's contents. In particular, a preserved
        // foreign file remains available for recovery after a failed restore.
        unlinkat(
            self.parent.file(),
            self.name.as_os_str(),
            AtFlags::REMOVEDIR,
        )
        .map_err(|error| {
            io::Error::other(format!(
                "Creation-probe recovery folder remains: {} ({error})",
                self.name.to_string_lossy()
            ))
        })?;
        self.removed = true;
        Ok(())
    }
}

impl Drop for Quarantine<'_> {
    fn drop(&mut self) {
        let _ = self.remove_empty();
    }
}

fn require_empty(metadata: &rustix::fs::Stat) -> io::Result<()> {
    if metadata.st_size == 0 {
        Ok(())
    } else {
        Err(io::Error::other(
            "The empty creation-permission probe was written by another process",
        ))
    }
}

impl Drop for Probe<'_> {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn directory_probe_never_deletes_added_contents_or_a_replacement_entry() {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let parent = Directory::open_path(&root).unwrap();
        let mut probe = Probe::create_directory(&parent).unwrap();
        let name = probe.name.as_ref().unwrap().clone();
        std::fs::write(root.join(&name).join("foreign"), b"preserve contents").unwrap();
        let error = probe.remove().unwrap_err();
        drop(probe);
        assert!(error.to_string().contains("recovery folder"));
        assert!(!root.join(name).exists());
        let recovered = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".kortty-creation-cleanup-")
            })
            .unwrap();
        assert_eq!(
            std::fs::read(recovered.join("probe/foreign")).unwrap(),
            b"preserve contents"
        );
        let mut probe = Probe::create_directory(&parent).unwrap();
        let path = root.join(probe.name.as_ref().unwrap());
        std::fs::rename(&path, root.join("moved-directory")).unwrap();
        std::fs::write(&path, b"new ordinary file").unwrap();
        assert!(probe.remove().is_err());
        drop(probe);
        assert_eq!(std::fs::read(path).unwrap(), b"new ordinary file");
    }

    #[test]
    fn changed_owned_probe_is_removed_but_a_replaced_name_is_preserved() {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let parent = Directory::open_path(&root).unwrap();
        let mut probe = Probe::create_named(&parent).unwrap();
        probe.file.write_all(b"foreign write").unwrap();
        assert!(probe.remove().is_err());
        assert!(!root.join(probe.name.as_ref().unwrap()).exists());
        drop(probe);
        let mut probe = Probe::create_named(&parent).unwrap();
        let path = root.join(probe.name.as_ref().unwrap());
        std::fs::rename(&path, root.join("moved-probe")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        assert!(probe.remove().is_err());
        drop(probe);
        assert_eq!(std::fs::read(path).unwrap(), b"replacement");
    }

    #[test]
    fn failed_foreign_restore_preserves_both_entries_and_reports_recovery_folder() {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let parent = Directory::open_path(&root).unwrap();
        let mut probe = Probe::create_named(&parent).unwrap();
        let original_name = probe.name.as_ref().unwrap();
        let expected = fstat(&probe.file).unwrap();
        std::fs::rename(root.join(original_name), root.join("original-probe")).unwrap();
        std::fs::write(root.join(original_name), b"foreign entry").unwrap();
        let quarantine = Quarantine::create(&parent).unwrap();
        renameat_with(
            parent.file(),
            original_name.as_os_str(),
            quarantine.directory.file(),
            "probe",
            RenameFlags::NOREPLACE,
        )
        .unwrap();
        probe.removed = true;
        std::fs::write(root.join(original_name), b"late destination arrival").unwrap();
        let error = quarantine.dispose(original_name, &expected).unwrap_err();
        assert!(
            error
                .to_string()
                .contains(quarantine.name.to_string_lossy().as_ref())
        );
        let recovery = root.join(&quarantine.name).join("probe");
        drop(quarantine);
        assert_eq!(
            std::fs::read(root.join(original_name)).unwrap(),
            b"late destination arrival"
        );
        assert_eq!(std::fs::read(&recovery).unwrap(), b"foreign entry");
        assert_eq!(
            std::fs::metadata(root.join("original-probe"))
                .unwrap()
                .len(),
            0
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn anonymous_probe_supports_zero_rights_without_a_public_directory_entry() {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let parent = Directory::open_path(&root).unwrap();
        let mut probe = Probe::create(&parent).unwrap();
        assert!(
            probe.name.is_none(),
            "The native Linux test filesystem must support O_TMPFILE"
        );
        fchmod(&probe.file, Mode::empty()).unwrap();
        assert_eq!(FileAccess::capture(&probe.file).unwrap().mode, 0);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        probe.remove().unwrap();
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }
}
