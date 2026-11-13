//! Private catalog of user-provided OS symbols.

use std::collections::BTreeSet;
use std::fs::{File, TryLockError};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::image::Handle;
use serde::{Deserialize, Serialize};

use crate::private_files::{Directory, nonce};

use super::{invalid, valid_text};

pub(super) mod raster;
#[cfg(test)]
mod tests;

const PREFIX: &str = "custom:";
const DIRECTORY: &str = "os-badges";
const MANIFEST: &str = "catalog.json";
const MAX_ENTRIES: usize = 64;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_STORED_PNG_BYTES: u64 = 96 * 1024;
const MAX_LABEL_BYTES: usize = 80;

pub(super) fn is_reference(value: &str) -> bool {
    value.strip_prefix(PREFIX).is_some_and(valid_id)
}

#[derive(Clone, Debug)]
pub(crate) struct Catalog {
    root: PathBuf,
    entries: Arc<Vec<CustomBadge>>,
}

#[derive(Clone, Debug)]
pub(crate) struct CustomBadge {
    record: Record,
    reference: String,
    image: Option<Handle>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    id: String,
    name: String,
    family: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    badges: Vec<Record>,
}

impl Catalog {
    pub(crate) fn load(profile_root: &Path) -> io::Result<Self> {
        let mut catalog = Self::empty(profile_root);
        let directory = match catalog.directory(false) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(catalog),
            Err(error) => return Err(error),
        };
        catalog.entries = Arc::new(
            read_manifest(&directory)?
                .into_iter()
                .map(|record| {
                    let image = directory
                        .open_private_file(&record.filename())
                        .and_then(|file| read_bounded(file, MAX_STORED_PNG_BYTES))
                        .and_then(raster::validate_stored)
                        .ok();
                    CustomBadge::new(record, image)
                })
                .collect(),
        );
        Ok(catalog)
    }

    pub(crate) fn empty(profile_root: &Path) -> Self {
        Self {
            root: profile_root.to_owned(),
            entries: Arc::new(Vec::new()),
        }
    }

    pub(crate) fn entries(&self) -> &[CustomBadge] {
        self.entries.as_slice()
    }

    pub(crate) fn find(&self, reference: &str) -> Option<&CustomBadge> {
        self.entries()
            .iter()
            .find(|badge| badge.reference() == reference && !badge.png().is_empty())
    }

    pub(crate) fn add(&mut self, name: &str, family: &str, source: &Path) -> io::Result<String> {
        let record = Record::new(new_id()?, name, family)?;
        let png = raster::load(source)?;
        let (directory, _lock, mut records) = self.edit(true)?;
        if records.len() >= MAX_ENTRIES {
            return Err(invalid("The custom OS symbol catalog is full."));
        }
        let filename = record.filename();
        directory.publish_copy(&filename, &png)?;
        records.push(record.clone());
        if let Err(error) = directory
            .sync()
            .and_then(|()| write_manifest(&directory, records))
        {
            let _ = directory.remove_copy(&filename);
            return Err(error);
        }
        let badge = CustomBadge::new(record, Some(png));
        let reference = badge.reference.clone();
        Arc::make_mut(&mut self.entries).push(badge);
        if let Err(error) = directory.sync() {
            eprintln!("kortty: could not sync the OS symbol catalog: {error}");
        }
        Ok(reference)
    }

    pub(crate) fn rename(&mut self, reference: &str, name: &str, family: &str) -> io::Result<()> {
        let index = self.entry_index(reference)?;
        let record = Record::new(self.entries[index].record.id.clone(), name, family)?;
        let (directory, _lock, mut records) = self.edit(false)?;
        records[index] = record.clone();
        write_manifest(&directory, records)?;
        Arc::make_mut(&mut self.entries)[index].record = record;
        if let Err(error) = directory.sync() {
            eprintln!("kortty: could not sync the OS symbol catalog: {error}");
        }
        Ok(())
    }

    pub(crate) fn remove(&mut self, reference: &str) -> io::Result<()> {
        let index = self.entry_index(reference)?;
        let (directory, _lock, mut records) = self.edit(false)?;
        let removed = records.remove(index);
        write_manifest(&directory, records)?;
        Arc::make_mut(&mut self.entries).remove(index);
        // A stale reference remains dormant, even if the unreferenced image cannot be removed.
        if let Err(error) = directory
            .sync()
            .and_then(|()| directory.remove_copy(&removed.filename()))
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!("kortty: could not remove an unused OS symbol image: {error}");
        }
        Ok(())
    }

    fn entry_index(&self, reference: &str) -> io::Result<usize> {
        self.entries
            .iter()
            .position(|badge| badge.reference == reference)
            .ok_or_else(|| invalid("The custom OS symbol no longer exists."))
    }

    fn directory(&self, create: bool) -> io::Result<Directory> {
        Directory::open(&self.root, create)?.private_child(DIRECTORY, create)
    }

    fn edit(&self, create: bool) -> io::Result<(Directory, File, Vec<Record>)> {
        let directory = self.directory(create)?;
        let lock = directory.open_lock("catalog.lock")?;
        lock.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => io::Error::other("The custom OS symbol catalog is busy."),
            TryLockError::Error(error) => error,
        })?;
        let records = read_manifest(&directory)?;
        if !self.entries.iter().map(|badge| &badge.record).eq(&records) {
            return Err(io::Error::other(
                "The custom OS symbol catalog changed. Reopen it before trying again.",
            ));
        }
        Ok((directory, lock, records))
    }
}

impl CustomBadge {
    fn new(record: Record, png: Option<Vec<u8>>) -> Self {
        Self {
            reference: format!("{PREFIX}{}", record.id),
            image: png.map(Handle::from_bytes),
            record,
        }
    }

    pub(crate) fn reference(&self) -> &str {
        &self.reference
    }

    pub(crate) fn name(&self) -> &str {
        &self.record.name
    }

    pub(crate) fn family(&self) -> &str {
        &self.record.family
    }

    pub(crate) fn png(&self) -> &[u8] {
        match &self.image {
            Some(Handle::Bytes(_, bytes)) => bytes,
            _ => &[],
        }
    }

    pub(crate) fn image_handle(&self) -> Option<Handle> {
        self.image.clone()
    }

    pub(crate) fn raster(&self) -> io::Result<image::RgbaImage> {
        raster::stored_pixels(self.png())
    }
}

impl Record {
    fn new(id: String, name: &str, family: &str) -> io::Result<Self> {
        let record = Self {
            id,
            name: name.trim().to_owned(),
            family: family.trim().to_owned(),
        };
        record.validate()?;
        Ok(record)
    }

    fn validate(&self) -> io::Result<()> {
        if !valid_id(&self.id)
            || !valid_text(&self.name, MAX_LABEL_BYTES, false)
            || !valid_text(&self.family, MAX_LABEL_BYTES, false)
            || self.name.trim() != self.name
            || self.family.trim() != self.family
        {
            return Err(invalid(
                "Enter a valid OS symbol name and family (up to 80 bytes each).",
            ));
        }
        Ok(())
    }

    fn filename(&self) -> String {
        format!("{}.png", self.id)
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'),
        })
}

fn new_id() -> io::Result<String> {
    let mut id = nonce()?.into_bytes();
    id[12] = b'4';
    id[16] = match id[16] {
        b'0'..=b'3' => b'8',
        b'4'..=b'7' => b'9',
        b'8'..=b'b' => b'a',
        _ => b'b',
    };
    let id = String::from_utf8(id).map_err(io::Error::other)?;
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &id[..8],
        &id[8..12],
        &id[12..16],
        &id[16..20],
        &id[20..],
    ))
}

fn read_manifest(directory: &Directory) -> io::Result<Vec<Record>> {
    let file = match directory.open_private_file(MANIFEST) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let bytes = read_bounded(file, MAX_MANIFEST_BYTES)?;
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if manifest.version != 1 || manifest.badges.len() > MAX_ENTRIES {
        return Err(invalid(
            "Unsupported or oversized custom OS symbol catalog.",
        ));
    }
    let mut ids = BTreeSet::new();
    for record in &manifest.badges {
        record.validate()?;
        if !ids.insert(&record.id) {
            return Err(invalid("Duplicate ID in the custom OS symbol catalog."));
        }
    }
    Ok(manifest.badges)
}

fn write_manifest(directory: &Directory, badges: Vec<Record>) -> io::Result<()> {
    let bytes = serde_json::to_vec(&Manifest { version: 1, badges }).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(invalid("The custom OS symbol catalog is too large."));
    }
    directory.replace_file(MANIFEST, &bytes)
}

fn read_bounded(file: File, limit: u64) -> io::Result<Vec<u8>> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(invalid(
            "Choose a regular file within the OS symbol size limit.",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("The OS symbol file exceeds its size limit."));
    }
    Ok(bytes)
}
