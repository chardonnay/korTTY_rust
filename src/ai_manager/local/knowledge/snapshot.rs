//! Versioned private snapshots. A scope has one atomically replaced active index.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::mem::size_of;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Mutex;

use pgp::crypto::hash::HashAlgorithm;

use crate::private_files::{Directory, FileStamp, require_single_link};

use super::{
    CHUNK_BYTES, Index, LEGACY_MAX_SOURCES, Location, MAX_CHUNKS, MAX_DIMENSIONS,
    MAX_LOCAL_INDEX_PASSAGES, MAX_QDRANT_INDEX_PASSAGES, MAX_SOURCES, MAX_STORES, Passage,
    QdrantReceipt, Scope, SourceMask, SourceResult, hnsw, pdf, qdrant, valid_text,
};

const DIRECTORY: &str = "knowledge-indexes";
const MAGIC_V1: &[u8; 8] = b"KTRAG\0\0\x01";
const MAGIC_V2: &[u8; 8] = b"KTRAG\0\0\x02";
const MAGIC_V3: &[u8; 8] = b"KTRAG\0\0\x03";
const MAGIC_V4: &[u8; 8] = b"KTRAG\0\0\x04";
const MAGIC_V5: &[u8; 8] = b"KTRAG\0\0\x05";
const MAGIC_V6: &[u8; 8] = b"KTRAG\0\0\x06";
const MAGIC_V7: &[u8; 8] = b"KTRAG\0\0\x07";
const MAGIC_V8: &[u8; 8] = b"KTRAG\0\0\x08";
const MAGIC_V9: &[u8; 8] = b"KTRAG\0\0\x09";
const MAGIC_V10: &[u8; 8] = b"KTRAG\0\0\x0a";
pub(super) const LEGACY_MAX_DIMENSIONS: usize = 16_384;
const V6_MAX_CHUNKS: usize = 32_768;
const LEGACY_MAX_CHUNKS: usize = 256;
const LEGACY_MAX_LINE: usize = 128 * 1024;
const V6_MAX_BYTES: u64 = 384 * 1024 * 1024;
const MAX_LOCAL_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_QDRANT_BYTES: u64 = 96 * 1024 * 1024;
pub(super) const V4_BASE_BYTES: usize = 47;
pub(super) const V8_BASE_BYTES: usize = V4_BASE_BYTES + 2;
pub(super) const V9_BASE_BYTES: usize = V8_BASE_BYTES + 2;
pub(super) const SOURCE_RESULT_BYTES: usize = size_of::<u32>() + size_of::<i64>();
const MAX_DIRECTORY_ENTRIES: usize = 4 * MAX_STORES;
const CHECK_INTERVAL: usize = 256;

pub(super) fn byte_limit(qdrant: bool) -> u64 {
    if qdrant {
        MAX_QDRANT_BYTES
    } else {
        MAX_LOCAL_BYTES
    }
}

fn passage_limit(qdrant: bool) -> usize {
    if qdrant {
        MAX_QDRANT_INDEX_PASSAGES
    } else {
        MAX_LOCAL_INDEX_PASSAGES
    }
}

pub(super) fn qdrant_receipt_bytes(source_count: usize) -> usize {
    size_of::<qdrant::Generation>() + SourceMask::word_count(source_count).max(1) * size_of::<u32>()
}

#[derive(Clone, Default)]
pub(in crate::ai_manager::local) struct Cache(PathBuf);

pub(super) struct AuthorityEvidence {
    pub(super) stamp: FileStamp,
    pub(super) digest: [u8; 32],
    pub(super) sources: Vec<Option<[u8; 32]>>,
    pub(super) receipt: Option<QdrantReceipt>,
}

pub(super) struct SnapshotVectors {
    file: Mutex<File>,
    stamp: FileStamp,
    offsets: Vec<u64>,
    text_lengths: Option<Vec<u16>>,
    staged_texts: Option<StagedTexts>,
    staged: bool,
}

pub(super) struct VectorStage {
    file: BufWriter<File>,
    offsets: Vec<Option<u64>>,
    block: Vec<u8>,
    end: u64,
}

impl VectorStage {
    pub(super) fn new(count: usize) -> io::Result<Self> {
        Ok(Self {
            file: BufWriter::with_capacity(64 * 1024, tempfile::tempfile()?),
            offsets: vec![None; count],
            block: Vec::new(),
            end: 0,
        })
    }

    pub(super) fn contains(&self, position: usize) -> bool {
        self.offsets[position].is_some()
    }

    pub(super) fn write(&mut self, position: usize, vector: &[f32]) -> io::Result<()> {
        self.block.resize(size_of_val(vector), 0);
        encode_vector(&mut self.block, vector);
        self.file.write_all(&self.block)?;
        self.record_vector(position, self.block.len());
        Ok(())
    }

    pub(super) fn write_encoded(&mut self, position: usize, encoded: &[u8]) -> io::Result<()> {
        self.file.write_all(encoded)?;
        self.record_vector(position, encoded.len());
        Ok(())
    }

    fn record_vector(&mut self, position: usize, bytes: usize) {
        self.offsets[position] = Some(self.end);
        self.end += bytes as u64;
    }

    pub(super) fn finish(self, order: impl Iterator<Item = usize>) -> io::Result<SnapshotVectors> {
        let file = self.file.into_inner().map_err(|error| error.into_error())?;
        let stamp = FileStamp::read(&file)?;
        let offsets = order
            .map(|position| self.offsets[position].ok_or_else(invalid))
            .collect::<io::Result<Vec<_>>>()?;
        Ok(SnapshotVectors {
            file: Mutex::new(file),
            stamp,
            offsets,
            text_lengths: None,
            staged_texts: None,
            staged: true,
        })
    }
}

pub(super) struct TextStage {
    file: BufWriter<File>,
    ends: Vec<u64>,
    lengths: Vec<u16>,
}

struct StagedTexts {
    file: Mutex<File>,
    stamp: FileStamp,
    ends: Vec<u64>,
    lengths: Vec<u16>,
}

impl TextStage {
    pub(super) fn new() -> io::Result<Self> {
        Ok(Self {
            file: BufWriter::with_capacity(64 * 1024, tempfile::tempfile()?),
            ends: Vec::new(),
            lengths: Vec::new(),
        })
    }

    pub(super) fn push(&mut self, text: &str) -> io::Result<()> {
        let length = u16::try_from(text.len()).map_err(|_| invalid())?;
        self.file.write_all(text.as_bytes())?;
        self.ends
            .push(self.ends.last().copied().unwrap_or(0) + u64::from(length));
        self.lengths.push(length);
        Ok(())
    }

    pub(super) fn length(&self, position: usize) -> usize {
        usize::from(self.lengths[position])
    }

    pub(super) fn with_texts<T>(
        &mut self,
        read: impl FnOnce(&mut SnapshotTextReader<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        self.file.flush().map_err(|error| error.to_string())?;
        let mut reader = SnapshotTextReader::new(self.file.get_mut(), &self.ends, &self.lengths);
        let result = read(&mut reader)?;
        self.file
            .get_mut()
            .seek(SeekFrom::End(0))
            .map_err(|error| error.to_string())?;
        Ok(result)
    }
}

impl SnapshotVectors {
    pub(super) fn len(&self) -> usize {
        self.offsets.len()
    }

    pub(super) fn bytes(&self) -> usize {
        self.offsets.capacity() * size_of::<u64>()
            + self
                .text_lengths
                .as_ref()
                .map_or(0, |lengths| lengths.capacity() * size_of::<u16>())
            + self.staged_texts.as_ref().map_or(0, |texts| {
                texts.ends.capacity() * size_of::<u64>()
                    + texts.lengths.capacity() * size_of::<u16>()
            })
    }

    pub(super) fn has_texts(&self) -> bool {
        self.text_lengths.is_some() || self.staged_texts.is_some()
    }

    pub(super) fn text_length(&self, position: usize) -> Option<usize> {
        self.staged_texts
            .as_ref()
            .map(|texts| usize::from(texts.lengths[position]))
            .or_else(|| {
                self.text_lengths
                    .as_ref()
                    .map(|lengths| usize::from(lengths[position]))
            })
    }

    pub(super) fn stage_texts(&mut self, stage: TextStage, order: &[usize]) -> io::Result<()> {
        let file = stage
            .file
            .into_inner()
            .map_err(|error| error.into_error())?;
        let stamp = FileStamp::read(&file)?;
        self.staged_texts = Some(StagedTexts {
            file: Mutex::new(file),
            stamp,
            ends: order.iter().map(|&position| stage.ends[position]).collect(),
            lengths: order
                .iter()
                .map(|&position| stage.lengths[position])
                .collect(),
        });
        Ok(())
    }

    pub(super) fn with_texts<T>(
        &self,
        read: impl FnOnce(&mut SnapshotTextReader<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        let changed = || super::retrieval::changed_index();
        let (file, stamp, ends, lengths) = if let Some(texts) = &self.staged_texts {
            (&texts.file, &texts.stamp, &texts.ends, &texts.lengths)
        } else {
            (
                &self.file,
                &self.stamp,
                &self.offsets,
                self.text_lengths.as_ref().ok_or_else(changed)?,
            )
        };
        let mut file = file.lock().map_err(|_| changed())?;
        stamp.require_unchanged(&file).map_err(|_| changed())?;
        let mut reader = SnapshotTextReader::new(&mut file, ends, lengths);
        let result = read(&mut reader)?;
        stamp.require_unchanged(&file).map_err(|_| changed())?;
        Ok(result)
    }

    pub(super) fn for_each_vector(
        &self,
        ranges: impl Iterator<Item = Range<usize>>,
        dimensions: usize,
        check: impl Fn() -> Result<(), String>,
        mut visit: impl FnMut(usize, &[f32]) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut values = vec![0.0; dimensions];
        self.for_each_encoded_vector(ranges, dimensions, check, |position, block| {
            decode_vector(block, &mut values);
            visit(position, &values)
        })
    }

    pub(super) fn for_each_encoded_vector(
        &self,
        ranges: impl Iterator<Item = Range<usize>>,
        dimensions: usize,
        check: impl Fn() -> Result<(), String>,
        mut visit: impl FnMut(usize, &[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        let changed = |_| super::retrieval::changed_index();
        let mut file = self.file.lock().map_err(changed)?;
        self.stamp
            .require_unchanged(&file)
            .map_err(|_| super::retrieval::changed_index())?;
        let mut block = vec![0; dimensions * size_of::<f32>()];
        let mut reader = BufReader::with_capacity(64 * 1024, &mut *file);
        let mut previous_end = None;
        for position in ranges.flatten() {
            check()?;
            let offset = self.offsets[position];
            let outcome = match previous_end {
                Some(end) => reader.seek_relative(offset as i64 - end as i64),
                None => reader.seek(SeekFrom::Start(offset)).map(|_| ()),
            };
            outcome
                .and_then(|()| reader.read_exact(&mut block))
                .map_err(|_| super::retrieval::changed_index())?;
            visit(position, &block)?;
            previous_end = Some(offset + block.len() as u64);
        }
        self.stamp
            .require_unchanged(&file)
            .map_err(|_| super::retrieval::changed_index())
    }

    fn for_each_encoded_entry(
        &self,
        dimensions: usize,
        mut visit: impl FnMut(usize, Option<&str>, &[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        let range = std::iter::once(0..self.len());
        if self.staged_texts.is_some() {
            return self.with_texts(|texts| {
                self.for_each_encoded_vector(
                    range,
                    dimensions,
                    || Ok(()),
                    |position, vector| {
                        let text = texts.read(position).map_err(|error| error.to_string())?;
                        visit(position, Some(text), vector)
                    },
                )
            });
        }
        self.for_each_encoded_vector(
            range,
            dimensions,
            || Ok(()),
            |position, vector| visit(position, None, vector),
        )
    }
}

pub(super) struct SnapshotTextReader<'a> {
    input: BufReader<&'a mut File>,
    offsets: &'a [u64],
    lengths: &'a [u16],
    buffer: Vec<u8>,
    previous_end: Option<u64>,
}

impl<'a> SnapshotTextReader<'a> {
    fn new(file: &'a mut File, offsets: &'a [u64], lengths: &'a [u16]) -> Self {
        Self {
            input: BufReader::new(file),
            offsets,
            lengths,
            buffer: Vec::new(),
            previous_end: None,
        }
    }

    pub(super) fn read(&mut self, position: usize) -> io::Result<&str> {
        let end = self.offsets[position];
        let length = usize::from(self.lengths[position]);
        let start = end - length as u64;
        match self.previous_end {
            Some(previous) => self.input.seek_relative(start as i64 - previous as i64)?,
            None => {
                self.input.seek(SeekFrom::Start(start))?;
            }
        }
        self.buffer.resize(length, 0);
        self.input.read_exact(&mut self.buffer)?;
        self.previous_end = Some(end);
        std::str::from_utf8(&self.buffer).map_err(|_| invalid())
    }
}

impl Cache {
    pub(in crate::ai_manager::local) fn new(profile: PathBuf) -> Self {
        Self(profile)
    }

    #[cfg(test)]
    pub(in crate::ai_manager::local) fn save(
        &self,
        index: &Index,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.save_checked(index, || Ok(()), check)
    }

    pub(super) fn save_checked(
        &self,
        index: &Index,
        active: impl Fn() -> Result<(), String>,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        if index.qdrant_receipt.is_some() {
            return Err(invalid());
        }
        self.save_index(index, None, || active().map_err(io::Error::other), check)
    }

    pub(in crate::ai_manager::local) fn save_with_receipt(
        &self,
        index: &Index,
        receipt: &QdrantReceipt,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.save_index(index, Some(receipt), || Ok(()), check)
    }

    fn save_index(
        &self,
        index: &Index,
        receipt: Option<&QdrantReceipt>,
        active: impl Fn() -> io::Result<()>,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        active()?;
        let directory = self.directory(true)?;
        directory.replace_with_shared_readers(
            &name(&index.scope),
            |file| {
                let mut writer = BufWriter::new(file);
                write_index_checked(&mut writer, index, receipt, &active)?;
                writer.flush()?;
                active()
            },
            || {
                active()?;
                check()?;
                active()
            },
        )?;
        directory.sync().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("the snapshot was replaced, but its directory could not be synchronized: {error}"),
            )
        })
    }

    pub(in crate::ai_manager::local) fn load(&self, scope: &Scope) -> io::Result<Index> {
        self.load_stamped(scope).map(|(index, _)| index)
    }

    pub(in crate::ai_manager::local) fn load_stamped(
        &self,
        scope: &Scope,
    ) -> io::Result<(Index, FileStamp)> {
        self.load_with_storage(scope, false, || Ok(()))
    }

    pub(in crate::ai_manager::local) fn load_file_backed(
        &self,
        scope: &Scope,
        active: impl Fn() -> Result<(), String>,
    ) -> io::Result<(Index, FileStamp)> {
        self.load_with_storage(scope, true, || {
            active().map_err(|error| io::Error::new(io::ErrorKind::Interrupted, error))
        })
    }

    pub(in crate::ai_manager::local) fn source_results(
        &self,
        scope: &Scope,
        active: impl Fn() -> Result<(), String>,
    ) -> io::Result<Vec<SourceResult>> {
        let file = self.open_snapshot(scope)?;
        let stamp = FileStamp::read(&file)?;
        let length = file.metadata()?.len();
        let mut reader = BufReader::new(&file).take(length);
        let header = read_header(&mut reader, scope)?;
        if length > header.byte_limit() {
            return Err(invalid());
        }
        header.require_minimum_length(reader.limit())?;
        let Some(results) = header.results else {
            // Older indexes store source positions only in the passage records.
            let (index, _) = self.load_file_backed(scope, active)?;
            return Ok(index
                .source_results(
                    index
                        .qdrant_receipt
                        .as_ref()
                        .map(|receipt| &receipt.sources),
                )
                .collect());
        };
        stamp.require_unchanged(&file)?;
        self.require_current(scope, &stamp)?;
        Ok(results
            .into_iter()
            .enumerate()
            .map(|(source, (chunks, indexed_at))| {
                if header
                    .receipt
                    .as_ref()
                    .is_some_and(|receipt| !receipt.sources.contains(source))
                {
                    SourceResult::default()
                } else {
                    SourceResult {
                        files: usize::from(chunks != 0),
                        chunks: chunks as usize,
                        indexed_at,
                    }
                }
            })
            .collect())
    }

    fn load_with_storage(
        &self,
        scope: &Scope,
        file_backed: bool,
        active: impl Fn() -> io::Result<()>,
    ) -> io::Result<(Index, FileStamp)> {
        active()?;
        let file = self.open_snapshot(scope)?;
        let stamp = FileStamp::read(&file)?;
        let mut reader = BufReader::new(&file);
        let length = file.metadata()?.len();
        let held = file_backed.then_some((&file, &stamp));
        let index = read_index_with_file(&mut reader, scope, length, held, &active)?;
        active()?;
        stamp.require_unchanged(&file)?;
        self.require_current(scope, &stamp).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "The knowledge index changed while it was being read",
                )
            } else {
                error
            }
        })?;
        Ok((index, stamp))
    }

    pub(in crate::ai_manager::local) fn require_current(
        &self,
        scope: &Scope,
        stamp: &FileStamp,
    ) -> io::Result<()> {
        stamp.require_unchanged(&self.open_snapshot(scope)?)
    }

    pub(super) fn digest_current(
        &self,
        scope: &Scope,
        stamp: &FileStamp,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<[u8; 32], String> {
        Ok(self.authority_evidence(scope, Some(stamp), check)?.digest)
    }

    pub(super) fn authority_evidence(
        &self,
        scope: &Scope,
        expected_stamp: Option<&FileStamp>,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<AuthorityEvidence, String> {
        let changed = |_| super::retrieval::changed_index();
        let file = self.open_snapshot(scope).map_err(changed)?;
        let stamp = FileStamp::read(&file).map_err(changed)?;
        if expected_stamp.is_some_and(|expected| *expected != stamp) {
            return Err(super::retrieval::changed_index());
        }
        let length = file.metadata().map_err(changed)?.len();
        if length > byte_limit(scope.qdrant.is_some()) {
            return Err(super::retrieval::changed_index());
        }
        let mut reader = (&file).take(length);
        // Save the bounded header's exact bytes,
        // then hash the remainder of the same held file without decoding vectors.
        let mut header_bytes = Vec::new();
        let header = read_header(&mut HeaderReader(&mut reader, &mut header_bytes), scope)
            .map_err(changed)?;
        if length > header.byte_limit() {
            return Err(super::retrieval::changed_index());
        }
        header
            .require_minimum_length(reader.limit())
            .map_err(changed)?;
        let mut hash = HashAlgorithm::Sha256
            .new_hasher()
            .map_err(|error| error.to_string())?;
        hash.update(&header_bytes);
        let mut buffer = vec![0; 64 * 1024];
        loop {
            check()?;
            let count = reader.read(&mut buffer).map_err(changed)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        if reader.limit() != 0 || reader.get_mut().read(&mut [0]).map_err(changed)? != 0 {
            return Err(super::retrieval::changed_index());
        }
        stamp.require_unchanged(&file).map_err(changed)?;
        self.require_current(scope, &stamp).map_err(changed)?;
        let digest = hash
            .finalize()
            .as_ref()
            .try_into()
            .expect("SHA-256 has 32 bytes");
        Ok(AuthorityEvidence {
            stamp,
            digest,
            sources: header.sources,
            receipt: header.receipt,
        })
    }

    fn open_snapshot(&self, scope: &Scope) -> io::Result<std::fs::File> {
        let file = self
            .directory(false)?
            .open_private_file(&name(&scope.digest))?;
        require_single_link(&file)?;
        if file.metadata()?.len() > byte_limit(scope.qdrant.is_some()) {
            return Err(invalid());
        }
        Ok(file)
    }

    pub(in crate::ai_manager::local) fn require_absent(&self, scope: &Scope) -> io::Result<()> {
        match self.open_snapshot(scope) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
            Ok(_) => Err(io::Error::other(
                "The knowledge index was created by another operation.",
            )),
        }
    }

    pub(in crate::ai_manager::local) fn remove(&self, index: &Index) -> io::Result<()> {
        ignore_missing(self.directory(false).and_then(|directory| {
            directory.remove_copy(&name(&index.scope))?;
            directory.sync()
        }))
    }

    pub(in crate::ai_manager::local) fn retain(&self, scopes: &[Scope]) -> io::Result<()> {
        let directory = match self.directory(false) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let retained: Vec<_> = scopes.iter().map(|scope| name(&scope.digest)).collect();
        for entry in directory.names(MAX_DIRECTORY_ENTRIES)? {
            if let Some(entry) = entry.to_str()
                && let Some(key) = entry.strip_suffix(".rag")
                && key.len() == 64
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                && !retained.iter().any(|retained| retained == entry)
            {
                ignore_missing(directory.remove_copy(entry))?;
            }
        }
        directory.sync()
    }

    fn directory(&self, create: bool) -> io::Result<Directory> {
        Directory::open(&self.0, create)?.private_child(DIRECTORY, create)
    }
}

fn name(digest: &[u8; 32]) -> String {
    let mut name = String::with_capacity(68);
    for byte in digest {
        let _ = write!(name, "{byte:02x}");
    }
    name.push_str(".rag");
    name
}

pub(super) fn encoded_entry_bytes(text_len: usize, dimensions: usize) -> usize {
    9_usize
        .saturating_add(text_len)
        .saturating_add(dimensions.saturating_mul(size_of::<f32>()))
}

fn encoded_size(
    index: &Index,
    receipt: Option<&QdrantReceipt>,
    active: &impl Fn() -> io::Result<()>,
) -> io::Result<usize> {
    let count = index.entries.len();
    let dimensions = index.dimensions;
    let limit = byte_limit(receipt.is_some());
    if index
        .snapshot_vectors
        .as_ref()
        .is_some_and(|vectors| !vectors.staged || receipt.is_some())
        || index.sources.len() > MAX_SOURCES
        || !valid_dimensions(count, dimensions, passage_limit(receipt.is_some()), true)
        || (receipt.is_some() && dimensions > qdrant::MAX_DIMENSIONS)
        || (count > MAX_CHUNKS && index.graph.is_some())
        || (count == 0 && index.graph.is_some())
        || (!index.indexed_at.is_empty() && index.indexed_at.len() != index.sources.len())
        || receipt.is_some_and(|receipt| !valid_receipt(receipt, index.sources.len()))
    {
        return Err(invalid());
    }
    let mut bytes = V4_BASE_BYTES
        + if receipt.is_some() {
            qdrant_receipt_bytes(index.sources.len())
        } else {
            0
        }
        + index
            .sources
            .iter()
            .map(|source| if source.is_some() { 33 } else { 1 })
            .sum::<usize>();
    let mut previous = 0;
    for (position, (passage, embedding)) in index.entries.iter().enumerate() {
        if position % CHECK_INTERVAL == 0 {
            active()?;
        }
        let staged_length = index
            .snapshot_vectors
            .as_ref()
            .and_then(|snapshot| snapshot.text_length(position));
        let length = staged_length.unwrap_or(passage.text.len());
        let location_valid = match passage.location {
            Location::Line(line) => (1..=super::source::MAX_CORPUS_BYTES).contains(&line),
            Location::Page(page) => (1..=pdf::MAX_PAGES).contains(&page),
        };
        if passage.source < previous
            || index
                .sources
                .get(passage.source)
                .is_none_or(Option::is_none)
            || !location_valid
            || !(1..=CHUNK_BYTES).contains(&length)
            || (staged_length.is_none()
                && (!valid_text(&passage.text) || passage.text.trim().is_empty()))
            || (index.snapshot_vectors.is_none()
                && (embedding.len() != dimensions || !normalized(embedding)))
        {
            return Err(invalid());
        }
        previous = passage.source;
        bytes += encoded_entry_bytes(length, dimensions);
        if bytes as u64 > limit {
            return Err(invalid());
        }
    }
    if let Some(graph) = &index.graph {
        let graph_bytes = graph.encoded_bytes();
        if graph.len() != count || graph_bytes > hnsw::MAX_SERIALIZED_BYTES {
            return Err(invalid());
        }
        bytes += graph_bytes;
    }
    if !index.indexed_at.is_empty() {
        bytes += V9_BASE_BYTES - V4_BASE_BYTES + SOURCE_RESULT_BYTES * index.sources.len();
    } else if dimensions > LEGACY_MAX_DIMENSIONS {
        bytes += V9_BASE_BYTES - V4_BASE_BYTES;
    } else if receipt.is_none() && wide_local_header(count, bytes) {
        bytes += V8_BASE_BYTES - V4_BASE_BYTES;
    }
    if bytes as u64 > limit {
        return Err(invalid());
    }
    Ok(bytes)
}

fn wide_local_header(count: usize, bytes: usize) -> bool {
    count > V6_MAX_CHUNKS || bytes as u64 > V6_MAX_BYTES
}

#[cfg(test)]
fn write_index(
    output: &mut impl Write,
    index: &Index,
    receipt: Option<&QdrantReceipt>,
) -> io::Result<()> {
    write_index_checked(output, index, receipt, &|| Ok(()))
}

fn write_index_checked(
    output: &mut impl Write,
    index: &Index,
    receipt: Option<&QdrantReceipt>,
    active: &impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    let bytes = encoded_size(index, receipt, active)?;
    let reports = !index.indexed_at.is_empty();
    let wide_dimensions = reports || index.dimensions > LEGACY_MAX_DIMENSIONS;
    let wide = wide_dimensions || (receipt.is_none() && wide_local_header(index.len(), bytes));
    let mut output = ExactWriter {
        output,
        remaining: bytes,
    };
    let expanded = index.sources.len() > LEGACY_MAX_SOURCES
        || index.entries.len() > MAX_CHUNKS
        || index.entries.iter().any(|(passage, _)| matches!(passage.location, Location::Line(line) if line > super::source::PREVIOUS_LARGE_TEXT_BYTES));
    output.write_all(if reports {
        MAGIC_V10
    } else if wide_dimensions {
        MAGIC_V9
    } else {
        match (receipt.is_some(), expanded, wide) {
            (_, _, true) => MAGIC_V8,
            (true, true, false) => MAGIC_V7,
            (true, false, false) => MAGIC_V5,
            (false, true, false) => MAGIC_V6,
            (false, false, false) => MAGIC_V4,
        }
    })?;
    output.write_all(&index.scope)?;
    if let Some(receipt) = receipt {
        output.write_all(&receipt.generation)?;
        for index in 0..SourceMask::word_count(index.sources.len()).max(1) {
            output.write_all(&receipt.sources.word(index).to_le_bytes())?;
        }
    }
    if wide_dimensions {
        output.write_all(&(index.dimensions as u32).to_le_bytes())?;
    } else {
        write_size(&mut output, index.dimensions)?;
    }
    if wide {
        output.write_all(
            &u32::try_from(index.len())
                .map_err(|_| invalid())?
                .to_le_bytes(),
        )?;
    } else {
        write_size(&mut output, index.len())?;
    }
    write_size(&mut output, index.sources.len())?;
    for source in &index.sources {
        output.write_all(&[u8::from(source.is_some())])?;
        if let Some(digest) = source {
            output.write_all(digest)?;
        }
    }
    if reports {
        for (range, &indexed_at) in index.source_ranges.iter().zip(&index.indexed_at) {
            output.write_all(&(range.len() as u32).to_le_bytes())?;
            output.write_all(&indexed_at.to_le_bytes())?;
        }
    }
    write_entries(&mut output, index, active).map_err(io::Error::other)?;
    output.write_all(&[u8::from(index.graph.is_some())])?;
    if let Some(graph) = &index.graph {
        graph.write(&mut output)?;
    }
    if output.remaining != 0 {
        return Err(invalid());
    }
    Ok(())
}

fn write_entries(
    output: &mut impl Write,
    index: &Index,
    active: &impl Fn() -> io::Result<()>,
) -> Result<(), String> {
    let check = |position| {
        if position % CHECK_INTERVAL == 0 {
            active().map_err(|error| error.to_string())?;
        }
        Ok::<(), String>(())
    };
    let mut write = |position: usize, text: &str, encoded: &[u8]| -> io::Result<()> {
        let passage = &index.entries[position].0;
        write_size(output, passage.source)?;
        let (tag, number) = match passage.location {
            Location::Line(line) => (0, line),
            Location::Page(page) => (1, page),
        };
        output.write_all(&[tag])?;
        output.write_all(&u32::try_from(number).map_err(|_| invalid())?.to_le_bytes())?;
        write_size(output, text.len())?;
        output.write_all(text.as_bytes())?;
        output.write_all(encoded)
    };
    if let Some(snapshot) = &index.snapshot_vectors {
        let mut values = vec![0.0; index.dimensions];
        return snapshot.for_each_encoded_entry(index.dimensions, |position, text, encoded| {
            check(position)?;
            decode_vector(encoded, &mut values);
            if !normalized(&values)
                || text.is_some_and(|text| !valid_text(text) || text.trim().is_empty())
            {
                return Err(invalid().to_string());
            }
            let text = text.unwrap_or(&index.entries[position].0.text);
            write(position, text, encoded).map_err(|error| error.to_string())
        });
    }

    let mut encoded = vec![0; index.dimensions * size_of::<f32>()];
    for (position, (passage, embedding)) in index.entries.iter().enumerate() {
        check(position)?;
        encode_vector(&mut encoded, embedding);
        write(position, &passage.text, &encoded).map_err(|error| error.to_string())?;
    }
    Ok(())
}

struct ExactWriter<'a, W> {
    output: &'a mut W,
    remaining: usize,
}

impl<W: Write> Write for ExactWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(invalid());
        }
        let written = self.output.write(bytes)?;
        self.remaining -= written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

struct HeaderReader<'a, R>(&'a mut R, &'a mut Vec<u8>);

impl<R: Read> Read for HeaderReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.0.read(bytes)?;
        self.1.extend_from_slice(&bytes[..count]);
        Ok(count)
    }
}

struct Header {
    wide: bool,
    typed: bool,
    graph_tag: bool,
    dimensions: usize,
    count: usize,
    sources: Vec<Option<[u8; 32]>>,
    results: Option<Vec<(u32, i64)>>,
    receipt: Option<QdrantReceipt>,
    expanded: bool,
}

impl Header {
    fn byte_limit(&self) -> u64 {
        match (self.wide, self.receipt.is_some()) {
            (_, true) => MAX_QDRANT_BYTES,
            (true, false) => MAX_LOCAL_BYTES,
            (false, false) => V6_MAX_BYTES,
        }
    }

    fn require_minimum_length(&self, remaining: u64) -> io::Result<()> {
        let vector_bytes = self.dimensions as u64 * size_of::<f32>() as u64;
        let minimum_entry = 9 + u64::from(self.typed) + vector_bytes;
        let minimum_tail = u64::from(self.graph_tag);
        if remaining < self.count as u64 * minimum_entry + minimum_tail {
            return Err(invalid());
        }
        Ok(())
    }
}

fn read_header(input: &mut impl Read, scope: &Scope) -> io::Result<Header> {
    let magic = read::<8>(input)?;
    let (optional, typed, graph_tag) = match &magic {
        MAGIC_V1 => (false, false, false),
        MAGIC_V2 => (true, false, false),
        MAGIC_V3 => (true, true, false),
        MAGIC_V4 | MAGIC_V5 | MAGIC_V6 | MAGIC_V7 | MAGIC_V8 | MAGIC_V9 | MAGIC_V10 => {
            (true, true, true)
        }
        _ => return Err(invalid()),
    };
    let reports = &magic == MAGIC_V10;
    let has_receipt = matches!(&magic, MAGIC_V5 | MAGIC_V7) || reports && scope.qdrant.is_some();
    if scope.sources.len() > MAX_SOURCES
        || has_receipt != scope.qdrant.is_some()
        || read::<32>(input)? != scope.digest
    {
        return Err(invalid());
    }
    let receipt = if has_receipt {
        let generation = read(input)?;
        let words = if &magic == MAGIC_V7 || reports {
            SourceMask::word_count(scope.sources.len()).max(1)
        } else {
            1
        };
        let sources = (0..words)
            .map(|_| read(input).map(u32::from_le_bytes))
            .collect::<io::Result<Vec<_>>>()?;
        Some(QdrantReceipt {
            generation,
            sources: SourceMask::from_words(sources),
        })
    } else {
        None
    };
    let wide_dimensions = matches!(&magic, MAGIC_V9 | MAGIC_V10);
    let dimensions = if wide_dimensions {
        u32::from_le_bytes(read(input)?) as usize
    } else {
        read_size(input)?
    };
    let wide = matches!(&magic, MAGIC_V8 | MAGIC_V9 | MAGIC_V10);
    let count = if wide {
        u32::from_le_bytes(read(input)?) as usize
    } else {
        read_size(input)?
    };
    let source_count = read_size(input)?;
    let expanded = matches!(
        &magic,
        MAGIC_V6 | MAGIC_V7 | MAGIC_V8 | MAGIC_V9 | MAGIC_V10
    );
    let max_count = match &magic {
        MAGIC_V8 | MAGIC_V9 => MAX_LOCAL_INDEX_PASSAGES,
        MAGIC_V7 | MAGIC_V10 if has_receipt => MAX_QDRANT_INDEX_PASSAGES,
        MAGIC_V10 => MAX_LOCAL_INDEX_PASSAGES,
        MAGIC_V6 => V6_MAX_CHUNKS,
        _ if graph_tag => MAX_CHUNKS,
        _ => LEGACY_MAX_CHUNKS,
    };
    if !valid_dimensions(count, dimensions, max_count, optional)
        || (!reports && wide_dimensions != (dimensions > LEGACY_MAX_DIMENSIONS))
        || (count != 0
            && scope
                .local_dimensions
                .is_some_and(|expected| expected != dimensions))
        || source_count > MAX_SOURCES
        || source_count != scope.sources.len()
        || (!expanded && source_count > LEGACY_MAX_SOURCES)
        || receipt
            .as_ref()
            .is_some_and(|receipt| !valid_receipt(receipt, source_count))
        || (count != 0
            && scope
                .qdrant
                .as_ref()
                .is_some_and(|settings| settings.dimensions != dimensions))
    {
        return Err(invalid());
    }
    let sources = (0..source_count)
        .map(|_| {
            if optional {
                match read::<1>(input).map_err(index_header_error)? {
                    [0] => return Ok(None),
                    [1] => {}
                    _ => return Err(invalid()),
                }
            }
            read::<32>(input).map(Some).map_err(index_header_error)
        })
        .collect::<io::Result<Vec<_>>>()?;
    let results = if reports {
        let mut results = Vec::with_capacity(source_count);
        let mut total = 0_u64;
        for digest in &sources {
            let chunks = u32::from_le_bytes(read(input)?);
            let indexed_at = i64::from_le_bytes(read(input)?);
            if (digest.is_none() && (chunks != 0 || indexed_at != 0))
                || (indexed_at != 0 && jiff::Timestamp::from_second(indexed_at).is_err())
            {
                return Err(invalid());
            }
            total += u64::from(chunks);
            results.push((chunks, indexed_at));
        }
        if total != count as u64 {
            return Err(invalid());
        }
        Some(results)
    } else {
        None
    };
    Ok(Header {
        wide,
        typed,
        graph_tag,
        dimensions,
        count,
        sources,
        results,
        receipt,
        expanded,
    })
}

fn index_header_error(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::UnexpectedEof {
        invalid()
    } else {
        error
    }
}

#[cfg(test)]
fn read_index(input: &mut &[u8], scope: &Scope) -> io::Result<Index> {
    let length = input.len() as u64;
    read_index_with_file(input, scope, length, None, &|| Ok(()))
}

fn read_index_with_file(
    input: &mut impl Read,
    scope: &Scope,
    file_length: u64,
    held: Option<(&File, &FileStamp)>,
    active: &impl Fn() -> io::Result<()>,
) -> io::Result<Index> {
    let mut input = input.take(file_length);
    let header = read_header(&mut input, scope)?;
    if file_length > header.byte_limit() {
        return Err(invalid());
    }
    header.require_minimum_length(input.limit())?;
    active()?;
    let Header {
        typed,
        graph_tag,
        dimensions,
        count,
        sources,
        results,
        receipt,
        expanded,
        wide: _,
    } = header;
    let source_count = sources.len();
    let vector_bytes = dimensions as u64 * size_of::<f32>() as u64;
    let minimum_entry = 9 + u64::from(typed) + vector_bytes;
    let minimum_tail = u64::from(graph_tag);
    let mut entries = Vec::with_capacity(count);
    let mut vector = vec![0; vector_bytes as usize];
    let mut embedding = vec![0.0; dimensions];
    let mut offsets = (held.is_some() && receipt.is_none() && count > MAX_CHUNKS)
        .then(|| Vec::with_capacity(count));
    let mut text_lengths = offsets.as_ref().map(|_| Vec::with_capacity(count));
    let mut text_buffer = Vec::new();
    let mut previous = 0;
    let max_line = if expanded {
        super::source::MAX_CORPUS_BYTES
    } else if graph_tag {
        super::source::PREVIOUS_LARGE_TEXT_BYTES
    } else {
        LEGACY_MAX_LINE
    };
    for remaining in (0..count).rev() {
        if remaining % CHECK_INTERVAL == 0 {
            active()?;
        }
        let source = read_size(&mut input)?;
        if graph_tag && source < previous {
            return Err(invalid());
        }
        previous = source;
        let tag = if typed { read::<1>(&mut input)?[0] } else { 0 };
        let number = u32::from_le_bytes(read(&mut input)?) as usize;
        let path = scope.sources.get(source).ok_or_else(invalid)?;
        let location = match tag {
            0 if (1..=max_line).contains(&number) && !pdf::is_pdf(path) => Location::Line(number),
            1 if (1..=pdf::MAX_PAGES).contains(&number) && pdf::is_pdf(path) => {
                Location::Page(number)
            }
            _ => return Err(invalid()),
        };
        let length = read_size(&mut input)?;
        if sources.get(source).is_none_or(Option::is_none)
            || !(1..=CHUNK_BYTES).contains(&length)
            || input.limit()
                < length as u64 + vector_bytes + remaining as u64 * minimum_entry + minimum_tail
        {
            return Err(invalid());
        }
        text_buffer.resize(length, 0);
        input.read_exact(&mut text_buffer)?;
        let text = std::str::from_utf8(&text_buffer).map_err(|_| invalid())?;
        if !valid_text(text) || text.trim().is_empty() {
            return Err(invalid());
        }
        let text = if let Some(lengths) = &mut text_lengths {
            lengths.push(u16::try_from(length).map_err(|_| invalid())?);
            String::new()
        } else {
            text.to_owned()
        };
        let offset = file_length - input.limit();
        input.read_exact(&mut vector)?;
        decode_vector(&vector, &mut embedding);
        if !normalized(&embedding) {
            return Err(invalid());
        }
        let stored_vector = if let Some(offsets) = &mut offsets {
            offsets.push(offset);
            Vec::new()
        } else {
            embedding.clone()
        };
        entries.push((
            Passage {
                source,
                location,
                text,
            },
            stored_vector,
        ));
    }
    let graph = if graph_tag {
        match read::<1>(&mut input)? {
            [0] => None,
            [1] if count != 0 && count <= MAX_CHUNKS => {
                let mut graph_input = (&mut input).take(hnsw::MAX_SERIALIZED_BYTES as u64);
                Some(hnsw::Graph::read(&mut graph_input, &entries, source_count)?)
            }
            _ => return Err(invalid()),
        }
    } else {
        entries.sort_by_key(|(passage, _)| passage.source);
        None
    };
    if input.get_mut().read(&mut [0])? != 0 {
        return Err(invalid());
    }
    let snapshot_vectors = match (offsets, held) {
        (Some(offsets), Some((file, stamp))) => Some(SnapshotVectors {
            file: Mutex::new(file.try_clone()?),
            stamp: stamp.clone(),
            offsets,
            text_lengths,
            staged_texts: None,
            staged: false,
        }),
        _ => None,
    };
    let mut index = Index::from_snapshot_parts(
        scope.digest,
        sources,
        entries,
        dimensions,
        graph,
        snapshot_vectors,
    )?;
    index.qdrant_receipt = receipt;
    if let Some(results) = results {
        for ((chunks, indexed_at), range) in results.into_iter().zip(&index.source_ranges) {
            if chunks as usize != range.len() {
                return Err(invalid());
            }
            index.indexed_at.push(indexed_at);
        }
    }
    Ok(index)
}

fn decode_vector(bytes: &[u8], values: &mut [f32]) {
    for (bits, value) in bytes.as_chunks::<4>().0.iter().zip(values) {
        *value = f32::from_le_bytes(*bits);
    }
}

fn encode_vector(bytes: &mut [u8], values: &[f32]) {
    for (bytes, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(values) {
        *bytes = value.to_le_bytes();
    }
}

fn valid_receipt(receipt: &QdrantReceipt, source_count: usize) -> bool {
    receipt.generation != [0; size_of::<qdrant::Generation>()]
        && receipt.sources.valid_for(source_count)
}

fn valid_dimensions(count: usize, dimensions: usize, max_count: usize, optional: bool) -> bool {
    if count == 0 {
        optional && dimensions == 0
    } else {
        count <= max_count && (1..=MAX_DIMENSIONS).contains(&dimensions)
    }
}

fn normalized(embedding: &[f32]) -> bool {
    let norm: f64 = embedding
        .iter()
        .map(|&value| f64::from(value).powi(2))
        .sum();
    (0.999..=1.001).contains(&norm)
}

fn read<const N: usize>(input: &mut impl Read) -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    input.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn read_size(input: &mut impl Read) -> io::Result<usize> {
    Ok(usize::from(u16::from_le_bytes(read(input)?)))
}

fn write_size(output: &mut impl Write, size: usize) -> io::Result<()> {
    output.write_all(&u16::try_from(size).map_err(|_| invalid())?.to_le_bytes())
}

fn ignore_missing(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid knowledge index snapshot",
    )
}

#[cfg(test)]
mod tests {
    use super::super::{Entry, Store};
    use super::*;

    #[test]
    fn source_results_reopen_without_counting_unpublished_remote_passages() {
        let profile = crate::snippets::tests::Profile::new();
        let cache = Cache::new(profile.0.clone());
        for remote in [false, true] {
            let mut scope = fixture_scope(&["first.txt", "second.txt", "unindexed.txt"]);
            scope.qdrant = remote.then(|| qdrant::Settings {
                dimensions: 2,
                ..Default::default()
            });
            let mut index = Index::from_parts(
                scope.digest,
                vec![Some([1; 32]), Some([2; 32]), None],
                vec![entry(0, "First"), entry(1, "Second"), entry(1, "Third")],
                2,
                None,
            )
            .unwrap();
            let receipt = remote.then_some(QdrantReceipt {
                generation: [7; 16],
                sources: 1.into(),
            });
            cache
                .save_index(&index, receipt.as_ref(), || Ok(()), || Ok(()))
                .unwrap();
            let legacy = cache.source_results(&scope, || Ok(())).unwrap();
            assert_eq!(
                legacy[0],
                SourceResult {
                    files: 1,
                    chunks: 1,
                    indexed_at: 0
                }
            );
            assert_eq!(legacy[1].chunks, if remote { 0 } else { 2 });
            index.indexed_at = vec![100, 200, 0];
            cache
                .save_index(&index, receipt.as_ref(), || Ok(()), || Ok(()))
                .unwrap();
            let results = cache.source_results(&scope, || Ok(())).unwrap();
            assert_eq!(results[0].indexed_at, 100);
            assert_eq!(results[1].indexed_at, if remote { 0 } else { 200 });
            assert_eq!(results[2], SourceResult::default());
            assert_eq!(cache.load(&scope).unwrap().indexed_at, [100, 200, 0]);

            let mut encoded = Vec::new();
            write_index(&mut encoded, &index, receipt.as_ref()).unwrap();
            assert!(encoded.starts_with(MAGIC_V10));
            let first_result = V9_BASE_BYTES - 1
                + 33 * 2
                + 1
                + receipt.as_ref().map_or(0, |_| qdrant_receipt_bytes(3));
            encoded[first_result] = 2;
            assert!(read_index(&mut encoded.as_slice(), &scope).is_err());
            // Even internally consistent summary totals must match the passage records.
            encoded[first_result + SOURCE_RESULT_BYTES] = 1;
            assert!(read_index(&mut encoded.as_slice(), &scope).is_err());
        }
    }

    #[test]
    fn expanded_qdrant_snapshot_keeps_high_source_positions_and_passages() {
        let profile = crate::snippets::tests::Profile::new();
        let mut store = Store::new("Expanded Qdrant fixture").unwrap();
        store.sources = (0..MAX_SOURCES)
            .map(|source| profile.0.join(format!("{source}.txt")))
            .collect();
        store.qdrant = Some(qdrant::Settings {
            dimensions: 2,
            ..Default::default()
        });
        let scope = Scope::new(&store, "127.0.0.1:1234", "fixture").unwrap();
        let entries = (0..MAX_QDRANT_INDEX_PASSAGES)
            .map(|_| entry(MAX_SOURCES - 1, "Evidence"))
            .collect();
        let mut sources = vec![None; MAX_SOURCES];
        sources[MAX_SOURCES - 1] = Some([7; 32]);
        let index = Index::from_parts(scope.digest, sources, entries, 2, None).unwrap();
        let mut receipt = QdrantReceipt {
            generation: [7; 16],
            sources: SourceMask::default(),
        };
        receipt.sources.insert(MAX_SOURCES - 1);
        let mut data = Vec::new();
        write_index(&mut data, &index, Some(&receipt)).unwrap();
        assert!(data.starts_with(MAGIC_V7));
        let reopened = read_index(&mut data.as_slice(), &scope).unwrap();
        assert_eq!(reopened.len(), MAX_QDRANT_INDEX_PASSAGES);
        assert_eq!(reopened.entries.last().unwrap().0.source, MAX_SOURCES - 1);
        assert_eq!(reopened.qdrant_receipt.as_ref(), Some(&receipt));
        let receipt_end = 40 + qdrant_receipt_bytes(MAX_SOURCES);
        data[receipt_end - 1] |= 1;
        assert!(read_index(&mut data.as_slice(), &scope).is_err());
    }

    #[test]
    fn knowledge_corpus_expanded_snapshot_reopens_and_rejects_legacy_header() {
        let profile = crate::snippets::tests::Profile::new();
        let mut store = Store::new("Expanded corpus fixture").unwrap();
        store.add_source(profile.0.join("source.txt")).unwrap();
        let scope = Scope::new(&store, "127.0.0.1:1234", "fixture").unwrap();
        let entries = (1..=MAX_CHUNKS + 1)
            .map(|line| {
                (
                    Passage {
                        source: 0,
                        location: Location::Line(line),
                        text: format!("Passage {line}"),
                    },
                    vec![
                        if line > MAX_CHUNKS { 1.0 } else { 0.0 },
                        if line > MAX_CHUNKS { 0.0 } else { 1.0 },
                    ],
                )
            })
            .collect();
        let index = Index::from_parts(scope.digest, vec![Some([8; 32])], entries, 2, None).unwrap();
        let cache = Cache::new(profile.0.clone());
        cache.save(&index, || Ok(())).unwrap();
        let (loaded, stamp) = cache.load_stamped(&scope).unwrap();
        assert_eq!(loaded.len(), MAX_CHUNKS + 1);
        assert_eq!(
            loaded
                .rank_candidates(&scope, &[1.0, 0.0], 1, || Ok(()))
                .unwrap(),
            [(1.0, MAX_CHUNKS)]
        );
        cache.require_current(&scope, &stamp).unwrap();
        let (file_backed, _) = cache.load_file_backed(&scope, || Ok(())).unwrap();
        let mut staged = VectorStage::new(MAX_CHUNKS + 2).unwrap();
        assert_eq!(
            file_backed.reuse_source_vectors(0, 1, &mut [], Some(&mut staged), || {
                Err("Canceled".into())
            }),
            Err("Canceled".into())
        );
        assert!(!staged.contains(1));
        file_backed
            .reuse_source_vectors(0, 1, &mut [], Some(&mut staged), || Ok(()))
            .unwrap();
        let mut staged = staged.finish(1..MAX_CHUNKS + 2).unwrap();
        staged
            .for_each_vector(
                [0..1, MAX_CHUNKS..MAX_CHUNKS + 1].into_iter(),
                2,
                || Ok(()),
                |position, vector| {
                    assert_eq!(
                        vector,
                        if position == 0 {
                            &[0.0, 1.0]
                        } else {
                            &[1.0, 0.0]
                        }
                    );
                    Ok(())
                },
            )
            .unwrap();
        let path = profile.0.join(DIRECTORY).join(name(&scope.digest));
        let mut bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], MAGIC_V6);
        assert_eq!(
            cache.save(&file_backed, || Ok(())).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let mut texts = TextStage::new().unwrap();
        let mut copies = Vec::with_capacity(index.len());
        for (passage, _) in &index.entries {
            texts.push(&passage.text).unwrap();
            copies.push((
                Passage {
                    text: String::new(),
                    ..passage.clone()
                },
                Vec::new(),
            ));
        }
        staged
            .stage_texts(texts, &(0..index.len()).collect::<Vec<_>>())
            .unwrap();
        let staged_index = Index::from_snapshot_parts(
            scope.digest,
            index.sources.clone(),
            copies,
            2,
            None,
            Some(staged),
        )
        .unwrap();
        cache.save(&staged_index, || Ok(())).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let (previous, previous_stamp) = cache.load_file_backed(&scope, || Ok(())).unwrap();
        bytes[..8].copy_from_slice(MAGIC_V4);
        assert!(read_index(&mut &bytes[..], &scope).is_err());

        let mut replacement = index;
        replacement.entries[MAX_CHUNKS].0.text = "Updated passage".into();
        cache.save(&replacement, || Ok(())).unwrap();
        assert_eq!(previous.len(), MAX_CHUNKS + 1);
        assert!(cache.require_current(&scope, &previous_stamp).is_err());
        let (current, _) = cache.load_file_backed(&scope, || Ok(())).unwrap();
        assert_eq!(current.passage_text(MAX_CHUNKS).unwrap(), "Updated passage");
    }

    #[test]
    fn canceled_snapshot_io_preserves_the_published_index() {
        use std::cell::Cell;

        let profile = crate::snippets::tests::Profile::new();
        let mut store = Store::new("Cancelable snapshot fixture").unwrap();
        store.add_source(profile.0.join("source.txt")).unwrap();
        let scope = Scope::new(&store, "127.0.0.1:1234", "fixture").unwrap();
        let cache = Cache::new(profile.0.clone());
        let mut index = Index::from_parts(
            scope.digest,
            vec![Some([8; 32])],
            (0..MAX_CHUNKS + 1)
                .map(|_| entry(0, "Published passage"))
                .collect(),
            2,
            None,
        )
        .unwrap();
        cache.save(&index, || Ok(())).unwrap();
        let (_, stamp) = cache.load_file_backed(&scope, || Ok(())).unwrap();
        let calls = Cell::new(0);
        let abort_after = |limit| {
            calls.set(calls.get() + 1);
            if calls.get() == limit {
                Err("Canceled".to_owned())
            } else {
                Ok(())
            }
        };
        let error = cache
            .load_file_backed(&scope, || abort_after(3))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(error.to_string(), "Canceled");

        calls.set(0);
        index.entries[0].0.text = "Replacement passage".into();
        // Pass the size checks and write the first block before canceling.
        let write_limit = 1 + index.len().div_ceil(CHECK_INTERVAL) + 2;
        let error = cache
            .save_checked(
                &index,
                || abort_after(write_limit),
                || panic!("The canceled write reached publication"),
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "Canceled");
        cache.require_current(&scope, &stamp).unwrap();
        assert_eq!(
            cache.directory(false).unwrap().names(2).unwrap(),
            [std::ffi::OsString::from(name(&scope.digest))]
        );
    }

    #[test]
    fn wide_local_dimensions_round_trip_and_require_the_reviewed_shape() {
        let mut scope = fixture_scope(&["wide.txt"]);
        scope.local_dimensions = Some(MAX_DIMENSIONS);
        let mut passage = entry(0, "Wide vector");
        passage.1.resize(MAX_DIMENSIONS, 0.0);
        let index = Index::from_parts(
            scope.digest,
            vec![Some([7; 32])],
            vec![passage],
            MAX_DIMENSIONS,
            None,
        )
        .unwrap();
        let mut bytes = Vec::new();
        write_index(&mut bytes, &index, None).unwrap();
        assert_eq!(&bytes[..8], MAGIC_V9);
        let reopened = read_index(&mut bytes.as_slice(), &scope).unwrap();
        assert_eq!(reopened.dimensions, MAX_DIMENSIONS);
        assert_eq!(reopened.entries[0].0.text, "Wide vector");
        assert_eq!(reopened.entries[0].1, index.entries[0].1);

        scope.local_dimensions = Some(MAX_DIMENSIONS - 1);
        assert!(read_index(&mut bytes.as_slice(), &scope).is_err());
    }

    #[test]
    fn wide_local_snapshot_round_trips_beyond_the_u16_boundary() {
        const PASSAGES: usize = 65_536;
        let profile = crate::snippets::tests::Profile::new();
        let mut store = Store::new("Wide snapshot fixture").unwrap();
        store.add_source(profile.0.join("source.txt")).unwrap();
        let scope = Scope::new(&store, "127.0.0.1:1234", "fixture").unwrap();
        let mut entries: Vec<_> = (0..PASSAGES).map(|_| entry(0, "Passage")).collect();
        entries.last_mut().unwrap().1 = vec![0.0, 1.0];
        let index = Index::from_parts(scope.digest, vec![Some([8; 32])], entries, 2, None).unwrap();
        let cache = Cache::new(profile.0.clone());
        cache.save(&index, || Ok(())).unwrap();
        let (loaded, stamp) = cache.load_file_backed(&scope, || Ok(())).unwrap();
        assert_eq!(loaded.len(), PASSAGES);
        assert_eq!(
            loaded
                .rank_candidates(&scope, &[0.0, 1.0], 5, || Ok(()))
                .unwrap(),
            [(1.0, PASSAGES - 1), (0.0, 0), (0.0, 1), (0.0, 2), (0.0, 3)]
        );
        let mut ranked = loaded.ranked_exact(&scope, &[0.0, 1.0], || Ok(())).unwrap();
        assert_eq!(ranked.next(), Some((1.0, PASSAGES - 1)));
        assert_eq!(ranked.next(), Some((0.0, 0)));
        let mut bytes = std::fs::read(profile.0.join(DIRECTORY).join(name(&scope.digest))).unwrap();
        assert_eq!(&bytes[..8], MAGIC_V8);
        assert_eq!(
            cache.digest_current(&scope, &stamp, || Ok(())).unwrap(),
            super::super::sha256(&bytes).unwrap()
        );
        let count_position = MAGIC_V8.len() + scope.digest.len() + size_of::<u16>();
        bytes[count_position..count_position + size_of::<u32>()]
            .copy_from_slice(&(MAX_LOCAL_INDEX_PASSAGES as u32 + 1).to_le_bytes());
        assert!(read_index(&mut bytes.as_slice(), &scope).is_err());
        bytes[count_position..count_position + size_of::<u32>()]
            .copy_from_slice(&(MAX_LOCAL_INDEX_PASSAGES as u32).to_le_bytes());
        let path = profile.0.join(DIRECTORY).join(name(&scope.digest));
        drop(ranked);
        drop(loaded);
        std::fs::write(path, &bytes[..V8_BASE_BYTES + 32]).unwrap();
        assert_eq!(
            cache
                .load_file_backed(&scope, || Ok(()))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    #[ignore = "measures the Java passage ceiling with synthetic records"]
    fn knowledge_local_snapshot_reopens_java_passage_ceiling() {
        const PASSAGES: usize = 5_000_000;
        const BATCH: usize = 8_192;
        let profile = crate::snippets::tests::Profile::new();
        let mut store = Store::new("Java passage ceiling fixture").unwrap();
        store.add_source(profile.0.join("source.txt")).unwrap();
        let scope = Scope::new(&store, "127.0.0.1:1234", "fixture").unwrap();
        let cache = Cache::new(profile.0.clone());
        let directory = cache.directory(true).unwrap();
        let mut entry = Vec::new();
        write_size(&mut entry, 0).unwrap();
        entry.push(0);
        entry.extend_from_slice(&1_u32.to_le_bytes());
        write_size(&mut entry, 1).unwrap();
        entry.push(b'x');
        entry.extend_from_slice(&1.0_f32.to_le_bytes());
        entry.extend_from_slice(&0.0_f32.to_le_bytes());
        let batch = entry.repeat(BATCH);
        let mut last = entry.clone();
        let vector_start = last.len() - 2 * size_of::<f32>();
        encode_vector(&mut last[vector_start..], &[0.0, 1.0]);
        directory
            .replace_with(
                &name(&scope.digest),
                |file| {
                    let mut writer = BufWriter::new(file);
                    writer.write_all(MAGIC_V8)?;
                    writer.write_all(&scope.digest)?;
                    write_size(&mut writer, 2)?;
                    writer.write_all(&(PASSAGES as u32).to_le_bytes())?;
                    write_size(&mut writer, 1)?;
                    writer.write_all(&[1])?;
                    writer.write_all(&[8; 32])?;
                    for _ in 0..(PASSAGES - 1) / BATCH {
                        writer.write_all(&batch)?;
                    }
                    writer.write_all(&batch[..((PASSAGES - 1) % BATCH) * entry.len()])?;
                    writer.write_all(&last)?;
                    writer.write_all(&[0])?;
                    writer.flush()
                },
                || Ok(()),
            )
            .unwrap();
        let start = std::time::Instant::now();
        let (index, stamp) = cache.load_file_backed(&scope, || Ok(())).unwrap();
        let load_ms = start.elapsed().as_millis();
        assert_eq!(index.len(), PASSAGES);
        let start = std::time::Instant::now();
        assert_eq!(
            index
                .rank_candidates(&scope, &[0.0, 1.0], 1, || Ok(()))
                .unwrap(),
            [(1.0, PASSAGES - 1)]
        );
        let rank_ms = start.elapsed().as_millis();
        assert_eq!(index.passage_text(PASSAGES - 1).unwrap(), "x");
        cache.require_current(&scope, &stamp).unwrap();
        println!(
            "passages={PASSAGES} dimensions=2 load_ms={load_ms} rank_ms={rank_ms} search_index_bytes={}",
            index.bytes()
        );
    }

    fn fixture_scope(paths: &[&str]) -> Scope {
        let profile = crate::snippets::tests::Profile::new();
        let mut store = Store::new("Snapshot fixture").unwrap();
        for name in paths {
            store.add_source(profile.0.join(name)).unwrap();
        }
        Scope::new(&store, "127.0.0.1:1234", "fixture").unwrap()
    }

    fn header(scope: &Scope, magic: &[u8; 8], dimensions: usize, entries: usize) -> Vec<u8> {
        let mut data = magic.to_vec();
        data.extend_from_slice(&scope.digest);
        write_size(&mut data, dimensions).unwrap();
        write_size(&mut data, entries).unwrap();
        write_size(&mut data, scope.sources.len()).unwrap();
        data
    }

    fn legacy_image(
        scope: &Scope,
        magic: &[u8; 8],
        sources: &[Option<[u8; 32]>],
        entries: &[(usize, Location, &str)],
    ) -> Vec<u8> {
        let mut data = header(
            scope,
            magic,
            usize::from(!entries.is_empty()),
            entries.len(),
        );
        for digest in sources {
            if magic != MAGIC_V1 {
                data.push(u8::from(digest.is_some()));
            }
            if let Some(digest) = digest {
                data.extend_from_slice(digest);
            }
        }
        for &(source, location, text) in entries {
            write_size(&mut data, source).unwrap();
            let (tag, number) = match location {
                Location::Line(number) => (0, number),
                Location::Page(number) => (1, number),
            };
            if magic == MAGIC_V3 {
                data.push(tag);
            }
            data.extend_from_slice(&(number as u32).to_le_bytes());
            write_size(&mut data, text.len()).unwrap();
            data.extend_from_slice(text.as_bytes());
            data.extend_from_slice(&1.0_f32.to_le_bytes());
        }
        data
    }

    #[test]
    fn legacy_snapshots_reopen_with_stable_source_ranges_and_page_locations() {
        let scope = fixture_scope(&["one.txt", "two.txt"]);
        let digests = [Some([1; 32]), Some([2; 32])];
        let v1 = legacy_image(
            &scope,
            MAGIC_V1,
            &digests,
            &[
                (1, Location::Line(1), "second source, first passage"),
                (0, Location::Line(1), "first source"),
                (1, Location::Line(2), "second source, second passage"),
            ],
        );
        let restored = read_index(&mut v1.as_slice(), &scope).unwrap();
        assert!(restored.graph.is_none());
        assert_eq!(restored.source_ranges, [0..1, 1..3]);
        assert_eq!(restored.entries[1].0.text, "second source, first passage");
        assert_eq!(restored.entries[2].0.text, "second source, second passage");

        let v2 = legacy_image(&scope, MAGIC_V2, &[None, Some([2; 32])], &[]);
        let restored = read_index(&mut v2.as_slice(), &scope).unwrap();
        assert!(restored.graph.is_none() && restored.entries.is_empty());
        assert_eq!(restored.source_ranges, [0..0, 0..0]);
        assert_eq!(restored.sources, [None, Some([2; 32])]);

        let scope = fixture_scope(&["one.txt", "two.pdf"]);
        let v3 = legacy_image(
            &scope,
            MAGIC_V3,
            &digests,
            &[(1, Location::Page(4), "page four")],
        );
        let restored = read_index(&mut v3.as_slice(), &scope).unwrap();
        assert!(restored.graph.is_none());
        assert_eq!(restored.source_ranges, [0..0, 0..1]);
        assert_eq!(restored.entries[0].0.location, Location::Page(4));
    }

    #[test]
    fn legacy_snapshots_keep_their_own_count_and_line_limits() {
        let scope = fixture_scope(&["legacy.txt"]);
        let sources = [Some([1; 32])];
        let legacy_line = 128 * 1024;
        let rows = vec![(0, Location::Line(legacy_line), "legacy passage"); LEGACY_MAX_CHUNKS];
        for magic in [MAGIC_V1, MAGIC_V2, MAGIC_V3] {
            let image = legacy_image(&scope, magic, &sources, &rows);
            let loaded = read_index(&mut image.as_slice(), &scope).unwrap();
            assert_eq!(loaded.entries.len(), 256);
            assert_eq!(loaded.entries[0].0.location, Location::Line(legacy_line));

            let excessive_count = header(&scope, magic, 1, LEGACY_MAX_CHUNKS + 1);
            let excessive_line = legacy_image(
                &scope,
                magic,
                &sources,
                &[(0, Location::Line(legacy_line + 1), "forged legacy location")],
            );
            for invalid in [excessive_count, excessive_line] {
                assert_eq!(
                    read_index(&mut invalid.as_slice(), &scope)
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::InvalidData,
                );
            }
        }
    }

    fn entry(source: usize, text: &str) -> Entry {
        (
            Passage {
                source,
                location: Location::Line(1),
                text: text.into(),
            },
            vec![1.0, 0.0],
        )
    }

    #[test]
    fn v4_refuses_malformed_shape_before_writing_and_rejects_trailing_bytes() {
        let scope = fixture_scope(&["one.txt", "two.txt"]);
        let mut index = Index::from_parts(
            scope.digest,
            vec![Some([1; 32]), Some([2; 32])],
            vec![entry(0, "first"), entry(1, "second")],
            2,
            None,
        )
        .unwrap();
        let mut data = Vec::new();
        write_index(&mut data, &index, None).unwrap();
        assert!(data.starts_with(MAGIC_V4));
        assert_eq!(
            data.len(),
            V4_BASE_BYTES + 66 + encoded_entry_bytes(5, 2) + encoded_entry_bytes(6, 2)
        );
        assert_eq!(
            read_index(&mut data.as_slice(), &scope)
                .unwrap()
                .source_ranges,
            [0..1, 1..2]
        );
        let tag = data.len() - 1;
        data[tag] = 2;
        assert_eq!(
            read_index(&mut data.as_slice(), &scope).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        data[tag] = 0;
        data.push(0);
        assert_eq!(
            read_index(&mut data.as_slice(), &scope).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        index.entries.swap(0, 1);
        let mut output = Vec::new();
        assert_eq!(
            write_index(&mut output, &index, None).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(output.is_empty());

        for invalid in [
            header(&scope, MAGIC_V4, LEGACY_MAX_DIMENSIONS, MAX_CHUNKS),
            header(&scope, MAGIC_V4, 1, MAX_CHUNKS + 1),
        ] {
            assert_eq!(
                read_index(&mut invalid.as_slice(), &scope)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData,
            );
        }
    }

    #[test]
    fn v4_round_trips_the_graph_and_rejects_invalid_graph_references() {
        let scope = fixture_scope(&["one.txt", "two.txt", "empty.txt"]);
        let entries = vec![entry(0, "first"), entry(1, "second")];
        let graph = hnsw::Graph::build(&entries, || Ok(())).unwrap();
        let graph_bytes = graph.encoded_bytes();
        let index = Index::from_parts(
            scope.digest,
            vec![Some([1; 32]), Some([2; 32]), None],
            entries,
            2,
            Some(graph),
        )
        .unwrap();
        let mut data = Vec::new();
        write_index(&mut data, &index, None).unwrap();
        let start = data.len() - graph_bytes;
        assert_eq!(data[start - 1], 1);
        let restored = read_index(&mut data.as_slice(), &scope).unwrap();
        assert_eq!(restored.source_ranges, [0..1, 1..2, 2..2]);
        let graph = restored.graph.as_ref().unwrap();
        assert_eq!(graph.encoded_bytes(), graph_bytes);
        let hits = graph
            .search(
                &restored.entries,
                &[1.0, 0.0],
                &[false, true, false],
                2,
                || Ok(()),
            )
            .unwrap();
        assert_eq!(hits, [(1.0, 1)]);
        let mut reencoded = Vec::new();
        write_index(&mut reencoded, &restored, None).unwrap();
        assert_eq!(reencoded, data);

        data[start..start + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(
            read_index(&mut data.as_slice(), &scope).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn expanded_snapshots_stream_exact_authority_for_local_and_qdrant() {
        let profile = crate::snippets::tests::Profile::new();
        let mut store = Store::new("Large snapshot fixture").unwrap();
        store.add_source(profile.0.join("source.txt")).unwrap();
        let cache = Cache::new(profile.0.clone());
        for backend in [false, true] {
            store.qdrant = backend.then(|| qdrant::Settings {
                dimensions: 2,
                ..Default::default()
            });
            let scope = Scope::new(&store, "127.0.0.1:1234", "fixture").unwrap();
            let entries = (0..MAX_CHUNKS).map(|_| entry(0, "Evidence")).collect();
            let sources = vec![Some([7; 32])];
            let index = Index::from_parts(scope.digest, sources.clone(), entries, 2, None).unwrap();
            let receipt = backend.then_some(QdrantReceipt {
                generation: [1; 16],
                sources: 1.into(),
            });
            cache
                .save_index(&index, receipt.as_ref(), || Ok(()), || Ok(()))
                .unwrap();
            let (reopened, stamp) = cache.load_stamped(&scope).unwrap();
            assert_eq!(reopened.entries.len(), MAX_CHUNKS);
            let evidence = cache
                .authority_evidence(&scope, Some(&stamp), || Ok(()))
                .unwrap();
            assert_eq!(evidence.sources, sources);
            assert_eq!(evidence.receipt, receipt);
            let mut encoded = Vec::new();
            write_index(&mut encoded, &index, receipt.as_ref()).unwrap();
            assert_eq!(evidence.digest, super::super::sha256(&encoded).unwrap());
            let mut changed = index;
            changed.entries[0].0.text = "Replaced".into();
            cache
                .save_index(&changed, receipt.as_ref(), || Ok(()), || Ok(()))
                .unwrap();
            assert!(
                cache
                    .authority_evidence(&scope, Some(&stamp), || Ok(()))
                    .is_err()
            );
            let latest = cache.authority_evidence(&scope, None, || Ok(())).unwrap();
            assert_ne!(latest.digest, evidence.digest);
            let checks = std::cell::Cell::new(0);
            let stopped = cache.authority_evidence(&scope, None, || {
                checks.set(checks.get() + 1);
                if checks.get() == 2 {
                    Err("Stopped streaming verification".into())
                } else {
                    Ok(())
                }
            });
            assert!(matches!(stopped, Err(error) if error == "Stopped streaming verification"));
        }
    }

    #[test]
    fn qdrant_receipt_publication_is_atomic_and_reopens_only_for_its_scope() {
        let profile = crate::snippets::tests::Profile::new();
        let cache = Cache::new(profile.0.clone());
        let local = fixture_scope(&["one.txt", "two.txt"]);
        let mut scope = local.clone();
        scope.qdrant = Some(qdrant::Settings {
            dimensions: 2,
            ..qdrant::Settings::default()
        });
        let index = Index::from_parts(
            scope.digest,
            vec![Some([1; 32]), Some([2; 32])],
            vec![entry(0, "first"), entry(1, "second")],
            2,
            None,
        )
        .unwrap();
        let mut v4 = Vec::new();
        write_index(&mut v4, &index, None).unwrap();
        assert!(read_index(&mut v4.as_slice(), &scope).is_err());
        assert!(read_index(&mut v4.as_slice(), &local).is_ok());

        let receipt = QdrantReceipt {
            generation: [7; 16],
            sources: 1.into(),
        };
        cache
            .save_with_receipt(&index, &receipt, || Ok(()))
            .unwrap();
        let mut v5 = Vec::new();
        cache
            .open_snapshot(&scope)
            .unwrap()
            .read_to_end(&mut v5)
            .unwrap();
        assert!(v5.starts_with(MAGIC_V5));
        assert_eq!(v5.len(), v4.len() + qdrant_receipt_bytes(2));
        assert_eq!(&v5[8..40], &v4[8..40]);
        assert_eq!(&v5[40..56], &receipt.generation);
        assert_eq!(&v5[56..60], &receipt.sources.word(0).to_le_bytes());
        assert_eq!(&v5[60..], &v4[40..]);
        let loaded = cache.load(&scope).unwrap();
        assert_eq!(loaded.qdrant_receipt.as_ref(), Some(&receipt));
        assert!(read_index(&mut v5.as_slice(), &local).is_err());
        let mut changed = scope.clone();
        changed.qdrant.as_mut().unwrap().dimensions = 3;
        assert!(read_index(&mut v5.as_slice(), &changed).is_err());
        changed = scope.clone();
        changed.digest[0] ^= 1;
        assert!(read_index(&mut v5.as_slice(), &changed).is_err());

        assert_eq!(
            cache.save(&loaded, || Ok(())).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let replacement = QdrantReceipt {
            generation: [8; 16],
            sources: 2.into(),
        };
        let aborted = cache.save_with_receipt(&index, &replacement, || {
            Err(io::ErrorKind::Interrupted.into())
        });
        assert_eq!(aborted.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert_eq!(cache.load(&scope).unwrap().qdrant_receipt, Some(receipt));
        cache
            .save_with_receipt(&index, &replacement, || Ok(()))
            .unwrap();
        assert_eq!(
            cache.load(&scope).unwrap().qdrant_receipt,
            Some(replacement)
        );
    }

    #[test]
    fn qdrant_receipt_rejects_invalid_generation_or_coverage_and_accepts_empty() {
        let mut scope = fixture_scope(&["one.txt"]);
        scope.qdrant = Some(qdrant::Settings::default());
        let index = Index::from_parts(scope.digest, vec![None], Vec::new(), 0, None).unwrap();
        for invalid in [
            QdrantReceipt {
                generation: [0; 16],
                sources: 0.into(),
            },
            QdrantReceipt {
                generation: [7; 16],
                sources: 2.into(),
            },
        ] {
            let mut output = Vec::new();
            assert_eq!(
                write_index(&mut output, &index, Some(&invalid))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
            assert!(output.is_empty());
        }
        let receipt = QdrantReceipt {
            generation: [7; 16],
            sources: 0.into(),
        };
        let mut output = Vec::new();
        write_index(&mut output, &index, Some(&receipt)).unwrap();
        let loaded = read_index(&mut output.as_slice(), &scope).unwrap();
        assert!(loaded.entries.is_empty());
        assert_eq!(loaded.qdrant_receipt.as_ref(), Some(&receipt));
        output[56] = 2;
        assert_eq!(
            read_index(&mut output.as_slice(), &scope)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        output[56] = 0;
        output[40..56].fill(0);
        assert_eq!(
            read_index(&mut output.as_slice(), &scope)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
