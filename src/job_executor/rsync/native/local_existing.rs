//! Replace a reviewed local folder through a private successor and recoverable original.

use std::cell::Cell;
use std::ffi::{OsStr, OsString};
use std::hash::Hasher as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use kortty_jobs::{RunId, RunOutcome};
use kortty_server_archive::rsync_tree::{DeleteScope, Inventory};

use crate::job_executor::rsync::LocalExistingSnapshot;
use crate::private_files::nonce;
use crate::sftp_window::local_tree::{self, Directory, Entry, ReviewedStamp};

use super::{Completion, Failure, require_running};

mod owned;
mod witness;
use owned::CreatedTree;
use witness::{Fingerprint, Tree, View};

#[cfg(test)]
mod tests;

const ORIGINAL: &str = "original";
const PAYLOAD: &str = "payload";
const REVIEW: &str = "The reviewed local rsync receiver changed or could not be checked.";
const PREPARE: &str = "Could not prepare the verified local rsync successor.";

pub(in crate::job_executor::rsync) fn review(
    path: &Path,
    cancelled: &AtomicBool,
) -> io::Result<LocalExistingSnapshot> {
    inspect(path, &|| cancelled.load(Ordering::Acquire))
}

pub(super) fn inspect(
    path: &Path,
    stopped: &impl Fn() -> bool,
) -> io::Result<LocalExistingSnapshot> {
    Receiver::open(path, stopped).map(|receiver| receiver.snapshot)
}

pub(super) struct Receiver {
    path: PathBuf,
    name: OsString,
    parent: Directory,
    parent_stamp: ReviewedStamp,
    directory: Directory,
    reviewed: Tree,
    snapshot: LocalExistingSnapshot,
}

pub(super) struct Published {
    pub(super) completion: Completion,
    pub(super) successor: Option<LocalExistingSnapshot>,
}

pub(super) struct Incoming<'a> {
    pub(super) directory: &'a Directory,
    pub(super) inventory: Inventory,
    pub(super) delete: &'a DeleteScope,
}

impl Receiver {
    pub(super) fn prepare(
        path: &Path,
        expected: LocalExistingSnapshot,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<Self> {
        let current = Self::open(path, stopped)?;
        if expected.valid() && current.snapshot == expected {
            Ok(current)
        } else {
            Err(changed())
        }
    }

    fn open(path: &Path, stopped: &impl Fn() -> bool) -> io::Result<Self> {
        require_running(stopped)?;
        if !path.is_absolute() {
            return Err(changed());
        }
        let parent_path = path.parent().ok_or_else(changed)?;
        let name = path.file_name().ok_or_else(changed)?;
        witness::checked_name(name)?;
        let parent = local_tree::open_reviewed_tree_parent(parent_path)?;
        let parent_stamp = local_tree::reviewed_tree_parent_stamp(&parent)?;
        let directory = local_tree::open_reviewed_tree_child(&parent, name)?;
        let volume = local_tree::reviewed_tree_volume(&parent)?;
        let (reviewed, inventory) = Tree::capture(&directory, volume, stopped)?;
        let snapshot = snapshot(path, &parent_stamp, &reviewed, inventory);
        let result = Self {
            path: path.to_owned(),
            name: name.to_owned(),
            parent,
            parent_stamp,
            directory,
            reviewed,
            snapshot,
        };
        result.require_anchor()?;
        result.require_original()?;
        require_running(stopped)?;
        Ok(result)
    }

    pub(super) fn publish(
        self,
        incoming: Incoming<'_>,
        stopped: &impl Fn() -> bool,
        require_authority: &impl Fn() -> Result<(), Failure>,
        id: RunId,
        arm: impl FnOnce(u32) -> Result<(), Failure>,
    ) -> Result<Published, Failure> {
        require_running(stopped).map_err(|_| super::transfer_stopped())?;
        require_authority()?;
        self.require_anchor()
            .and_then(|()| self.require_original())
            .map_err(|_| Failure::blocked(REVIEW))?;
        let mut stage = Stage::create(&self.parent).map_err(|_| Failure::failed(PREPARE))?;
        let stage_path = stage.path(self.parent_path());
        let recovery = stage_path.join(ORIGINAL);
        let recovery = recovery
            .to_str()
            .filter(|path| {
                path.len() <= 4_192
                    && !crate::ssh_dialog::unsafe_text(path)
                    && path
                        .chars()
                        .all(crate::session_journal::is_display_character)
            })
            .ok_or_else(|| {
                Failure::blocked("Choose a local rsync folder with a reportable recovery path.")
            })?;
        let stage_location = stage_path.display();
        let admission_outcome = Cell::new(None);
        let phase = || {
            require_authority().map_err(|failure| {
                admission_outcome.set(Some(failure.outcome));
                io::Error::new(io::ErrorKind::PermissionDenied, failure.detail)
            })?;
            require_running(stopped)
        };
        let arm_failure = Cell::new(None);
        let arm = |removed| {
            arm(removed).map_err(|failure| {
                let error = io::Error::other(failure.detail);
                arm_failure.set(Some(failure));
                error
            })
        };
        let result = self
            .compose(
                &mut stage,
                incoming.directory,
                incoming.inventory,
                incoming.delete,
                stopped,
            )
            .and_then(|candidate| self.commit(&mut stage, &candidate, stopped, &phase, arm));
        let cleanup_failed = stage.cleanup().is_err();
        let (completion, successor) = match result {
            Ok(State::Published(successor)) => {
                let mut completion = Completion::from_result(id, Ok(0));
                completion.detail = Some(format!(
                    "Published the verified local rsync tree. The original is retained at {recovery}."
                ));
                (completion, Some(successor))
            }
            Ok(State::Restored(error)) => {
                let denial = admission_outcome
                    .get()
                    .or_else(|| require_authority().err().map(|failure| failure.outcome));
                let mut completion = incomplete(id, &error, denial);
                completion.append_detail(
                    "The original local folder was restored to the reviewed destination.",
                );
                if cleanup_failed {
                    completion.append_detail(&format!(
                        "Private staging folder {stage_location} may remain."
                    ));
                }
                (completion, None)
            }
            Ok(State::Uncertain) => {
                let mut completion = Completion::from_result(
                    id,
                    Err(Failure::failed(
                        "Local rsync publication or restoration could not be confirmed.",
                    )),
                );
                completion.append_detail(&format!("Check the reviewed destination and the last reported recovery location {recovery} before retrying."));
                (completion, None)
            }
            Err(error) => {
                let failure = arm_failure
                    .into_inner()
                    .unwrap_or_else(|| classify(&error, admission_outcome.into_inner()));
                if !cleanup_failed {
                    return Err(failure);
                }
                let mut completion = Completion::from_result(id, Err(failure));
                completion.append_detail(&format!(
                    "Private staging folder {stage_location} may remain."
                ));
                (completion, None)
            }
        };
        Ok(Published {
            completion,
            successor,
        })
    }

    fn compose(
        &self,
        stage: &mut Stage<'_>,
        incoming: &Directory,
        expected: Inventory,
        delete: &DeleteScope,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<Tree> {
        let incoming_volume = local_tree::reviewed_tree_volume(incoming)?;
        let (received, inventory) = Tree::capture(incoming, incoming_volume, stopped)?;
        if !expected.valid() || inventory != expected || !received.accepts_delete(delete) {
            return Err(changed());
        }
        let (payload, created) = stage.candidate();
        let composed = witness::compose(
            payload,
            created,
            View {
                directory: &self.directory,
                tree: &self.reviewed,
            },
            View {
                directory: incoming,
                tree: &received,
            },
            delete,
            stopped,
        )?;
        let (candidate, actual) =
            Tree::capture(stage.payload(), self.reviewed.stamp.volume(), stopped)?;
        if actual != composed || *delete == DeleteScope::All && actual != expected {
            return Err(changed());
        }
        received.require_current(incoming, false, stopped)?;
        Ok(candidate)
    }

    fn parent_path(&self) -> &Path {
        self.path.parent().expect("checked receiver parent")
    }

    fn require_anchor(&self) -> io::Result<()> {
        let current = local_tree::open_reviewed_tree_parent(self.parent_path())?;
        if !self.parent.same_as(&current)?
            || local_tree::reviewed_tree_parent_stamp(&current)? != self.parent_stamp
        {
            return Err(changed());
        }
        Ok(())
    }

    fn require_original(&self) -> io::Result<()> {
        let current = require_named(&self.parent, &self.name, &self.directory)?;
        if local_tree::reviewed_directory_stamp(&current)? != self.reviewed.stamp {
            return Err(changed());
        }
        Ok(())
    }

    fn commit(
        &self,
        stage: &mut Stage<'_>,
        candidate: &Tree,
        stopped: &impl Fn() -> bool,
        phase: &impl Fn() -> io::Result<()>,
        arm: impl FnOnce(u32) -> io::Result<()>,
    ) -> io::Result<State> {
        self.require_anchor()?;
        self.require_original()?;
        self.reviewed
            .require_current(&self.directory, false, stopped)?;
        candidate.require_current(stage.payload(), false, stopped)?;
        local_tree::sync_reviewed_tree_directory(stage.outer())?;
        local_tree::sync_reviewed_tree_directory(&self.parent)?;
        phase()?;
        arm(self.reviewed.removed_paths(candidate))?;
        // A failed rename can leave a displaced original. Suppress cleanup until
        // the held destination or confirmed restoration establishes ownership.
        stage.retain = true;
        let displaced = local_tree::move_reviewed_tree(
            &self.parent,
            &self.name,
            &self.directory,
            stage.outer(),
            OsStr::new(ORIGINAL),
            &|| {
                self.require_anchor()?;
                self.require_original()?;
                stage.require_successor(candidate)?;
                phase()
            },
        );
        if let Err(error) = displaced {
            if self
                .require_anchor()
                .and_then(|()| self.require_original())
                .is_ok()
            {
                stage.retain = false;
                return Err(error);
            }
            return Ok(self.restore(stage, error));
        }
        let result = (|| {
            self.require_displaced(stage)?;
            local_tree::sync_reviewed_tree_directory(stage.outer())?;
            local_tree::sync_reviewed_tree_directory(&self.parent)?;
            phase()?;
            self.reviewed
                .require_current(&self.directory, true, stopped)?;
            candidate.require_current(stage.payload(), false, stopped)?;
            local_tree::move_reviewed_tree(
                stage.outer(),
                OsStr::new(PAYLOAD),
                stage.payload(),
                &self.parent,
                &self.name,
                &|| {
                    self.require_displaced(stage)?;
                    stage.require_successor(candidate)?;
                    phase()
                },
            )?;
            // A renamed successor must still be the exact held tree before success.
            self.require_displaced(stage)?;
            require_named(&self.parent, &self.name, stage.payload())?;
            let (successor, inventory) = candidate.recapture(stage.payload(), true, &|| false)?;
            local_tree::sync_reviewed_tree_directory(stage.outer())?;
            local_tree::sync_reviewed_tree_directory(&self.parent)?;
            self.require_displaced(stage)?;
            let current = require_named(&self.parent, &self.name, stage.payload())?;
            if local_tree::reviewed_directory_stamp(&current)? != successor.stamp {
                return Err(changed());
            }
            Ok(snapshot(
                &self.path,
                &self.parent_stamp,
                &successor,
                inventory,
            ))
        })();
        match result {
            Ok(successor) => Ok(State::Published(successor)),
            Err(error) => Ok(self.restore(stage, error)),
        }
    }

    fn require_displaced(&self, stage: &Stage<'_>) -> io::Result<()> {
        self.require_anchor()?;
        stage.require_wrapper()?;
        require_named(stage.outer(), OsStr::new(ORIGINAL), &self.directory)?;
        Ok(())
    }

    fn restore(&self, stage: &mut Stage<'_>, error: io::Error) -> State {
        let restored = (|| {
            self.require_displaced(stage)?;
            local_tree::move_reviewed_tree(
                stage.outer(),
                OsStr::new(ORIGINAL),
                &self.directory,
                &self.parent,
                &self.name,
                &|| self.require_displaced(stage),
            )?;
            local_tree::sync_reviewed_tree_directory(stage.outer())?;
            local_tree::sync_reviewed_tree_directory(&self.parent)?;
            self.require_anchor()?;
            // Preserve intervening edits to that same held original.
            require_named(&self.parent, &self.name, &self.directory)?;
            Ok::<_, io::Error>(())
        })();
        if restored.is_ok() {
            stage.retain = false;
            State::Restored(error)
        } else {
            State::Uncertain
        }
    }
}

enum State {
    Published(LocalExistingSnapshot),
    Restored(io::Error),
    Uncertain,
}

fn snapshot(
    path: &Path,
    parent: &ReviewedStamp,
    tree: &Tree,
    inventory: Inventory,
) -> LocalExistingSnapshot {
    let mut identity = Fingerprint::new(b"kortty-local-rsync-root-v1\0");
    tree.stamp.hash_identity_into(&mut identity);
    let mut fingerprint = Fingerprint::new(b"kortty-local-rsync-receiver-v1\0");
    let path_bytes = path.as_os_str().as_encoded_bytes();
    fingerprint.write_usize(path_bytes.len());
    fingerprint.write(path_bytes);
    parent.hash_into(&mut fingerprint);
    tree.hash_into(&mut fingerprint);
    LocalExistingSnapshot {
        inventory,
        fingerprint: fingerprint.digest(),
        root_identity: identity.digest(),
    }
}

struct Stage<'a> {
    parent: &'a Directory,
    name: OsString,
    outer: Option<Directory>,
    payload: Option<Directory>,
    created: Option<CreatedTree>,
    retain: bool,
}

impl<'a> Stage<'a> {
    fn create(parent: &'a Directory) -> io::Result<Self> {
        let name = OsString::from(format!(".kortty-{}.kortty-part", nonce()?));
        let outer = local_tree::create_reviewed_tree_stage(parent, &name)?;
        let mut stage = Self {
            parent,
            name,
            outer: Some(outer),
            payload: None,
            created: None,
            retain: false,
        };
        stage.payload = Some(local_tree::create_reviewed_tree_stage(
            stage.outer(),
            OsStr::new(PAYLOAD),
        )?);
        stage.created = Some(CreatedTree::new(stage.payload())?);
        stage.require_wrapper()?;
        Ok(stage)
    }

    fn outer(&self) -> &Directory {
        self.outer.as_ref().expect("held private stage")
    }

    fn payload(&self) -> &Directory {
        self.payload.as_ref().expect("held private successor")
    }

    fn candidate(&mut self) -> (&Directory, &mut CreatedTree) {
        (
            self.payload.as_ref().expect("held private successor"),
            self.created
                .as_mut()
                .expect("remembered private successor entries"),
        )
    }

    fn require_wrapper(&self) -> io::Result<()> {
        require_named(self.parent, &self.name, self.outer())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = self.outer().metadata()?;
            if metadata.uid() != rustix::process::geteuid().as_raw()
                || metadata.mode() & 0o7777 != 0o700
            {
                return Err(changed());
            }
            #[cfg(target_os = "macos")]
            kortty_files::require_no_extended_acl(self.outer().file())?;
            Ok(())
        }
        #[cfg(windows)]
        self.outer().require_owner_only()
    }

    fn require_successor(&self, candidate: &Tree) -> io::Result<()> {
        self.require_wrapper()?;
        let current = require_named(self.outer(), OsStr::new(PAYLOAD), self.payload())?;
        if local_tree::reviewed_directory_stamp(&current)? != candidate.stamp {
            return Err(changed());
        }
        Ok(())
    }

    fn path(&self, parent: &Path) -> PathBuf {
        parent.join(&self.name)
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if self.retain || self.outer.is_none() {
            return Ok(());
        }
        self.require_wrapper()?;
        let outer = self.outer.take().expect("held private stage");
        if let Some(payload) = self.payload.take() {
            let name = OsStr::new(PAYLOAD);
            require_named(&outer, name, &payload)?;
            let created = self.created.take().ok_or_else(changed)?;
            created.discard(&outer, name, payload)?;
        }
        local_tree::remove_empty_reviewed_tree_stage(self.parent, &self.name, outer)
    }
}

impl Drop for Stage<'_> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn require_named(parent: &Directory, name: &OsStr, held: &Directory) -> io::Result<Directory> {
    let Entry::Directory(path) = local_tree::entry(parent, name)? else {
        return Err(changed());
    };
    if held.same_as(&path)? {
        Ok(path)
    } else {
        Err(changed())
    }
}

fn changed() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, REVIEW)
}

fn classify(error: &io::Error, denied: Option<RunOutcome>) -> Failure {
    if let Some(outcome) = denied {
        Failure {
            outcome,
            detail: "The local rsync publication approval changed.",
        }
    } else if error.kind() == io::ErrorKind::Interrupted {
        super::transfer_stopped()
    } else if error.kind() == io::ErrorKind::InvalidData {
        Failure::blocked(REVIEW)
    } else {
        Failure::failed(PREPARE)
    }
}

fn incomplete(id: RunId, error: &io::Error, denied: Option<RunOutcome>) -> Completion {
    Completion::from_result(id, Err(classify(error, denied)))
}
