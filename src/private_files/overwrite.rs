//! Recoverable publication of a new file over an explicitly reviewed destination.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use super::{Directory, EntryIdentity, FileStamp, invalid, nonce, require_single_link};

const PREVIOUS: &str = ".previous";

pub(crate) fn review(
    parent: &Directory,
    name: &str,
    check_file: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<Option<FileStamp>> {
    let file = match parent.open_file(name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = file.metadata()?;
        if parent.deletion_entry(OsStr::new(name))? != ((metadata.dev(), metadata.ino()), false) {
            return Err(destination_changed());
        }
    }
    require_single_link(&file)?;
    check_file(&file)?;
    FileStamp::read(&file).map(Some)
}

pub(crate) struct Reviewed<'a> {
    pub(crate) parent: &'a Directory,
    pub(crate) path: &'a Path,
    pub(crate) previous: &'a FileStamp,
    pub(crate) stage_prefix: &'static str,
}

impl Reviewed<'_> {
    pub(crate) fn publish(
        self,
        write: impl FnOnce(&mut File) -> io::Result<()>,
        require_current: impl Fn() -> io::Result<()>,
        check_file: impl Fn(&File) -> io::Result<()>,
    ) -> io::Result<Option<String>> {
        let Self {
            parent,
            path,
            previous,
            stage_prefix,
        } = self;
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| invalid("Choose a regular destination filename"))?;
        let mut stage = Stage::create(parent, name, stage_prefix)?;
        stage.write(write)?;
        require_current()?;
        stage.require_current()?;
        if review(parent, name, &check_file)?.as_ref() != Some(previous) {
            return Err(destination_changed());
        }

        // Both moves refuse to clobber a name. Keep the claimed file if a writer races us.
        parent.move_entry(OsStr::new(name), stage.previous())?;
        let published = (|| {
            let file = stage.previous().open_file(name)?;
            require_single_link(&file)?;
            check_file(&file)?;
            let stamp = stamp_after_claim(previous, &file)?;
            drop(file);
            stage.previous().sync()?;
            parent.sync()?;
            require_current()?;
            stage.require_current()?;
            stage.publish()?;
            Ok(stamp)
        })();
        let claimed = match published {
            Ok(claimed) => claimed,
            Err(error) => {
                return match stage.previous().move_entry(OsStr::new(name), parent) {
                    Ok(()) => Err(error),
                    Err(_) => Err(io::Error::new(
                        error.kind(),
                        format!(
                            "{error}. A file from the destination slot was retained at {}.",
                            stage.recovery_path(path).display()
                        ),
                    )),
                };
            }
        };
        if parent.sync().is_err() {
            return Ok(Some(format!(
                "The destination folder could not be synchronized. The previous file was retained at {}.",
                stage.recovery_path(path).display()
            )));
        }
        if stage.previous().remove_unchanged(name, &claimed).is_err() {
            return Ok(Some(format!(
                "The previous file was retained at {}.",
                stage.recovery_path(path).display()
            )));
        }
        if stage.previous().sync().is_err() {
            return Ok(Some(
                "Previous-file cleanup could not be synchronized.".into(),
            ));
        }
        if stage.remove_empty().is_err() {
            return Ok(Some(format!(
                "The private staging folder could not be removed at {}.",
                stage.path(path).display()
            )));
        }
        Ok(None)
    }
}

#[cfg(unix)]
fn stamp_after_claim(expected: &FileStamp, file: &File) -> io::Result<FileStamp> {
    expected
        .stamp_after_move(file)
        .map_err(|_| destination_changed())
}

#[cfg(windows)]
fn stamp_after_claim(expected: &FileStamp, file: &File) -> io::Result<FileStamp> {
    expected
        .require_unchanged(file)
        .map_err(|_| destination_changed())?;
    Ok(expected.clone())
}

fn destination_changed() -> io::Error {
    invalid("The selected destination file changed. Review the destination again.")
}

struct Stage<'a> {
    parent: &'a Directory,
    directory: Option<Directory>,
    previous: Option<Directory>,
    previous_identity: Option<EntryIdentity>,
    stage_name: String,
    identity: EntryIdentity,
    filename: &'a str,
    output: Option<FileStamp>,
    removed: bool,
}

impl<'a> Stage<'a> {
    fn create(parent: &'a Directory, filename: &'a str, stage_prefix: &str) -> io::Result<Self> {
        let stage_name = format!(".kortty-{stage_prefix}-{}", nonce()?);
        let directory = parent.create_private_subdirectory(OsStr::new(&stage_name))?;
        let mut stage = Self {
            parent,
            identity: directory.identity()?,
            directory: Some(directory),
            previous: None,
            previous_identity: None,
            stage_name,
            filename,
            output: None,
            removed: false,
        };
        let previous = stage
            .directory()
            .create_private_subdirectory(OsStr::new(PREVIOUS))?;
        stage.previous_identity = Some(previous.identity()?);
        stage.previous = Some(previous);
        Ok(stage)
    }

    fn directory(&self) -> &Directory {
        self.directory.as_ref().expect("ready overwrite stage")
    }

    fn previous(&self) -> &Directory {
        self.previous.as_ref().expect("ready previous-file stage")
    }

    fn path(&self, destination: &Path) -> PathBuf {
        destination.with_file_name(&self.stage_name)
    }

    fn recovery_path(&self, destination: &Path) -> PathBuf {
        self.path(destination).join(PREVIOUS).join(self.filename)
    }

    fn write(&mut self, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
        self.directory().publish_with(self.filename, write)?;
        let stamp = self
            .directory()
            .open_private_file(self.filename)
            .and_then(|file| FileStamp::read(&file));
        match stamp {
            Ok(stamp) => self.output = Some(stamp),
            Err(error) => {
                let _ = self.directory().remove_copy(self.filename);
                return Err(error);
            }
        }
        self.directory().sync()
    }

    fn require_current(&self) -> io::Result<()> {
        if self
            .parent
            .deletion_child(OsStr::new(&self.stage_name))?
            .identity()?
            != self.identity
            || !self
                .previous()
                .same_as(&self.directory().deletion_child(OsStr::new(PREVIOUS))?)?
        {
            return Err(destination_changed());
        }
        Ok(())
    }

    fn publish(&mut self) -> io::Result<()> {
        self.output
            .as_ref()
            .expect("staged replacement")
            .require_unchanged(&self.directory().open_private_file(self.filename)?)?;
        self.directory()
            .move_entry(OsStr::new(self.filename), self.parent)?;
        self.output = None;
        Ok(())
    }

    fn remove_empty(&mut self) -> io::Result<()> {
        if self.removed {
            return Ok(());
        }
        // Windows keeps removed directories delete-pending until their handles close.
        drop(self.previous.take());
        if let Some(identity) = self.previous_identity {
            self.directory()
                .remove_deletion_entry(OsStr::new(PREVIOUS), identity, true)?;
            self.previous_identity = None;
        }
        drop(self.directory.take());
        self.parent
            .remove_deletion_entry(OsStr::new(&self.stage_name), self.identity, true)?;
        self.removed = true;
        Ok(())
    }
}

impl Drop for Stage<'_> {
    fn drop(&mut self) {
        if let (Some(directory), Some(output)) = (&self.directory, &self.output) {
            let _ = directory.remove_unchanged(self.filename, output);
        }
        // A claimed file that could not be restored keeps the stage nonempty.
        let _ = self.remove_empty();
    }
}
