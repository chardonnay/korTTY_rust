//! Publish one verified regular file together with its remaining missing parents.

use std::fs::File;

use crate::sftp_window::{FileWitness, LocalFileSnapshot};

use super::*;

pub(in crate::job_executor::rsync::native) struct MissingLocalFileReceiver(CheckedVacantPath);

pub(in crate::job_executor::rsync::native) struct MissingLocalFileStage {
    file: File,
    stage: Stage,
    path: PathBuf,
    destination: PathBuf,
}

impl MissingLocalFileStage {
    pub(in crate::job_executor::rsync::native) fn writer(&self) -> io::Result<&File> {
        Ok(&self.file)
    }
}

pub(in crate::job_executor::rsync::native) enum MissingLocalFilePublication {
    Published {
        successor: Box<LocalFileSnapshot>,
        parents: Vec<LocalParentReceipt>,
        remaining_stage: Option<PathBuf>,
    },
    Uncertain {
        error: io::Error,
        stage: PathBuf,
    },
}

impl MissingLocalFileReceiver {
    pub(in crate::job_executor::rsync::native) fn prepare(
        path: &Path,
        expected: VacantLocalTreeSnapshot,
        parents: &[LocalParentReceipt],
        stopped: &impl Fn() -> bool,
    ) -> io::Result<Self> {
        if expected.missing_parent_count() == 0 {
            return Err(changed());
        }
        CheckedVacantPath::prepare_with_parents(path, expected, parents, stopped).map(Self)
    }

    pub(in crate::job_executor::rsync::native) fn missing_parent_count(&self) -> u32 {
        self.0.missing_parent_count()
    }

    pub(in crate::job_executor::rsync::native) fn stage(
        &self,
    ) -> io::Result<MissingLocalFileStage> {
        self.0.require_vacant()?;
        let mut stage = Stage::create(&self.0.parent)?;
        let path = stage.path(self.0.anchor_path());
        reportable_path(&path).ok_or_else(changed)?;
        let (root, created, directories) = stage.create_nesting(self.directories(), &|| false)?;
        let file = local_tree::create_reviewed_tree_read_write_file(
            directories.last().unwrap_or(root),
            self.name(),
        )?;
        created.file(witness::checked_name(self.name())?, &file)?;
        self.0.require_vacant()?;
        Ok(MissingLocalFileStage {
            file,
            stage,
            path,
            destination: self.0.path.clone(),
        })
    }

    pub(in crate::job_executor::rsync::native) fn publish(
        self,
        download: MissingLocalFileStage,
        incoming: Inventory,
        stopped: &impl Fn() -> bool,
        authorize: &impl Fn() -> io::Result<()>,
        arm: impl FnOnce(u32) -> io::Result<()>,
    ) -> io::Result<MissingLocalFilePublication> {
        let MissingLocalFileStage {
            mut stage,
            path,
            file,
            destination,
        } = download;
        let _ancestors = local_tree::hold_reviewed_file_ancestors(self.0.anchor_path())?;
        require_running(stopped)?;
        authorize()?;
        self.0.require_vacant()?;
        if destination.as_os_str() != self.0.path.as_os_str()
            || !stage.parent.same_as(&self.0.parent)?
            || path != stage.path(self.0.anchor_path())
        {
            return Err(changed());
        }
        file.sync_all()?;
        let witness = FileWitness::capture(&file, stopped)?;
        if witness.inventory != incoming || witness.stamp.volume() != self.0.parent_stamp.volume() {
            return Err(changed());
        }
        let candidate = self.inspect_candidate(stage.payload(), stopped)?;
        if local_tree::reviewed_file_stamp(&candidate.file)? != witness.stamp {
            return Err(changed());
        }
        for directory in std::iter::once(candidate.root)
            .chain(&candidate.directories)
            .rev()
        {
            local_tree::sync_reviewed_tree_directory(directory)?;
        }
        let stamps = candidate.stamps;
        drop(candidate.file);
        drop(candidate.directories);
        local_tree::sync_reviewed_tree_directory(stage.outer())?;
        local_tree::sync_reviewed_tree_directory(&self.0.parent)?;
        let require_current = || {
            require_running(stopped)?;
            authorize()?;
            self.0.require_vacant()?;
            stage.require_successor(&stamps[0])?;
            self.require_candidate(stage.payload(), &stamps, &witness, stopped)
        };
        let recheck_move = || {
            let candidate = self.inspect_candidate(stage.payload(), stopped)?;
            let observed = FileWitness::capture(&candidate.file, stopped)?;
            if candidate.stamps != stamps
                || observed.stamp != witness.stamp
                || observed.inventory != witness.inventory
            {
                return Err(changed());
            }
            require_current()
        };

        let result = if self.0.nesting.is_empty() {
            require_current()?;
            arm(0)?;
            recheck_move()?;
            stage.retain = true;
            let result = local_tree::publish_file(
                &self.0.parent,
                stage.payload(),
                self.name(),
                &file,
                self.name(),
            );
            drop(file);
            result
        } else {
            // Windows can refuse to rename a directory while its descendant is held for writing.
            drop(file);
            let arm = Cell::new(Some(arm));
            let require_current = || {
                require_current()?;
                if let Some(arm) = arm.take() {
                    arm(0)?;
                }
                recheck_move()
            };
            local_tree::move_reviewed_tree(
                stage.outer(),
                OsStr::new(PAYLOAD),
                stage.payload(),
                &self.0.parent,
                self.0.prefix_name(),
                &require_current,
            )
        };
        stage.retain = true;
        if let Err(error) = result {
            let remains_private = stage.require_wrapper().is_ok()
                && require_named(stage.outer(), OsStr::new(PAYLOAD), stage.payload()).is_ok()
                && (!self.0.nesting.is_empty()
                    || named_file(stage.payload(), self.name())
                        .and_then(|file| local_tree::reviewed_file_stamp(&file))
                        .is_ok_and(|stamp| witness.stamp.same_identity(&stamp)));
            if remains_private {
                stage.retain = false;
                return Err(if error.kind() == io::ErrorKind::AlreadyExists {
                    changed()
                } else {
                    error
                });
            }
            return Ok(MissingLocalFilePublication::Uncertain { error, stage: path });
        }

        let confirmation = self.confirm(&stage, &stamps, &witness);
        match confirmation {
            Ok((successor, parents)) => {
                if self.0.nesting.is_empty() {
                    // The now-empty payload directory remains private after moving just its file.
                    if let Some(created) = &mut stage.created {
                        created
                            .forget_published_file(self.name().to_str().expect("checked filename"));
                    }
                } else {
                    drop(stage.payload.take());
                    drop(stage.created.take());
                }
                stage.retain = false;
                Ok(MissingLocalFilePublication::Published {
                    successor: Box::new(successor),
                    parents,
                    remaining_stage: stage.cleanup().err().map(|_| path),
                })
            }
            Err(error) => Ok(MissingLocalFilePublication::Uncertain { error, stage: path }),
        }
    }

    fn name(&self) -> &OsStr {
        self.0
            .path
            .file_name()
            .expect("checked destination filename")
    }

    fn directories(&self) -> &[OsString] {
        self.0
            .nesting
            .split_last()
            .map_or(&[], |(_, directories)| directories)
    }

    fn inspect_candidate<'a>(
        &self,
        root: &'a Directory,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<FileCandidate<'a>> {
        let mut directories = Vec::with_capacity(self.directories().len());
        for name in self.directories() {
            require_running(stopped)?;
            let parent = directories.last().unwrap_or(root);
            require_only_child(parent, name)?;
            let Entry::Directory(child) = local_tree::entry(parent, name)? else {
                return Err(changed());
            };
            directories.push(child);
        }
        let parent = directories.last().unwrap_or(root);
        require_only_child(parent, self.name())?;
        let file = named_file(parent, self.name())?;
        let volume = self.0.parent_stamp.volume();
        let stamps = std::iter::once(root)
            .chain(&directories)
            .map(|directory| {
                let stamp = local_tree::reviewed_directory_stamp(directory)?;
                if stamp.volume() != volume {
                    return Err(changed());
                }
                Ok(stamp)
            })
            .collect::<io::Result<_>>()?;
        Ok(FileCandidate {
            root,
            directories,
            stamps,
            file,
        })
    }

    fn require_candidate(
        &self,
        root: &Directory,
        stamps: &[ReviewedStamp],
        expected: &FileWitness,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<()> {
        let candidate = self.inspect_candidate(root, stopped)?;
        if stamps == candidate.stamps
            && local_tree::reviewed_file_stamp(&candidate.file)? == expected.stamp
        {
            Ok(())
        } else {
            Err(changed())
        }
    }

    fn confirm(
        &self,
        stage: &Stage,
        stamps: &[ReviewedStamp],
        expected: &FileWitness,
    ) -> io::Result<(LocalFileSnapshot, Vec<LocalParentReceipt>)> {
        local_tree::sync_reviewed_tree_directory(stage.outer())?;
        local_tree::sync_reviewed_tree_directory(&self.0.parent)?;
        self.0.require_anchor()?;
        let name = self.name();
        let mut parents = Vec::new();
        let immediate = self.0.nesting.is_empty();
        let mut successor = if immediate {
            let file = named_file(&self.0.parent, name)?;
            let observed = FileWitness::capture(&file, &|| false)?;
            if !expected.stamp.same_after_first_publication(&observed.stamp)
                || expected.inventory != observed.inventory
            {
                return Err(changed());
            }
            self.0.require_anchor()?;
            require_file_stamp(&self.0.parent, name, &observed.stamp)?;
            observed.snapshot(&self.0.path, &self.0.parent, &self.0.parent_stamp)
        } else {
            let root = require_named(&self.0.parent, self.0.prefix_name(), stage.payload())?;
            #[cfg(windows)]
            root.require_reviewed_tree_basename(self.0.prefix_name())?;
            let candidate = self.inspect_candidate(&root, &|| false)?;
            if !stamps
                .iter()
                .zip(&candidate.stamps)
                .enumerate()
                .all(|(i, (expected, observed))| same_stamp(expected, observed, i == 0))
            {
                return Err(changed());
            }
            let observed = FileWitness::capture(&candidate.file, &|| false)?;
            if expected.stamp != observed.stamp || expected.inventory != observed.inventory {
                return Err(changed());
            }
            self.0.require_anchor()?;
            let named = require_named(&self.0.parent, self.0.prefix_name(), &root)?;
            self.require_candidate(&named, &candidate.stamps, &observed, &|| false)?;
            let mut path = self.0.first_missing().to_path_buf();
            for ((directory, stamp), next) in std::iter::once(&root)
                .chain(&candidate.directories)
                .zip(&candidate.stamps)
                .zip(&self.0.nesting)
            {
                if let Some(receipt) = parent_receipt(&path, directory, &stamp.for_tree_parent()) {
                    parents.push(receipt);
                }
                path.push(next);
            }
            let parent = candidate.directories.last().unwrap_or(&root);
            let stamp = candidate
                .stamps
                .last()
                .ok_or_else(changed)?
                .for_tree_parent();
            observed.snapshot(&self.0.path, parent, &stamp)
        };
        successor.published_from = Some(self.0.original.fingerprint);
        if !crate::job_executor::rsync::VacantFileProof::Hierarchy(self.0.original)
            .allows_successor(&successor)
        {
            return Err(changed());
        }
        Ok((successor, parents))
    }
}

struct FileCandidate<'a> {
    root: &'a Directory,
    directories: Vec<Directory>,
    stamps: Vec<ReviewedStamp>,
    file: File,
}

fn named_file(parent: &Directory, name: &OsStr) -> io::Result<File> {
    let Entry::File(file) = local_tree::entry(parent, name)? else {
        return Err(changed());
    };
    #[cfg(windows)]
    kortty_files::tree::require_reviewed_file_basename(&file, name)?;
    Ok(file)
}

fn require_file_stamp(
    parent: &Directory,
    name: &OsStr,
    expected: &ReviewedStamp,
) -> io::Result<()> {
    if local_tree::reviewed_file_stamp(&named_file(parent, name)?)? == *expected {
        Ok(())
    } else {
        Err(changed())
    }
}
