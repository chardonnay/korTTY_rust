//! Versioned private snapshots. A scope has one atomically replaced active index.

use std::fmt::Write as _;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::mem::size_of;
use std::path::PathBuf;

use pgp::crypto::hash::HashAlgorithm;

use crate::private_files::{Directory, FileStamp, require_single_link};

use super::{
    CHUNK_BYTES, Index, LEGACY_MAX_SOURCES, Location, MAX_CHUNKS, MAX_DIMENSIONS,
    MAX_INDEX_PASSAGES, MAX_SOURCES, MAX_STORES, Passage, QdrantReceipt, Scope, SourceMask, hnsw,
    pdf, qdrant, valid_text,
};

const DIRECTORY: &str = "knowledge-indexes";
const MAGIC_V1: &[u8; 8] = b"KTRAG\0\0\x01";
const MAGIC_V2: &[u8; 8] = b"KTRAG\0\0\x02";
const MAGIC_V3: &[u8; 8] = b"KTRAG\0\0\x03";
const MAGIC_V4: &[u8; 8] = b"KTRAG\0\0\x04";
const MAGIC_V5: &[u8; 8] = b"KTRAG\0\0\x05";
const MAGIC_V6: &[u8; 8] = b"KTRAG\0\0\x06";
const MAGIC_V7: &[u8; 8] = b"KTRAG\0\0\x07";
const LEGACY_MAX_CHUNKS: usize = 256;
const LEGACY_MAX_LINE: usize = 128 * 1024;
pub(super) const MAX_BYTES: u64 = 96 * 1024 * 1024;
pub(super) const V4_BASE_BYTES: usize = 47;
const MAX_DIRECTORY_ENTRIES: usize = 4 * MAX_STORES;

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

impl Cache {
    pub(in crate::ai_manager::local) fn new(profile: PathBuf) -> Self {
        Self(profile)
    }

    pub(in crate::ai_manager::local) fn save(
        &self,
        index: &Index,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        if index.qdrant_receipt.is_some() {
            return Err(invalid());
        }
        self.save_index(index, None, check)
    }

    pub(in crate::ai_manager::local) fn save_with_receipt(
        &self,
        index: &Index,
        receipt: &QdrantReceipt,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.save_index(index, Some(receipt), check)
    }

    fn save_index(
        &self,
        index: &Index,
        receipt: Option<&QdrantReceipt>,
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        let directory = self.directory(true)?;
        directory.replace_with(
            &name(&index.scope),
            |file| {
                let mut writer = BufWriter::new(file);
                write_index(&mut writer, index, receipt)?;
                writer.flush()
            },
            check,
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
        let file = self.open_snapshot(scope)?;
        let stamp = FileStamp::read(&file)?;
        let index = read_index(&mut BufReader::new((&file).take(MAX_BYTES + 1)), scope)?;
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
        if length > MAX_BYTES {
            return Err(super::retrieval::changed_index());
        }
        let mut reader = (&file).take(length);
        // Save the bounded header's exact bytes,
        // then hash the remainder of the same held file without decoding vectors.
        let mut header_bytes = Vec::new();
        let header = read_header(&mut HeaderReader(&mut reader, &mut header_bytes), scope)
            .map_err(changed)?;
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
        if file.metadata()?.len() > MAX_BYTES {
            return Err(invalid());
        }
        Ok(file)
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

fn encoded_size(index: &Index, receipt: Option<&QdrantReceipt>) -> io::Result<usize> {
    let count = index.entries.len();
    let dimensions = index.dimensions;
    if index.sources.len() > MAX_SOURCES
        || !valid_dimensions(count, dimensions, MAX_INDEX_PASSAGES, true)
        || (count > MAX_CHUNKS && index.graph.is_some())
        || (count == 0 && index.graph.is_some())
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
    for (passage, embedding) in &index.entries {
        let location_valid = match passage.location {
            Location::Line(line) => (1..=super::source::MAX_TEXT_BYTES).contains(&line),
            Location::Page(page) => (1..=pdf::MAX_PAGES).contains(&page),
        };
        if passage.source < previous
            || index
                .sources
                .get(passage.source)
                .is_none_or(Option::is_none)
            || !location_valid
            || !(1..=CHUNK_BYTES).contains(&passage.text.len())
            || !valid_text(&passage.text)
            || passage.text.trim().is_empty()
            || embedding.len() != dimensions
            || !normalized(embedding)
        {
            return Err(invalid());
        }
        previous = passage.source;
        bytes += encoded_entry_bytes(passage.text.len(), dimensions);
        if bytes as u64 > MAX_BYTES {
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
    if bytes as u64 > MAX_BYTES {
        return Err(invalid());
    }
    Ok(bytes)
}

fn write_index(
    output: &mut impl Write,
    index: &Index,
    receipt: Option<&QdrantReceipt>,
) -> io::Result<()> {
    let mut output = ExactWriter {
        output,
        remaining: encoded_size(index, receipt)?,
    };
    let expanded = index.sources.len() > LEGACY_MAX_SOURCES
        || index.entries.len() > MAX_CHUNKS
        || index.entries.iter().any(|(passage, _)| matches!(passage.location, Location::Line(line) if line > super::source::PREVIOUS_LARGE_TEXT_BYTES));
    output.write_all(match (receipt.is_some(), expanded) {
        (true, true) => MAGIC_V7,
        (true, false) => MAGIC_V5,
        (false, true) => MAGIC_V6,
        (false, false) => MAGIC_V4,
    })?;
    output.write_all(&index.scope)?;
    if let Some(receipt) = receipt {
        output.write_all(&receipt.generation)?;
        for index in 0..SourceMask::word_count(index.sources.len()).max(1) {
            output.write_all(&receipt.sources.word(index).to_le_bytes())?;
        }
    }
    write_size(&mut output, index.dimensions)?;
    write_size(&mut output, index.entries.len())?;
    write_size(&mut output, index.sources.len())?;
    for source in &index.sources {
        output.write_all(&[u8::from(source.is_some())])?;
        if let Some(digest) = source {
            output.write_all(digest)?;
        }
    }
    let mut vector = vec![0; index.dimensions * size_of::<f32>()];
    for (passage, embedding) in &index.entries {
        write_size(&mut output, passage.source)?;
        let (tag, number) = match passage.location {
            Location::Line(line) => (0, line),
            Location::Page(page) => (1, page),
        };
        output.write_all(&[tag])?;
        output.write_all(&u32::try_from(number).map_err(|_| invalid())?.to_le_bytes())?;
        write_size(&mut output, passage.text.len())?;
        output.write_all(passage.text.as_bytes())?;
        for (bytes, value) in vector.as_chunks_mut::<4>().0.iter_mut().zip(embedding) {
            *bytes = value.to_le_bytes();
        }
        output.write_all(&vector)?;
    }
    output.write_all(&[u8::from(index.graph.is_some())])?;
    if let Some(graph) = &index.graph {
        graph.write(&mut output)?;
    }
    if output.remaining != 0 {
        return Err(invalid());
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
    typed: bool,
    graph_tag: bool,
    dimensions: usize,
    count: usize,
    sources: Vec<Option<[u8; 32]>>,
    receipt: Option<QdrantReceipt>,
    expanded: bool,
}

impl Header {
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
        MAGIC_V4 | MAGIC_V5 | MAGIC_V6 | MAGIC_V7 => (true, true, true),
        _ => return Err(invalid()),
    };
    let has_receipt = matches!(&magic, MAGIC_V5 | MAGIC_V7);
    if scope.sources.len() > MAX_SOURCES
        || has_receipt != scope.qdrant.is_some()
        || read::<32>(input)? != scope.digest
    {
        return Err(invalid());
    }
    let receipt = if has_receipt {
        let generation = read(input)?;
        let words = if &magic == MAGIC_V7 {
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
    let dimensions = read_size(input)?;
    let count = read_size(input)?;
    let source_count = read_size(input)?;
    let expanded = matches!(&magic, MAGIC_V6 | MAGIC_V7);
    let max_count = if expanded {
        MAX_INDEX_PASSAGES
    } else if graph_tag {
        MAX_CHUNKS
    } else {
        LEGACY_MAX_CHUNKS
    };
    if !valid_dimensions(count, dimensions, max_count, optional)
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
    Ok(Header {
        typed,
        graph_tag,
        dimensions,
        count,
        sources,
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

fn read_index(input: &mut impl Read, scope: &Scope) -> io::Result<Index> {
    let mut input = input.take(MAX_BYTES);
    let header = read_header(&mut input, scope)?;
    header.require_minimum_length(input.limit())?;
    let Header {
        typed,
        graph_tag,
        dimensions,
        count,
        sources,
        receipt,
        expanded,
    } = header;
    let source_count = sources.len();
    let vector_bytes = dimensions as u64 * size_of::<f32>() as u64;
    let minimum_entry = 9 + u64::from(typed) + vector_bytes;
    let minimum_tail = u64::from(graph_tag);
    let mut entries = Vec::with_capacity(count);
    let mut vector = vec![0; vector_bytes as usize];
    let mut previous = 0;
    let max_line = if expanded {
        super::source::MAX_TEXT_BYTES
    } else if graph_tag {
        super::source::PREVIOUS_LARGE_TEXT_BYTES
    } else {
        LEGACY_MAX_LINE
    };
    for remaining in (0..count).rev() {
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
        let mut text = vec![0; length];
        input.read_exact(&mut text)?;
        let text = String::from_utf8(text).map_err(|_| invalid())?;
        if !valid_text(&text) || text.trim().is_empty() {
            return Err(invalid());
        }
        input.read_exact(&mut vector)?;
        let embedding = vector
            .as_chunks::<4>()
            .0
            .iter()
            .copied()
            .map(f32::from_le_bytes)
            .collect::<Vec<_>>();
        if !normalized(&embedding) {
            return Err(invalid());
        }
        entries.push((
            Passage {
                source,
                location,
                text,
            },
            embedding,
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
    let mut index = Index::from_parts(scope.digest, sources, entries, dimensions, graph)?;
    index.qdrant_receipt = receipt;
    Ok(index)
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
        let entries = (0..MAX_INDEX_PASSAGES)
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
        assert_eq!(reopened.len(), MAX_INDEX_PASSAGES);
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
        let mut bytes = std::fs::read(profile.0.join(DIRECTORY).join(name(&scope.digest))).unwrap();
        assert_eq!(&bytes[..8], MAGIC_V6);
        bytes[..8].copy_from_slice(MAGIC_V4);
        assert!(read_index(&mut &bytes[..], &scope).is_err());
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
            header(&scope, MAGIC_V4, MAX_DIMENSIONS, MAX_CHUNKS),
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
                .save_index(&index, receipt.as_ref(), || Ok(()))
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
                .save_index(&changed, receipt.as_ref(), || Ok(()))
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
