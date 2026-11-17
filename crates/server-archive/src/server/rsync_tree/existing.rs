//! Build a successor from two checked trees and retain the displaced receiver.

use std::collections::BTreeMap;
use std::fs::FileTimes;

use kortty_server_archive::rsync_tree::{
    DeleteScope, ExistingDecision, ExistingReply, ExistingRequest, ExistingRequestV2, ExistingTree,
    MAX_BYTES, RecurringReceiver, read_existing_v2_request,
};
use rustix::fs::{FlockOperation, XattrFlags, fchown, flock, fsetxattr};
use rustix::process::Gid;

use super::super::draft_replace::{mount_id, read_xattrs};
use super::*;

const ORIGINAL: &str = "original";

#[derive(Default)]
pub(in crate::server) struct Recovery {
    pub(in crate::server) uncertain: bool,
    pub(in crate::server) path: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Version {
    V1,
    V2,
    V3,
}

pub(crate) fn serve(input: impl Read, output: impl io::Write) -> io::Result<()> {
    serve_with(input, output, Version::V1)
}

pub(crate) fn serve_v2(input: impl Read, output: impl io::Write) -> io::Result<()> {
    serve_with(input, output, Version::V2)
}

pub(crate) fn serve_v3(input: impl Read, output: impl io::Write) -> io::Result<()> {
    serve_with(input, output, Version::V3)
}

fn serve_with(
    mut input: impl Read,
    mut output: impl io::Write,
    version: Version,
) -> io::Result<()> {
    let mut recovery = Recovery::default();
    let reply = (|| {
        if getuid() != geteuid() || getgid() != getegid() || geteuid().is_root() {
            return Err(invalid(
                "Rsync tree replacement requires an ordinary account",
            ));
        }
        umask(Mode::from_bits_truncate(0o077));
        let request = match version {
            Version::V1 => read_frame::<ExistingRequest>(&mut input)?.into(),
            Version::V2 | Version::V3 => read_existing_v2_request(&mut input)?,
        };
        match request {
            ExistingRequestV2::Review { destination } => {
                let target = Target::open(&destination)?;
                target.verify(false)?;
                Ok(ExistingReply::Reviewed {
                    original: target.review,
                })
            }
            ExistingRequestV2::Prepare {
                destination,
                incoming,
                original,
                delete,
            } => {
                if !incoming.valid() || !original.inventory.valid() {
                    return Err(invalid("The reviewed rsync inventory is invalid"));
                }
                if !delete.valid() {
                    return Err(invalid("The reviewed rsync deletion scope is invalid"));
                }
                let target = Target::open(&destination)?;
                if target.review != original {
                    return Err(changed());
                }
                transact(
                    target,
                    incoming,
                    &delete,
                    &mut input,
                    &mut output,
                    &mut recovery,
                    version,
                )
            }
            ExistingRequestV2::ReviewRecurring {
                destination,
                directory,
                delete,
            } if version == Version::V3 => {
                require_receipt_space(&destination)?;
                let target = RecurringTarget::open(&destination, directory, &delete, None)?;
                let receiver = target.review()?;
                Ok(ExistingReply::ReviewedRecurring {
                    destination,
                    receiver,
                })
            }
            ExistingRequestV2::PrepareRecurring {
                destination,
                incoming,
                directory,
                receiver,
                delete,
            } if version == Version::V3 => {
                require_receipt_space(&destination)?;
                let target =
                    RecurringTarget::open(&destination, directory, &delete, Some(receiver))?;
                if target.review()? != receiver {
                    return Err(changed());
                }
                let exchange = Exchange {
                    input: &mut input,
                    output: &mut output,
                    recovery: &mut recovery,
                };
                match (target, receiver) {
                    (RecurringTarget::Tree(target), _) => transact(
                        *target,
                        incoming,
                        &delete,
                        exchange.input,
                        exchange.output,
                        exchange.recovery,
                        version,
                    ),
                    (RecurringTarget::File(target), _) => {
                        transact_file(&target, incoming, exchange)
                    }
                    (
                        RecurringTarget::VacantTree(target),
                        RecurringReceiver::Vacant { fingerprint },
                    ) => transact_first(
                        target.parent(),
                        &destination,
                        fingerprint,
                        incoming,
                        exchange,
                        |stage| target.prepare(stage, incoming),
                        |stage, candidate, uncertain| {
                            target.publish(stage, candidate, uncertain, || Ok(()))
                        },
                    ),
                    (
                        RecurringTarget::VacantFile(target),
                        RecurringReceiver::Vacant { fingerprint },
                    ) => transact_first(
                        target.parent(),
                        &destination,
                        fingerprint,
                        incoming,
                        exchange,
                        |stage| target.prepare(stage, incoming),
                        |stage, candidate, uncertain| {
                            target.publish(stage, candidate, uncertain, || Ok(()))
                        },
                    ),
                    _ => Err(changed()),
                }
            }
            _ => Err(invalid(
                "Recurring rsync publication requires helper protocol v3",
            )),
        }
    })()
    .unwrap_or_else(|error| ExistingReply::Failed {
        message: Failure::from(error).message,
        outcome_unknown: recovery.uncertain,
        recovery: recovery.path,
    });
    write_frame(output, &reply)
}

enum RecurringTarget<'a> {
    Tree(Box<Target<'a>>),
    File(Box<super::existing_file::Target<'a>>),
    VacantTree(Box<VacantTarget<'a>>),
    VacantFile(Box<super::existing_file::VacantTarget<'a>>),
}

impl<'a> RecurringTarget<'a> {
    fn open(
        destination: &'a str,
        directory: bool,
        delete: &DeleteScope,
        reviewed: Option<RecurringReceiver>,
    ) -> io::Result<Self> {
        let (path, name) = absolute_file(destination)?;
        let parent = Directory::open_parent(path)?;
        let _ancestors = lock_receiver_ancestors(path, &parent)?;
        let vacant = match reviewed {
            Some(receiver) => matches!(receiver, RecurringReceiver::Vacant { .. }),
            None => absent(&parent.fd, name)?,
        };
        Ok(match (directory, vacant) {
            (true, false) => Self::Tree(Box::new(Target::open_at(destination, parent)?)),
            (false, false) => Self::File(Box::new(super::existing_file::Target::open_at(
                destination,
                parent,
            )?)),
            (true, true) => Self::VacantTree(Box::new(VacantTarget::open_at(
                destination,
                parent,
                delete,
            )?)),
            (false, true) => Self::VacantFile(Box::new(
                super::existing_file::VacantTarget::open_at(destination, parent)?,
            )),
        })
    }

    fn review(&self) -> io::Result<RecurringReceiver> {
        Ok(match self {
            Self::Tree(target) => RecurringReceiver::Existing {
                snapshot: target.review(),
            },
            Self::File(target) => RecurringReceiver::Existing {
                snapshot: target.review(),
            },
            Self::VacantTree(target) => RecurringReceiver::Vacant {
                fingerprint: target.fingerprint()?,
            },
            Self::VacantFile(target) => RecurringReceiver::Vacant {
                fingerprint: target.fingerprint(),
            },
        })
    }
}

fn require_receipt_space(destination: &str) -> io::Result<()> {
    let (parent, _) = absolute_file(destination)?;
    // Reserve space for every byte to expand to three digits when serialized.
    let snapshot = ExistingTree {
        inventory: Inventory {
            entries: MAX_ENTRIES,
            files: MAX_ENTRIES,
            bytes: MAX_BYTES,
            digest: [255; 32],
        },
        fingerprint: [255; 32],
    };
    for reply in [
        ExistingReply::FirstRecurringPublished {
            destination: destination.to_owned(),
            vacancy: [255; 32],
            receiver: snapshot,
        },
        ExistingReply::PublishedWithReceiver {
            destination: destination.to_owned(),
            receiver: snapshot,
            recovery: format!(
                "{}/.kortty-{}.kortty-part/{ORIGINAL}",
                parent.trim_end_matches('/'),
                "0".repeat(32)
            ),
        },
    ] {
        write_frame(io::sink(), &reply).map_err(|_| {
            invalid("The recurring rsync destination is too long for a publication receipt")
        })?;
    }
    Ok(())
}

struct Exchange<'a, R, W> {
    input: &'a mut R,
    output: &'a mut W,
    recovery: &'a mut Recovery,
}

impl<R: Read, W: io::Write> Exchange<'_, R, W> {
    fn begin(&mut self, stage: &UploadStage<'_>) -> io::Result<bool> {
        write_frame(
            &mut self.output,
            &ExistingReply::Prepared {
                stage: stage.name.clone(),
            },
        )?;
        match read_frame(&mut self.input)? {
            ExistingDecision::Verify => Ok(true),
            ExistingDecision::Abort => Ok(false),
            _ => Err(invalid("The rsync successor has not been verified")),
        }
    }

    fn verified(&mut self, successor: Inventory) -> io::Result<ExistingDecision> {
        write_frame(
            &mut self.output,
            &ExistingReply::VerifiedWithRemovals {
                successor,
                removed: 0,
            },
        )?;
        read_frame(&mut self.input)
    }
}

fn transact_first<C>(
    parent: &Directory,
    destination: &str,
    vacancy: [u8; 32],
    incoming: Inventory,
    mut exchange: Exchange<'_, impl Read, impl io::Write>,
    prepare: impl FnOnce(&UploadStage<'_>) -> io::Result<C>,
    publish: impl FnOnce(&mut UploadStage<'_>, &C, &mut bool) -> io::Result<ExistingTree>,
) -> io::Result<ExistingReply> {
    let mut stage = UploadStage::create_checked(parent)?;
    if !exchange.begin(&stage)? {
        stage.discard()?;
        return Ok(ExistingReply::Aborted);
    }
    let candidate = prepare(&stage)?;
    match exchange.verified(incoming)? {
        ExistingDecision::Publish => {
            let receiver = publish(&mut stage, &candidate, &mut exchange.recovery.uncertain)?;
            stage.discard()?;
            Ok(ExistingReply::FirstRecurringPublished {
                destination: destination.to_owned(),
                vacancy,
                receiver,
            })
        }
        ExistingDecision::Abort => {
            stage.discard()?;
            Ok(ExistingReply::Aborted)
        }
        _ => Err(invalid("Unexpected rsync first-publication decision")),
    }
}

fn transact_file(
    target: &super::existing_file::Target<'_>,
    incoming: Inventory,
    mut exchange: Exchange<'_, impl Read, impl io::Write>,
) -> io::Result<ExistingReply> {
    let mut stage = UploadStage::create_checked(target.parent())?;
    if !exchange.begin(&stage)? {
        stage.discard()?;
        return Ok(ExistingReply::Aborted);
    }
    let candidate = target.prepare_successor(&stage, incoming)?;
    match exchange.verified(incoming)? {
        ExistingDecision::Quarantine => {
            let published = target.commit_checked(
                &mut stage,
                &candidate,
                true,
                exchange.recovery,
                || Ok(()),
                |path| confirm_publication(exchange.input, exchange.output, path),
            )?;
            if let Some(Published { recovery, receiver }) = published {
                return Ok(ExistingReply::PublishedWithReceiver {
                    destination: target.destination().to_owned(),
                    receiver: receiver.ok_or_else(changed)?,
                    recovery,
                });
            }
            stage.discard()?;
            Ok(ExistingReply::Aborted)
        }
        ExistingDecision::Abort => {
            stage.discard()?;
            Ok(ExistingReply::Aborted)
        }
        _ => Err(invalid("Unexpected rsync existing-file decision")),
    }
}

fn confirm_publication(
    input: impl Read,
    output: impl io::Write,
    recovery: &str,
) -> io::Result<bool> {
    write_frame(
        output,
        &ExistingReply::Quarantined {
            recovery: recovery.to_owned(),
        },
    )?;
    match read_frame(input)? {
        ExistingDecision::Publish => Ok(true),
        ExistingDecision::Abort => Ok(false),
        _ => Err(invalid("Unexpected rsync publication decision")),
    }
}

fn transact(
    target: Target<'_>,
    incoming: Inventory,
    delete: &DeleteScope,
    mut input: impl Read,
    mut output: impl io::Write,
    recovery: &mut Recovery,
    version: Version,
) -> io::Result<ExistingReply> {
    let mut received = UploadStage::create_checked(&target.parent)?;
    write_frame(
        &mut output,
        &ExistingReply::Prepared {
            stage: received.name.clone(),
        },
    )?;
    match read_frame(&mut input)? {
        ExistingDecision::Verify => {}
        ExistingDecision::Abort => {
            received.discard()?;
            return Ok(ExistingReply::Aborted);
        }
        _ => return Err(invalid("The rsync successor has not been verified")),
    }
    target.verify(false)?;
    received.verify(target.path)?;
    let mut checked = target.prepare_successor(&received, incoming, delete)?;
    received.discard()?;
    let successor = checked.inventory;
    let receipt = version == Version::V3;
    let verified = if receipt {
        ExistingReply::VerifiedWithRemovals {
            successor,
            removed: checked.removed,
        }
    } else {
        ExistingReply::Verified { successor }
    };
    write_frame(&mut output, &verified)?;
    match read_frame(&mut input)? {
        ExistingDecision::Quarantine => {
            let published = target.commit(
                &mut checked.stage,
                &checked.witness,
                receipt.then_some(successor),
                recovery,
                || Ok(()),
                |path| confirm_publication(input, &mut output, path),
            )?;
            let Some(Published { recovery, receiver }) = published else {
                checked.stage.discard()?;
                return Ok(ExistingReply::Aborted);
            };
            let destination = target.destination.to_owned();
            Ok(match receiver {
                Some(receiver) => ExistingReply::PublishedWithReceiver {
                    destination,
                    receiver,
                    recovery,
                },
                None => ExistingReply::Published {
                    destination,
                    successor,
                    recovery,
                },
            })
        }
        ExistingDecision::Abort => {
            checked.stage.discard()?;
            Ok(ExistingReply::Aborted)
        }
        _ => Err(invalid("Unexpected rsync existing-tree decision")),
    }
}

pub(in crate::server) struct Published {
    pub(in crate::server) recovery: String,
    pub(in crate::server) receiver: Option<ExistingTree>,
}

pub(in crate::server) struct Successor<'a> {
    stage: UploadStage<'a>,
    witness: Witness,
    pub(in crate::server) inventory: Inventory,
    pub(in crate::server) removed: u32,
}

pub(in crate::server) struct Target<'a> {
    destination: &'a str,
    path: &'a str,
    name: &'a str,
    parent: Directory,
    directory: Directory,
    scan: Scan,
    parent_attributes: Option<[u8; 32]>,
    witness: Witness,
    review: ExistingTree,
}

pub(in crate::server) struct VacantTarget<'a> {
    slot: super::existing_file::VacantTarget<'a>,
    delete: DeleteScope,
    scan: Scan,
    parent_attributes: Option<[u8; 32]>,
}

pub(in crate::server) struct FirstTree {
    witness: Witness,
    inventory: Inventory,
}

impl<'a> VacantTarget<'a> {
    pub(in crate::server) fn open_at(
        destination: &'a str,
        parent: Directory,
        delete: &DeleteScope,
    ) -> io::Result<Self> {
        if !delete.valid() {
            return Err(changed());
        }
        let slot = super::existing_file::VacantTarget::open_at(destination, parent)?;
        let scan = Scan::Existing { mount: slot.mount };
        scan.require_entry(&slot.parent().fd, 0o700)?;
        let parent_attributes = scan.directory_attributes(&slot.parent().fd)?;
        slot.require_vacant()?;
        Ok(Self {
            slot,
            delete: delete.clone(),
            scan,
            parent_attributes,
        })
    }

    pub(in crate::server) fn parent(&self) -> &Directory {
        self.slot.parent()
    }

    pub(in crate::server) fn fingerprint(&self) -> io::Result<[u8; 32]> {
        let mut hash = Sha256::new_with_prefix(
            self.slot
                .fingerprint_with_domain(b"kortty-rsync-first-recurring-tree-v1\0"),
        );
        hash.update(serde_json::to_vec(&self.delete).map_err(io::Error::other)?);
        Ok(hash.finalize().into())
    }

    pub(in crate::server) fn require_vacant(&self) -> io::Result<()> {
        self.slot.require_vacant()?;
        if self.scan.directory_attributes(&self.parent().fd)? != self.parent_attributes {
            return Err(changed());
        }
        Ok(())
    }

    pub(in crate::server) fn prepare(
        &self,
        stage: &UploadStage<'_>,
        expected: Inventory,
    ) -> io::Result<FirstTree> {
        self.prepare_with(stage, expected, Manifest::default)
    }

    pub(in crate::server) fn prepare_transfer(
        &self,
        stage: &UploadStage<'_>,
        expected: Inventory,
    ) -> io::Result<FirstTree> {
        self.prepare_with(stage, expected, || {
            Manifest::bounded_transfer(expected.entries, expected.bytes)
        })
    }

    fn prepare_with(
        &self,
        stage: &UploadStage<'_>,
        expected: Inventory,
        manifest: impl Fn() -> Manifest,
    ) -> io::Result<FirstTree> {
        self.require_vacant()?;
        self.slot.verify_stage(stage)?;
        let received = stage.verify_received_with(expected, manifest())?;
        require_delete_roots(&self.delete, &received)?;
        // Recheck the same private tree with the metadata rules of its future receiver.
        let mut manifest = manifest();
        let witness = scan(&stage.payload, "", 0, &mut manifest, self.scan)?;
        stage.require_received(&received)?;
        if manifest.finish() != expected {
            return Err(changed());
        }
        fsync(&stage.payload.fd)?;
        fsync(&stage.directory.fd)?;
        Ok(FirstTree {
            witness,
            inventory: expected,
        })
    }

    pub(in crate::server) fn publish(
        &self,
        stage: &mut UploadStage<'_>,
        candidate: &FirstTree,
        outcome_unknown: &mut bool,
        permit: impl Fn() -> io::Result<()>,
    ) -> io::Result<ExistingTree> {
        let _ancestors = lock_receiver_ancestors(self.slot.path, self.parent())?;
        flock(&stage.payload.fd, FlockOperation::NonBlockingLockExclusive)?;
        self.slot.verify_stage(stage)?;
        candidate
            .witness
            .require_tree(&stage.payload, self.scan, false)?;
        let result = stage.publish(self.slot.path, self.slot.name, &candidate.witness, || {
            permit()?;
            self.require_vacant()
        });
        *outcome_unknown = stage.published;
        result?;

        let root = Stamp::of(&stage.payload.fd.metadata()?);
        self.parent()
            .require_child(self.slot.name, &stage.payload.stat)?;
        candidate
            .witness
            .require_tree(&stage.payload, self.scan, true)?;
        permit()?;
        self.slot.verify_parent()?;
        let proof = tree_proof(
            self.slot.destination,
            self.parent(),
            self.parent_attributes,
            candidate.inventory,
            &candidate.witness,
            &root,
        )?;
        self.parent()
            .require_child(self.slot.name, &stage.payload.stat)?;
        if !root.matches(&stage.payload.fd, false)? {
            return Err(changed());
        }
        Ok(proof)
    }
}

impl<'a> Target<'a> {
    fn open(destination: &'a str) -> io::Result<Self> {
        let (path, _) = absolute_file(destination)?;
        let parent = Directory::open_parent(path)?;
        Self::open_at(destination, parent)
    }

    pub(in crate::server) fn open_at(destination: &'a str, parent: Directory) -> io::Result<Self> {
        let (path, name) = absolute_file(destination)?;
        if parent.stat.st_uid != geteuid().as_raw() {
            return Err(invalid(
                "Choose an rsync destination parent owned by the login",
            ));
        }
        let scan_mode = Scan::Existing {
            mount: mount_id(&parent.fd)?,
        };
        scan_mode.require_entry(&parent.fd, 0o700)?;
        let parent_attributes = scan_mode.directory_attributes(&parent.fd)?;
        let directory = parent.child(name)?;
        let mut manifest = Manifest::default();
        let witness = scan(&directory, "", 0, &mut manifest, scan_mode)?;
        let inventory = manifest.finish();
        let review = tree_proof(
            destination,
            &parent,
            parent_attributes,
            inventory,
            &witness,
            &witness.stamp,
        )?;
        let target = Self {
            destination,
            path,
            name,
            parent,
            directory,
            scan: scan_mode,
            parent_attributes,
            witness,
            review,
        };
        target.verify(false)?;
        Ok(target)
    }

    pub(in crate::server) fn review(&self) -> ExistingTree {
        self.review
    }

    pub(in crate::server) fn parent(&self) -> &Directory {
        &self.parent
    }

    pub(in crate::server) fn destination(&self) -> &str {
        self.destination
    }

    pub(in crate::server) fn prepare_successor<'target>(
        &'target self,
        received: &UploadStage<'_>,
        incoming: Inventory,
        delete: &DeleteScope,
    ) -> io::Result<Successor<'target>> {
        let received_witness = received.verify_received(incoming)?;
        require_delete_roots(delete, &received_witness)?;
        let stage = UploadStage::create_checked(&self.parent)?;
        let mut manifest = Manifest::default();
        compose(
            &stage.payload,
            Some(Tree {
                directory: &self.directory,
                witness: &self.witness,
            }),
            Some(Tree {
                directory: &received.payload,
                witness: &received_witness,
            }),
            delete,
            "",
            &mut manifest,
        )?;
        let inventory = manifest.finish();
        let mut checked = Manifest::default();
        let witness = scan(&stage.payload, "", 0, &mut checked, self.scan)?;
        if checked.finish() != inventory {
            return Err(invalid(
                "The prepared rsync successor differs from the reviewed inputs",
            ));
        }
        self.verify(false)?;
        received.require_received(&received_witness)?;
        let removed = removed_entries(&self.witness, &witness);
        Ok(Successor {
            stage,
            witness,
            inventory,
            removed,
        })
    }

    pub(in crate::server) fn commit_checked(
        &self,
        successor: &mut Successor<'_>,
        recurring: bool,
        recovery: &mut Recovery,
        permit: impl Fn() -> io::Result<()>,
        confirm: impl FnOnce(&str) -> io::Result<bool>,
    ) -> io::Result<Option<Published>> {
        self.commit(
            &mut successor.stage,
            &successor.witness,
            recurring.then_some(successor.inventory),
            recovery,
            permit,
            confirm,
        )
    }

    fn verify_parent(&self) -> io::Result<()> {
        let now = Directory::open_parent(self.path)?;
        if !same_node(&now.stat, &self.parent.stat)
            || now.stat.st_mode != self.parent.stat.st_mode
            || now.stat.st_uid != self.parent.stat.st_uid
            || now.stat.st_gid != self.parent.stat.st_gid
        {
            return Err(changed());
        }
        self.scan.require_entry(&now.fd, 0o700)?;
        if self.scan.directory_attributes(&now.fd)? != self.parent_attributes {
            return Err(changed());
        }
        Ok(())
    }

    fn verify(&self, moved: bool) -> io::Result<()> {
        self.verify_parent()?;
        if !moved {
            self.parent.require_child(self.name, &self.directory.stat)?;
        }
        self.witness.require_tree(&self.directory, self.scan, moved)
    }

    fn commit(
        &self,
        stage: &mut UploadStage<'_>,
        checked: &Witness,
        receipt_inventory: Option<Inventory>,
        recovery: &mut Recovery,
        permit: impl Fn() -> io::Result<()>,
        confirm: impl FnOnce(&str) -> io::Result<bool>,
    ) -> io::Result<Option<Published>> {
        let _ancestors = lock_receiver_ancestors(self.path, &self.parent)?;
        match flock(&self.directory.fd, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(Errno::WOULDBLOCK) => {
                return Err(invalid(
                    "Another rsync publication holds the reviewed receiver",
                ));
            }
            Err(error) => return Err(error.into()),
        }
        // The candidate root can inherit the receiver's permissions during composition.
        self.parent
            .require_child(&stage.name, &stage.directory.stat)?;
        stage
            .directory
            .require_child(PAYLOAD, &stage.payload.stat)?;
        require_private(&stage.directory.fd, 0o700)?;
        checked.require_tree(&stage.payload, self.scan, false)?;
        if receipt_inventory.is_some() {
            flock(&stage.payload.fd, FlockOperation::NonBlockingLockExclusive)?;
        }
        self.verify(false)?;
        let recovery_path = format!(
            "{}/{}/{ORIGINAL}",
            self.path.trim_end_matches('/'),
            stage.name
        );
        permit()?;
        if let Err(error) = renameat_with(
            &self.parent.fd,
            self.name,
            &stage.directory.fd,
            ORIGINAL,
            RenameFlags::NOREPLACE,
        ) {
            // A remote filesystem can report a rename failure after it took effect.
            if !absent(&stage.directory.fd, ORIGINAL).unwrap_or(false) {
                stage.owned = false;
                recovery.uncertain = true;
                recovery.path = self.checked_recovery(stage, &recovery_path);
            }
            return Err(error.into());
        }
        // From this point, dropping the helper must leave the displaced directory in place.
        stage.owned = false;
        recovery.uncertain = true;
        recovery.path = self.checked_recovery(stage, &recovery_path);
        let displaced = statat(&stage.directory.fd, ORIGINAL, AtFlags::SYMLINK_NOFOLLOW)?;
        let publication = (|| -> io::Result<Option<Published>> {
            if !same_node(&displaced, &self.directory.stat) {
                return Err(changed());
            }
            self.verify(true)?;
            fsync(&stage.directory.fd)?;
            fsync(&self.parent.fd)?;
            checked.require_tree(&stage.payload, self.scan, false)?;
            let current_recovery = self
                .checked_recovery(stage, &recovery_path)
                .ok_or_else(changed)?;
            if !confirm(&current_recovery)? {
                return Ok(None);
            }
            self.parent
                .require_child(&stage.name, &stage.directory.stat)?;
            stage
                .directory
                .require_child(PAYLOAD, &stage.payload.stat)?;
            stage
                .directory
                .require_child(ORIGINAL, &self.directory.stat)?;
            self.verify(true)?;
            checked.require_tree(&stage.payload, self.scan, false)?;
            self.verify_parent()?;
            permit()?;
            renameat_with(
                &stage.directory.fd,
                PAYLOAD,
                &self.parent.fd,
                self.name,
                RenameFlags::NOREPLACE,
            )?;
            stage.published = true;
            // Renaming updates the held root ctime. Descendants must stay exact.
            let renamed_root = receipt_inventory
                .map(|_| stage.payload.fd.metadata().map(|value| Stamp::of(&value)))
                .transpose()?;
            self.parent.require_child(self.name, &stage.payload.stat)?;
            stage
                .directory
                .require_child(ORIGINAL, &self.directory.stat)?;
            fsync(&stage.directory.fd)?;
            fsync(&self.parent.fd)?;
            self.verify_parent()?;
            if receipt_inventory.is_some() {
                permit()?;
            }
            checked.require_tree(&stage.payload, self.scan, true)?;
            let receiver = if let Some((inventory, root)) = receipt_inventory.zip(renamed_root) {
                let proof = tree_proof(
                    self.destination,
                    &self.parent,
                    self.parent_attributes,
                    inventory,
                    checked,
                    &root,
                )?;
                self.parent.require_child(self.name, &stage.payload.stat)?;
                if !root.matches(&stage.payload.fd, false)? {
                    return Err(changed());
                }
                self.verify_parent()?;
                Some(proof)
            } else {
                None
            };
            Ok(Some(Published {
                recovery: self
                    .checked_recovery(stage, &recovery_path)
                    .ok_or_else(changed)?,
                receiver,
            }))
        })();
        if matches!(publication, Ok(Some(_))) {
            return publication;
        }
        let restored = if stage.published {
            Err(invalid(
                "The rsync successor may already have been published",
            ))
        } else {
            self.restore(stage, &displaced).inspect(|()| {
                stage.owned = true;
                *recovery = Recovery::default();
            })
        };
        if recovery.uncertain {
            recovery.path = self.checked_recovery(stage, &recovery_path);
        }
        publication?;
        restored?;
        Ok(None)
    }

    fn checked_recovery(&self, stage: &UploadStage<'_>, path: &str) -> Option<String> {
        (self.verify_parent().is_ok()
            && self
                .parent
                .require_child(&stage.name, &stage.directory.stat)
                .is_ok()
            && stage
                .directory
                .require_child(ORIGINAL, &self.directory.stat)
                .is_ok())
        .then(|| path.to_owned())
    }

    fn restore(&self, stage: &UploadStage<'_>, displaced: &Stat) -> io::Result<()> {
        self.verify_parent()?;
        self.parent
            .require_child(&stage.name, &stage.directory.stat)?;
        // Return the exact displaced entry, including any intervening edits.
        stage.directory.require_child(ORIGINAL, displaced)?;
        renameat_with(
            &stage.directory.fd,
            ORIGINAL,
            &self.parent.fd,
            self.name,
            RenameFlags::NOREPLACE,
        )?;
        self.parent.require_child(self.name, displaced)?;
        fsync(&stage.directory.fd)?;
        fsync(&self.parent.fd)?;
        self.verify_parent()
    }
}

fn require_delete_roots(delete: &DeleteScope, received: &Witness) -> io::Result<()> {
    if delete.valid_for(|name| {
        received
            .children
            .iter()
            .any(|(entry, node)| entry == name && matches!(node, Node::Directory(_)))
    }) {
        Ok(())
    } else {
        Err(invalid(
            "A named rsync deletion root is missing from the checked incoming directories",
        ))
    }
}

fn tree_proof(
    destination: &str,
    parent: &Directory,
    parent_attributes: Option<[u8; 32]>,
    inventory: Inventory,
    witness: &Witness,
    root: &Stamp,
) -> io::Result<ExistingTree> {
    let mut fingerprint = Sha256::new_with_prefix(b"kortty-rsync-existing-tree-v1\0");
    fingerprint.update((destination.len() as u32).to_be_bytes());
    fingerprint.update(destination.as_bytes());
    let parent_metadata = parent.fd.metadata()?;
    fingerprint.update(parent_metadata.dev().to_be_bytes());
    fingerprint.update(parent_metadata.ino().to_be_bytes());
    fingerprint.update(parent_metadata.mode().to_be_bytes());
    fingerprint.update(parent_metadata.gid().to_be_bytes());
    fingerprint.update(mount_id(&parent.fd)?.to_be_bytes());
    if let Some(attributes) = parent_attributes {
        fingerprint.update(attributes);
    }
    fingerprint.update(inventory.digest);
    fingerprint_tree(witness, root, &mut fingerprint);
    Ok(ExistingTree {
        inventory,
        fingerprint: fingerprint.finalize().into(),
    })
}

fn fingerprint_tree(tree: &Witness, root: &Stamp, fingerprint: &mut Sha256) {
    root.hash_into(fingerprint);
    if let Some(attributes) = tree.attributes {
        fingerprint.update(attributes);
    }
    for (name, node) in &tree.children {
        fingerprint.update((name.len() as u32).to_be_bytes());
        fingerprint.update(name.as_bytes());
        match node {
            Node::Directory(child) => {
                fingerprint.update([0]);
                fingerprint_tree(child, &child.stamp, fingerprint);
            }
            Node::File { stamp, .. } => {
                fingerprint.update([1]);
                stamp.hash_into(fingerprint);
            }
        }
    }
    fingerprint.update(b"\0");
}

fn removed_entries(original: &Witness, successor: &Witness) -> u32 {
    original
        .children
        .iter()
        .map(|(name, node)| {
            let successor = successor
                .children
                .binary_search_by(|(candidate, _)| candidate.cmp(name))
                .ok()
                .map(|index| &successor.children[index].1);
            match (node, successor) {
                (Node::Directory(before), Some(Node::Directory(after))) => {
                    removed_entries(before, after)
                }
                (Node::File { .. }, Some(Node::File { .. })) => 0,
                _ => entry_count(node),
            }
        })
        .sum()
}

fn entry_count(node: &Node) -> u32 {
    1 + match node {
        Node::Directory(directory) => directory
            .children
            .iter()
            .map(|(_, child)| entry_count(child))
            .sum(),
        Node::File { .. } => 0,
    }
}

#[derive(Clone, Copy)]
struct Tree<'a> {
    directory: &'a Directory,
    witness: &'a Witness,
}

fn compose(
    successor: &Directory,
    original: Option<Tree<'_>>,
    incoming: Option<Tree<'_>>,
    delete: &DeleteScope,
    prefix: &str,
    manifest: &mut Manifest,
) -> io::Result<()> {
    let mut entries = BTreeMap::<&str, [Option<(&Directory, &Node)>; 2]>::new();
    for (index, tree) in [original, incoming].into_iter().enumerate() {
        if let Some(tree) = tree {
            for (name, node) in &tree.witness.children {
                entries.entry(name).or_default()[index] = Some((tree.directory, node));
            }
        }
    }
    for (name, [original, incoming]) in entries {
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        let Some((parent, node)) = incoming.or(original.filter(|_| !delete.allows(&path))) else {
            continue;
        };
        match node {
            Node::Directory(_) => {
                let original = open_subtree(original, name)?;
                let incoming = open_subtree(incoming, name)?;
                manifest.add(&path, None)?;
                mkdirat(&successor.fd, name, PRIVATE_DIRECTORY)?;
                let result = successor.child(name)?;
                compose(
                    &result,
                    original
                        .as_ref()
                        .map(|(directory, witness)| Tree { directory, witness }),
                    incoming
                        .as_ref()
                        .map(|(directory, witness)| Tree { directory, witness }),
                    delete,
                    &path,
                    manifest,
                )?;
            }
            Node::File { stamp, digest } => {
                if !delete.replaces_nonempty()
                    && matches!(original, Some((_, Node::Directory(child))) if !child.children.is_empty())
                {
                    return Err(invalid(
                        "A source file would remove a nonempty destination directory",
                    ));
                }
                manifest.add(&path, Some((stamp.size, *digest)))?;
                let data = open_file(parent, name, stamp)?;
                let attributes = match original {
                    Some((parent, Node::File { stamp, .. })) => {
                        Some(open_file(parent, name, stamp)?)
                    }
                    _ => None,
                };
                let mut output = File::from(openat(
                    &successor.fd,
                    name,
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    PRIVATE_FILE,
                )?);
                if io::copy(&mut (&data).take(stamp.size + 1), &mut output)? != stamp.size
                    || !stamp.matches(&data, false)?
                {
                    return Err(changed());
                }
                copy_metadata(attributes.as_ref().unwrap_or(&data), &output)?;
            }
        }
    }
    let metadata = original
        .or(incoming)
        .expect("one tree supplies the directory");
    copy_metadata(&metadata.directory.fd, &successor.fd)
}

fn open_subtree<'a>(
    entry: Option<(&Directory, &'a Node)>,
    name: &str,
) -> io::Result<Option<(Directory, &'a Witness)>> {
    let Some((parent, Node::Directory(witness))) = entry else {
        return Ok(None);
    };
    let directory = parent.child(name)?;
    if !witness.stamp.matches(&directory.fd, false)? {
        return Err(changed());
    }
    Ok(Some((directory, witness)))
}

fn open_file(parent: &Directory, name: &str, stamp: &Stamp) -> io::Result<File> {
    let file = File::from(openat(&parent.fd, name, SOURCE, Mode::empty())?);
    if !stamp.matches(&file, false)? {
        return Err(changed());
    }
    Ok(file)
}

pub(super) fn copy_metadata(source: &File, target: &File) -> io::Result<()> {
    let expected = source.metadata()?;
    if target.metadata()?.gid() != expected.gid() {
        fchown(target, None, Some(Gid::from_raw(expected.gid())))?;
    }
    let attributes = read_xattrs(source)?;
    if attributes.get("security.selinux") != read_xattrs(target)?.get("security.selinux") {
        return Err(invalid(
            "The rsync stage cannot preserve the server security label",
        ));
    }
    for (name, value) in attributes
        .iter()
        .filter(|(name, _)| name.starts_with("user."))
    {
        fsetxattr(target, name.as_str(), value, XattrFlags::empty())?;
    }
    fchmod(target, Mode::from_bits_truncate(expected.mode() & 0o777))?;
    if read_xattrs(target)? != attributes {
        return Err(invalid(
            "The rsync stage did not retain the reviewed attributes",
        ));
    }
    target.set_times(
        FileTimes::new()
            .set_modified(expected.modified()?)
            .set_accessed(expected.accessed()?),
    )?;
    fsync(target)?;
    Ok(())
}

fn changed() -> io::Error {
    invalid("The reviewed rsync receiver or incoming tree changed")
}
