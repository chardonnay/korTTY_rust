use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write as _;
use std::path::Path;

use serde::Serialize;

use crate::sftp_window::tree::privileged_download::{Destination, PublishError, validate_depth};
use crate::sftp_window::worker::sync::review_root;
use crate::sftp_window::{Location, files, scheduled};

use super::transfer::{MAX_TRANSFER_BYTES, hash_copy};
use super::tree::{MAX_ENTRIES, MAX_PATH_BYTES};
use super::{
    Auth, Connection, ExecOpener, Failure, HashAlgorithm, Helper, Response, lowercase_hex,
    valid_path,
};

#[derive(Serialize)]
struct Entry {
    path: String,
    file: Option<FileProof>,
    #[serde(skip)]
    parts: Vec<OsString>,
}

#[derive(Serialize)]
struct FileProof {
    size: u64,
    sha256: String,
}

struct Manifest {
    witness: String,
    entries: Vec<Entry>,
    size: u64,
}

impl Manifest {
    fn read(channel: &mut Connection) -> Result<Self, Failure> {
        let (witness, count, size) = match channel.response()? {
            Response::TreeManifest {
                witness,
                entries,
                size,
            } if entries <= MAX_ENTRIES
                && size <= MAX_TRANSFER_BYTES
                && lowercase_hex(&witness, 64) =>
            {
                (witness, entries, size)
            }
            response => return Err(channel.rejection(response, Failure::Failed)),
        };
        let mut entries = Vec::with_capacity(count);
        let mut seen = BTreeMap::<String, bool>::new();
        let mut remaining = size;
        let mut labels = 0;
        for _ in 0..count {
            let (path, file) = match channel.response()? {
                Response::TreeDirectory { path } => (path, None),
                Response::TreeFile { path, size, sha256 } if lowercase_hex(&sha256, 64) => {
                    remaining = remaining.checked_sub(size).ok_or(Failure::Blocked)?;
                    (path, Some(FileProof { size, sha256 }))
                }
                response => return Err(channel.rejection(response, Failure::Failed)),
            };
            labels += path.as_bytes().escape_ascii().count();
            if labels > MAX_PATH_BYTES
                || path
                    .rsplit_once('/')
                    .is_some_and(|(parent, _)| seen.get(parent) != Some(&true))
                || seen.insert(path.clone(), file.is_none()).is_some()
            {
                return Err(Failure::Blocked);
            }
            let parts = local_parts(&path, file.is_none())?;
            entries.push(Entry { path, file, parts });
        }
        if remaining != 0 {
            return Err(Failure::Blocked);
        }
        Ok(Self {
            witness,
            entries,
            size,
        })
    }

    fn proof(&self, destination: &Destination) -> Result<[u8; 32], Failure> {
        let bytes = serde_json::to_vec(&(
            "kortty-privileged-tree-download-v1",
            &self.witness,
            &self.entries,
            self.size,
            destination.witness().map_err(|_| Failure::Blocked)?,
        ))
        .map_err(|_| Failure::Failed)?;
        let mut hash = HashAlgorithm::Sha256
            .new_hasher()
            .map_err(|_| Failure::Failed)?;
        hash.update(&bytes);
        Ok(hash.finalize().as_ref().try_into().expect("SHA-256 size"))
    }
}

fn local_parts(path: &str, directory: bool) -> Result<Vec<OsString>, Failure> {
    if !valid_path(&format!("/{path}")) {
        return Err(Failure::Blocked);
    }
    validate_depth(path.split('/').count(), directory).map_err(|_| Failure::Blocked)?;
    path.split('/')
        .map(|part| files::local_name(part.as_bytes()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| Failure::Blocked)
}

#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Request<'a> {
    InspectTreeSource { path: &'a str },
    DownloadTree { path: &'a str },
}

pub(crate) fn preview_tree_download(
    opener: ExecOpener,
    path: &str,
    local: &Path,
    auth: Auth,
    allowed: impl Fn() -> bool + Send + Sync + 'static,
) -> Result<scheduled::Preview, Failure> {
    if !valid_path(path) {
        return Err(Failure::Blocked);
    }
    let roots = [
        review_root(&Location::Remote(path.as_bytes().to_vec())).map_err(|_| Failure::Blocked)?,
        review_root(&Location::Local(local.to_owned())).map_err(|_| Failure::Blocked)?,
    ];
    let destination = Destination::inspect(local).map_err(|_| Failure::Blocked)?;
    let mut channel = Connection::open_for(&opener, auth, allowed, Helper::Transfer)?;
    channel.send(&Request::InspectTreeSource { path })?;
    let manifest = Manifest::read(&mut channel)?;
    channel.finish().map_err(|_| channel.failure())?;
    Ok(scheduled::Preview {
        proof: manifest.proof(&destination)?,
        replacements: Vec::new(),
        roots,
        paths: manifest
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.file.is_none(),
                    entry.path.as_bytes().escape_ascii().to_string(),
                )
            })
            .collect(),
        bytes: manifest.size,
    })
}

pub(crate) fn download_tree(
    opener: ExecOpener,
    path: &str,
    local: &Path,
    auth: Auth,
    allowed: impl Fn() -> bool + Send + Sync + 'static,
    claim: impl FnOnce([u8; 32]) -> Result<(), Failure>,
    mut publication_allowed: impl FnMut() -> bool,
) -> Result<(usize, u64), Failure> {
    if !valid_path(path) {
        return Err(Failure::Blocked);
    }
    let destination = Destination::inspect(local).map_err(|_| Failure::Blocked)?;
    let mut channel = Connection::open_for(&opener, auth, allowed, Helper::Transfer)?;
    channel.send(&Request::DownloadTree { path })?;
    let manifest = Manifest::read(&mut channel)?;
    let token = match channel.response()? {
        Response::Ready { token } if lowercase_hex(&token, 32) => token,
        response => return Err(channel.rejection(response, Failure::Failed)),
    };
    channel.gate.check().map_err(|_| channel.failure())?;
    claim(manifest.proof(&destination)?)?;
    let mut stage = destination.stage().map_err(|_| Failure::Conflict)?;
    channel
        .write_all(token.as_bytes())
        .map_err(|_| channel.failure())?;
    for entry in &manifest.entries {
        channel.gate.check().map_err(|_| channel.failure())?;
        if let Some(file) = &entry.file {
            let mut output = stage.file(&entry.parts).map_err(|_| Failure::Blocked)?;
            if hash_copy(&mut channel, &mut output, file.size, || true)? != file.sha256 {
                return Err(Failure::Conflict);
            }
            output.sync_all().map_err(|_| Failure::Failed)?;
        } else {
            stage
                .directory(&entry.parts)
                .map_err(|_| Failure::Blocked)?;
        }
    }
    match channel.response()? {
        Response::TreeRead { witness } if witness == manifest.witness => {}
        response => return Err(channel.rejection(response, Failure::Failed)),
    }
    channel.finish().map_err(|_| channel.failure())?;
    stage
        .publish(|| channel.gate.running() && publication_allowed() && channel.gate.running())
        .map_err(|error| match error {
            PublishError::Denied | PublishError::Refused if !channel.gate.running() => {
                channel.failure()
            }
            PublishError::Denied => Failure::Blocked,
            PublishError::Refused => Failure::Conflict,
            PublishError::Uncertain => Failure::Uncertain,
        })?;
    Ok((manifest.entries.len(), manifest.size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privileged_tree_manifest_rejects_depth_beyond_destination_cleanup() {
        let path = |parts| vec!["d"; parts].join("/");
        assert_eq!(local_parts(&path(32), true).unwrap().len(), 32);
        assert_eq!(local_parts(&path(33), false).unwrap().len(), 33);
        assert_eq!(local_parts(&path(33), true), Err(Failure::Blocked));
        assert_eq!(local_parts(&path(34), false), Err(Failure::Blocked));
        for invalid in ["", "/d", "d/../f", "d//f"] {
            assert_eq!(local_parts(invalid, false), Err(Failure::Blocked));
        }
    }
}
