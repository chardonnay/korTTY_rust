//! Held-descriptor POSIX permission preservation for reviewed folder sync.

use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt as _;

use rustix::fs::{Mode, fchmod};

const RWX: u32 = 0o777;
const PERMISSION_BITS: u32 = 0o7777;

pub(in crate::sftp_window::worker) struct Metadata {
    mode: u32,
}

impl std::fmt::Debug for Metadata {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UnixReplacementMetadata")
            .finish_non_exhaustive()
    }
}

impl Metadata {
    pub(in crate::sftp_window::worker) fn for_new_file(remote_mode: Option<u32>) -> Self {
        Self {
            mode: remote_mode.map_or(0o644, |mode| mode & 0o755),
        }
    }

    pub(in crate::sftp_window::worker::sync::replacement) fn capture(
        file: &File,
    ) -> io::Result<Self> {
        Ok(Self {
            mode: regular_mode(file)? & RWX,
        })
    }

    pub(in crate::sftp_window::worker::sync::replacement) fn verify_original(
        &self,
        file: &File,
    ) -> io::Result<()> {
        if regular_mode(file)? & RWX == self.mode {
            Ok(())
        } else {
            Err(io::Error::other(
                "The reviewed local file permissions changed",
            ))
        }
    }

    pub(in crate::sftp_window::worker) fn apply_to(&self, file: &File) -> io::Result<()> {
        regular_mode(file)?;
        // Folder sync carries the nine POSIX rwx bits to new content. Special
        // bits from either the original or the stage must never be restored.
        // All nine rwx bits fit both the 16-bit and 32-bit POSIX mode types.
        fchmod(file, Mode::from_raw_mode(self.mode as _))?;
        if regular_mode(file)? & PERMISSION_BITS == self.mode {
            Ok(())
        } else {
            Err(io::Error::other(
                "The staged local file permissions could not be verified",
            ))
        }
    }
}

fn regular_mode(file: &File) -> io::Result<u32> {
    let metadata = file.metadata()?;
    if metadata.is_file() {
        Ok(metadata.mode())
    } else {
        Err(io::Error::other(
            "Reviewed replacement requires a regular local file",
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn new_download_modes_preserve_execute_bits_mask_unsafe_bits_and_use_only_absent_fallback() {
        let fixture = crate::snippets::tests::Profile::new();
        for (index, (remote_mode, expected)) in [
            (None, 0o644),
            (Some(0o6777), 0o755),
            (Some(0o1666), 0o644),
            (Some(0), 0),
        ]
        .into_iter()
        .enumerate()
        {
            let path = fixture.0.join(format!("download-{index}"));
            let file = File::options()
                .write(true)
                .create_new(true)
                .open(path)
                .unwrap();
            file.set_permissions(fs::Permissions::from_mode(0o4700))
                .unwrap();
            Metadata::for_new_file(remote_mode).apply_to(&file).unwrap();
            assert_eq!(
                file.metadata().unwrap().mode() & PERMISSION_BITS,
                expected,
                "{remote_mode:?}"
            );
        }
    }

    #[test]
    fn held_file_mode_preserves_execute_bits_clears_setid_and_denies_stale_review() {
        let fixture = crate::snippets::tests::Profile::new();
        let original_path = fixture.0.join("original");
        let stage_path = fixture.0.join("stage");
        fs::write(&original_path, b"original contents").unwrap();
        fs::write(&stage_path, b"replacement contents").unwrap();
        let original = File::open(&original_path).unwrap();
        let staged = File::options().write(true).open(&stage_path).unwrap();
        original
            .set_permissions(fs::Permissions::from_mode(0o4751))
            .unwrap();
        let reviewed = Metadata::capture(&original).unwrap();
        reviewed.verify_original(&original).unwrap();
        staged
            .set_permissions(fs::Permissions::from_mode(0o4700))
            .unwrap();
        reviewed.apply_to(&staged).unwrap();
        assert_eq!(staged.metadata().unwrap().mode() & PERMISSION_BITS, 0o751);
        assert_eq!(original.metadata().unwrap().mode() & RWX, 0o751);

        // The review remains bound to its held original after a path moves.
        fs::rename(&original_path, fixture.0.join("retained")).unwrap();
        fs::write(&original_path, b"different file at the old name").unwrap();
        reviewed.verify_original(&original).unwrap();
        original
            .set_permissions(fs::Permissions::from_mode(0o741))
            .unwrap();
        assert!(reviewed.verify_original(&original).is_err());
        assert_eq!(staged.metadata().unwrap().mode() & PERMISSION_BITS, 0o751);
    }
}
