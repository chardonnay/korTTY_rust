use std::io::{self, Write as _};
use std::path::Path;

use serde::Serialize;

use crate::private_files::require_single_link;
use crate::sftp_window::compare::folders::{Excludes, excludes};
use crate::sftp_window::tree::snapshot::{self, DirectoryStamp, Entry, Manifest, Relative};
use crate::sftp_window::worker::sync::review_root;
use crate::sftp_window::{LocalSource, Location, scheduled};

use super::transfer::{MAX_TRANSFER_BYTES, hash_copy};
use super::{
    Auth, Connection, ExecOpener, Failure, HashAlgorithm, Helper, Response, lowercase_hex,
    valid_path,
};

pub(super) const MAX_ENTRIES: usize = 128;
pub(super) const MAX_PATH_BYTES: usize = 32 * 1024;

pub(crate) struct TreeUpload {
    manifest: Manifest,
    excludes: Excludes,
    entries: Vec<ReviewedEntry>,
    roots: [String; 2],
    size: u64,
    source_proof: [u8; 32],
}

struct ReviewedEntry {
    relative: Relative,
    path: String,
    file: Option<ReviewedFile>,
}

struct ReviewedFile {
    size: u64,
    sha256: String,
}

impl TreeUpload {
    pub(crate) fn scan(
        local: &Path,
        remote: &str,
        patterns: &str,
        allowed: impl Fn() -> bool,
    ) -> Result<Self, Failure> {
        if !valid_path(remote) {
            return Err(Failure::Blocked);
        }
        let roots = [
            review_root(&Location::Local(local.to_owned())).map_err(|_| Failure::Blocked)?,
            review_root(&Location::Remote(remote.as_bytes().to_vec()))
                .map_err(|_| Failure::Blocked)?,
        ];
        let excludes = excludes(patterns).map_err(|_| Failure::Blocked)?;
        let manifest = snapshot::read_local(local, &|path| excludes.matches(path), &|| !allowed())
            .map_err(|_| {
                if allowed() {
                    Failure::Blocked
                } else {
                    Failure::Cancelled
                }
            })?;
        if manifest.entries.len() > MAX_ENTRIES {
            return Err(Failure::Blocked);
        }
        let mut hasher = HashAlgorithm::Sha256
            .new_hasher()
            .map_err(|_| Failure::Failed)?;
        hasher.update(b"kortty-privileged-tree-source-v1");
        manifest
            .review_bytes(|bytes| {
                hasher.update(&(bytes.len() as u64).to_le_bytes());
                hasher.update(bytes);
            })
            .map_err(|_| Failure::Failed)?;
        let mut size = 0_u64;
        let mut labels = 0_usize;
        let mut entries = Vec::with_capacity(manifest.entries.len());
        for (relative, entry) in &manifest.entries {
            if !allowed() {
                return Err(Failure::Cancelled);
            }
            let path = std::str::from_utf8(relative.bytes()).map_err(|_| Failure::Blocked)?;
            labels += relative.review_label().len();
            if !valid_path(&format!("/{path}")) || labels > MAX_PATH_BYTES {
                return Err(Failure::Blocked);
            }
            let file = match entry {
                Entry::LocalFolder(_) => None,
                Entry::LocalFile { stamp, .. } => {
                    size = size
                        .checked_add(stamp.size)
                        .filter(|size| *size <= MAX_TRANSFER_BYTES)
                        .ok_or(Failure::Blocked)?;
                    let source = open_file(&manifest, relative)?;
                    let sha256 =
                        hash_copy(&mut source.file(), &mut io::sink(), stamp.size, &allowed)?;
                    source.verify().map_err(|_| Failure::Conflict)?;
                    hasher.update(sha256.as_bytes());
                    Some(ReviewedFile {
                        size: stamp.size,
                        sha256,
                    })
                }
                _ => return Err(Failure::Blocked),
            };
            entries.push(ReviewedEntry {
                relative: relative.clone(),
                path: path.to_owned(),
                file,
            });
        }
        let plan = Self {
            manifest,
            excludes,
            entries,
            roots,
            size,
            source_proof: hasher.finalize().as_ref().try_into().expect("SHA-256 size"),
        };
        plan.verify(&allowed)?;
        Ok(plan)
    }

    fn verify(&self, allowed: &impl Fn() -> bool) -> Result<(), Failure> {
        let Location::Local(local) = &self.manifest.root else {
            return Err(Failure::Blocked);
        };
        let current =
            snapshot::read_local(local, &|path| self.excludes.matches(path), &|| !allowed())
                .map_err(|_| {
                    if allowed() {
                        Failure::Conflict
                    } else {
                        Failure::Cancelled
                    }
                })?;
        if !self.manifest.matches(&current) {
            return Err(Failure::Conflict);
        }
        Ok(())
    }

    pub(crate) fn proof(&self, witness: &str) -> Result<[u8; 32], Failure> {
        let mut hasher = HashAlgorithm::Sha256
            .new_hasher()
            .map_err(|_| Failure::Failed)?;
        hasher.update(b"kortty-privileged-tree-publication-v1");
        hasher.update(&self.source_proof);
        hasher.update(witness.as_bytes());
        Ok(hasher.finalize().as_ref().try_into().expect("SHA-256 size"))
    }

    pub(crate) fn preview(&self, witness: &str) -> Result<scheduled::Preview, Failure> {
        Ok(scheduled::Preview {
            proof: self.proof(witness)?,
            roots: self.roots.clone(),
            paths: self
                .entries
                .iter()
                .map(|entry| (entry.file.is_none(), entry.relative.review_label()))
                .collect(),
            bytes: self.size,
            replacements: Vec::new(),
        })
    }

    pub(crate) fn totals(&self) -> (usize, u64) {
        (self.entries.len(), self.size)
    }
}

fn open_file(manifest: &Manifest, relative: &Relative) -> Result<LocalSource, Failure> {
    let Some(Entry::LocalFile { stamp, identity }) = manifest.entries.get(relative) else {
        return Err(Failure::Conflict);
    };
    let Some((_, DirectoryStamp::Local(parent))) = manifest.parent(relative) else {
        return Err(Failure::Conflict);
    };
    let Location::Local(path) = manifest.location(relative).map_err(|_| Failure::Blocked)? else {
        return Err(Failure::Blocked);
    };
    let source = LocalSource::open(&path).map_err(|_| Failure::Conflict)?;
    require_single_link(source.file())
        .and_then(|()| source.verify_reviewed(stamp, identity, &parent))
        .map_err(|_| Failure::Conflict)?;
    Ok(source)
}

#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Request<'a> {
    InspectTree {
        path: &'a str,
    },
    CreateTree {
        path: &'a str,
        entries: usize,
        size: u64,
    },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EntryFrame<'a> {
    Folder {
        path: &'a str,
    },
    File {
        path: &'a str,
        size: u64,
        sha256: &'a str,
    },
}

pub(crate) fn inspect_tree(
    opener: ExecOpener,
    path: &str,
    auth: Auth,
    allowed: impl Fn() -> bool + Send + Sync + 'static,
) -> Result<String, Failure> {
    if !valid_path(path) {
        return Err(Failure::Blocked);
    }
    let mut channel = Connection::open_for(&opener, auth, allowed, Helper::Transfer)?;
    channel.send(&Request::InspectTree { path })?;
    let witness = match channel.response()? {
        Response::TreeTarget {
            witness,
            token: None,
        } if lowercase_hex(&witness, 64) => witness,
        response => return Err(channel.rejection(response, Failure::Failed)),
    };
    channel.finish().map_err(|_| channel.failure())?;
    Ok(witness)
}

pub(crate) fn upload_tree(
    opener: ExecOpener,
    path: &str,
    plan: &TreeUpload,
    auth: Auth,
    allowed: impl Fn() -> bool + Send + Sync + 'static,
    claim: impl FnOnce([u8; 32]) -> Result<(), Failure>,
    publication_allowed: impl FnOnce() -> bool,
) -> Result<(), Failure> {
    if !valid_path(path) {
        return Err(Failure::Blocked);
    }
    let (entries, size) = plan.totals();
    let mut channel = Connection::open_for(&opener, auth, allowed, Helper::Transfer)?;
    channel.send(&Request::CreateTree {
        path,
        entries,
        size,
    })?;
    let (witness, token) = match channel.response()? {
        Response::TreeTarget {
            witness,
            token: Some(token),
        } if lowercase_hex(&witness, 64) && lowercase_hex(&token, 32) => (witness, token),
        response => return Err(channel.rejection(response, Failure::Failed)),
    };
    channel.gate.check().map_err(|_| channel.failure())?;
    claim(plan.proof(&witness)?)?;
    channel
        .write_all(token.as_bytes())
        .map_err(|_| channel.failure())?;
    match channel.response()? {
        Response::Accept => {}
        response => return Err(channel.rejection(response, Failure::Failed)),
    }
    for entry in &plan.entries {
        let path = entry.path.as_str();
        if let Some(file) = &entry.file {
            let source = open_file(&plan.manifest, &entry.relative)?;
            channel.send(&EntryFrame::File {
                path,
                size: file.size,
                sha256: &file.sha256,
            })?;
            let hashed = hash_copy(&mut source.file(), &mut channel, file.size, || true)?;
            if hashed != file.sha256 || source.verify().is_err() {
                return Err(Failure::Conflict);
            }
        } else {
            channel.send(&EntryFrame::Folder { path })?;
        }
    }
    let token = match channel.response()? {
        Response::Ready { token } if lowercase_hex(&token, 32) => token,
        response => return Err(channel.rejection(response, Failure::Failed)),
    };
    plan.verify(&|| channel.gate.running())?;
    if !publication_allowed() {
        return Err(Failure::Blocked);
    }
    channel.gate.check().map_err(|_| channel.failure())?;
    channel
        .write_all(token.as_bytes())
        .map_err(|_| Failure::Uncertain)?;
    match channel.response().map_err(|_| Failure::Uncertain)? {
        Response::TreeCreated {
            entries: count,
            size: bytes,
        } if count == entries && bytes == size => {}
        response => return Err(channel.rejection(response, Failure::Uncertain)),
    }
    channel.finish().map_err(|_| Failure::Uncertain)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn privileged_tree_proof_binds_file_bytes_and_destination_parent() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        fs::create_dir(root.join("empty")).unwrap();
        fs::write(root.join("file.txt"), b"first").unwrap();
        fs::write(root.join("excluded.tmp"), b"excluded").unwrap();
        let scan = || TreeUpload::scan(&root, "/protected/new", "*.tmp", || true).unwrap();
        let original = scan();
        let preview = original.preview(&"0".repeat(64)).unwrap();
        assert_eq!(
            preview.paths,
            vec![(true, "empty".into()), (false, "file.txt".into())]
        );
        assert_eq!(preview.bytes, 5);
        assert_ne!(preview.proof, original.proof(&"1".repeat(64)).unwrap());
        fs::write(root.join("file.txt"), b"other").unwrap();
        assert_ne!(preview.proof, scan().proof(&"0".repeat(64)).unwrap());
        fs::hard_link(root.join("file.txt"), root.join("extra.txt")).unwrap();
        assert!(matches!(
            TreeUpload::scan(&root, "/protected/new", "*.tmp", || true),
            Err(Failure::Conflict)
        ));
    }
}
