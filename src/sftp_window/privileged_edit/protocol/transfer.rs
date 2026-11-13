use std::io::{Read, Seek, Write};

use serde::Serialize;

use super::{
    Auth, Connection, ExecOpener, Failure, HashAlgorithm, Helper, Response, Zeroizing, digest_hex,
    lowercase_hex, valid_path,
};
use crate::private_files::require_single_link;
use crate::sftp_window::LocalSource;

pub(crate) const MAX_TRANSFER_BYTES: u64 = 1024 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;

#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Request<'a> {
    Download {
        path: &'a str,
    },
    Create {
        path: &'a str,
        size: u64,
        sha256: &'a str,
    },
}

pub(crate) fn download(
    opener: ExecOpener,
    path: &str,
    mut destination: impl Write,
    auth: Auth,
    allowed: impl Fn() -> bool + Send + Sync + 'static,
) -> Result<u64, Failure> {
    if !valid_path(path) {
        return Err(Failure::Blocked);
    }
    let mut channel = Connection::open_for(&opener, auth, allowed, Helper::Transfer)?;
    channel.send(&Request::Download { path })?;
    let size = match channel.response()? {
        Response::Download { size } if size <= MAX_TRANSFER_BYTES => size,
        response => return Err(channel.rejection(response, Failure::Failed)),
    };
    let digest = hash_copy(&mut channel, &mut destination, size, || true)?;
    match channel.response()? {
        Response::Downloaded {
            size: count,
            sha256,
        } if count == size && sha256 == digest => {}
        response => return Err(channel.rejection(response, Failure::Failed)),
    }
    channel.finish().map_err(|_| channel.failure())?;
    Ok(size)
}

pub(crate) fn upload(
    opener: ExecOpener,
    path: &str,
    source: &LocalSource,
    auth: Auth,
    allowed: impl Fn() -> bool + Send + Sync + 'static,
    publication_allowed: impl FnOnce() -> bool,
) -> Result<u64, Failure> {
    let size = source.size();
    if !valid_path(path) || size > MAX_TRANSFER_BYTES {
        return Err(Failure::Blocked);
    }
    verify_source(source)?;
    let mut input = source.file();
    let digest = hash_copy(&mut input, &mut std::io::sink(), size, &allowed)?;
    verify_source(source)?;
    input.rewind().map_err(|_| Failure::Failed)?;
    let mut channel = Connection::open_for(&opener, auth, allowed, Helper::Transfer)?;
    channel.send(&Request::Create {
        path,
        size,
        sha256: &digest,
    })?;
    match channel.response()? {
        Response::Accept => {}
        response => return Err(channel.rejection(response, Failure::Failed)),
    }
    if hash_copy(&mut input, &mut channel, size, || true)? != digest {
        return Err(Failure::Conflict);
    }
    let token = match channel.response()? {
        Response::Ready { token } if lowercase_hex(&token, 32) => token,
        response => return Err(channel.rejection(response, Failure::Failed)),
    };
    verify_source(source)?;
    if !publication_allowed() {
        return Err(Failure::Blocked);
    }
    channel.gate.check().map_err(|_| channel.failure())?;
    // After queueing the token the remote publication outcome requires a reply.
    channel
        .write_all(token.as_bytes())
        .map_err(|_| Failure::Uncertain)?;
    match channel.response().map_err(|_| Failure::Uncertain)? {
        Response::Created {
            size: count,
            sha256,
        } if count == size && sha256 == digest => {}
        response => return Err(channel.rejection(response, Failure::Uncertain)),
    }
    channel.finish().map_err(|_| Failure::Uncertain)?;
    Ok(size)
}

fn verify_source(source: &LocalSource) -> Result<(), Failure> {
    require_single_link(source.file())
        .and_then(|()| source.verify())
        .map_err(|_| Failure::Conflict)
}

pub(super) fn hash_copy(
    source: &mut impl Read,
    destination: &mut impl Write,
    mut remaining: u64,
    allowed: impl Fn() -> bool,
) -> Result<String, Failure> {
    let mut hash = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(|_| Failure::Failed)?;
    let mut buffer = Zeroizing::new(vec![0; CHUNK]);
    while remaining != 0 {
        if !allowed() {
            return Err(Failure::Cancelled);
        }
        let count = remaining.min(CHUNK as u64) as usize;
        let bytes = &mut buffer[..count];
        source.read_exact(bytes).map_err(|_| Failure::Failed)?;
        hash.update(bytes);
        destination.write_all(bytes).map_err(|_| Failure::Failed)?;
        remaining -= count as u64;
    }
    digest_hex(hash.finalize().as_ref())
}
