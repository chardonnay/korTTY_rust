//! Bind explicit FFmpeg approval to file contents and protect the launch path.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::private_files::executable::{Opened, digest, open};

use super::invalid;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Selection {
    pub(in crate::recording_window) path: PathBuf,
    pub(in crate::recording_window) sha256: [u8; 32],
}

pub(in crate::recording_window) fn valid_encoder_path(path: &Path) -> bool {
    crate::private_files::executable::valid_path(path)
}

pub(in crate::recording_window) fn resolve_encoder(path: &Path) -> Result<PathBuf, String> {
    if !valid_encoder_path(path) {
        return Err("Enter an absolute path to a local FFmpeg executable.".into());
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| "Could not resolve the selected FFmpeg executable.")?;
    open(&canonical).map_err(|error| error.to_string())?;
    Ok(canonical)
}

pub(in crate::recording_window) fn fingerprint(path: &Path) -> io::Result<[u8; 32]> {
    let mut opened = open(path)?;
    let digest = digest(&mut opened.file, None)?;
    opened.recheck(path)?;
    Ok(digest)
}

pub(super) struct Proof {
    path: PathBuf,
    opened: Opened,
}

impl Proof {
    pub(super) fn verify(selection: &Selection, canceled: &AtomicBool) -> io::Result<Self> {
        let mut opened = open(&selection.path)?;
        if digest(&mut opened.file, Some(canceled))? != selection.sha256 {
            return Err(invalid(
                "FFmpeg contents changed. Re-enter its path and save to approve the replacement",
            ));
        }
        opened.recheck(&selection.path)?;
        Ok(Self {
            path: selection.path.clone(),
            opened,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn recheck(&self) -> io::Result<()> {
        self.opened.recheck(&self.path)
    }
}

#[cfg(test)]
pub(in crate::recording_window) fn fixture_encoder(directory: &Path) -> PathBuf {
    let path = directory.join("ffmpeg.exe");
    std::fs::write(&path, b"test-only executable identity").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    path.canonicalize().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaced_encoder_at_the_same_path_needs_new_approval() {
        let scratch = crate::recording_window::tests::TestDirectory::new();
        let path = fixture_encoder(&scratch.0);
        let selection = Selection {
            sha256: fingerprint(&path).unwrap(),
            path,
        };
        let cancel = AtomicBool::new(false);
        let proof = Proof::verify(&selection, &cancel).unwrap();
        proof.recheck().unwrap();
        #[cfg(windows)]
        {
            assert!(
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&selection.path)
                    .is_err()
            );
            assert!(
                std::fs::rename(
                    selection.path.parent().unwrap(),
                    scratch.0.with_extension("moved")
                )
                .is_err()
            );
        }
        drop(proof);
        std::fs::write(&selection.path, b"different executable contents").unwrap();
        assert!(
            matches!(Proof::verify(&selection, &cancel), Err(error) if error.to_string().contains("contents changed"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn executable_approval_rejects_group_writable_files_and_ancestors() {
        use std::os::unix::fs::PermissionsExt as _;

        let scratch = crate::recording_window::tests::TestDirectory::new();
        let path = fixture_encoder(&scratch.0);
        let mode = |mode| std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(&path, mode(0o770)).unwrap();
        assert!(fingerprint(&path).is_err());
        std::fs::set_permissions(&path, mode(0o700)).unwrap();
        std::fs::set_permissions(path.parent().unwrap(), mode(0o770)).unwrap();
        assert!(fingerprint(&path).is_err());
        std::fs::set_permissions(path.parent().unwrap(), mode(0o700)).unwrap();
        assert!(fingerprint(&path).is_ok());
    }
}
