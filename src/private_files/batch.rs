//! No-replace publication of a staged file batch with identity-checked rollback.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use super::{Directory, EntryIdentity, FileStamp, invalid, nonce};

struct Asset {
    name: String,
    stamp: FileStamp,
}

pub(crate) struct Stage<'a> {
    parent: &'a Directory,
    directory: Option<Directory>,
    name: String,
    identity: EntryIdentity,
    assets: Vec<Asset>,
    published: usize,
}

impl<'a> Stage<'a> {
    pub(crate) fn new(parent: &'a Directory, prefix: &str) -> io::Result<Self> {
        let name = format!("{prefix}-{}", nonce()?);
        let directory = parent.create_private_subdirectory(OsStr::new(&name))?;
        Ok(Self {
            parent,
            identity: directory.identity()?,
            directory: Some(directory),
            name,
            assets: Vec::new(),
            published: 0,
        })
    }

    pub(crate) fn write_file(
        &mut self,
        name: &str,
        write: impl FnOnce(&mut File) -> io::Result<()>,
    ) -> io::Result<()> {
        let stage = self.directory.as_ref().expect("active file batch");
        stage.publish_with(name, write)?;
        let stamp = stage
            .open_private_file(name)
            .and_then(|file| FileStamp::read(&file));
        match stamp {
            Ok(stamp) => self.assets.push(Asset {
                name: name.into(),
                stamp,
            }),
            Err(error) => {
                let _ = stage.remove_copy(name);
                return Err(error);
            }
        }
        Ok(())
    }

    pub(crate) fn publish(&mut self, check: impl Fn() -> io::Result<()>) -> io::Result<()> {
        let stage = self.directory.as_ref().expect("active file batch");
        stage.sync()?;
        let (last, preceding) = self
            .assets
            .split_last()
            .ok_or_else(|| invalid("No files were staged for export"))?;
        for asset in preceding {
            check()?;
            self.publish_file(stage, asset)?;
            self.published += 1;
        }
        for asset in preceding {
            moved_stamp(&asset.stamp, &self.parent.open_private_file(&asset.name)?)?;
        }
        // Publish the final file after the preceding files are verified and synced.
        self.parent.sync()?;
        check()?;
        self.publish_file(stage, last)?;
        self.published += 1;
        moved_stamp(&last.stamp, &self.parent.open_private_file(&last.name)?)?;
        Ok(())
    }

    pub(crate) fn recovery_path(&self, parent: &Path) -> PathBuf {
        parent.join(&self.name)
    }

    fn publish_file(&self, stage: &Directory, asset: &Asset) -> io::Result<()> {
        if !stage.same_as(&self.parent.deletion_child(OsStr::new(&self.name))?)? {
            return Err(invalid("The staged export directory changed"));
        }
        asset
            .stamp
            .require_unchanged(&stage.open_private_file(&asset.name)?)?;
        stage.move_entry(OsStr::new(&asset.name), self.parent)
    }

    pub(crate) fn cleanup(&mut self, committed: bool) -> io::Result<()> {
        let Some(stage) = self.directory.take() else {
            return Ok(());
        };
        let mut result = Ok(());
        if !committed {
            for asset in &self.assets[..self.published] {
                let cleaned = self.reclaim(&stage, asset);
                result = result.and(cleaned);
            }
        }
        for asset in &self.assets[self.published..] {
            let cleaned = stage.remove_unchanged(&asset.name, &asset.stamp);
            result = result.and(cleaned);
        }
        // Windows releases the staging directory handle before removing it.
        drop(stage);
        result = result.and(self.parent.remove_deletion_entry(
            OsStr::new(&self.name),
            self.identity,
            true,
        ));
        if self.published > 0 && !committed {
            result = result.and(self.parent.sync());
        }
        result
    }

    fn reclaim(&self, stage: &Directory, asset: &Asset) -> io::Result<()> {
        let name = OsStr::new(&asset.name);
        let current = match self.parent.open_private_file(&asset.name) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            result => result?,
        };
        moved_stamp(&asset.stamp, &current)?;
        drop(current);
        // Reclaim the name before unlinking. If another entry won the race, move it back intact.
        self.parent.move_entry(name, stage)?;
        match stage
            .open_private_file(&asset.name)
            .and_then(|file| moved_stamp(&asset.stamp, &file))
        {
            Ok(stamp) => stage.remove_unchanged(&asset.name, &stamp),
            Err(error) => stage.move_entry(name, self.parent).and(Err(error)),
        }
    }
}

impl Drop for Stage<'_> {
    fn drop(&mut self) {
        let _ = self.cleanup(false);
    }
}

#[cfg(unix)]
fn moved_stamp(expected: &FileStamp, file: &File) -> io::Result<FileStamp> {
    expected.stamp_after_move(file)
}

#[cfg(windows)]
fn moved_stamp(expected: &FileStamp, file: &File) -> io::Result<FileStamp> {
    expected.require_unchanged(file)?;
    Ok(expected.clone())
}
