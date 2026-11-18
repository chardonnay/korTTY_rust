//! Stage a protected folder upload before publishing its reviewed entries.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::time::Duration;

use kortty_server_archive::privileged_tree_merge::{
    Change, Decision, MAX_BYTES, Payload, READY, Reply, Request, Review, read, write,
};
use kortty_server_archive::rsync_tree::{DeleteScope, Manifest};
use rustix::fs::{AtFlags, FileType, Mode, OFlags, fchmod, mkdirat, openat, statat};
use rustix::io::Errno;
use rustix::process::umask;
use sha2::{Digest, Sha256};

use super::Rules;
use crate::server::draft_replace::mount_id;
use crate::server::rsync_tree::{
    CheckedFile, CheckedTree, ExistingFile, ExistingRecovery, RECEIVED_FILE, UploadStage,
    VacantFile, VacantTree, directory_fingerprint,
};
use crate::server::{
    Directory, File, PRIVATE_DIRECTORY, PRIVATE_FILE, absolute_file, invalid, same_node,
};

mod creation;

pub(crate) fn serve() -> io::Result<()> {
    let mut output = io::stdout().lock();
    let mut recovery = ExistingRecovery::default();
    let result = (|| {
        umask(Mode::from_bits_truncate(0o077));
        let rules = Rules::load_inline_upload()?;
        std::thread::Builder::new()
            .name("kortty-protected-folder-deadline".into())
            .spawn(|| {
                std::thread::sleep(Duration::from_secs(3600));
                std::process::exit(124);
            })?;
        output.write_all(READY)?;
        output.flush()?;
        exchange(&rules, &mut io::stdin().lock(), &mut output, &mut recovery)
    })();
    match result {
        Ok(Some(reply)) => write(output, &reply),
        Ok(None) => Ok(()),
        Err(_) => write(
            output,
            &Reply::Failed {
                outcome_unknown: recovery.uncertain,
                recovery: recovery.path,
            },
        ),
    }
}

fn exchange(
    rules: &Rules,
    input: &mut impl Read,
    output: &mut impl Write,
    recovery: &mut ExistingRecovery,
) -> io::Result<Option<Reply>> {
    let request: Request = read(&mut *input)?;
    if !request.valid() {
        return Err(invalid("Invalid protected folder upload"));
    }
    if request.create_root {
        let (path, _) = absolute_file(&request.root)?;
        let parent = rules.destination_parent(path, false)?;
        let mount = mount_id(&parent.fd)?;
        let binding = BoundDirectory::new(path, parent, mount)?;
        return creation::exchange(
            &request,
            Directory::from_fd(binding.directory.fd.try_clone()?.into())?,
            rules.revision(),
            input,
            output,
            recovery,
            || {
                rules.require_current()?;
                binding.require(path, &rules.destination_parent(path, false)?)
            },
        );
    }
    let paths = request
        .entries
        .iter()
        .map(|entry| request.destination(entry))
        .collect::<Vec<_>>();
    let plan = Plan::inspect(&request, &paths, rules)?;
    let groups = plan.review.publications(&request).ok_or_else(changed)?;
    write(
        &mut *output,
        &Reply::Reviewed {
            review: plan.review.clone(),
        },
    )?;
    if request.review_only {
        return Ok(None);
    }
    match read(&mut *input) {
        Ok(Decision::Receive) => {}
        Ok(Decision::Abort) => return Ok(Some(Reply::Aborted)),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        _ => return Err(changed()),
    }
    plan.require_current(rules)?;
    let mut prefix = [0; 8];
    getrandom::fill(&mut prefix).map_err(io::Error::other)?;
    write(&mut *output, &Reply::Receive)?;
    let prepared = groups
        .iter()
        .map(|group| plan.slots[group[0]].prepare(&request, group, prefix, input))
        .collect::<io::Result<Vec<_>>>()?;
    plan.require_current(rules)?;
    write(
        &mut *output,
        &Reply::Verified {
            recovery_prefix: prefix,
        },
    )?;

    let permit = || plan.require_scope(rules);
    for (group, candidate) in groups.iter().zip(prepared) {
        let index = group[0];
        match read(&mut *input)? {
            Decision::Abort => return Ok(Some(Reply::Aborted)),
            Decision::Publish => {}
            Decision::Receive => return Err(changed()),
        }
        if !plan.slots[index].publish(candidate, recovery, &permit, |path| {
            write(
                &mut *output,
                &Reply::Quarantined {
                    index,
                    recovery: path.to_owned(),
                },
            )?;
            match read(&mut *input)? {
                Decision::Publish => Ok(true),
                Decision::Abort => Ok(false),
                Decision::Receive => Err(changed()),
            }
        })? {
            return Ok(Some(Reply::Aborted));
        }
        write(&mut *output, &Reply::Applied { index })?;
        *recovery = ExistingRecovery::default();
    }
    plan.require_scope(rules)?;
    Ok(Some(Reply::Complete {
        receiver: plan.review.receiver,
    }))
}

struct Plan<'a> {
    directories: BTreeMap<&'a str, BoundDirectory>,
    slots: Vec<Slot<'a>>,
    review: Review,
}

struct BoundDirectory {
    directory: Directory,
    fingerprint: [u8; 32],
}

impl BoundDirectory {
    fn new(path: &str, directory: Directory, mount: u64) -> io::Result<Self> {
        if mount_id(&directory.fd)? != mount {
            return Err(changed());
        }
        Ok(Self {
            fingerprint: directory_fingerprint(path, &directory)?,
            directory,
        })
    }

    fn require(&self, path: &str, current: &Directory) -> io::Result<()> {
        if !same_node(&self.directory.stat, &current.stat)
            || directory_fingerprint(path, current)? != self.fingerprint
        {
            return Err(changed());
        }
        Ok(())
    }
}

enum Slot<'a> {
    KeepDirectory,
    KeepFile(Box<ExistingFile<'a>>),
    WithinNewTree,
    NewFile(VacantFile<'a>),
    NewTree(VacantTree<'a>),
    Replace(Box<ExistingFile<'a>>),
}

impl<'a> Plan<'a> {
    fn inspect(request: &'a Request, paths: &'a [String], rules: &Rules) -> io::Result<Self> {
        rules.require_current()?;
        let root = request.root.as_str();
        let root_directory = rules.destination_parent(root, false)?;
        let mount = mount_id(&root_directory.fd)?;
        let mut seen = BTreeSet::from([(root_directory.stat.st_dev, root_directory.stat.st_ino)]);
        let mut directories =
            BTreeMap::from([(root, BoundDirectory::new(root, root_directory, mount)?)]);
        let mut slots = Vec::with_capacity(paths.len());
        let mut changes = Vec::with_capacity(paths.len());
        let mut remaining = MAX_BYTES;
        let mut fingerprint = Sha256::new_with_prefix(b"kortty-protected-folder-merge-v1\0");
        fingerprint.update(rules.revision());
        let source = serde_json::to_vec(&(root, &request.entries)).map_err(io::Error::other)?;
        fingerprint.update((source.len() as u32).to_be_bytes());
        fingerprint.update(source);

        for (entry, path) in request.entries.iter().zip(paths) {
            let (parent, name) = absolute_file(path)?;
            let Some(binding) = directories.get(parent) else {
                slots.push(Slot::WithinNewTree);
                changes.push(Change::Create);
                fingerprint.update([0]);
                continue;
            };
            let directory = &binding.directory;
            let (slot, change) = match statat(&directory.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
                Err(Errno::NOENT) => {
                    let parent = Directory::from_fd(directory.fd.try_clone()?.into())?;
                    let slot = if entry.file.is_some() {
                        let target = VacantFile::open_at(path, parent)?;
                        fingerprint.update([1]);
                        fingerprint.update(target.fingerprint());
                        Slot::NewFile(target)
                    } else {
                        let target = VacantTree::open_at(path, parent, &DeleteScope::Retain)?;
                        fingerprint.update([2]);
                        fingerprint.update(target.fingerprint()?);
                        Slot::NewTree(target)
                    };
                    (slot, Change::Create)
                }
                Ok(named) if seen.insert((named.st_dev, named.st_ino)) => {
                    match (FileType::from_raw_mode(named.st_mode), entry.file) {
                        (FileType::Directory, None) => {
                            let current = rules.destination_parent(path, false)?;
                            if !same_node(&named, &current.stat) {
                                return Err(changed());
                            }
                            directories
                                .insert(path.as_str(), BoundDirectory::new(path, current, mount)?);
                            fingerprint.update([3]);
                            (Slot::KeepDirectory, Change::Keep)
                        }
                        (FileType::RegularFile, Some(incoming)) => {
                            let destination = rules.destination_parent(parent, true)?;
                            binding.require(parent, &destination)?;
                            let target =
                                ExistingFile::open_transfer_limited(path, destination, remaining)?;
                            let original = target.content();
                            remaining =
                                remaining.checked_sub(original.bytes).ok_or_else(changed)?;
                            fingerprint.update([4]);
                            fingerprint.update(target.review().fingerprint);
                            if incoming == original {
                                (Slot::KeepFile(Box::new(target)), Change::Keep)
                            } else {
                                (
                                    Slot::Replace(Box::new(target)),
                                    Change::Replace { original },
                                )
                            }
                        }
                        _ => return Err(changed()),
                    }
                }
                _ => return Err(changed()),
            };
            slots.push(slot);
            changes.push(change);
        }
        for binding in directories.values() {
            fingerprint.update(binding.fingerprint);
        }
        let receiver = request
            .receiver_receipt
            .then(|| {
                receiver_anchor(
                    rules.revision(),
                    root,
                    &directories.get(root).ok_or_else(changed)?.directory,
                )
            })
            .transpose()?;
        let plan = Self {
            directories,
            slots,
            review: Review {
                fingerprint: fingerprint.finalize().into(),
                receiver,
                changes,
            },
        };
        plan.require_current(rules)?;
        Ok(plan)
    }

    fn require_scope(&self, rules: &Rules) -> io::Result<()> {
        rules.require_current()?;
        for (path, directory) in &self.directories {
            directory.require(path, &rules.destination_parent(path, false)?)?;
        }
        Ok(())
    }

    fn require_current(&self, rules: &Rules) -> io::Result<()> {
        self.require_scope(rules)?;
        for slot in &self.slots {
            match slot {
                Slot::KeepFile(target) | Slot::Replace(target) => target.require_current()?,
                Slot::NewFile(target) => target.require_vacant()?,
                Slot::NewTree(target) => target.require_vacant()?,
                Slot::KeepDirectory | Slot::WithinNewTree => {}
            }
        }
        Ok(())
    }
}

enum Prepared<'a> {
    File(UploadStage<'a>, CheckedFile),
    Tree(UploadStage<'a>, CheckedTree),
}

impl Slot<'_> {
    fn prepare(
        &self,
        request: &Request,
        group: &[usize],
        prefix: [u8; 8],
        input: &mut impl Read,
    ) -> io::Result<Prepared<'_>> {
        let entry = &request.entries[group[0]];
        match self {
            Self::NewFile(target) => {
                let stage = UploadStage::create_checked(target.parent())?;
                let payload = entry.file.ok_or_else(changed)?;
                receive_file(&stage.payload, RECEIVED_FILE, payload, input)?;
                let file = target.prepare_transfer(
                    &stage,
                    Manifest::regular_file(payload.bytes, payload.sha256),
                )?;
                Ok(Prepared::File(stage, file))
            }
            Self::Replace(target) => {
                let (_, name) = absolute_file(target.destination())?;
                let stage = UploadStage::create_for_transfer(target.parent(), prefix, name)?;
                let payload = entry.file.ok_or_else(changed)?;
                receive_file(&stage.payload, RECEIVED_FILE, payload, input)?;
                let file = target.prepare_successor(
                    &stage,
                    Manifest::regular_file(payload.bytes, payload.sha256),
                )?;
                Ok(Prepared::File(stage, file))
            }
            Self::NewTree(target) => {
                let stage = UploadStage::create_checked(target.parent())?;
                let expected = receive_tree(&stage.payload, request, group, input, false)?;
                let tree = target.prepare_transfer(&stage, expected)?;
                Ok(Prepared::Tree(stage, tree))
            }
            _ => Err(changed()),
        }
    }

    fn publish(
        &self,
        prepared: Prepared<'_>,
        recovery: &mut ExistingRecovery,
        permit: &impl Fn() -> io::Result<()>,
        confirm: impl FnOnce(&str) -> io::Result<bool>,
    ) -> io::Result<bool> {
        match (self, prepared) {
            (Self::NewFile(target), Prepared::File(stage, file)) => {
                target.publish(&stage, &file, &mut recovery.uncertain, permit)?;
            }
            (Self::NewTree(target), Prepared::Tree(mut stage, tree)) => {
                target.publish(&mut stage, &tree, &mut recovery.uncertain, permit)?;
            }
            (Self::Replace(target), Prepared::File(mut stage, file)) => {
                return target
                    .commit_checked(&mut stage, &file, false, recovery, permit, confirm)
                    .map(|published| published.is_some());
            }
            _ => return Err(changed()),
        }
        Ok(true)
    }
}

fn receive_file(
    parent: &Directory,
    name: &str,
    payload: Payload,
    input: &mut impl Read,
) -> io::Result<()> {
    let mut file = File::from(openat(
        &parent.fd,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        PRIVATE_FILE,
    )?);
    fchmod(&file, PRIVATE_FILE)?;
    if io::copy(&mut input.take(payload.bytes), &mut file)? != payload.bytes {
        return Err(invalid("The upload ended before its declared size"));
    }
    Ok(())
}

fn receive_tree(
    root: &Directory,
    request: &Request,
    group: &[usize],
    input: &mut impl Read,
    include_root: bool,
) -> io::Result<kortty_server_archive::rsync_tree::Inventory> {
    let path = (!include_root).then(|| request.entries[group[0]].path.as_str());
    let mut directories = BTreeMap::new();
    let mut inventory = BTreeMap::new();
    for index in group.iter().skip(usize::from(!include_root)) {
        let entry = &request.entries[*index];
        let relative = if let Some(path) = path {
            entry
                .path
                .strip_prefix(path)
                .and_then(|name| name.strip_prefix('/'))
                .ok_or_else(changed)?
        } else {
            entry.path.as_str()
        };
        let (parent, name) = relative.rsplit_once('/').unwrap_or(("", relative));
        let directory = if parent.is_empty() {
            root
        } else {
            directories.get(parent).ok_or_else(changed)?
        };
        if let Some(payload) = entry.file {
            receive_file(directory, name, payload, input)?;
        } else {
            mkdirat(&directory.fd, name, PRIVATE_DIRECTORY)?;
            directories.insert(relative, directory.child(name)?);
        }
        inventory.insert(relative.split('/').collect::<Vec<_>>(), entry.file);
    }
    let mut manifest =
        Manifest::bounded_transfer((group.len() - usize::from(!include_root)) as u32, MAX_BYTES);
    for (parts, file) in inventory {
        manifest.add(&parts.join("/"), file.map(|file| (file.bytes, file.sha256)))?;
    }
    Ok(manifest.finish())
}

fn receiver_anchor(policy: [u8; 32], path: &str, directory: &Directory) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new_with_prefix(b"kortty-protected-tree-receiver-v1\0");
    hash.update(policy);
    hash.update(directory_fingerprint(path, directory)?);
    Ok(hash.finalize().into())
}

fn changed() -> io::Error {
    invalid("A reviewed protected folder entry changed or is outside its scope")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, DirBuilder, Permissions};
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

    #[test]
    fn protected_tree_receiver_anchor_binds_policy_identity_and_metadata_not_payload() {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("kortty-merge-receiver-{}", hex::encode(nonce)));
        DirBuilder::new().mode(0o700).create(&root).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let path = root.join("receiver");
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        let anchor = |policy| {
            receiver_anchor(
                policy,
                path.to_str().unwrap(),
                &Directory::open_parent(path.to_str().unwrap()).unwrap(),
            )
            .unwrap()
        };
        let initial = anchor([1; 32]);
        fs::write(path.join("file"), b"original").unwrap();
        assert_eq!(anchor([1; 32]), initial);
        fs::write(path.join("file"), b"changed source publication").unwrap();
        assert_eq!(anchor([1; 32]), initial);
        assert_ne!(anchor([2; 32]), initial);
        fs::set_permissions(&path, Permissions::from_mode(0o750)).unwrap();
        assert_ne!(anchor([1; 32]), initial);
        fs::set_permissions(&path, Permissions::from_mode(0o700)).unwrap();
        assert_eq!(anchor([1; 32]), initial);
        fs::rename(&path, root.join("prior")).unwrap();
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        assert_ne!(anchor([1; 32]), initial);
    }
}
