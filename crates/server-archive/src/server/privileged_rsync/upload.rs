//! Accept an ordinary rsync transfer into a bounded handoff, then publish root-owned data.

use std::io::Write;

use kortty_server_archive::privileged_rsync::{
    Decision, Operation, Reply, Review, Spec, read_decision, valid_inventory, write_reply,
};
use kortty_server_archive::rsync_tree::{DeleteScope, Inventory, Manifest, hash};

use super::super::draft_replace::mount_id;
use super::super::rsync_tree::{
    ExistingFile, ExistingPublication, ExistingRecovery, ExistingTarget, RECEIVED_FILE as FILE,
    UploadStage,
};
use super::super::*;
use super::{Recovery, Rules};

mod handoff;

use handoff::Handoff;

enum Receiver<'a> {
    Vacant(Directory),
    ExistingTree(Box<ExistingTarget<'a>>),
    ExistingFile(Box<ExistingFile<'a>>),
}

impl<'a> Receiver<'a> {
    fn open(
        rules: &Rules,
        destination: &'a str,
        directory: bool,
        replace: bool,
    ) -> io::Result<Self> {
        let (path, name) = absolute_file(destination)?;
        let parent = rules.destination_parent(path, replace)?;
        match (replace, directory) {
            (true, true) => Ok(Self::ExistingTree(Box::new(ExistingTarget::open_at(
                destination,
                parent,
            )?))),
            (true, false) => Ok(Self::ExistingFile(Box::new(ExistingFile::open_at(
                destination,
                parent,
            )?))),
            (false, _) => {
                require_absent(&parent.fd, name)?;
                Ok(Self::Vacant(parent))
            }
        }
    }

    fn parent(&self) -> &Directory {
        match self {
            Self::Vacant(parent) => parent,
            Self::ExistingTree(target) => target.parent(),
            Self::ExistingFile(target) => target.parent(),
        }
    }

    fn proof(&self, destination: &str) -> io::Result<([u8; 32], Option<Inventory>)> {
        match self {
            Self::ExistingTree(target) => {
                let original = target.review();
                Ok((original.fingerprint, Some(original.inventory)))
            }
            Self::ExistingFile(target) => {
                let original = target.review();
                Ok((original.fingerprint, Some(original.inventory)))
            }
            Self::Vacant(parent) => {
                let mut fingerprint =
                    Sha256::new_with_prefix(b"kortty-privileged-rsync-vacancy-v1\0");
                fingerprint.update(destination.as_bytes());
                fingerprint.update(parent.stat.st_dev.to_be_bytes());
                fingerprint.update(parent.stat.st_ino.to_be_bytes());
                fingerprint.update(parent.stat.st_mode.to_be_bytes());
                fingerprint.update(parent.stat.st_gid.to_be_bytes());
                fingerprint.update(mount_id(&parent.fd)?.to_be_bytes());
                Ok((fingerprint.finalize().into(), None))
            }
        }
    }
}

pub(super) fn review(rules: &Rules, spec: Spec) -> io::Result<Review> {
    let Operation::Upload {
        directory, replace, ..
    } = spec.operation
    else {
        return Err(invalid("Expected a privileged rsync upload"));
    };
    let (fingerprint, inventory) =
        Receiver::open(rules, &spec.path, directory, replace)?.proof(&spec.path)?;
    Ok(Review {
        spec,
        policy: rules.revision(),
        fingerprint,
        inventory,
        handoff: rules.handoff()?.0,
    })
}

pub(super) fn apply(
    rules: &Rules,
    review: Review,
    incoming: Inventory,
    input: &mut impl Read,
    output: &mut impl Write,
    recovery: &mut Recovery,
    recurring: bool,
) -> io::Result<Reply> {
    let Operation::Upload {
        directory,
        replace,
        ref delete,
    } = review.spec.operation
    else {
        return Err(invalid("Expected a privileged rsync upload"));
    };
    if recurring && !replace {
        return Err(invalid(
            "Recurring privileged rsync requires an existing receiver",
        ));
    }
    let destination = &review.spec.path;
    let receiver = Receiver::open(rules, destination, directory, replace)?;
    if !valid_inventory(incoming, directory)
        || receiver.proof(destination)? != (review.fingerprint, review.inventory)
        || rules.handoff()?.0 != review.handoff
    {
        return Err(invalid("The reviewed privileged rsync receiver changed"));
    }
    rules.require_current()?;
    let mut handoff = Handoff::create(rules)?;
    if !review.contains_payload(handoff.path()) {
        return Err(invalid("Invalid privileged rsync handoff address"));
    }
    write_reply(
        &mut *output,
        &Reply::Prepared {
            path: handoff.path().to_owned(),
        },
    )?;
    match read_decision(&mut *input)? {
        Decision::Abort => {
            handoff.discard()?;
            return Ok(Reply::Aborted);
        }
        Decision::Verify => {}
        _ => {
            return Err(invalid(
                "The privileged rsync handoff must be verified first",
            ));
        }
    }
    rules.require_current()?;
    let mut received = UploadStage::create(receiver.parent())?;
    handoff.copy(&received.payload, directory, incoming)?;
    handoff.discard()?;
    let (path, name) = absolute_file(destination)?;
    let permit = || {
        rules.require_current()?;
        let held = receiver.parent();
        let current = rules.destination_parent(path, replace)?;
        if !same_node(&current.stat, &held.stat)
            || current.stat.st_mode != held.stat.st_mode
            || current.stat.st_gid != held.stat.st_gid
            || mount_id(&current.fd)? != mount_id(&held.fd)?
        {
            return Err(invalid("The privileged rsync receiver parent changed"));
        }
        Ok(())
    };
    match &receiver {
        Receiver::Vacant(parent) => {
            let mut publish = Vacancy {
                parent,
                path,
                name,
                received: &mut received,
                recovery,
            };
            Ok(
                match publish.apply(input, output, directory, incoming, permit)? {
                    true => Reply::Published {
                        destination: destination.to_owned(),
                        inventory: incoming,
                        recovery: None,
                    },
                    false => Reply::Aborted,
                },
            )
        }
        Receiver::ExistingTree(target) => replace_tree(
            target,
            received,
            (incoming, delete),
            Replacement {
                input,
                output,
                recovery,
                recurring,
            },
            permit,
        ),
        Receiver::ExistingFile(target) => replace_file(
            target,
            received,
            incoming,
            Replacement {
                input,
                output,
                recovery,
                recurring,
            },
            permit,
        ),
    }
}

struct Vacancy<'a, 'stage> {
    parent: &'a Directory,
    path: &'a str,
    name: &'a str,
    received: &'a mut UploadStage<'stage>,
    recovery: &'a mut Recovery,
}

impl Vacancy<'_, '_> {
    fn apply(
        &mut self,
        input: &mut impl Read,
        output: &mut impl Write,
        directory: bool,
        expected: Inventory,
        permit: impl Fn() -> io::Result<()>,
    ) -> io::Result<bool> {
        let witness = directory
            .then(|| self.received.verify_received(expected))
            .transpose()?;
        let file = if directory {
            None
        } else {
            Some(file_witness(&self.received.payload, expected)?)
        };
        permit()?;
        require_absent(&self.parent.fd, self.name)?;
        write_reply(
            output,
            &Reply::Verified {
                inventory: expected,
                removed: 0,
            },
        )?;
        match read_decision(input)? {
            Decision::Abort => {
                self.received.discard()?;
                Ok(false)
            }
            Decision::Publish => {
                if let Some(witness) = witness {
                    let result = self
                        .received
                        .publish(self.path, self.name, &witness, permit);
                    self.recovery.outcome_unknown = self.received.published;
                    result?;
                } else if let Some(before) = file {
                    self.publish_file(before, expected, permit)?;
                }
                self.received.discard()?;
                Ok(true)
            }
            _ => Err(invalid("Unexpected privileged rsync publication decision")),
        }
    }

    fn publish_file(
        &mut self,
        before: Stamp,
        expected: Inventory,
        permit: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        let payload = &self.received.payload;
        if file_witness(payload, expected)? != before {
            return Err(invalid("The private rsync file changed before publication"));
        }
        let stat = statat(&payload.fd, FILE, AtFlags::SYMLINK_NOFOLLOW)?;
        permit()?;
        match renameat_with(
            &payload.fd,
            FILE,
            &self.parent.fd,
            self.name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => self.recovery.outcome_unknown = true,
            Err(Errno::EXIST | Errno::NOTEMPTY) => {
                return Err(invalid("The reviewed server destination already exists"));
            }
            Err(error) => {
                self.recovery.outcome_unknown = payload.require_child(FILE, &stat).is_err();
                return Err(error.into());
            }
        }
        self.parent.require_child(self.name, &stat)?;
        fsync(&payload.fd)?;
        fsync(&self.parent.fd)?;
        Ok(())
    }
}

fn file_witness(payload: &Directory, expected: Inventory) -> io::Result<Stamp> {
    let opened = File::from(openat(&payload.fd, FILE, SOURCE, Mode::empty())?);
    let stat = fstat(&opened)?;
    let before = Stamp::of(&opened.metadata()?);
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || (stat.st_uid, stat.st_nlink, before.size) != (0, 1, expected.bytes)
        || stat.st_mode & 0o7777 != 0o600
    {
        return Err(invalid("The received root-private rsync file changed"));
    }
    let mut manifest = Manifest::default();
    manifest.add("", Some((before.size, hash(&opened, before.size)?)))?;
    if manifest.finish() != expected || Stamp::of(&opened.metadata()?) != before {
        return Err(invalid(
            "The received root-private rsync file differs from the reviewed input",
        ));
    }
    payload.require_child(FILE, &stat)?;
    Ok(before)
}

fn replace_tree(
    target: &ExistingTarget<'_>,
    mut received: UploadStage<'_>,
    (incoming, delete): (Inventory, &DeleteScope),
    replacement: Replacement<'_, impl Read, impl Write>,
    permit: impl Fn() -> io::Result<()>,
) -> io::Result<Reply> {
    let mut successor = target.prepare_successor(&received, incoming, delete)?;
    received.discard()?;
    permit()?;
    replacement.publish(
        target.destination(),
        successor.inventory,
        successor.removed,
        |recurring, state, input, output| {
            target.commit_checked(&mut successor, recurring, state, permit, |path| {
                confirm_publication(input, output, path)
            })
        },
    )
}

fn replace_file(
    target: &ExistingFile<'_>,
    mut stage: UploadStage<'_>,
    expected: Inventory,
    replacement: Replacement<'_, impl Read, impl Write>,
    permit: impl Fn() -> io::Result<()>,
) -> io::Result<Reply> {
    let successor = target.prepare_successor(&stage, expected)?;
    permit()?;
    replacement.publish(
        target.destination(),
        expected,
        0,
        |recurring, state, input, output| {
            target.commit_checked(&mut stage, &successor, recurring, state, permit, |path| {
                confirm_publication(input, output, path)
            })
        },
    )
}

struct Replacement<'a, R, W> {
    input: &'a mut R,
    output: &'a mut W,
    recovery: &'a mut Recovery,
    recurring: bool,
}

impl<R: Read, W: Write> Replacement<'_, R, W> {
    fn publish(
        self,
        destination: &str,
        inventory: Inventory,
        removed: u32,
        commit: impl FnOnce(
            bool,
            &mut ExistingRecovery,
            &mut R,
            &mut W,
        ) -> io::Result<Option<ExistingPublication>>,
    ) -> io::Result<Reply> {
        write_reply(&mut *self.output, &Reply::Verified { inventory, removed })?;
        match read_decision(&mut *self.input)? {
            Decision::Abort => Ok(Reply::Aborted),
            Decision::Quarantine => {
                let mut state = ExistingRecovery::default();
                let result = commit(self.recurring, &mut state, self.input, self.output);
                self.recovery.outcome_unknown = state.uncertain;
                self.recovery.recovery = state.path;
                let Some(ExistingPublication { recovery, receiver }) = result? else {
                    return Ok(Reply::Aborted);
                };
                let destination = destination.to_owned();
                match (self.recurring, receiver) {
                    (true, Some(receiver)) if receiver.inventory == inventory => {
                        Ok(Reply::PublishedWithReceiver {
                            destination,
                            receiver,
                            recovery,
                        })
                    }
                    (false, None) => Ok(Reply::Published {
                        destination,
                        inventory,
                        recovery: Some(recovery),
                    }),
                    _ => Err(invalid(
                        "The privileged rsync helper could not confirm the published receiver",
                    )),
                }
            }
            _ => Err(invalid(
                "The privileged rsync receiver must be quarantined before publication",
            )),
        }
    }
}

fn confirm_publication(
    input: &mut impl Read,
    output: &mut impl Write,
    path: &str,
) -> io::Result<bool> {
    write_reply(
        output,
        &Reply::Quarantined {
            recovery: path.to_owned(),
        },
    )?;
    match read_decision(input)? {
        Decision::Publish => Ok(true),
        Decision::Abort => Ok(false),
        _ => Err(invalid("Unexpected privileged rsync publication decision")),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::DirBuilderExt as _;
    use std::path::PathBuf;

    use kortty_server_archive::privileged_rsync::read_reply;
    use kortty_server_archive::rsync_tree::write_frame;

    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn recurring_privileged_tree_receipt_matches_the_published_receiver_review() {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("kortty-privileged-tree-{}", hex::encode(nonce)));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = Scratch(root);
        let destination = root.0.join("receiver");
        let path = destination.to_str().unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&destination)
            .unwrap();
        fs::write(destination.join("retained"), b"preserved").unwrap();
        let open_parent = || Directory::open_parent(root.0.to_str().unwrap()).unwrap();
        let target = ExistingTarget::open_at(path, open_parent()).unwrap();
        let original = target.review();
        let received = UploadStage::create(target.parent()).unwrap();
        let mut source = File::from(
            openat(
                &received.payload.fd,
                "incoming",
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW,
                PRIVATE_FILE,
            )
            .unwrap(),
        );
        let bytes = b"mixed upload file";
        source.write_all(bytes).unwrap();
        source.sync_all().unwrap();
        let mut manifest = Manifest::default();
        manifest
            .add(
                "incoming",
                Some((bytes.len() as u64, Sha256::digest(bytes).into())),
            )
            .unwrap();
        let incoming = manifest.finish();
        let mut decisions = Vec::new();
        write_frame(&mut decisions, &Decision::Quarantine).unwrap();
        write_frame(&mut decisions, &Decision::Publish).unwrap();
        let mut output = Vec::new();
        let reply = replace_tree(
            &target,
            received,
            (incoming, &DeleteScope::Retain),
            Replacement {
                input: &mut decisions.as_slice(),
                output: &mut output,
                recovery: &mut Recovery::default(),
                recurring: true,
            },
            || Ok(()),
        )
        .unwrap();
        let Reply::PublishedWithReceiver {
            destination: reply_path,
            receiver,
            recovery,
        } = reply
        else {
            panic!("expected a recurring privileged receiver receipt");
        };
        assert_eq!(reply_path, path);
        assert_ne!(receiver, original);
        assert_eq!(
            receiver,
            ExistingTarget::open_at(path, open_parent())
                .unwrap()
                .review()
        );
        assert_eq!(fs::read(destination.join("incoming")).unwrap(), bytes);
        assert_eq!(
            fs::read(destination.join("retained")).unwrap(),
            b"preserved"
        );
        assert_eq!(
            fs::read(PathBuf::from(&recovery).join("retained")).unwrap(),
            b"preserved"
        );
        let mut output = output.as_slice();
        assert!(
            matches!(read_reply(&mut output).unwrap(), Reply::Verified { inventory, removed: 0 } if inventory == receiver.inventory)
        );
        assert!(
            matches!(read_reply(&mut output).unwrap(), Reply::Quarantined { recovery: path } if path == recovery)
        );
        assert!(output.is_empty());
    }
}
