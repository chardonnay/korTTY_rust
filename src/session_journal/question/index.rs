//! Private per-journal vectors for unchanged, reviewed semantic cards.

use std::io::{self, Read as _, Write as _};

use pgp::crypto::hash::HashAlgorithm;
use roxmltree::Document;
use zeroize::Zeroizing;

use super::{changed, check_cancel};
use crate::private_files::{Directory, FileStamp, require_single_link};
use crate::session_journal::{deletion::DeletionIdentity, viewer::Source};

const DIRECTORY: &str = ".semantic-index";
const FILE: &str = "embedding-v1";
const MAGIC: &[u8] = b"kortty-journal-embedding-v1\0";
// Match the embedding response limit. The binary cache stays below 257 KiB per journal.
const MAX_DIMENSIONS: usize = 65_536;
const MAX_BYTES: usize = MAGIC.len() + 32 + MAX_DIMENSIONS * size_of::<f32>();

pub(super) fn document_revision(document: &Document<'_>) -> io::Result<[u8; 32]> {
    digest(document.input_text().as_bytes())
}

pub(super) fn key(
    stamp: &DeletionIdentity,
    document: [u8; 32],
    card: &str,
    model: [u8; 32],
) -> io::Result<[u8; 32]> {
    let card = digest(card.as_bytes())?;
    digest(&serde_json::to_vec(&(
        stamp.location(),
        document,
        card,
        model,
    ))?)
}

fn digest(bytes: &[u8]) -> io::Result<[u8; 32]> {
    HashAlgorithm::Sha256
        .digest(bytes)
        .map_err(io::Error::other)?
        .as_slice()
        .try_into()
        .map_err(io::Error::other)
}

pub(super) fn load(source: &Source, key: &[u8; 32]) -> Option<Vec<f32>> {
    let journal = Directory::open(source.directory(), false).ok()?;
    source.require_location(source.directory(), &journal).ok()?;
    let directory = journal.private_child(DIRECTORY, false).ok()?;
    let file = directory.open_private_file(FILE).ok()?;
    require_single_link(&file).ok()?;
    let stamp = FileStamp::read(&file).ok()?;
    let mut bytes = Zeroizing::new(Vec::new());
    (&file)
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_BYTES {
        return None;
    }
    stamp.require_unchanged(&file).ok()?;
    let payload = bytes.strip_prefix(MAGIC)?.strip_prefix(key.as_slice())?;
    let (chunks, remainder) = payload.as_chunks::<4>();
    if !remainder.is_empty() {
        return None;
    }
    let vector: Vec<_> = chunks.iter().copied().map(f32::from_le_bytes).collect();
    valid(&vector).then_some(vector)
}

pub(super) fn save(
    source: &Source,
    stamp: &DeletionIdentity,
    key: &[u8; 32],
    vector: &[f32],
    cancelled: &dyn Fn() -> bool,
) -> io::Result<()> {
    check_cancel(cancelled)?;
    if !valid(vector) || source.read(Some(stamp))?.is_some() {
        return Err(changed());
    }
    let journal = Directory::open(source.directory(), false)?;
    source.require_location(source.directory(), &journal)?;
    let directory = journal.private_child(DIRECTORY, true)?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(
        MAGIC.len() + key.len() + vector.len() * 4,
    ));
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(key);
    for value in vector {
        bytes.extend(value.to_le_bytes());
    }
    directory.replace_with(
        FILE,
        |file| file.write_all(&bytes),
        || {
            check_cancel(cancelled)?;
            source.require_location(source.directory(), &journal)?;
            if !directory.same_as(&journal.private_child(DIRECTORY, false)?)?
                || source.read(Some(stamp))?.is_some()
            {
                return Err(changed());
            }
            if directory.has_entry(FILE)? {
                require_single_link(&directory.open_private_file(FILE)?)?;
            }
            Ok(())
        },
    )?;
    directory.sync()
}

fn valid(vector: &[f32]) -> bool {
    (1..=MAX_DIMENSIONS).contains(&vector.len())
        && vector.iter().all(|value| value.is_finite())
        && vector.iter().any(|&value| value != 0.0)
}
