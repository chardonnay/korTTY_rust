use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::sync::{Arc, Mutex};

use zeroize::Zeroizing;

use crate::logfile_window::zstandard::{Decoder, ENCODE_CHUNK_BYTES};
use crate::private_files::{EntryIdentity, FileStamp};

use super::super::metadata::require_single_link;
use super::invalid;

const CHUNK: u64 = ENCODE_CHUNK_BYTES as u64;

/// Only the writer can register a verified plain-to-archive transition for the live reader.
#[derive(Default)]
pub(in crate::session_journal) struct LiveArchives(Mutex<BTreeMap<u32, Arc<LiveArchive>>>);

impl LiveArchives {
    pub(in crate::session_journal) fn record(&self, part: u32, archive: LiveArchive) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(part, Arc::new(archive));
    }

    pub(super) fn get(&self, part: u32) -> Option<Arc<LiveArchive>> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&part)
            .cloned()
    }
}

pub(in crate::session_journal) struct LiveArchive {
    pub(in crate::session_journal) original: EntryIdentity,
    pub(in crate::session_journal) stamp: FileStamp,
    pub(in crate::session_journal) length: u64,
    pub(in crate::session_journal) frame_ends: Vec<u64>,
}

pub(super) struct ArchiveReader {
    file: File,
    archive: Arc<LiveArchive>,
    frame: RefCell<Option<Decoded>>,
}

struct Decoded {
    index: usize,
    bytes: Zeroizing<Vec<u8>>,
}

impl ArchiveReader {
    pub(super) fn open(file: File, archive: Arc<LiveArchive>) -> io::Result<Self> {
        require_single_link(&file)?;
        archive.stamp.require_unchanged(&file)?;
        Ok(Self {
            file,
            archive,
            frame: RefCell::new(None),
        })
    }

    pub(super) fn read(&self, offset: u64, count: u64) -> io::Result<Vec<u8>> {
        if offset > self.archive.length || count > self.archive.length - offset {
            return Err(invalid("Live journal archive read exceeds its part size"));
        }
        let mut output = Vec::with_capacity(count as usize);
        let mut frame = self.frame.borrow_mut();
        let end = offset + count;
        let mut cursor = offset;
        while cursor < end {
            let index = (cursor / CHUNK) as usize;
            if frame.as_ref().is_none_or(|cached| cached.index != index) {
                *frame = Some(self.decode(index)?);
            }
            let decoded = &frame.as_ref().expect("requested frame decoded").bytes;
            let within = (cursor % CHUNK) as usize;
            let count = (decoded.len() - within).min((end - cursor) as usize);
            output.extend_from_slice(&decoded[within..within + count]);
            cursor += count as u64;
        }
        self.archive.stamp.require_unchanged(&self.file)?;
        Ok(output)
    }

    fn decode(&self, index: usize) -> io::Result<Decoded> {
        let end = *self
            .archive
            .frame_ends
            .get(index)
            .ok_or_else(|| invalid("Missing live journal archive frame"))?;
        let start = if index == 0 {
            0
        } else {
            self.archive.frame_ends[index - 1]
        };
        let expected = (self.archive.length - index as u64 * CHUNK).min(CHUNK);
        let mut file = &self.file;
        file.seek(SeekFrom::Start(start))?;
        let mut decoded = Zeroizing::new(Vec::with_capacity(expected as usize));
        Decoder::new(file.take(end - start), || false)
            .take(expected + 1)
            .read_to_end(&mut decoded)?;
        if decoded.len() as u64 != expected {
            return Err(invalid("Live journal archive frame has an unexpected size"));
        }
        Ok(Decoded {
            index,
            bytes: decoded,
        })
    }
}
