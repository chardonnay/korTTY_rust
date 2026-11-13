//! A private read-only Mach-O image preserving the approved source's OS metadata.

use std::fs::{File, Permissions};
use std::io::{self, Read as _, Seek as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use crate::private_files::executable::{self, MAX_EXECUTABLE, Opened};

use super::{Error, Program, Scratch, active};

const IMAGE: &str = ".approved-cli-image";
const DARWIN_MNT_NOEXEC: u32 = 0x0000_0004;

pub(super) struct Image {
    original: PathBuf,
    path: PathBuf,
    opened: Opened,
    security_metadata: kortty_macos::ExecutableMetadata,
}

impl Image {
    pub(super) fn prepare(
        program: &Program,
        source: &mut File,
        scratch: &Scratch,
        deadline: Instant,
        canceled: &AtomicBool,
    ) -> Result<Self, Error> {
        let refused = || {
            Error::Review("Could not prepare a verified CLI executable copy with its original security metadata.".into())
        };
        require_executable(&program.path, source).map_err(|_| refused())?;
        let directory = scratch
            .directory
            .as_ref()
            .expect("owned private working directory");
        let mut security_metadata = None;
        let staged = directory.publish_checked_readable(
            IMAGE,
            |destination| {
                copy_data(source, destination, deadline, canceled)?;
                security_metadata =
                    Some(kortty_macos::copy_executable_metadata(source, destination)?);
                destination.set_permissions(Permissions::from_mode(0o500))
            },
            || scratch.check(),
        );
        active(deadline, canceled)?;
        staged.map_err(|_| refused())?;
        let path = scratch.path.join(IMAGE);
        let mut opened = executable::open(&path).map_err(|_| refused())?;
        executable::require_native_image(&mut opened.file).map_err(|_| refused())?;
        let digest = executable::digest(&mut opened.file, Some(canceled));
        active(deadline, canceled)?;
        if digest.map_err(|_| refused())? != program.fingerprint {
            return Err(refused());
        }
        let image = Self {
            original: program.path.clone(),
            path,
            opened,
            security_metadata: security_metadata.ok_or_else(refused)?,
        };
        image.require_current(source).map_err(|_| refused())?;
        Ok(image)
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn require_current(&self, source: &File) -> io::Result<()> {
        require_executable(&self.original, source)?;
        self.opened.recheck(&self.path)?;
        let metadata = self.opened.file.metadata()?;
        if metadata.mode() & 0o7777 != 0o500
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.nlink() != 1
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The staged executable must be private and read-only",
            ));
        }
        require_executable(&self.path, &self.opened.file)?;
        self.security_metadata.verify(source, &self.opened.file)
    }
}

fn require_executable(path: &Path, file: &File) -> io::Result<()> {
    if rustix::fs::fstatfs(file)?.f_flags & DARWIN_MNT_NOEXEC != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The executable volume disallows execution",
        ));
    }
    rustix::fs::access(path, rustix::fs::Access::EXEC_OK)?;
    Ok(())
}

fn copy_data(
    source: &mut File,
    destination: &mut File,
    deadline: Instant,
    canceled: &AtomicBool,
) -> io::Result<()> {
    let expected = source.metadata()?.len();
    if !(1..=MAX_EXECUTABLE).contains(&expected) {
        return Err(io::Error::other("The executable exceeds the staging limit"));
    }
    source.rewind()?;
    let mut copied = 0;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        active(deadline, canceled).map_err(|_| {
            io::Error::new(io::ErrorKind::Interrupted, "Executable staging stopped")
        })?;
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        copied += count as u64;
        if copied > expected {
            break;
        }
        destination.write_all(&buffer[..count])?;
    }
    if copied != expected {
        return Err(io::Error::other(
            "The executable size changed during staging",
        ));
    }
    Ok(())
}
