//! First publication of a reviewed local rsync directory and its successor receipt.

use std::path::Component;

use super::*;
use crate::job_executor::rsync::local_existing_snapshot::MAX_MISSING_PARENTS;

mod file;
pub(in crate::job_executor::rsync::native) use file::{
    MissingLocalFilePublication, MissingLocalFileReceiver, MissingLocalFileStage,
};

pub(in crate::job_executor::rsync::native) type VacantLocalTreeReceiver = CheckedVacantPath;

pub(in crate::job_executor::rsync::native) struct CheckedVacantPath {
    path: PathBuf,
    nesting: Vec<OsString>,
    parent: Directory,
    parent_stamp: ReviewedStamp,
    snapshot: VacantLocalTreeSnapshot,
    original: VacantLocalTreeSnapshot,
    borrowed_parents: Vec<LocalParentReceipt>,
}

impl CheckedVacantPath {
    pub(in crate::job_executor::rsync::native) fn inspect(
        path: &Path,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<VacantLocalTreeSnapshot> {
        Self::open(path, stopped).map(|receiver| receiver.snapshot)
    }

    #[cfg(test)]
    pub(in crate::job_executor::rsync::native) fn prepare(
        path: &Path,
        expected: VacantLocalTreeSnapshot,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<Self> {
        Self::prepare_with_parents(path, expected, &[], stopped)
    }

    pub(in crate::job_executor::rsync::native) fn prepare_with_parents(
        path: &Path,
        expected: VacantLocalTreeSnapshot,
        parents: &[LocalParentReceipt],
        stopped: &impl Fn() -> bool,
    ) -> io::Result<Self> {
        let mut receiver = Self::open(path, stopped)?;
        let missing = receiver.snapshot.missing_parents;
        let covered = expected
            .missing_parents
            .checked_sub(missing)
            .ok_or_else(changed)?;
        let paths = expected.missing_parent_paths(path).ok_or_else(changed)?;
        for (index, parent_path) in paths.enumerate() {
            require_running(stopped)?;
            let key = LocalParentReceipt::key_for(parent_path);
            let mut matching = parents.iter().filter(|receipt| receipt.path_key() == key);
            match (
                index >= usize::from(missing),
                matching.next(),
                matching.next(),
            ) {
                (true, Some(receipt), None) if receipt.valid() => {
                    receiver.borrowed_parents.push(*receipt);
                }
                (false, None, None) => {}
                _ => return Err(changed()),
            }
        }
        receiver.borrowed_parents.reverse();
        if covered == 0 {
            let mut comparable = receiver.snapshot;
            if expected.prefix.is_none() {
                comparable.prefix = None;
            }
            if comparable != expected {
                return Err(changed());
            }
        } else if expected.prefix.is_none() {
            return Err(changed());
        }
        receiver.original = expected;
        receiver.require_vacant()?;
        require_running(stopped)?;
        Ok(receiver)
    }

    fn open(path: &Path, stopped: &impl Fn() -> bool) -> io::Result<Self> {
        require_running(stopped)?;
        if !path.is_absolute() || reportable_path(path).is_none() {
            return Err(changed());
        }
        let mut first_missing = path;
        let mut nesting = Vec::new();
        let parent = loop {
            require_running(stopped)?;
            let parent_path = first_missing.parent().ok_or_else(changed)?;
            let name = first_missing.file_name().ok_or_else(changed)?;
            witness::checked_name(name)?;
            match local_tree::open_reviewed_tree_parent(parent_path) {
                Ok(parent) => break parent,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    if nesting.len() >= usize::from(MAX_MISSING_PARENTS) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "The local rsync destination has too many missing parent folders.",
                        ));
                    }
                    nesting.push(name.to_owned());
                    first_missing = parent_path;
                }
                Err(error) => return Err(error),
            }
        };
        nesting.reverse();
        let parent_stamp = local_tree::reviewed_tree_parent_stamp(&parent)?;
        let snapshot = capture_vacancy(
            path,
            first_missing,
            &parent,
            &parent_stamp,
            nesting.len() as u8,
        )?;
        if snapshot.first_missing(path) != Some(first_missing) {
            return Err(changed());
        }
        let receiver = Self {
            path: path.to_owned(),
            nesting,
            parent,
            parent_stamp,
            snapshot,
            original: snapshot,
            borrowed_parents: Vec::new(),
        };
        receiver.require_vacant()?;
        require_running(stopped)?;
        if snapshot.valid() {
            Ok(receiver)
        } else {
            Err(changed())
        }
    }

    pub(in crate::job_executor::rsync::native) fn missing_parent_count(&self) -> u32 {
        u32::from(self.snapshot.missing_parent_count())
    }

    pub(in crate::job_executor::rsync::native) fn publish(
        self,
        incoming: Incoming<'_>,
        stopped: &impl Fn() -> bool,
        require_authority: &impl Fn() -> Result<(), Failure>,
        id: RunId,
        arm: impl FnOnce(u32) -> Result<(), Failure>,
    ) -> Result<Published, Failure> {
        require_running(stopped).map_err(|_| super::super::transfer_stopped())?;
        require_authority()?;
        let _ancestors = local_tree::hold_reviewed_file_ancestors(self.anchor_path())
            .map_err(|_| Failure::blocked(REVIEW))?;
        self.require_vacant()
            .map_err(|_| Failure::blocked(REVIEW))?;
        let mut stage = Stage::create(&self.parent).map_err(|_| Failure::failed(PREPARE))?;
        let stage_path = stage.path(self.anchor_path());
        let stage_location = reportable_path(&stage_path).ok_or_else(|| {
            Failure::blocked("Choose a local rsync folder with a reportable staging path.")
        })?;
        let admission_outcome = Cell::new(None);
        let phase = || publication_phase(stopped, require_authority, &admission_outcome);
        let arm_failure = Cell::new(None);
        let arm = || record_arm(arm(0), &arm_failure);
        let expected = incoming.inventory;
        let result = stage
            .compose_at(
                &self.nesting,
                None,
                incoming,
                self.parent_stamp.volume(),
                stopped,
            )
            .and_then(|candidate| {
                self.commit(&mut stage, &candidate, expected, stopped, &phase, arm)
            });
        let cleanup_failed = stage.cleanup().is_err();
        let (mut completion, successor, parents) = match result {
            Ok(Some((successor, parents))) => {
                let mut completion = Completion::from_result(id, Ok(0));
                completion.detail = Some("Published the verified local rsync tree.".into());
                (completion, Some(successor), parents)
            }
            Ok(None) => {
                let mut completion = Completion::from_result(
                    id,
                    Err(Failure::failed(
                        "The first local rsync publication could not be confirmed.",
                    )),
                );
                completion.append_detail(&format!(
                    "Check the reviewed destination and staging folder {stage_location} before reviewing this route again."
                ));
                (completion, None, Vec::new())
            }
            Err(error) => {
                let failure = arm_failure
                    .into_inner()
                    .unwrap_or_else(|| classify(&error, admission_outcome.into_inner()));
                if !cleanup_failed {
                    return Err(failure);
                }
                (Completion::from_result(id, Err(failure)), None, Vec::new())
            }
        };
        if cleanup_failed {
            completion.append_detail(&format!(
                "Private staging folder {stage_location} may remain."
            ));
        }
        Ok(Published {
            completion,
            successor,
            parents,
        })
    }

    fn commit(
        &self,
        stage: &mut Stage,
        candidate: &StagedTree,
        expected: Inventory,
        stopped: &impl Fn() -> bool,
        phase: &impl Fn() -> io::Result<()>,
        arm: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Option<(LocalExistingSnapshot, Vec<LocalParentReceipt>)>> {
        local_tree::sync_reviewed_tree_directory(stage.outer())?;
        local_tree::sync_reviewed_tree_directory(&self.parent)?;
        let arm = Cell::new(Some(arm));
        let root_stamp = candidate.ancestors.first().unwrap_or(&candidate.tree.stamp);
        stage.retain = true;
        let authorize = || {
            phase()?;
            self.require_vacant()?;
            stage.require_successor(root_stamp)?;
            // Windows can retry the handle-relative rename after a sharing violation.
            if let Some(arm) = arm.take() {
                arm()?;
            }
            candidate.recapture(stage.payload(), &self.nesting, false, stopped)?;
            phase()?;
            self.require_vacant()?;
            stage.require_successor(root_stamp)?;
            Ok(())
        };
        let moved = local_tree::move_reviewed_tree(
            stage.outer(),
            OsStr::new(PAYLOAD),
            stage.payload(),
            &self.parent,
            self.prefix_name(),
            &authorize,
        );
        if let Err(error) = moved {
            if stage.require_wrapper().is_ok()
                && require_named(stage.outer(), OsStr::new(PAYLOAD), stage.payload()).is_ok()
            {
                stage.retain = false;
                return Err(if error.kind() == io::ErrorKind::AlreadyExists {
                    changed()
                } else {
                    error
                });
            }
            return Ok(None);
        }
        let confirmed = (|| {
            self.require_anchor()?;
            let published = require_named(&self.parent, self.prefix_name(), stage.payload())?;
            #[cfg(windows)]
            published.require_reviewed_tree_basename(self.prefix_name())?;
            drop(published);
            let (successor, inventory, held_children) =
                candidate.recapture(stage.payload(), &self.nesting, true, &|| false)?;
            if inventory != expected {
                return Err(changed());
            }
            local_tree::sync_reviewed_tree_directory(stage.outer())?;
            local_tree::sync_reviewed_tree_directory(&self.parent)?;
            self.require_anchor()?;
            // Rebind the receipt through the held tree. On Windows the payload
            // handle already controls deletion, so a second removal handle or
            // an absolute walk through that directory can fail sharing checks.
            let mut current = require_named(&self.parent, self.prefix_name(), stage.payload())?;
            let mut created_parent = None;
            let mut created_path = self.first_missing().to_path_buf();
            let mut parents = Vec::with_capacity(self.nesting.len());
            for (index, ((name, held), expected)) in self
                .nesting
                .iter()
                .zip(&held_children)
                .zip(&candidate.ancestors)
                .enumerate()
            {
                let observed = local_tree::reviewed_directory_stamp(&current)?;
                if !same_stamp(expected, &observed, index == 0) {
                    return Err(changed());
                }
                require_only_child(&current, name)?;
                let child = require_named(&current, name, held)?;
                let stamp = observed.for_tree_parent();
                if let Some(receipt) = parent_receipt(&created_path, &current, &stamp) {
                    parents.push(receipt);
                }
                created_path.push(name);
                created_parent = Some((std::mem::replace(&mut current, child), stamp));
            }
            let leaf_name = self.path.file_name().expect("checked rsync receiver name");
            let (parent, stamp) = created_parent
                .as_ref()
                .map_or((&self.parent, &self.parent_stamp), |(parent, stamp)| {
                    (parent, stamp)
                });
            let mut snapshot =
                snapshot(&self.path, parent, stamp, leaf_name, &successor, inventory);
            if self.original.missing_parents != 0 {
                snapshot.published_from = Some(self.original.fingerprint);
            }
            if local_tree::reviewed_directory_stamp(&current)? != successor.stamp
                || !self.original.allows_successor(&snapshot)
            {
                return Err(changed());
            }
            Ok((snapshot, parents))
        })();
        if let Ok(successor) = confirmed {
            // The checked payload is now a public receiver. Only the empty wrapper is removable.
            drop(stage.payload.take());
            drop(stage.created.take());
            stage.retain = false;
            Ok(Some(successor))
        } else {
            Ok(None)
        }
    }

    fn first_missing(&self) -> &Path {
        self.snapshot
            .first_missing(&self.path)
            .expect("checked absent prefix")
    }

    fn prefix_name(&self) -> &OsStr {
        self.first_missing()
            .file_name()
            .expect("checked absent name")
    }

    fn anchor_path(&self) -> &Path {
        self.first_missing().parent().expect("checked anchor")
    }

    fn require_anchor(&self) -> io::Result<()> {
        if self.borrowed_parents.is_empty() {
            return require_anchor(self.anchor_path(), &self.parent, &self.parent_stamp);
        }
        let original_prefix = self
            .original
            .first_missing(&self.path)
            .ok_or_else(changed)?;
        let original_anchor = original_prefix.parent().ok_or_else(changed)?;
        let mut parent = local_tree::open_reviewed_tree_parent(original_anchor)?;
        let mut stamp = local_tree::reviewed_tree_parent_stamp(&parent)?;
        if capture_vacancy(
            &self.path,
            original_prefix,
            &parent,
            &stamp,
            self.original.missing_parents,
        )? != self.original
        {
            return Err(changed());
        }
        let volume = stamp.volume();
        let components = self
            .anchor_path()
            .strip_prefix(original_anchor)
            .map_err(|_| changed())?
            .components();
        if components.clone().count() != self.borrowed_parents.len() {
            return Err(changed());
        }
        let mut path = original_anchor.to_path_buf();
        for (component, expected) in components.zip(&self.borrowed_parents) {
            let Component::Normal(name) = component else {
                return Err(changed());
            };
            let Entry::Directory(child) = local_tree::entry(&parent, name)? else {
                return Err(changed());
            };
            #[cfg(windows)]
            child.require_reviewed_tree_basename(name)?;
            stamp = local_tree::reviewed_tree_parent_stamp(&child)?;
            path.push(name);
            if stamp.volume() != volume || parent_receipt(&path, &child, &stamp) != Some(*expected)
            {
                return Err(changed());
            }
            parent = child;
        }
        if parent.same_as(&self.parent)? && stamp == self.parent_stamp {
            Ok(())
        } else {
            Err(changed())
        }
    }

    fn require_vacant(&self) -> io::Result<()> {
        self.require_anchor()?;
        #[cfg(windows)]
        return self
            .parent
            .require_absent_after_removal(self.prefix_name())
            .map_err(|_| changed());
        #[cfg(unix)]
        match rustix::fs::statat(
            self.parent.file(),
            self.prefix_name(),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Err(rustix::io::Errno::NOENT) => Ok(()),
            Ok(_) => Err(changed()),
            Err(error) => Err(error.into()),
        }
    }
}

fn capture_vacancy(
    path: &Path,
    first_missing: &Path,
    parent: &Directory,
    stamp: &ReviewedStamp,
    missing_parents: u8,
) -> io::Result<VacantLocalTreeSnapshot> {
    let name = first_missing.file_name().ok_or_else(changed)?;
    let mut fingerprint = path_fingerprint(b"kortty-vacant-local-rsync-tree-v1\0", path, stamp);
    let prefix = if missing_parents == 0 {
        None
    } else {
        fingerprint.write_u8(missing_parents);
        parent_fingerprint(
            b"kortty-local-rsync-vacant-prefix-v1\0",
            first_missing,
            parent,
            stamp,
        )
        .map(|proof| proof.fingerprint)
    };
    Ok(VacantLocalTreeSnapshot {
        fingerprint: fingerprint.digest(),
        location: location(parent, stamp, name),
        missing_parents,
        prefix,
    })
}

fn parent_receipt(
    path: &Path,
    parent: &Directory,
    stamp: &ReviewedStamp,
) -> Option<LocalParentReceipt> {
    parent_fingerprint(
        b"kortty-local-rsync-published-parent-v1\0",
        path,
        parent,
        stamp,
    )
}

fn parent_fingerprint(
    domain: &[u8],
    path: &Path,
    parent: &Directory,
    stamp: &ReviewedStamp,
) -> Option<LocalParentReceipt> {
    let (seconds, nanos) = local_tree::reviewed_tree_parent_birth(parent, stamp)?;
    let path_key = LocalParentReceipt::key_for(path);
    let mut fingerprint = Fingerprint::new(domain);
    fingerprint.write(&path_key);
    stamp.hash_into(&mut fingerprint);
    fingerprint.write_i64(seconds);
    fingerprint.write_u32(nanos);
    Some(LocalParentReceipt {
        path_key,
        fingerprint: fingerprint.digest(),
    })
}

impl StagedTree {
    fn recapture(
        &self,
        payload: &Directory,
        nesting: &[OsString],
        moved: bool,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<(Tree, Inventory, Vec<Directory>)> {
        let mut children = Vec::with_capacity(nesting.len());
        for (index, (expected, name)) in self.ancestors.iter().zip(nesting).enumerate() {
            require_running(stopped)?;
            let parent = children.last().unwrap_or(payload);
            let observed = local_tree::reviewed_directory_stamp(parent)?;
            if !same_stamp(expected, &observed, moved && index == 0) {
                return Err(changed());
            }
            require_only_child(parent, name)?;
            children.push(local_tree::open_reviewed_tree_child(parent, name)?);
        }
        let (tree, inventory) = if moved && nesting.is_empty() {
            self.tree.recapture_first_publication(payload)
        } else {
            self.tree
                .recapture(children.last().unwrap_or(payload), false, stopped)
        }?;
        Ok((tree, inventory, children))
    }
}

fn same_stamp(expected: &ReviewedStamp, observed: &ReviewedStamp, moved: bool) -> bool {
    if moved {
        expected.same_after_first_publication(observed)
    } else {
        expected == observed
    }
}
