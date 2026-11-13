//! Checkpointed manual ZIP transfer bound to a reviewed signed catalog entry.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};
use ureq::{Body, http::Response};

use crate::private_files::{Directory, FileStamp, require_single_link};

use super::{Choice, MAX_ARCHIVE_BYTES, Progress, check_cancelled};

const PART: &str = "manual.zip.part";
const RECEIPT: &str = "manual-download.json";
const MAX_RECEIPT: u64 = 8 * 1024;
const CHECKPOINT_INTERVAL: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    catalog: String,
    package: String,
    digest: String,
    size: u64,
}

impl Plan {
    pub(super) fn new(catalog: &str, selected: &Choice) -> Self {
        Self {
            catalog: catalog.into(),
            package: selected.id.clone(),
            digest: selected.digest.clone(),
            size: selected.size,
        }
    }

    fn valid(&self) -> bool {
        lowercase_sha256(&self.catalog)
            && lowercase_sha256(&self.digest)
            && !self.package.is_empty()
            && self.package.len() <= 512
            && self
                .package
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            && (1..=MAX_ARCHIVE_BYTES).contains(&self.size)
    }

    pub(super) fn required_space(&self, saved: Option<&Saved>) -> u64 {
        let verified = saved
            .filter(|saved| &saved.plan == self)
            .map_or(0, |saved| saved.received);
        self.size.saturating_add(self.size - verified)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    plan: Plan,
    received: u64,
    etag: Option<String>,
    prefix: String,
}

impl Receipt {
    fn valid(&self) -> bool {
        self.plan.valid()
            && (1..=self.plan.size).contains(&self.received)
            && lowercase_sha256(&self.prefix)
            && if self.received == self.plan.size {
                self.prefix == self.plan.digest && self.etag.is_none()
            } else {
                self.etag.as_deref().is_some_and(strong_etag)
            }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Saved {
    pub(super) plan: Plan,
    pub(super) received: u64,
}

pub(super) struct Range<'a> {
    pub(super) offset: u64,
    pub(super) etag: &'a str,
}

pub(super) enum Outcome {
    Ready(File),
    Paused { received: u64, interrupted: bool },
}

pub(super) fn saved(directory: &Directory) -> io::Result<Option<Saved>> {
    Ok(read_receipt(directory)?.map(|receipt| Saved {
        plan: receipt.plan,
        received: receipt.received,
    }))
}

pub(super) fn discard(directory: &Directory) -> io::Result<bool> {
    let mut removed = false;
    for name in [PART, RECEIPT] {
        let file = match directory.open_private_file(name) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        require_single_link(&file)?;
        let stamp = FileStamp::read(&file)?;
        drop(file);
        directory.remove_unchanged(name, &stamp)?;
        removed = true;
    }
    if removed {
        directory.sync()?;
    }
    Ok(removed)
}

pub(super) fn fetch(
    directory: &Directory,
    plan: &Plan,
    cancelled: &AtomicBool,
    pausing: &AtomicBool,
    mut open: impl FnMut(Option<Range<'_>>) -> io::Result<Response<Body>>,
    mut progress: impl FnMut(Progress),
) -> io::Result<Outcome> {
    if !plan.valid() {
        return Err(io::Error::other("The reviewed runtime package is invalid"));
    }
    let prior = read_receipt(directory)?;
    if prior.as_ref().is_some_and(|receipt| &receipt.plan != plan)
        || (prior.is_none() && directory.has_entry(PART)?)
    {
        return Err(io::Error::other(
            "Discard the saved runtime download before starting another package",
        ));
    }
    let mut received = prior.as_ref().map_or(0, |receipt| receipt.received);
    let mut buffer = vec![0; 128 * 1024];
    let mut hash = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    let mut prior_stamp = None;
    if let Some(prior) = &prior {
        let mut file = directory.open_private_file(PART)?;
        require_single_link(&file)?;
        let stamp = FileStamp::read(&file)?;
        let size = file.metadata()?.len();
        if size < received || size > plan.size {
            return Err(io::Error::other("The saved runtime archive length changed"));
        }
        let mut remaining = received;
        while remaining > 0 {
            check_cancelled(cancelled)?;
            let size = remaining.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..size])?;
            hash.update(&buffer[..size]);
            remaining -= size as u64;
        }
        stamp.require_unchanged(&file)?;
        if hex(&hash.box_clone().finalize()) != prior.prefix {
            return Err(io::Error::other(
                "The saved runtime archive prefix failed SHA-256 verification",
            ));
        }
        if received == plan.size {
            file.rewind()?;
            return Ok(Outcome::Ready(file));
        }
        prior_stamp = Some(stamp);
    }

    check_cancelled(cancelled)?;
    if pausing.load(Ordering::Acquire) && received > 0 {
        return Ok(Outcome::Paused {
            received,
            interrupted: false,
        });
    }
    let etag = prior.as_ref().and_then(|receipt| receipt.etag.as_deref());
    let mut response = match open(etag.map(|etag| Range {
        offset: received,
        etag,
    })) {
        Ok(response) => response,
        Err(_) if received > 0 && pausing.load(Ordering::Acquire) => {
            return Ok(Outcome::Paused {
                received,
                interrupted: false,
            });
        }
        Err(error) => return Err(error),
    };
    let validator = validate(&response, received, plan.size, etag)?;
    check_cancelled(cancelled)?;
    let mut file = directory.open_lock(PART)?;
    require_single_link(&file)?;
    if let Some(stamp) = prior_stamp {
        stamp.require_unchanged(&file)?;
        // Bytes beyond the last durable checkpoint may remain after a crash.
        file.set_len(received)?;
    } else if file.metadata()?.len() != 0 {
        return Err(io::Error::other(
            "The runtime archive changed before download",
        ));
    }
    file.seek(SeekFrom::Start(received))?;
    let mut input = response.body_mut().as_reader();
    let mut checkpoint = received;
    let mut updated = Instant::now();
    let current = |received| Progress {
        received,
        total: plan.size,
        resumable: validator.is_some() && (1..plan.size).contains(&received),
    };
    progress(current(received));
    let mut interruption = None;
    while received < plan.size {
        check_cancelled(cancelled)?;
        if pausing.load(Ordering::Acquire) {
            interruption = Some(false);
            break;
        }
        let size = (plan.size - received).min(buffer.len() as u64) as usize;
        let count = match input.read(&mut buffer[..size]) {
            Ok(0) | Err(_) => {
                interruption = Some(!pausing.load(Ordering::Acquire));
                break;
            }
            Ok(count) => count,
        };
        file.write_all(&buffer[..count])?;
        hash.update(&buffer[..count]);
        received += count as u64;
        if received < plan.size
            && let Some(etag) = &validator
            && (checkpoint == 0 || received - checkpoint >= CHECKPOINT_INTERVAL)
        {
            write_checkpoint(
                directory,
                &file,
                plan,
                received,
                Some(etag.clone()),
                hex(&hash.box_clone().finalize()),
            )?;
            checkpoint = received;
        }
        if received == checkpoint
            || received == plan.size
            || updated.elapsed() >= Duration::from_millis(100)
        {
            progress(current(received));
            updated = Instant::now();
        }
    }
    check_cancelled(cancelled)?;
    if let Some(interrupted) = interruption {
        if let Some(etag) = validator.filter(|_| received > 0) {
            write_checkpoint(
                directory,
                &file,
                plan,
                received,
                Some(etag),
                hex(&hash.finalize()),
            )?;
            return Ok(Outcome::Paused {
                received,
                interrupted,
            });
        }
        drop(file);
        discard(directory)?;
        return Err(io::Error::other(
            "The runtime download stopped without a strong resume validator",
        ));
    }
    if input.read(&mut buffer[..1])? != 0 || hex(&hash.finalize()) != plan.digest {
        drop(file);
        discard(directory)?;
        return Err(io::Error::other(
            "The downloaded runtime ZIP does not match its signed length and SHA-256",
        ));
    }
    check_cancelled(cancelled)?;
    write_checkpoint(directory, &file, plan, received, None, plan.digest.clone())?;
    drop(file);
    if pausing.load(Ordering::Acquire) {
        return Ok(Outcome::Paused {
            received,
            interrupted: false,
        });
    }
    let file = directory.open_private_file(PART)?;
    require_single_link(&file)?;
    Ok(Outcome::Ready(file))
}

fn validate(
    response: &Response<impl Sized>,
    received: u64,
    total: u64,
    previous_etag: Option<&str>,
) -> io::Result<Option<String>> {
    let header = |name| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
    };
    let etag = header("etag").filter(|etag| strong_etag(etag));
    let status = response.status().as_u16();
    let valid_range = if received == 0 {
        status == 200 && !response.headers().contains_key("content-range")
    } else {
        status == 206
            && previous_etag.is_some()
            && etag == previous_etag
            && header("content-range")
                == Some(format!("bytes {received}-{}/{total}", total - 1).as_str())
    };
    let correct_header = |name, matches: bool| !response.headers().contains_key(name) || matches;
    if !valid_range
        || !correct_header(
            "content-encoding",
            header("content-encoding")
                .is_some_and(|encoding| encoding.eq_ignore_ascii_case("identity")),
        )
        || !correct_header(
            "content-length",
            header("content-length").and_then(|value| value.parse().ok()) == Some(total - received),
        )
    {
        return Err(io::Error::other(
            "The runtime response does not match the signed size or saved range validator",
        ));
    }
    Ok(etag.map(str::to_owned))
}

fn strong_etag(etag: &str) -> bool {
    etag.len() <= 1024
        && etag
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .is_some_and(|value| {
                value
                    .bytes()
                    .all(|byte| byte == b'!' || (b'#'..=b'~').contains(&byte))
            })
}

fn lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(result, "{byte:02x}");
    }
    result
}

fn read_receipt(directory: &Directory) -> io::Result<Option<Receipt>> {
    let file = match directory.open_private_file(RECEIPT) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Vec::new();
    (&file).take(MAX_RECEIPT + 1).read_to_end(&mut bytes)?;
    stamp.require_unchanged(&file)?;
    if bytes.len() as u64 > MAX_RECEIPT {
        return Err(io::Error::other(
            "The saved runtime receipt exceeds its size limit",
        ));
    }
    let receipt: Receipt = serde_json::from_slice(&bytes)?;
    if !receipt.valid() {
        return Err(io::Error::other("The saved runtime receipt is invalid"));
    }
    Ok(Some(receipt))
}

fn write_checkpoint(
    directory: &Directory,
    file: &File,
    plan: &Plan,
    received: u64,
    etag: Option<String>,
    prefix: String,
) -> io::Result<()> {
    file.sync_all()?;
    let receipt = Receipt {
        plan: plan.clone(),
        received,
        etag,
        prefix,
    };
    let bytes = serde_json::to_vec(&receipt)?;
    directory.replace_file(RECEIPT, &bytes)?;
    directory.sync()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_ai::test_support::{accept, listener};
    use std::io::BufRead as _;

    #[test]
    fn interrupted_zip_resumes_only_the_validated_range_and_rechecks_complete_bytes() {
        let profile = crate::snippets::tests::Profile::new();
        let root = Directory::open(&profile.0.canonicalize().unwrap(), false).unwrap();
        let directory = root.private_child("runtime-fixture", true).unwrap();
        let (server, _) = listener();
        let url = format!("http://{}/runtime.zip", server.local_addr().unwrap());
        let bytes = b"PK\x03\x04signed runtime fixture bytes";
        let split = 9;
        let total = bytes.len();
        let worker = std::thread::spawn(move || {
            let read = |socket: &mut std::io::BufReader<std::net::TcpStream>| {
                let mut line = String::new();
                socket.read_line(&mut line).unwrap();
                assert_eq!(line.trim_end(), "GET /runtime.zip HTTP/1.1");
                let mut headers = Vec::new();
                loop {
                    line.clear();
                    assert_ne!(socket.read_line(&mut line).unwrap(), 0);
                    if line == "\r\n" {
                        break;
                    }
                    headers.push(line.trim_end().to_ascii_lowercase());
                }
                headers
            };
            let mut first = accept(&server);
            assert!(
                !read(&mut first)
                    .iter()
                    .any(|line| line.starts_with("range:"))
            );
            write!(first.get_mut(), "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nETag: \"fixture\"\r\nConnection: close\r\n\r\n").unwrap();
            first.get_mut().write_all(&bytes[..split]).unwrap();
            drop(first);
            for etag in ["changed", "fixture"] {
                let mut next = accept(&server);
                let headers = read(&mut next);
                assert!(headers.contains(&format!("range: bytes={split}-")));
                assert!(headers.iter().any(|line| line == "if-range: \"fixture\""));
                write!(next.get_mut(), "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {split}-{}/{total}\r\nETag: \"{etag}\"\r\nConnection: close\r\n\r\n", total - split, total - 1).unwrap();
                next.get_mut().write_all(&bytes[split..]).unwrap();
            }
        });
        let plan = Plan {
            catalog: "a".repeat(64),
            package: "llama-b123-kortty1-fixture-cpu".into(),
            digest: hex(&HashAlgorithm::Sha256.digest(bytes).unwrap()),
            size: total as u64,
        };
        let agent = ureq::Agent::new_with_config(crate::local_ai::connection_config(
            Duration::from_secs(5),
        ));
        let active = AtomicBool::new(false);
        let request = |range: Option<Range<'_>>| {
            let mut request = agent.get(&url);
            if let Some(range) = range {
                request = request
                    .header("Range", format!("bytes={}-", range.offset))
                    .header("If-Range", range.etag);
            }
            request.call().map_err(io::Error::other)
        };
        assert!(
            matches!(fetch(&directory, &plan, &active, &active, request, |_| {}).unwrap(), Outcome::Paused { received, interrupted: true } if received == split as u64)
        );
        assert_eq!(saved(&directory).unwrap().unwrap().received, split as u64);
        let mut crash_tail = directory.open_lock(PART).unwrap();
        crash_tail.seek(SeekFrom::End(0)).unwrap();
        crash_tail.write_all(b"tail").unwrap();
        drop(crash_tail);
        assert!(fetch(&directory, &plan, &active, &active, request, |_| {}).is_err());
        assert_eq!(
            directory
                .open_private_file(PART)
                .unwrap()
                .metadata()
                .unwrap()
                .len(),
            (split + 4) as u64
        );
        let Outcome::Ready(mut zip) =
            fetch(&directory, &plan, &active, &active, request, |_| {}).unwrap()
        else {
            panic!("expected a verified resumed ZIP");
        };
        let mut actual = Vec::new();
        zip.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, bytes);
        drop(zip);
        worker.join().unwrap();
        assert!(matches!(
            fetch(
                &directory,
                &plan,
                &active,
                &active,
                |_| panic!("complete checkpoint must not use the network"),
                |_| {}
            )
            .unwrap(),
            Outcome::Ready(_)
        ));
        let mut changed = directory.open_lock(PART).unwrap();
        changed.write_all(b"bad").unwrap();
        drop(changed);
        assert!(
            fetch(
                &directory,
                &plan,
                &active,
                &active,
                |_| panic!("tampered prefix must fail before the network"),
                |_| {}
            )
            .is_err()
        );
        assert!(discard(&directory).unwrap());
        assert!(saved(&directory).unwrap().is_none());
    }

    #[test]
    fn wrong_status_range_encoding_or_validator_cannot_append() {
        for (status, etag, range, encoding) in [
            (200, "\"strong\"", "bytes 4-7/8", "identity"),
            (206, "W/\"strong\"", "bytes 4-7/8", "identity"),
            (206, "\"changed\"", "bytes 4-7/8", "identity"),
            (206, "\"strong\"", "bytes 4-6/8", "identity"),
            (206, "\"strong\"", "bytes 4-7/8", "gzip"),
        ] {
            let response = Response::builder()
                .status(status)
                .header("etag", etag)
                .header("content-range", range)
                .header("content-encoding", encoding)
                .body(())
                .unwrap();
            assert!(validate(&response, 4, 8, Some("\"strong\"")).is_err());
        }
    }
}
