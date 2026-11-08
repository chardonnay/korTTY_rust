use std::ffi::OsStr;
use std::fs::File;
use std::io;

use crate::private_files::{Directory, EntryIdentity, FileStamp};
use crate::session_journal::capture::bundle::ScreenshotRef;
use crate::session_journal::metadata::require_single_link;
use crate::session_journal::{invalid, screenshot_reference};

const SCREENSHOTS: &str = "screenshots";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::session_journal) struct Review {
    journal: EntryIdentity,
    screenshots: EntryIdentity,
    image: FileStamp,
}

impl Review {
    /// Reads the actual displayed file before the deletion confirmation opens.
    pub(in crate::session_journal) fn read(
        journal: &Directory,
        target: &str,
    ) -> io::Result<Option<Self>> {
        Ok(Cleanup::open(journal, target)?.map(|cleanup| cleanup.review))
    }
}

pub(in crate::session_journal) struct Cleanup {
    screenshots: Directory,
    // Keep the reviewed file identity allocated while the XML is committed and the name is claimed.
    image: File,
    name: String,
    review: Review,
}

impl Cleanup {
    pub(in crate::session_journal) fn prepare<'a>(
        journal: &Directory,
        target: &str,
        remaining: impl Iterator<Item = &'a str>,
        reviewed: Option<&Review>,
    ) -> io::Result<Option<Self>> {
        let cleanup = match (Self::open(journal, target)?, reviewed) {
            (Some(cleanup), Some(reviewed)) if cleanup.review == *reviewed => cleanup,
            (None, None) => return Ok(None),
            _ => return Err(changed()),
        };
        for reference in remaining {
            if reference.eq_ignore_ascii_case(target) && cleanup.is_alias(journal, reference)? {
                return Ok(None);
            }
        }
        Ok(Some(cleanup))
    }

    fn open(journal: &Directory, target: &str) -> io::Result<Option<Self>> {
        if screenshot_reference(target).is_none() {
            return Err(invalid("The journal screenshot reference is unsafe"));
        }
        if ScreenshotRef::parse(target).is_none() {
            // Java keeps the untouched annotation backup separately for local use.
            return Ok(None);
        }
        let name = target
            .strip_prefix("screenshots/")
            .ok_or_else(|| invalid("The journal screenshot reference is unsafe"))?;
        let Some(screenshots) = optional(journal.deletion_child(OsStr::new(SCREENSHOTS)))? else {
            return Ok(None);
        };
        let Some(image) = optional(screenshots.open_file(name))? else {
            return Ok(None);
        };
        require_single_link(&image)?;
        let review = Review {
            journal: journal.identity()?,
            screenshots: screenshots.identity()?,
            image: FileStamp::read(&image)?,
        };
        Ok(Some(Self {
            screenshots,
            image,
            name: name.to_owned(),
            review,
        }))
    }

    fn is_alias(&self, journal: &Directory, reference: &str) -> io::Result<bool> {
        // The caller already matched the entire strict ASCII path case-insensitively.
        let (directory, name) = reference.split_once('/').ok_or_else(changed)?;
        let Some(directory) = optional(journal.deletion_child(OsStr::new(directory)))? else {
            return Ok(false);
        };
        if !directory.same_as(&self.screenshots)? {
            return Ok(false);
        }
        let Some(file) = optional(directory.open_file(name))? else {
            return Ok(false);
        };
        Ok(FileStamp::read(&file)? == self.review.image)
    }

    fn require_directory(&self, journal: &Directory) -> io::Result<()> {
        if journal.identity()? != self.review.journal
            || !journal
                .deletion_child(OsStr::new(SCREENSHOTS))?
                .same_as(&self.screenshots)?
        {
            return Err(changed());
        }
        Ok(())
    }

    /// Removes only the reviewed image, after the caller has published the XML without its row.
    pub(in crate::session_journal) fn remove(
        self,
        journal: &Directory,
        require_current: impl Fn() -> io::Result<()>,
    ) -> io::Result<()> {
        require_current()?;
        self.require_directory(journal)?;
        require_single_link(&self.image)?;
        self.review.image.require_unchanged(&self.image)?;

        #[cfg(unix)]
        self.remove_claimed(journal, require_current)?;
        #[cfg(windows)]
        self.screenshots
            .remove_unchanged_checked(&self.name, &self.review.image, |file| {
                require_current()?;
                self.require_directory(journal)?;
                require_single_link(file)
            })?;

        self.screenshots.sync()
    }

    #[cfg(unix)]
    fn remove_claimed(
        &self,
        journal: &Directory,
        require_current: impl Fn() -> io::Result<()>,
    ) -> io::Result<()> {
        let image = self.screenshots.open_file(&self.name)?;
        require_single_link(&image)?;
        self.review.image.require_unchanged(&image)?;
        let stage = Stage::create(&self.screenshots)?;
        let name = OsStr::new(&self.name);
        self.screenshots.move_entry(name, &stage.directory)?;
        // Claim first so a racing rename at the public name cannot redirect the unlink.
        let removed = (|| {
            require_current()?;
            self.require_directory(journal)?;
            let image = stage.directory.open_file(&self.name)?;
            require_single_link(&image)?;
            let claimed = self.review.image.stamp_after_move(&image)?;
            stage.directory.remove_unchanged(&self.name, &claimed)
        })();
        if let Err(error) = removed {
            return match stage.directory.move_entry(name, &self.screenshots) {
                Ok(()) => Err(error),
                Err(restore) => Err(io::Error::new(
                    error.kind(),
                    format!(
                        "{error}. The image was retained in {} because it could not be restored ({restore}).",
                        stage.name
                    ),
                )),
            };
        }
        stage.directory.sync()?;
        stage.remove_empty()
    }
}

fn changed() -> io::Error {
    invalid("The journal screenshot changed after it was reviewed")
}

fn optional<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        result => result.map(Some),
    }
}

#[cfg(unix)]
struct Stage<'a> {
    parent: &'a Directory,
    directory: Directory,
    identity: EntryIdentity,
    name: String,
}

#[cfg(unix)]
impl<'a> Stage<'a> {
    fn create(parent: &'a Directory) -> io::Result<Self> {
        let name = format!(
            ".kortty-journal-image-delete-{}",
            crate::private_files::nonce()?
        );
        let directory = parent.create_private_subdirectory(OsStr::new(&name))?;
        Ok(Self {
            parent,
            identity: directory.identity()?,
            directory,
            name,
        })
    }

    fn remove_empty(&self) -> io::Result<()> {
        self.parent
            .remove_deletion_entry(OsStr::new(&self.name), self.identity, true)
    }
}

#[cfg(unix)]
impl Drop for Stage<'_> {
    fn drop(&mut self) {
        let _ = self.remove_empty();
    }
}

#[cfg(test)]
mod tests;
