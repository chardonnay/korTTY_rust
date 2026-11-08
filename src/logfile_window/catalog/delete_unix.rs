use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use rustix::fs::{
    AtFlags, FileType, Mode, OFlags, RenameFlags, fstat, mkdirat, openat, renameat_with, statat,
    unlinkat,
};

use super::{Stamp, invalid};

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

pub(super) fn remove(
    parent: &File,
    path: &Path,
    expected: &Stamp,
    guard: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let stage = Stage::new(parent, path)?;
    stage.claim()?;
    stage.remove_or_restore(expected, guard)
}

struct Stage<'a> {
    parent: &'a File,
    source: &'a Path,
    source_name: &'a OsStr,
    name: String,
    directory: File,
}

impl<'a> Stage<'a> {
    fn new(parent: &'a File, source: &'a Path) -> io::Result<Self> {
        let source_name = source
            .file_name()
            .ok_or_else(|| invalid("The log has no filename"))?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(io::Error::other)?;
        let name = format!(".kortty-logfile-delete-{:032x}", u128::from_le_bytes(nonce));
        mkdirat(parent, &name, Mode::RUSR | Mode::WUSR | Mode::XUSR)?;
        let directory = File::from(openat(parent, &name, DIRECTORY, Mode::empty())?);
        let stage = Self {
            parent,
            source,
            source_name,
            name,
            directory,
        };
        let metadata = stage.directory.metadata()?;
        if metadata.mode() & 0o777 != 0o700 || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The logfile staging directory is not private to the current user",
            ));
        }
        #[cfg(target_os = "macos")]
        kortty_files::require_no_extended_acl(&stage.directory)?;
        Ok(stage)
    }

    fn claim(&self) -> io::Result<()> {
        // Atomic move without following a symlink or replacing an existing entry.
        Ok(renameat_with(
            self.parent,
            self.source_name,
            &self.directory,
            self.source_name,
            RenameFlags::NOREPLACE,
        )?)
    }

    fn remove_or_restore(
        &self,
        expected: &Stamp,
        guard: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        let delete = || -> io::Result<()> {
            let staged = statat(&self.directory, self.source_name, AtFlags::SYMLINK_NOFOLLOW)?;
            if FileType::from_raw_mode(staged.st_mode) != FileType::RegularFile {
                return Err(invalid(
                    "The logfile was replaced by a different type of entry",
                ));
            }
            let file = File::from(openat(
                &self.directory,
                self.source_name,
                OFlags::RDONLY
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::NOCTTY
                    | OFlags::CLOEXEC,
                Mode::empty(),
            )?);
            guard()?;
            if Stamp::new(&file)? != *expected {
                return Err(invalid("The logfile changed before it could be deleted"));
            }
            // The verified entry is now in an owner-only directory.
            Ok(unlinkat(
                &self.directory,
                self.source_name,
                AtFlags::empty(),
            )?)
        };
        match delete() {
            Ok(()) => Ok(()),
            Err(error) => self.restore(error),
        }
    }

    fn restore(&self, error: io::Error) -> io::Result<()> {
        match renameat_with(
            &self.directory,
            self.source_name,
            self.parent,
            self.source_name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => Err(error),
            Err(restore) => {
                let location = if let Some(directory) = open_directory_path(&self.directory) {
                    format!(
                        "Staged entry preserved at {}",
                        directory.join(self.source_name).display()
                    )
                } else {
                    let original_stage = self
                        .source
                        .with_file_name(&self.name)
                        .join(self.source_name);
                    format!(
                        "Staged entry preserved. Its original recovery path was {}",
                        original_stage.display()
                    )
                };
                Err(io::Error::new(
                    error.kind(),
                    format!("{error}. {location}. Restore failed: {restore}"),
                ))
            }
        }
    }
}

impl Drop for Stage<'_> {
    fn drop(&mut self) {
        // Only remove an empty staging directory. A failed restore leaves its
        // entry intact, and an unexpected directory replacement is left alone.
        let Ok(actual) = statat(self.parent, &self.name, AtFlags::SYMLINK_NOFOLLOW) else {
            return;
        };
        let Ok(expected) = fstat(&self.directory) else {
            return;
        };
        if FileType::from_raw_mode(actual.st_mode) == FileType::Directory
            && (actual.st_dev, actual.st_ino) == (expected.st_dev, expected.st_ino)
            && expected.st_uid == rustix::process::geteuid().as_raw()
        {
            let _ = unlinkat(self.parent, &self.name, AtFlags::REMOVEDIR);
        }
    }
}

#[cfg(target_os = "macos")]
fn open_directory_path(directory: &File) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;

    rustix::fs::getpath(directory)
        .ok()
        .map(|path| PathBuf::from(OsStr::from_bytes(path.to_bytes())))
}

#[cfg(target_os = "linux")]
fn open_directory_path(directory: &File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd as _;

    std::fs::read_link(format!("/proc/self/fd/{}", directory.as_raw_fd())).ok()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::sftp_window::LocalSource;
    use crate::snippets::tests::Profile;

    #[test]
    fn replacement_after_the_initial_check_is_restored_without_removal() {
        let fixture = Profile::new();
        let directory = fixture.0.canonicalize().unwrap();
        let path = directory.join("2026-10-30-07-23-10-source_1.log");
        let original = directory.join("moved-original");
        fs::write(&path, b"original").unwrap();
        let source = LocalSource::open(&path).unwrap();
        let expected = Stamp::new(source.file()).unwrap();
        source.verify().unwrap();

        let stage = Stage::new(source.parent_file(), &path).unwrap();
        fs::rename(&path, &original).unwrap();
        fs::write(&path, b"replaced").unwrap();
        stage.claim().unwrap();
        assert!(!path.exists());
        assert!(stage.remove_or_restore(&expected, || Ok(())).is_err());
        drop(stage);

        assert_eq!(fs::read(&path).unwrap(), b"replaced");
        assert_eq!(fs::read(&original).unwrap(), b"original");
        assert_eq!(fs::read_dir(directory).unwrap().count(), 2);
    }

    #[test]
    fn failed_restore_reports_preserved_stage_without_overwriting_a_new_file() {
        let fixture = Profile::new();
        let directory = fixture.0.canonicalize().unwrap();
        let path = directory.join("2026-10-30-07-23-10-source_1.log");
        fs::write(&path, b"original").unwrap();
        let source = LocalSource::open(&path).unwrap();
        let expected = Stamp::new(source.file()).unwrap();

        let stage = Stage::new(source.parent_file(), &path).unwrap();
        stage.claim().unwrap();
        fs::write(&path, b"new file").unwrap();
        let recovered = directory.join(&stage.name).join(path.file_name().unwrap());
        let held = io::Error::new(io::ErrorKind::PermissionDenied, "New Hold");
        let failure = stage
            .remove_or_restore(&expected, || Err(held))
            .unwrap_err();
        assert_eq!(failure.kind(), io::ErrorKind::PermissionDenied);
        assert!(failure.to_string().contains(&*recovered.to_string_lossy()));
        drop(stage);

        assert_eq!(fs::read(&path).unwrap(), b"new file");
        assert_eq!(fs::read(&recovered).unwrap(), b"original");
    }

    #[test]
    fn post_claim_guards_follow_the_open_parent_after_its_path_is_replaced() {
        use super::super::{Badge, scan};

        for add_hold in [false, true] {
            let fixture = Profile::new();
            let root = fixture.0.canonicalize().unwrap();
            let directory = root.join("terminal-logs");
            fs::create_dir(&directory).unwrap();
            let filename = "2026-10-30-07-23-10-source_1.log.gz";
            let path = directory.join(filename);
            fs::write(&path, b"original").unwrap();
            let entry = scan(&root, || false).unwrap().entries.remove(0);
            let source = entry.require_closed().unwrap();
            entry.require_deletable(&source).unwrap();
            let stage = Stage::new(source.parent_file(), &path).unwrap();
            stage.claim().unwrap();

            let moved = root.join("moved-parent");
            fs::rename(&directory, &moved).unwrap();
            fs::create_dir(&directory).unwrap();
            fs::write(&path, b"new file").unwrap();
            if add_hold {
                let badge = moved.join(entry.badge_name());
                fs::write(badge, serde_json::to_vec(&[Badge::hold()]).unwrap()).unwrap();
            } else {
                fs::write(moved.join(format!(".{filename}.lock")), b"lock").unwrap();
            }

            let failure = stage
                .remove_or_restore(&entry.stamp, || entry.require_deletable(&source))
                .unwrap_err();
            assert!(
                failure
                    .to_string()
                    .contains(if add_hold { "held" } else { "active" })
            );
            drop(stage);
            assert_eq!(fs::read(moved.join(filename)).unwrap(), b"original");
            assert_eq!(fs::read(&path).unwrap(), b"new file");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn inherited_darwin_acl_blocks_the_move_even_with_private_posix_bits() {
        let fixture = Profile::new();
        let directory = fixture.0.canonicalize().unwrap();
        let path = directory.join("2026-10-30-07-23-10-source_1.log");
        fs::write(&path, b"original").unwrap();
        assert!(
            std::process::Command::new("/bin/chmod")
                .args(["+a", "everyone allow list,search,add_file,add_subdirectory,delete_child,directory_inherit"])
                .arg(&directory)
                .status().unwrap().success()
        );
        let source = LocalSource::open(&path).unwrap();
        let expected = Stamp::new(source.file()).unwrap();
        let failure = remove(source.parent_file(), &path, &expected, || Ok(())).unwrap_err();
        assert_eq!(failure.kind(), io::ErrorKind::PermissionDenied);
        assert!(failure.to_string().contains("inherited an extended ACL"));
        assert_eq!(fs::read(path).unwrap(), b"original");
        assert_eq!(fs::read_dir(directory).unwrap().count(), 1);
    }
}
