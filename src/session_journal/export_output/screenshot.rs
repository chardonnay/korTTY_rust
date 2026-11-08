//! Publication of the exact reviewed journal PNG, with a no-clobber replacement protocol.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use crate::private_files::{FileStamp, overwrite};
use crate::session_journal::viewer::Screenshot;

use super::{Destination, check_cancelled, invalid, write_bytes};

#[derive(Debug)]
pub(crate) struct PngDestination {
    destination: Destination,
    previous: Option<FileStamp>,
}

#[derive(Debug)]
pub(crate) struct PublishedScreenshot {
    pub(crate) path: PathBuf,
    pub(crate) warning: Option<String>,
}

impl PngDestination {
    pub(crate) fn review(destination: Destination, image: &Screenshot) -> io::Result<Self> {
        let parent = destination.open_parent()?;
        image.require_current()?;
        let name = png_name(&destination.path)?;
        let previous = overwrite::review(&parent, name, |file| check_target(image, file))?;
        destination.require_current(&parent, &|| false)?;
        Ok(Self {
            destination,
            previous,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.destination.path
    }

    pub(crate) fn exists(&self) -> bool {
        self.previous.is_some()
    }

    pub(crate) fn publish(
        &self,
        image: &Screenshot,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<PublishedScreenshot> {
        check_cancelled(cancelled)?;
        let destination = &self.destination;
        let parent = destination.open_parent()?;
        let name = png_name(self.path())?;
        let require_current = || {
            destination.require_current(&parent, cancelled)?;
            image.require_current()?;
            destination.require_current(&parent, cancelled)
        };
        let write = |file: &mut File| write_bytes(file, image.bytes(), cancelled);
        let warning = if let Some(previous) = &self.previous {
            overwrite::Reviewed {
                parent: &parent,
                path: self.path(),
                previous,
                stage_prefix: "journal-png",
            }
            .publish(write, require_current, |file| check_target(image, file))?
            .map(|warning| format!("The screenshot was saved. {warning}"))
        } else {
            parent.publish_checked(name, write, require_current)?;
            parent.sync().err().map(|_| {
                "The screenshot was saved, but the export folder could not be synchronized."
                    .to_owned()
            })
        };
        Ok(PublishedScreenshot {
            path: self.path().to_owned(),
            warning,
        })
    }
}

fn png_name(path: &Path) -> io::Result<&str> {
    if !path
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
    {
        return Err(invalid("Choose a PNG image filename."));
    }
    path.file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| invalid("Choose a PNG image filename."))
}

fn check_target(image: &Screenshot, file: &File) -> io::Result<()> {
    if image.is_file(file)? {
        return Err(invalid(
            "Choose a destination other than the original journal screenshot.",
        ));
    }
    Ok(())
}
