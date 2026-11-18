//! Compare a protected source with a held local folder and publish only reviewed changes.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Seek as _};
use std::path::{Path, PathBuf};

use kortty_server_archive::rsync_tree::Manifest as Inventory;
use sha2::{Digest as _, Sha256};

use crate::sftp_window::local_tree::{self, Directory, Entry, ReviewedStamp};
use crate::sftp_window::tree::privileged_download::{
    Destination, PublishError, Stage, private_directory, private_regular_file,
    receiver_for_directory,
};
use crate::sftp_window::tree::single_file::FilePublishError;
use crate::sftp_window::worker::resume::persist::Identity;
use crate::sftp_window::{
    ReviewedFilePublication, ReviewedFileReceiver, StagedDownload, scheduled,
};

use super::super::transfer::copy_digest;
use super::super::{Failure, MergeReport, digest_hex};
use super::{MAX_TRANSFER_BYTES, Manifest};

type Relative = Vec<OsString>;

pub(super) struct Plan {
    scope: Scope,
    targets: Vec<Target>,
    pub(super) preview: scheduled::Preview,
}

struct Scope {
    root: PathBuf,
    directories: BTreeMap<Relative, DirectoryBinding>,
}

struct DirectoryBinding {
    directory: Directory,
    stamp: ReviewedStamp,
    birth: Option<(i64, u32)>,
}

impl DirectoryBinding {
    fn new(directory: Directory) -> io::Result<Self> {
        let stamp = local_tree::reviewed_tree_parent_stamp(&directory)?;
        #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
        let birth = local_tree::reviewed_tree_parent_birth(&directory, &stamp);
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        let birth = None;
        Ok(Self {
            directory,
            stamp,
            birth,
        })
    }
}

impl Scope {
    fn receiver(&self) -> Result<Option<[u8; 32]>, Failure> {
        let root = self.directories.get(&Vec::new()).ok_or(Failure::Conflict)?;
        receiver_for_directory(&self.root, &root.directory).map_err(|_| Failure::Conflict)
    }

    fn current(&self) -> io::Result<()> {
        for (parts, expected) in &self.directories {
            let current = if let Some((name, parent)) = parts.split_last() {
                let parent = &self.directories.get(parent).ok_or_else(changed)?.directory;
                local_tree::open_reviewed_tree_child_for_files(parent, name)?
            } else {
                local_tree::open_reviewed_tree_parent(&self.root)?
            };
            let current = DirectoryBinding::new(current)?;
            if !expected.directory.same_as(&current.directory)?
                || expected.stamp != current.stamp
                || expected.birth != current.birth
            {
                return Err(changed());
            }
        }
        Ok(())
    }
}

enum Target {
    Keep,
    NewTree(Destination),
    WithinNewTree { root: usize, depth: usize },
    NewFile { path: PathBuf, parent: Identity },
    Replace(Box<ReviewedFileReceiver>),
}

impl Plan {
    pub(super) fn receiver(&self) -> Result<Option<[u8; 32]>, Failure> {
        self.scope.receiver()
    }

    pub(super) fn inspect(
        manifest: &Manifest,
        root: &Path,
        roots: [String; 2],
        stopped: &impl Fn() -> bool,
    ) -> Result<Self, Failure> {
        let directory =
            local_tree::open_reviewed_tree_parent(root).map_err(|_| Failure::Blocked)?;
        let root_binding = DirectoryBinding::new(directory).map_err(|_| Failure::Blocked)?;
        let mut scope = Scope {
            root: root.to_owned(),
            directories: BTreeMap::from([(Vec::new(), root_binding)]),
        };
        let mut proof = Sha256::new_with_prefix(b"kortty-privileged-tree-download-merge-v1\0");
        bind(&mut proof, &(&roots, &manifest.witness, &manifest.entries))?;
        let mut targets = Vec::with_capacity(manifest.entries.len());
        let mut missing = BTreeMap::<Relative, (usize, usize)>::new();
        let mut preview = scheduled::Preview {
            proof: [0; 32],
            roots,
            paths: Vec::new(),
            replacements: Vec::new(),
            bytes: 0,
        };
        let mut receiver_bytes = 0u64;
        for entry in &manifest.entries {
            if stopped() {
                return Err(Failure::Cancelled);
            }
            let (name, parts) = entry.parts.split_last().ok_or(Failure::Blocked)?;
            let path = root.join(entry.parts.iter().collect::<PathBuf>());
            let directory = entry.file.is_none();
            let target = if let Some(&(index, depth)) = missing.get(parts) {
                if directory {
                    missing.insert(entry.parts.clone(), (index, depth));
                }
                Target::WithinNewTree { root: index, depth }
            } else {
                let parent = &scope
                    .directories
                    .get(parts)
                    .ok_or(Failure::Conflict)?
                    .directory;
                match local_tree::entry(parent, name) {
                    Ok(Entry::Directory(_)) if directory => {
                        let current = local_tree::open_reviewed_tree_child_for_files(parent, name)
                            .and_then(DirectoryBinding::new)
                            .map_err(|_| Failure::Conflict)?;
                        scope.directories.insert(entry.parts.clone(), current);
                        Target::Keep
                    }
                    Ok(Entry::File(file)) if !directory => {
                        let source = entry.file.as_ref().ok_or(Failure::Blocked)?;
                        let length = file.metadata().map_err(|_| Failure::Conflict)?.len();
                        receiver_bytes = receiver_bytes
                            .checked_add(length)
                            .filter(|total| *total <= MAX_TRANSFER_BYTES)
                            .ok_or(Failure::Blocked)?;
                        let receiver = ReviewedFileReceiver::open_transfer(&path, stopped)
                            .map_err(|_| {
                                if stopped() {
                                    Failure::Cancelled
                                } else {
                                    Failure::Conflict
                                }
                            })?;
                        let (bytes, digest) = receiver.content();
                        if bytes != length {
                            return Err(Failure::Conflict);
                        }
                        let digest = digest_hex(&digest)?;
                        bind(&mut proof, &("file", &entry.path, receiver.fingerprint()))?;
                        if source.size == bytes && source.sha256 == digest {
                            Target::Keep
                        } else {
                            preview.replacements.push(scheduled::ReplacementPreview {
                                source: entry.path.as_bytes().escape_ascii().to_string(),
                                target: entry.path.as_bytes().escape_ascii().to_string(),
                                source_sha256: source.sha256.clone(),
                                target_sha256: digest,
                            });
                            Target::Replace(Box::new(receiver))
                        }
                    }
                    result => {
                        let missing_entry = matches!(result, Ok(Entry::Skip))
                            || matches!(result, Err(error) if error.kind() == io::ErrorKind::NotFound);
                        if !missing_entry || !is_absent(&path) {
                            return Err(Failure::Conflict);
                        }
                        bind(&mut proof, &("vacant", &entry.path))?;
                        if directory {
                            missing.insert(entry.parts.clone(), (targets.len(), entry.parts.len()));
                            Target::NewTree(
                                Destination::inspect(&path).map_err(|_| Failure::Conflict)?,
                            )
                        } else {
                            Target::NewFile {
                                path,
                                parent: Identity::directory(parent)
                                    .map_err(|_| Failure::Conflict)?,
                            }
                        }
                    }
                }
            };
            match target {
                Target::Keep => {}
                Target::Replace(_) => {
                    preview.bytes += entry.file.as_ref().ok_or(Failure::Blocked)?.size
                }
                _ => {
                    preview
                        .paths
                        .push((directory, entry.path.as_bytes().escape_ascii().to_string()));
                    preview.bytes += entry.file.as_ref().map_or(0, |file| file.size);
                }
            }
            targets.push(target);
        }
        for (parts, binding) in &scope.directories {
            bind(
                &mut proof,
                &("directory", parts, binding.stamp, binding.birth),
            )?;
        }
        scope.current().map_err(|_| Failure::Conflict)?;
        preview.proof = proof.finalize().into();
        Ok(Self {
            scope,
            targets,
            preview,
        })
    }

    pub(super) fn receive(
        self,
        manifest: &Manifest,
        input: &mut impl Read,
        stopped: &impl Fn() -> bool,
    ) -> Result<Prepared, Failure> {
        if manifest.entries.len() != self.targets.len() {
            return Err(Failure::Conflict);
        }
        let mut pending = Vec::with_capacity(self.targets.len());
        for (entry, target) in manifest.entries.iter().zip(self.targets) {
            if stopped() {
                return Err(Failure::Cancelled);
            }
            let publication = match target {
                Target::Keep => {
                    if let Some(file) = &entry.file {
                        receive_file(input, &mut io::sink(), file, stopped)?;
                    }
                    None
                }
                Target::NewTree(destination) => Some(Publication::NewTree {
                    stage: destination.stage().map_err(|_| Failure::Conflict)?,
                    tree: BTreeMap::new(),
                }),
                Target::WithinNewTree { root, depth } => {
                    let Some(Some(Publication::NewTree { stage, tree })) = pending.get_mut(root)
                    else {
                        return Err(Failure::Blocked);
                    };
                    let parts = entry.parts.get(depth..).ok_or(Failure::Blocked)?;
                    let payload = if let Some(file) = &entry.file {
                        let mut output = stage.file(parts).map_err(|_| Failure::Conflict)?;
                        let payload = receive_file(input, &mut output, file, stopped)?;
                        output.sync_all().map_err(|_| Failure::Failed)?;
                        Some(payload)
                    } else {
                        stage.directory(parts).map_err(|_| Failure::Conflict)?;
                        None
                    };
                    tree.insert(parts.to_vec(), payload);
                    None
                }
                Target::NewFile { path, parent } => {
                    if !is_absent(&path) {
                        return Err(Failure::Conflict);
                    }
                    let stage = StagedDownload::stage_for_reviewed_file(&path, &parent)
                        .map_err(|_| Failure::Conflict)?;
                    let payload = receive_file(
                        input,
                        &mut stage.writer().map_err(|_| Failure::Failed)?,
                        entry.file.as_ref().ok_or(Failure::Blocked)?,
                        stopped,
                    )?;
                    Some(Publication::NewFile { stage, payload })
                }
                Target::Replace(receiver) => {
                    let stage = receiver.stage().map_err(|_| Failure::Conflict)?;
                    let payload = receive_file(
                        input,
                        &mut stage.writer().map_err(|_| Failure::Failed)?,
                        entry.file.as_ref().ok_or(Failure::Blocked)?,
                        stopped,
                    )?;
                    Some(Publication::Replace {
                        receiver,
                        stage,
                        payload,
                    })
                }
            };
            pending.push(publication);
        }
        Ok(Prepared {
            scope: self.scope,
            pending: pending.into_iter().flatten().collect(),
        })
    }
}

fn bind(proof: &mut Sha256, field: &impl serde::Serialize) -> Result<(), Failure> {
    let bytes = serde_json::to_vec(field).map_err(|_| Failure::Failed)?;
    proof.update((bytes.len() as u64).to_be_bytes());
    proof.update(bytes);
    Ok(())
}

fn is_absent(path: &Path) -> bool {
    matches!(fs::symlink_metadata(path), Err(error) if error.kind() == io::ErrorKind::NotFound)
}

type Payload = (u64, [u8; 32]);
type Tree = BTreeMap<Relative, Option<Payload>>;

fn receive_file(
    input: &mut impl Read,
    output: &mut impl io::Write,
    file: &super::FileProof,
    stopped: &impl Fn() -> bool,
) -> Result<Payload, Failure> {
    let digest = copy_digest(input, output, file.size, || !stopped())?;
    if digest_hex(&digest)? != file.sha256 {
        return Err(Failure::Conflict);
    }
    Ok((file.size, digest))
}

fn verify_file(
    mut file: &File,
    (size, digest): Payload,
    stopped: &impl Fn() -> bool,
) -> io::Result<()> {
    private_regular_file(file)?;
    crate::private_files::require_single_link(file)?;
    file.rewind()?;
    let before = local_tree::reviewed_file_stamp(file)?;
    if file.metadata()?.len() != size
        || copy_digest(&mut file, &mut io::sink(), size, || !stopped()).ok() != Some(digest)
        || before != local_tree::reviewed_file_stamp(file)?
    {
        return Err(changed());
    }
    Ok(())
}

fn verify_tree(
    directory: &Directory,
    tree: &Tree,
    parts: &[OsString],
    stopped: &impl Fn() -> bool,
) -> io::Result<()> {
    private_directory(directory)?;
    let children = tree
        .iter()
        .filter_map(|(path, payload)| {
            let (name, parent) = path.split_last()?;
            (parent == parts).then_some((name, payload))
        })
        .collect::<BTreeMap<_, _>>();
    let names = directory.names(children.len())?;
    if names.len() != children.len() {
        return Err(changed());
    }
    for name in names {
        let expected = children.get(&name).ok_or_else(changed)?;
        match (local_tree::entry(directory, &name)?, expected) {
            (Entry::Directory(child), None) => {
                let mut path = parts.to_vec();
                path.push(name);
                verify_tree(&child, tree, &path, stopped)?;
            }
            (Entry::File(file), Some(payload)) => verify_file(&file, *payload, stopped)?,
            _ => return Err(changed()),
        }
    }
    Ok(())
}

pub(super) struct Prepared {
    scope: Scope,
    pending: Vec<Publication>,
}

enum Publication {
    NewTree {
        stage: Stage,
        tree: Tree,
    },
    NewFile {
        stage: StagedDownload,
        payload: Payload,
    },
    Replace {
        receiver: Box<ReviewedFileReceiver>,
        stage: StagedDownload,
        payload: Payload,
    },
}

impl Prepared {
    pub(super) fn publish(
        self,
        stopped: &impl Fn() -> bool,
        allowed: &impl Fn() -> bool,
    ) -> MergeReport {
        let mut report = MergeReport::default();
        let authorize = || {
            if stopped() || !allowed() {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            self.scope.current()
        };
        let has_replacements = self
            .pending
            .iter()
            .any(|entry| matches!(entry, Publication::Replace { .. }));
        let prefix = if has_replacements {
            let Ok(nonce) = crate::private_files::nonce() else {
                report.error = Some(Failure::Failed);
                return report;
            };
            Some(nonce[..16].to_owned())
        } else {
            None
        };
        for publication in self.pending {
            if let Err(error) =
                publication.apply(stopped, &authorize, prefix.as_deref(), &mut report)
            {
                report.error = Some(error);
                return report;
            }
        }
        if self.scope.current().is_err() {
            report.error = Some(Failure::Uncertain);
        } else {
            match self.scope.receiver() {
                Ok(receiver) => report.receiver = receiver,
                Err(error) => report.error = Some(error),
            }
        }
        report
    }
}

impl Publication {
    fn apply(
        self,
        stopped: &impl Fn() -> bool,
        authorize: &impl Fn() -> io::Result<()>,
        prefix: Option<&str>,
        report: &mut MergeReport,
    ) -> Result<(), Failure> {
        authorize().map_err(|error| {
            if error.kind() == io::ErrorKind::PermissionDenied {
                Failure::Blocked
            } else {
                Failure::Conflict
            }
        })?;
        match self {
            Self::NewTree { stage, tree } => {
                verify_tree(
                    stage.root().map_err(|_| Failure::Conflict)?,
                    &tree,
                    &[],
                    stopped,
                )
                .map_err(|_| Failure::Conflict)?;
                stage
                    .publish(|| authorize().is_ok())
                    .map_err(|error| match error {
                        PublishError::Denied => Failure::Blocked,
                        PublishError::Refused => Failure::Conflict,
                        PublishError::Uncertain => Failure::Uncertain,
                    })?;
                report.created += 1 + tree.len();
                report.bytes += tree.values().flatten().map(|(bytes, _)| bytes).sum::<u64>();
            }
            Self::NewFile { stage, payload } => {
                stage
                    .publish_with_receipt(
                        true,
                        |file| {
                            verify_file(file, payload, stopped)?;
                            authorize()
                        },
                        |parent, _, _| local_tree::sync_reviewed_tree_directory(parent),
                    )
                    .map_err(|error| match error {
                        FilePublishError::Before(_) => Failure::Conflict,
                        FilePublishError::Uncertain(_) => Failure::Uncertain,
                    })?;
                report.created += 1;
                report.bytes += payload.0;
            }
            Self::Replace {
                receiver,
                stage,
                payload,
            } => {
                let prefix = prefix.ok_or(Failure::Failed)?;
                let manifest = Inventory::regular_file(payload.0, payload.1);
                match receiver.publish_with_recovery_prefix(
                    stage,
                    manifest,
                    stopped,
                    authorize,
                    |_| Ok(()),
                    Some(prefix),
                ) {
                    Ok(ReviewedFilePublication::Published { .. }) => {
                        report.replaced += 1;
                        report.bytes += payload.0;
                        report
                            .recovery_prefix
                            .get_or_insert_with(|| prefix.to_owned());
                    }
                    Ok(ReviewedFilePublication::Uncertain { recovery, .. }) => {
                        if recovery.is_some() {
                            report
                                .recovery_prefix
                                .get_or_insert_with(|| prefix.to_owned());
                        }
                        return Err(Failure::Uncertain);
                    }
                    Ok(ReviewedFilePublication::Restored { .. }) | Err(_) => {
                        return Err(Failure::Conflict);
                    }
                }
            }
        }
        Ok(())
    }
}

fn changed() -> io::Error {
    io::Error::other("The reviewed folder or file changed")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::io::Write as _;

    use super::super::{Entry as SourceEntry, FileProof, local_parts};
    use super::*;
    use crate::snippets::tests::Profile;

    fn fixture() -> (Profile, PathBuf, Manifest, Vec<u8>) {
        let profile = Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let parent = Directory::open_path(&root).unwrap();
        let directory =
            local_tree::create_reviewed_tree_stage(&parent, OsStr::new("receiver")).unwrap();
        let existing =
            local_tree::create_reviewed_tree_stage(&directory, OsStr::new("existing")).unwrap();
        for (parent, name, bytes) in [
            (&existing, "a-change", b"original bytes".as_slice()),
            (&directory, "b-same", b"same bytes"),
            (&directory, "receiver-only", b"local note"),
        ] {
            let mut file = local_tree::create_reviewed_tree_file(parent, OsStr::new(name)).unwrap();
            file.write_all(bytes).unwrap();
        }
        let mut wire = Vec::new();
        let entries = [
            ("existing", None),
            ("existing/a-change", Some(b"replacement bytes".as_slice())),
            ("b-same", Some(b"same bytes".as_slice())),
            ("new-file", Some(b"new root file".as_slice())),
            ("new-tree", None),
            ("new-tree/nested", None),
            ("new-tree/nested/data", Some(b"new subtree data".as_slice())),
        ]
        .into_iter()
        .map(|(path, contents)| SourceEntry {
            path: path.to_owned(),
            parts: local_parts(path, contents.is_none()).unwrap(),
            file: contents.map(|bytes| {
                wire.extend_from_slice(bytes);
                FileProof {
                    size: bytes.len() as u64,
                    sha256: digest_hex(&Sha256::digest(bytes)).unwrap(),
                }
            }),
        })
        .collect();
        let manifest = Manifest {
            witness: "ab".repeat(32),
            entries,
            size: wire.len() as u64,
        };
        (profile, root.join("receiver"), manifest, wire)
    }

    fn inspect(manifest: &Manifest, root: &Path) -> Plan {
        Plan::inspect(
            manifest,
            root,
            ["/protected/source".into(), root.display().to_string()],
            &|| false,
        )
        .unwrap()
    }

    #[test]
    fn protected_folder_download_merge_keeps_receiver_identity_and_recovers_originals() {
        let (_profile, root, manifest, wire) = fixture();
        let directory = Directory::open_path(&root).unwrap();
        let original_identity = local_tree::reviewed_tree_parent_stamp(&directory).unwrap();
        let existing = root.join("existing");
        let nested_identity =
            local_tree::reviewed_tree_parent_stamp(&Directory::open_path(&existing).unwrap())
                .unwrap();
        let changed = existing.join("a-change");
        let same_file =
            ReviewedFileReceiver::inspect_transfer(&root.join("b-same"), &|| false).unwrap();
        let plan = inspect(&manifest, &root);
        let receiver = plan.receiver().unwrap();
        assert!(receiver.is_some());
        assert_eq!(plan.preview.paths.len(), 4);
        assert_eq!(plan.preview.replacements.len(), 1);
        assert_eq!(plan.preview.replacements[0].target, "existing/a-change");
        let prepared = plan
            .receive(&manifest, &mut wire.as_slice(), &|| false)
            .unwrap();
        assert_eq!(fs::read(&changed).unwrap(), b"original bytes");
        assert!(!root.join("new-tree").exists());
        let result = prepared.publish(&|| false, &|| true);
        assert_eq!(result.error, None);
        assert_eq!(result.receiver, receiver);
        assert_eq!(inspect(&manifest, &root).receiver().unwrap(), receiver);
        assert_eq!((result.created, result.replaced), (4, 1));
        assert_eq!(fs::read(changed).unwrap(), b"replacement bytes");
        assert_eq!(
            fs::read(root.join("new-tree").join("nested").join("data")).unwrap(),
            b"new subtree data"
        );
        assert_eq!(fs::read(root.join("receiver-only")).unwrap(), b"local note");
        assert_eq!(
            ReviewedFileReceiver::inspect_transfer(&root.join("b-same"), &|| false).unwrap(),
            same_file
        );
        assert_eq!(
            local_tree::reviewed_tree_parent_stamp(&Directory::open_path(&root).unwrap()).unwrap(),
            original_identity
        );
        assert_eq!(
            local_tree::reviewed_tree_parent_stamp(&Directory::open_path(&existing).unwrap())
                .unwrap(),
            nested_identity
        );
        let prefix = format!(".kortty-{}", result.recovery_prefix.unwrap());
        let recovery = fs::read_dir(existing)
            .unwrap()
            .map(|entry| entry.unwrap())
            .find(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
            .unwrap();
        assert_eq!(
            fs::read(recovery.path().join("a-change")).unwrap(),
            b"original bytes"
        );
    }

    #[test]
    fn protected_folder_receiver_receipt_rejects_a_replaced_root() {
        let (_profile, root, manifest, _) = fixture();
        let before = inspect(&manifest, &root).receiver().unwrap();
        assert!(before.is_some());
        fs::rename(&root, root.with_file_name("prior-receiver")).unwrap();
        let parent = Directory::open_path(root.parent().unwrap()).unwrap();
        let _replacement =
            local_tree::create_reviewed_tree_stage(&parent, OsStr::new("receiver")).unwrap();
        assert_ne!(inspect(&manifest, &root).receiver().unwrap(), before);
    }

    #[test]
    fn protected_folder_download_merge_refuses_a_changed_receiver_before_later_publications() {
        let (_profile, root, manifest, wire) = fixture();
        let changed = root.join("existing").join("a-change");
        let plan = inspect(&manifest, &root);
        let reviewed = plan.preview.proof;
        let receiver = plan.receiver().unwrap();
        let prepared = plan
            .receive(&manifest, &mut wire.as_slice(), &|| false)
            .unwrap();
        fs::write(&changed, b"external change").unwrap();
        assert_ne!(inspect(&manifest, &root).preview.proof, reviewed);
        assert_eq!(inspect(&manifest, &root).receiver().unwrap(), receiver);
        let result = prepared.publish(&|| false, &|| true);
        assert_eq!(result.error, Some(Failure::Conflict));
        assert_eq!(result.receiver, None);
        assert_eq!((result.created, result.replaced), (0, 0));
        assert_eq!(fs::read(changed).unwrap(), b"external change");
        assert!(!root.join("new-file").exists());
        assert!(!root.join("new-tree").exists());
    }
}
