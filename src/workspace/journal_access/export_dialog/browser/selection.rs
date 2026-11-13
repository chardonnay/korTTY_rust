//! Publication to the exact file and folder reviewed in the export browser.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::private_files::{Directory, EntryIdentity, FileStamp, overwrite};

#[derive(Clone, Debug)]
pub(in crate::workspace) struct Selection {
    path: PathBuf,
    identity: EntryIdentity,
}

impl Selection {
    pub(super) fn new(path: PathBuf, identity: EntryIdentity) -> Self {
        Self { path, identity }
    }

    pub(in crate::workspace) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn identity(&self) -> EntryIdentity {
        self.identity
    }

    pub(in crate::workspace) fn review(self) -> io::Result<ReviewedSelection> {
        let parent = self.open_parent()?;
        let previous = overwrite::review(&parent, self.filename()?, |_| Ok(()))?;
        self.require_current(&parent)?;
        Ok(ReviewedSelection {
            selection: self,
            previous,
        })
    }

    fn filename(&self) -> io::Result<&str> {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("Enter a valid export file name."))
    }

    fn open_parent(&self) -> io::Result<Directory> {
        let path = self.path.parent().ok_or_else(folder_changed)?;
        let parent = Directory::open(path, false)?;
        if parent.identity()? != self.identity {
            return Err(folder_changed());
        }
        Ok(parent)
    }

    fn require_current(&self, parent: &Directory) -> io::Result<()> {
        if !parent.same_as(&self.open_parent()?)? {
            return Err(folder_changed());
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(in crate::workspace) struct ReviewedSelection {
    selection: Selection,
    previous: Option<FileStamp>,
}

impl ReviewedSelection {
    pub(in crate::workspace) fn path(&self) -> &Path {
        self.selection.path()
    }

    pub(in crate::workspace) fn exists(&self) -> bool {
        self.previous.is_some()
    }

    /// Publishes bytes, returning a warning when the file was saved but
    /// cleanup or directory synchronization failed.
    pub(in crate::workspace) fn publish_bytes(&self, bytes: &[u8]) -> io::Result<Option<String>> {
        let parent = self.selection.open_parent()?;
        let require_current = || self.selection.require_current(&parent);
        let write = |file: &mut File| file.write_all(bytes);
        if let Some(previous) = &self.previous {
            return overwrite::Reviewed {
                parent: &parent,
                path: self.path(),
                previous,
                stage_prefix: "export",
            }
            .publish(write, require_current, |_| Ok(()));
        }
        parent.publish_checked(self.selection.filename()?, write, require_current)?;
        Ok(parent.sync().err().map(|_| {
            "The file was saved, but the export folder could not be synchronized.".to_owned()
        }))
    }
}

fn folder_changed() -> io::Error {
    io::Error::other("The selected export folder changed. Choose it again.")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> io::Result<Self> {
            let path = std::env::temp_dir().canonicalize()?.join(format!(
                "kortty-export-selection-{}",
                crate::private_files::nonce()?
            ));
            fs::create_dir(&path)?;
            Ok(Self(path))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn select(folder: &Path) -> io::Result<Selection> {
        let identity = Directory::open(folder, false)?.identity()?;
        Ok(Selection::new(folder.join("overview.json"), identity))
    }

    #[test]
    fn export_selection_respects_the_reviewed_file() -> io::Result<()> {
        let scratch = Scratch::new()?;
        let selection = select(&scratch.0)?;
        let empty = selection.clone().review()?;
        assert!(!empty.exists());
        fs::write(selection.path(), "Created after review")?;
        assert_eq!(
            empty.publish_bytes(b"New inventory").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            fs::read_to_string(selection.path())?,
            "Created after review"
        );

        let stale = selection.clone().review()?;
        assert!(stale.exists());
        fs::write(selection.path(), "Edited while the confirmation was shown")?;
        assert!(stale.publish_bytes(b"New inventory").is_err());
        assert_eq!(
            fs::read_to_string(selection.path())?,
            "Edited while the confirmation was shown"
        );

        let confirmed = selection.review()?;
        confirmed.publish_bytes(b"New inventory")?;
        assert_eq!(fs::read_to_string(confirmed.path())?, "New inventory");
        Ok(())
    }

    #[test]
    fn export_selection_rejects_a_replaced_folder() -> io::Result<()> {
        let scratch = Scratch::new()?;
        let selected_folder = scratch.0.join("selected");
        let moved_folder = scratch.0.join("moved");
        fs::create_dir(&selected_folder)?;
        let selection = select(&selected_folder)?;
        let filename = selection.filename()?.to_owned();
        let reviewed = selection.clone().review()?;
        fs::rename(&selected_folder, &moved_folder)?;
        fs::create_dir(&selected_folder)?;
        assert!(selection.review().is_err());
        assert!(reviewed.publish_bytes(b"New inventory").is_err());
        assert!(!selected_folder.join(&filename).exists());
        assert!(!moved_folder.join(&filename).exists());
        Ok(())
    }
}
