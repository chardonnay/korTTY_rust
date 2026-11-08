//! Checked publication of the image pair and the separate journal XML.

use std::ffi::OsStr;
use std::io;

use crate::private_files::{Directory, EntryIdentity, FileStamp};

use super::{
    Edit, ImageEdit, Original, Outcome, annotation_stage_name, changed, open_image, original_name,
    read_asset, screenshot_folder,
};

const PREVIOUS: &str = "previous";
const ATTEMPT: &str = "attempt";

pub(crate) enum XmlWrite<'a> {
    /// Verify the current XML revision and the actor's lock without publishing anything.
    Check,
    /// Publish XML, calling the guard again immediately before its atomic replacement.
    Commit(&'a dyn Fn() -> io::Result<()>),
}

impl Edit {
    /// A caller must hold the writer lock. XML errors must mean it was not published.
    /// `Commit` reports a directory durability warning as `Ok(true)` after publication.
    pub(crate) fn publish_with(
        &self,
        journal: &Directory,
        mut write_xml: impl FnMut(XmlWrite<'_>) -> io::Result<bool>,
    ) -> io::Result<Outcome> {
        self.source()
            .require_location(self.source().directory(), journal)?;
        let folder = screenshot_folder(journal)?;
        let screenshot = &self.draft.screenshot;
        let require_image = || {
            require_folder(self, journal, &folder)?;
            screenshot.require_image(&folder)
        };
        write_xml(XmlWrite::Check)?;
        require_image()?;
        let visible = read_asset(&folder, screenshot.filename())?.ok_or_else(changed)?;
        if visible.bytes != self.draft.visible() {
            return Err(changed());
        }
        require_image()?;
        let Some(image) = &self.image else {
            return Ok(Outcome {
                durability_warning: write_xml(XmlWrite::Commit(&require_image))?,
                cleanup_warning: false,
            });
        };
        let filename = screenshot.filename();
        let original_name = original_name(filename);
        let original = read_asset(&folder, &original_name)?;
        let original_stamp = match (&self.draft.original, original) {
            (Original::Missing, None) => None,
            (Original::Available(expected), Some(current))
                if current.stamp == expected.stamp && current.bytes == expected.bytes =>
            {
                Some(current.stamp)
            }
            _ => return Err(changed()),
        };
        let next = match image {
            ImageEdit::Restore => self.draft.original().ok_or_else(changed)?,
            ImageEdit::Render(bytes) => bytes,
        };
        let mut stage = Stage::create(&folder, filename)?;
        stage.write(filename, next)?;
        let create_original = original_stamp.is_none();
        if create_original {
            stage.write(&original_name, &visible.bytes)?;
        }
        stage.directory()?.sync()?;
        folder.sync()?;
        Transaction {
            edit: self,
            journal,
            stage,
            filename,
            original_name,
            original_stamp,
            visible_before: visible.stamp,
            visible_stamp: None,
            create_original,
            original_moved: false,
            visible_published: false,
        }
        .publish(write_xml)
    }
}

struct Transaction<'a> {
    edit: &'a Edit,
    journal: &'a Directory,
    stage: Stage<'a>,
    filename: &'a str,
    original_name: String,
    original_stamp: Option<FileStamp>,
    visible_before: FileStamp,
    visible_stamp: Option<FileStamp>,
    create_original: bool,
    original_moved: bool,
    visible_published: bool,
}

impl Transaction<'_> {
    fn publish(
        mut self,
        mut write_xml: impl FnMut(XmlWrite<'_>) -> io::Result<bool>,
    ) -> io::Result<Outcome> {
        match self.commit(&mut write_xml) {
            Ok(durability_warning) => {
                let restoring = matches!(&self.edit.image, Some(ImageEdit::Restore));
                let mut cleanup_warning = restoring
                    && (durability_warning || self.remove_original(&mut write_xml).is_err());
                cleanup_warning |= self.stage.cleanup().is_err();
                Ok(Outcome {
                    durability_warning,
                    cleanup_warning,
                })
            }
            Err(error) => {
                if (self.stage.previous()?.occupied
                    || self.visible_published
                    || self.original_moved)
                    && self.rollback(&mut write_xml).is_err()
                {
                    self.stage.retained = true;
                    return Err(recovery_required(self.filename));
                }
                if self.stage.cleanup().is_err() {
                    self.stage.retained = true;
                    return Err(recovery_required(self.filename));
                }
                Err(error)
            }
        }
    }

    fn commit(
        &mut self,
        write_xml: &mut impl FnMut(XmlWrite<'_>) -> io::Result<bool>,
    ) -> io::Result<bool> {
        let folder = self.stage.destination;
        write_xml(XmlWrite::Check)?;
        self.require_assets()?;
        self.stage
            .previous_mut()?
            .claim(folder, self.filename, &self.visible_before)?;
        let previous = self.stage.previous()?.directory()?;
        let claimed = read_asset(previous, self.filename)?.ok_or_else(changed)?;
        if claimed.bytes != self.edit.draft.visible() {
            return Err(changed());
        }
        previous.sync()?;
        folder.sync()?;
        // This second XML check runs while the public slot is vacant. Another occupant may
        // arrive at any point. Publication and restoration both use NOREPLACE.
        write_xml(XmlWrite::Check)?;
        self.require_claimed()?;
        if self.create_original {
            let stamp = self.stage.require_file(&self.original_name)?;
            self.stage
                .directory()?
                .move_entry(OsStr::new(&self.original_name), folder)?;
            self.original_moved = true;
            self.original_stamp = Some(after_move(
                stamp,
                &open_image(folder, &self.original_name)?,
            )?);
            self.require_bytes(&self.original_name, self.edit.draft.visible())?;
        }
        write_xml(XmlWrite::Check)?;
        self.require_claimed()?;
        self.stage.require_file(self.filename)?;
        self.stage
            .directory()?
            .move_entry(OsStr::new(self.filename), folder)?;
        self.visible_published = true;
        self.visible_stamp = Some(after_move(
            self.stage.stamp(self.filename)?,
            &open_image(folder, self.filename)?,
        )?);
        let next = match &self.edit.image {
            Some(ImageEdit::Restore) => self.edit.draft.original().ok_or_else(changed)?,
            Some(ImageEdit::Render(bytes)) => bytes,
            None => return Err(changed()),
        };
        self.require_bytes(self.filename, next)?;
        self.stage.directory()?.sync()?;
        folder.sync()?;
        write_xml(XmlWrite::Commit(&|| self.require_assets()))
    }

    fn rollback(
        &mut self,
        write_xml: &mut impl FnMut(XmlWrite<'_>) -> io::Result<bool>,
    ) -> io::Result<()> {
        let folder = self.stage.destination;
        write_xml(XmlWrite::Check)?;
        self.require_folders()?;
        self.require_original()?;
        if self.visible_published {
            self.require_visible()?;
            let published = self.visible_stamp.as_ref().ok_or_else(changed)?;
            if let Err(error) = self
                .stage
                .attempt_mut()?
                .claim(folder, self.filename, published)
            {
                // A different file was claimed during a race. Give it back without replacing
                // another arrival. The reviewed earlier image remains in the recovery stage.
                self.require_folders()?;
                if self.stage.attempt()?.occupied {
                    let _ = self.stage.attempt_mut()?.restore(folder, self.filename);
                }
                return Err(error);
            }
        }
        write_xml(XmlWrite::Check)?;
        self.require_folders()?;
        if self.stage.attempt()?.occupied {
            self.stage.attempt()?.require_claimed(self.filename)?;
        }
        if self.stage.previous()?.occupied {
            self.visible_stamp = Some(self.stage.previous_mut()?.restore(folder, self.filename)?);
        }
        if self.original_moved {
            self.require_bytes(self.filename, self.edit.draft.visible())?;
            self.remove_original(write_xml)?;
        }
        folder.sync()
    }

    fn require_assets(&self) -> io::Result<()> {
        self.require_folders()?;
        self.require_visible()?;
        self.stage.previous()?.require_claimed(self.filename)?;
        self.require_original()
    }

    fn require_folders(&self) -> io::Result<()> {
        require_folder(self.edit, self.journal, self.stage.destination)?;
        self.stage.require_current()
    }

    fn require_claimed(&self) -> io::Result<()> {
        self.require_folders()?;
        self.stage.previous()?.require_claimed(self.filename)?;
        self.require_original()
    }

    fn require_original(&self) -> io::Result<()> {
        let folder = self.stage.destination;
        match &self.original_stamp {
            Some(stamp) => stamp.require_unchanged(&open_image(folder, &self.original_name)?),
            None if self.original_moved || folder.has_entry(&self.original_name)? => Err(changed()),
            None => Ok(()),
        }
    }

    fn require_visible(&self) -> io::Result<()> {
        let folder = self.stage.destination;
        match &self.visible_stamp {
            Some(stamp) => stamp.require_unchanged(&open_image(folder, self.filename)?),
            None if self.visible_published => Err(changed()),
            None => self.edit.draft.screenshot.require_image(folder),
        }
    }

    fn require_bytes(&self, name: &str, expected: &[u8]) -> io::Result<()> {
        let current = read_asset(self.stage.destination, name)?.ok_or_else(changed)?;
        if current.bytes != expected {
            return Err(changed());
        }
        Ok(())
    }

    fn remove_original(
        &mut self,
        write_xml: &mut impl FnMut(XmlWrite<'_>) -> io::Result<bool>,
    ) -> io::Result<()> {
        write_xml(XmlWrite::Check)?;
        self.require_folders()?;
        self.require_visible()?;
        self.require_original()?;
        let expected = self.original_stamp.as_ref().ok_or_else(changed)?;
        let folder = self.stage.destination;
        let name = &self.original_name;
        #[cfg(windows)]
        folder.remove_unchanged_checked(name, expected, |file| {
            write_xml(XmlWrite::Check)?;
            require_folder(self.edit, self.journal, folder)?;
            self.stage.require_current()?;
            self.require_visible()?;
            crate::session_journal::metadata::require_single_link(file)?;
            expected.require_unchanged(file)
        })?;
        #[cfg(unix)]
        {
            let component = OsStr::new(name);
            folder.move_entry(component, self.stage.directory()?)?;
            let remove = (|| {
                write_xml(XmlWrite::Check)?;
                require_folder(self.edit, self.journal, folder)?;
                self.stage.require_current()?;
                self.require_visible()?;
                let directory = self.stage.directory()?;
                let file = open_image(directory, name)?;
                let stamp = after_move(expected, &file)?;
                directory.remove_unchanged(name, &stamp)
            })();
            if let Err(error) = remove {
                if self
                    .stage
                    .directory()?
                    .move_entry(component, folder)
                    .is_err()
                {
                    self.stage.retained = true;
                }
                return Err(error);
            }
        }
        folder.sync()
    }
}

fn require_folder(edit: &Edit, journal: &Directory, folder: &Directory) -> io::Result<()> {
    edit.source()
        .require_location(edit.source().directory(), journal)?;
    if !folder.same_as(&screenshot_folder(journal)?)? {
        return Err(changed());
    }
    Ok(())
}

fn after_move(stamp: &FileStamp, file: &std::fs::File) -> io::Result<FileStamp> {
    #[cfg(unix)]
    return stamp.stamp_after_move(file);
    #[cfg(windows)]
    {
        stamp.require_unchanged(file)?;
        Ok(stamp.clone())
    }
}

pub(super) fn recovery_required(filename: &str) -> io::Error {
    io::Error::other(format!(
        "A screenshot edit needs recovery. The private stage {} in the journal's screenshots folder has been retained.",
        annotation_stage_name(filename)
    ))
}

struct Stage<'a> {
    destination: &'a Directory,
    directory: Option<Directory>,
    previous: Option<Slot>,
    attempt: Option<Slot>,
    filename: &'a str,
    name: String,
    identity: EntryIdentity,
    files: Vec<(String, FileStamp)>,
    cleaned: bool,
    retained: bool,
}

impl<'a> Stage<'a> {
    fn create(destination: &'a Directory, filename: &'a str) -> io::Result<Self> {
        let name = annotation_stage_name(filename);
        if destination.has_entry(&name)? {
            return Err(recovery_required(filename));
        }
        let directory = destination.create_private_subdirectory(OsStr::new(&name))?;
        let mut stage = Self {
            destination,
            identity: directory.identity()?,
            directory: Some(directory),
            previous: None,
            attempt: None,
            filename,
            name,
            files: Vec::new(),
            cleaned: false,
            retained: false,
        };
        stage.previous = Some(Slot::create(stage.directory()?, PREVIOUS)?);
        stage.attempt = Some(Slot::create(stage.directory()?, ATTEMPT)?);
        Ok(stage)
    }

    fn directory(&self) -> io::Result<&Directory> {
        self.directory.as_ref().ok_or_else(changed)
    }

    fn previous(&self) -> io::Result<&Slot> {
        self.previous.as_ref().ok_or_else(changed)
    }

    fn previous_mut(&mut self) -> io::Result<&mut Slot> {
        self.previous.as_mut().ok_or_else(changed)
    }

    fn attempt(&self) -> io::Result<&Slot> {
        self.attempt.as_ref().ok_or_else(changed)
    }

    fn attempt_mut(&mut self) -> io::Result<&mut Slot> {
        self.attempt.as_mut().ok_or_else(changed)
    }

    fn write(&mut self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.require_current()?;
        let directory = self.directory()?;
        directory.publish_copy(name, bytes)?;
        let stamp = FileStamp::read(&directory.open_private_file(name)?)?;
        self.files.push((name.into(), stamp));
        Ok(())
    }

    fn stamp(&self, name: &str) -> io::Result<&FileStamp> {
        self.files
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, stamp)| stamp)
            .ok_or_else(changed)
    }

    fn require_current(&self) -> io::Result<()> {
        if self
            .destination
            .deletion_child(OsStr::new(&self.name))?
            .identity()?
            != self.identity
        {
            return Err(changed());
        }
        if let Some(directory) = &self.directory {
            for slot in self.previous.iter().chain(&self.attempt) {
                slot.require_current(directory)?;
            }
        }
        Ok(())
    }

    fn require_file(&self, name: &str) -> io::Result<&FileStamp> {
        self.require_current()?;
        let stamp = self.stamp(name)?;
        let file = self.directory()?.open_private_file(name)?;
        crate::session_journal::metadata::require_single_link(&file)?;
        stamp.require_unchanged(&file)?;
        Ok(stamp)
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        if self.retained {
            return Err(changed());
        }
        self.require_current()?;
        if let Some(directory) = &self.directory {
            for slot in [&mut self.previous, &mut self.attempt] {
                if let Some(current) = slot {
                    current.cleanup(directory, self.filename)?;
                    *slot = None;
                }
            }
            for (name, stamp) in &self.files {
                if directory.has_entry(name)? {
                    directory.remove_unchanged(name, stamp)?;
                }
            }
        }
        drop(self.directory.take());
        self.destination
            .remove_deletion_entry(OsStr::new(&self.name), self.identity, true)?;
        self.cleaned = true;
        self.destination.sync()
    }
}

struct Slot {
    directory: Option<Directory>,
    name: &'static str,
    identity: EntryIdentity,
    occupied: bool,
    stamp: Option<FileStamp>,
}

impl Slot {
    fn create(parent: &Directory, name: &'static str) -> io::Result<Self> {
        let directory = parent.create_private_subdirectory(OsStr::new(name))?;
        Ok(Self {
            identity: directory.identity()?,
            directory: Some(directory),
            name,
            occupied: false,
            stamp: None,
        })
    }

    fn directory(&self) -> io::Result<&Directory> {
        self.directory.as_ref().ok_or_else(changed)
    }

    fn require_current(&self, parent: &Directory) -> io::Result<()> {
        if parent.deletion_child(OsStr::new(self.name))?.identity()? != self.identity {
            return Err(changed());
        }
        Ok(())
    }

    fn claim(&mut self, parent: &Directory, name: &str, expected: &FileStamp) -> io::Result<()> {
        parent.move_entry(OsStr::new(name), self.directory()?)?;
        self.occupied = true;
        let file = open_image(self.directory()?, name)?;
        self.stamp = Some(FileStamp::read(&file)?);
        after_move(expected, &file)?;
        Ok(())
    }

    fn require_claimed(&self, name: &str) -> io::Result<()> {
        let directory = self.directory()?;
        if self.occupied {
            self.stamp
                .as_ref()
                .ok_or_else(changed)?
                .require_unchanged(&open_image(directory, name)?)
        } else if directory.has_entry(name)? {
            Err(changed())
        } else {
            Ok(())
        }
    }

    fn restore(&mut self, parent: &Directory, name: &str) -> io::Result<FileStamp> {
        self.require_claimed(name)?;
        let expected = self.stamp.clone().ok_or_else(changed)?;
        self.directory()?.move_entry(OsStr::new(name), parent)?;
        self.occupied = false;
        after_move(&expected, &open_image(parent, name)?)
    }

    fn cleanup(&mut self, parent: &Directory, filename: &str) -> io::Result<()> {
        if self.occupied {
            self.require_claimed(filename)?;
            self.directory()?
                .remove_unchanged(filename, self.stamp.as_ref().ok_or_else(changed)?)?;
            self.occupied = false;
        }
        drop(self.directory.take());
        parent.remove_deletion_entry(OsStr::new(self.name), self.identity, true)
    }
}

impl Drop for Stage<'_> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
