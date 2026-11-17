//! Cleanup is limited to the entries created in one held private candidate.

use std::collections::{BTreeMap, btree_map::Entry as Slot};
use std::ffi::OsStr;
use std::fs::File;
use std::io;

use crate::sftp_window::local_tree::{self, Directory, Entry, ReviewedStamp};

use super::changed;
use super::witness::checked_name;

pub(super) struct CreatedTree {
    stamp: ReviewedStamp,
    children: BTreeMap<String, Created>,
}

enum Created {
    Directory(CreatedTree),
    File(ReviewedStamp),
}

impl CreatedTree {
    pub(super) fn new(directory: &Directory) -> io::Result<Self> {
        Ok(Self {
            stamp: local_tree::reviewed_directory_stamp(directory)?,
            children: BTreeMap::new(),
        })
    }

    pub(super) fn directory(&mut self, name: &str, directory: &Directory) -> io::Result<&mut Self> {
        let child = Self::new(directory)?;
        if child.stamp.volume() != self.stamp.volume() {
            return Err(changed());
        }
        match self.children.entry(name.to_owned()) {
            Slot::Vacant(slot) => {
                let Created::Directory(child) = slot.insert(Created::Directory(child)) else {
                    unreachable!();
                };
                Ok(child)
            }
            Slot::Occupied(_) => Err(changed()),
        }
    }

    pub(super) fn file(&mut self, name: &str, file: &File) -> io::Result<()> {
        let stamp = local_tree::reviewed_file_stamp(file)?;
        if stamp.volume() != self.stamp.volume() {
            return Err(changed());
        }
        match self.children.entry(name.to_owned()) {
            Slot::Vacant(slot) => {
                slot.insert(Created::File(stamp));
                Ok(())
            }
            Slot::Occupied(_) => Err(changed()),
        }
    }

    pub(super) fn forget_published_file(&mut self, name: &str) {
        if matches!(self.children.get(name), Some(Created::File(_))) {
            self.children.remove(name);
        }
    }

    pub(super) fn discard(
        &self,
        parent: &Directory,
        name: &OsStr,
        directory: Directory,
    ) -> io::Result<()> {
        self.require_owned(&directory)?;
        self.remove_children(&directory)?;
        drop(directory);
        local_tree::remove_reviewed_tree_entry(parent, name, &self.stamp, true)
    }

    fn require_owned(&self, directory: &Directory) -> io::Result<()> {
        if !self
            .stamp
            .same_identity(&local_tree::reviewed_directory_stamp(directory)?)
        {
            return Err(changed());
        }
        let names = directory.names(self.children.len())?;
        if names.len() != self.children.len() {
            return Err(changed());
        }
        for name in names {
            let entry = self
                .children
                .get(checked_name(&name)?)
                .ok_or_else(changed)?;
            match (entry, local_tree::entry(directory, &name)?) {
                (Created::Directory(created), Entry::Directory(child)) => {
                    created.require_owned(&child)?
                }
                (Created::File(expected), Entry::File(file))
                    if expected.same_identity(&local_tree::reviewed_file_stamp(&file)?) => {}
                _ => return Err(changed()),
            }
        }
        Ok(())
    }

    fn remove_children(&self, parent: &Directory) -> io::Result<()> {
        for (name, child) in &self.children {
            let name = OsStr::new(name);
            match child {
                Created::Directory(child) => {
                    // Open the held directory for removal only after its name and volume agree.
                    let directory = local_tree::open_reviewed_tree_child(parent, name)?;
                    if !child
                        .stamp
                        .same_identity(&local_tree::reviewed_directory_stamp(&directory)?)
                    {
                        return Err(changed());
                    }
                    child.remove_children(&directory)?;
                    drop(directory);
                    local_tree::remove_reviewed_tree_entry(parent, name, &child.stamp, true)?;
                }
                Created::File(stamp) => {
                    local_tree::remove_reviewed_tree_entry(parent, name, stamp, false)?
                }
            }
        }
        Ok(())
    }
}
