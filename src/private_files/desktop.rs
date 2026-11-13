//! Desktop handoff of an export whose published file identity was verified.

use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use super::{Directory, EntryIdentity, FileStamp, require_single_link};

#[derive(Clone)]
pub struct Document {
    path: PathBuf,
    parent: EntryIdentity,
    file: FileStamp,
}

impl fmt::Debug for Document {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Verified desktop export")
    }
}

impl Document {
    /// Capture the written stage before publication and verify the renamed entry.
    pub(crate) fn from_publication(
        path: PathBuf,
        parent: &Directory,
        name: &str,
        staged: &FileStamp,
    ) -> io::Result<Self> {
        let file = parent.open_private_file(name)?;
        require_single_link(&file)?;
        Ok(Self {
            path,
            parent: parent.identity()?,
            file: staged.stamp_after_move(&file)?,
        })
    }

    fn verify(&self) -> io::Result<File> {
        let parent = Directory::open(
            self.path
                .parent()
                .ok_or_else(|| io::Error::other("Invalid export path"))?,
            false,
        )?;
        let name = self
            .path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(|| io::Error::other("Invalid export filename"))?;
        if parent.identity()? != self.parent {
            return Err(io::Error::other("The export directory changed"));
        }
        let file = parent.open_private_file(name)?;
        require_single_link(&file)?;
        self.file.require_unchanged(&file)?;
        Ok(file)
    }

    pub(crate) fn open(&self) -> Result<(), String> {
        self.open_with(kortty_windowing::open_desktop_document)
    }

    fn open_with(&self, launch: impl FnOnce(&Path) -> bool) -> Result<(), String> {
        let _file = self.verify().map_err(
            |_| "The exported file changed or is unavailable. Export it again before opening.",
        )?;
        if launch(&self.path) {
            Ok(())
        } else {
            Err("The desktop could not open the exported file.".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn desktop_export_opens_only_the_original_published_file() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.canonicalize()?;
        let parent = Directory::open(&path, false)?;
        let file = path.join("Report #1.md");
        let mut staged = None;
        parent.publish_with("Report #1.md", |file| {
            file.write_all(b"Original report")?;
            staged = Some(FileStamp::read(file)?);
            Ok(())
        })?;
        let document =
            Document::from_publication(file.clone(), &parent, "Report #1.md", &staged.unwrap())?;
        assert!(document.open_with(|actual| actual == file).is_ok());
        parent.replace_file("Report #1.md", b"Different report")?;
        assert!(
            document
                .open_with(|_| panic!("must not open a replaced file"))
                .is_err()
        );
        Ok(())
    }
}
