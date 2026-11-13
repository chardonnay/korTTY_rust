//! Bounded UTF-8 validation, hashing and passage extraction from one held file.

use std::fs::File;
use std::io::{self, Read};

use pgp::crypto::hash::HashAlgorithm;

use super::{CHUNK_BYTES, ConfiguredFileTooLarge, Location, SourceSummary, chunk_end, valid_text};

const READ_BYTES: usize = 32 * 1024;

pub(super) fn scan(
    file: &File,
    limit: u64,
    active: impl Fn() -> bool,
    mut passage: impl FnMut(Location, &str) -> io::Result<()>,
) -> io::Result<SourceSummary> {
    if file.metadata()?.len() > limit {
        return Err(oversized());
    }
    let mut reader = file.take(limit + 1);
    let mut hash = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    let mut buffer = vec![0; READ_BYTES + CHUNK_BYTES + 4];
    let mut held = 0;
    let mut bytes = 0;
    let mut line = 1;
    let mut has_passages = false;
    loop {
        if !active() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "The knowledge request was cancelled",
            ));
        }
        let count = reader.read(&mut buffer[held..held + READ_BYTES])?;
        bytes += count;
        if reader.limit() == 0 {
            return Err(oversized());
        }
        hash.update(&buffer[held..held + count]);
        held += count;
        let text = match std::str::from_utf8(&buffer[..held]) {
            Ok(text) => text,
            Err(error) if error.error_len().is_none() && count != 0 => {
                std::str::from_utf8(&buffer[..error.valid_up_to()]).expect("validated UTF-8 prefix")
            }
            Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
        };
        if !valid_text(text) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Source contains binary control characters",
            ));
        }
        let mut remaining = text;
        while remaining.len() > CHUNK_BYTES || (count == 0 && !remaining.is_empty()) {
            if !active() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "The knowledge request was cancelled",
                ));
            }
            let (prefix, rest) = remaining.split_at(chunk_end(remaining));
            let trimmed = prefix.trim_end();
            if !trimmed.trim_start().is_empty() {
                passage(Location::Line(line), trimmed)?;
                has_passages = true;
            }
            line += prefix.bytes().filter(|&byte| byte == b'\n').count();
            remaining = rest;
        }
        if count == 0 {
            break;
        }
        let used = text.len() - remaining.len();
        buffer.copy_within(used..held, 0);
        held -= used;
    }
    let digest = hash
        .finalize()
        .as_ref()
        .try_into()
        .expect("SHA-256 has 32 bytes");
    Ok(SourceSummary {
        digest,
        bytes,
        has_passages,
    })
}

fn oversized() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, ConfiguredFileTooLarge)
}

#[cfg(test)]
#[test]
fn knowledge_corpus_stream_matches_whole_file_chunks_and_cancels() {
    use std::cell::Cell;

    let profile = crate::snippets::tests::Profile::new();
    let path = profile.0.join("stream.txt");
    let text = format!(
        "{}\r\n{}界\n\n{}Ω\n tail",
        "a".repeat(CHUNK_BYTES - 1),
        "b".repeat(READ_BYTES - CHUNK_BYTES - 2),
        "x".repeat(READ_BYTES)
    );
    std::fs::write(&path, &text).unwrap();
    let original = super::chunks(&text)
        .map(|(line, text)| (Location::Line(line), text.to_owned()))
        .collect::<Vec<_>>();
    let mut actual = Vec::new();
    let limit = text.len() as u64;
    let source = scan(
        &File::open(&path).unwrap(),
        limit,
        || true,
        |location, text| {
            actual.push((location, text.to_owned()));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(actual, original);
    assert_eq!(source.bytes, text.len());
    assert_eq!(source.digest, super::sha256(text.as_bytes()).unwrap());
    assert!(source.has_passages);
    assert!(
        scan(
            &File::open(&path).unwrap(),
            limit - 1,
            || true,
            |_, _| Ok(())
        )
        .is_err()
    );
    let checks = Cell::new(0);
    let error = scan(
        &File::open(&path).unwrap(),
        limit,
        || {
            checks.set(checks.get() + 1);
            checks.get() < 3
        },
        |_, _| Ok(()),
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    std::fs::write(&path, [b'a', 0xf0, 0x90]).unwrap();
    let error = scan(&File::open(&path).unwrap(), 3, || true, |_, _| Ok(()))
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}
