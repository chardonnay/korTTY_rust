//! Durable new-target uploads using a private same-directory stage.

use std::cell::Cell;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{Seek, SeekFrom};

use kortty_ssh::sftp::UploadStage;
use pgp::crypto::hash::HashAlgorithm;

use crate::sftp_window::LocalSource;

use super::persist::{Lease, Phase, Store, UploadRecord, remote_components};
use super::*;

#[derive(Clone)]
pub(super) struct Saved {
    pub(super) record: UploadRecord,
    profile: PathBuf,
}

pub(in crate::sftp_window::worker) fn upload(
    client: &mut Client,
    transfer: &Transfer,
    source: &Path,
    target: &[u8],
    progress: &Progress,
    control: &Control,
) -> Result<u64> {
    if !control.resume_preferences().enabled
        && let Some((parent, _)) = remote_components(target)
    {
        return upload_without_receipt(client, source, target, parent, progress, control);
    }
    let Some(snapshot) = durable::snapshot(control) else {
        return super::super::upload(client, source, target, None, progress);
    };
    let Some((parent, _)) = remote_components(target) else {
        return super::super::upload(client, source, target, None, progress);
    };
    let local = LocalSource::open(source)?;
    if local.size() > MAX_PARTIAL_BYTES {
        return super::super::upload(client, source, target, None, progress);
    }
    let allowed = || durable::matches(control, &snapshot);
    let cancelled = || !allowed() || progress.stopped();
    let context = &snapshot.1;
    if client.canonicalize(parent)? != parent {
        return Err(invalid("The upload destination folder changed").into());
    }
    require_absent(client, target)?;
    let fingerprint = local.upload_source(&cancelled)?;
    let mut record = UploadRecord {
        id: crate::private_files::nonce()?,
        route: context.route.clone(),
        host_key: context.host_key.to_openssh()?,
        remote: target.to_vec(),
        source: fingerprint,
        uid: 0,
        gid: 0,
        title: transfer.title.clone(),
        phase: Phase::Writing,
    };
    let store = Store::new(&context.profile);
    let mut lease = store.lease_upload(&record.id, false)?;
    let stage = record.stage()?;
    let mut saved = false;
    let mut observed = false;
    let offset = Cell::new(0);
    let writing = Cell::new(false);
    let result = (|| -> Result<u64> {
        let count = client.upload_private_verified(
            &stage,
            UploadStage::Create,
            record.source.size,
            Stream {
                file: local.file(),
                progress,
                writing: &writing,
            },
            |owner, position| {
                record.uid = owner.uid;
                record.gid = owner.gid;
                observed = true;
                local.verify()?;
                // A directory sync failure can follow a successful index replacement.
                // Leave the lease intact until the index can establish its exact state.
                lease.retain(true);
                store.save_upload(record.clone(), &|| {
                    allowed() && control.resume_preferences().enabled
                })?;
                saved = true;
                offset.set(position);
                progress.set_position(position)?;
                writing.set(true);
                Ok(())
            },
            || !allowed() || writing.get() && progress.stopped(),
        )?;
        publish(client, &mut record, &local, &store, &allowed, progress)?;
        lease.retain(false);
        Ok(count)
    })();
    result.map_err(|error| {
        if !saved && let Ok(receipts) = store.uploads() {
            saved = receipts.contains(&record);
            if !saved
                && observed
                && !error.disconnected
                && allowed()
                && client
                    .verify_private_upload(&stage, record.owner(), 0, local.file(), || !allowed())
                    .is_ok()
                && allowed()
                && client.remove_file(&stage).is_ok()
            {
                lease.retain(false);
                return error.context(" · Empty upload stage removed");
            }
        }
        if saved {
            let mut interrupted = InterruptedUpload {
                record,
                store: &store,
                lease: &mut lease,
                control,
                snapshot: &snapshot,
            };
            interrupted.finish(client, &local, progress, error, offset.get())
        } else {
            failure(error, record, context, offset.get(), false)
        }
    })
}

fn upload_without_receipt(
    client: &mut Client,
    source: &Path,
    target: &[u8],
    parent: &[u8],
    progress: &Progress,
    control: &Control,
) -> Result<u64> {
    progress.check_cancelled()?;
    let local = LocalSource::open(source)?;
    if client.canonicalize(parent)? != parent {
        return Err(invalid("The upload destination folder changed").into());
    }
    require_absent(client, target)?;
    let stage = files::remote_join(parent, files::nonce()?.as_bytes());
    let mut owner = None;
    let writing = Cell::new(false);
    let result = (|| -> Result<u64> {
        let count = client.upload_private_verified(
            &stage,
            UploadStage::Create,
            local.size(),
            Stream {
                file: local.file(),
                progress,
                writing: &writing,
            },
            |identity, position| {
                owner = Some(identity);
                local.verify()?;
                progress.set_position(position)?;
                writing.set(true);
                Ok(())
            },
            || control.is_closed() || writing.get() && progress.stopped(),
        )?;
        local.verify()?;
        if client.canonicalize(parent)? != parent {
            return Err(invalid("The upload destination folder changed").into());
        }
        progress.check_cancelled()?;
        client.rename(&stage, target)?;
        Ok(count)
    })();
    result.map_err(|mut error| {
        if !error.disconnected
            && !control.is_closed()
            && control.transfer_allowed(&ClipboardPolicy::load())
            && let Some(owner) = owner
        {
            let cleanup = (|| -> Result<()> {
                if client.canonicalize(parent)? != parent {
                    return Err(invalid("The upload destination folder changed").into());
                }
                local.verify()?;
                client.verify_private_upload(&stage, owner, local.size(), local.file(), || {
                    control.is_closed()
                })?;
                local.verify()?;
                if client.canonicalize(parent)? != parent
                    || !control.transfer_allowed(&ClipboardPolicy::load())
                {
                    return Err(invalid("The private upload stage could not be verified").into());
                }
                client.remove_file(&stage)?;
                Ok(())
            })();
            match cleanup {
                Ok(()) => return error.context(" · Partial upload removed"),
                Err(cleanup) => {
                    error.disconnected |= cleanup.disconnected;
                    error = error.context(format!(" · Stage cleanup failed ({})", cleanup.message));
                }
            }
        }
        error.context(format!(
            " · staging path {}",
            files::display(&String::from_utf8_lossy(&stage))
        ))
    })
}

impl Saved {
    fn new(record: UploadRecord, context: &durable::Context) -> Self {
        Self {
            record,
            profile: context.profile.clone(),
        }
    }

    fn require_context(&self, snapshot: &(u64, durable::Context)) -> io::Result<()> {
        let context = &snapshot.1;
        if self.profile != context.profile
            || self.record.route != context.route
            || HostKey::from_openssh(&self.record.host_key)? != context.host_key
        {
            return Err(invalid(
                "The saved upload belongs to another authenticated route",
            ));
        }
        Ok(())
    }

    pub(super) fn run(
        mut self,
        client: &mut Client,
        progress: &Progress,
        control: &Control,
    ) -> Result<Reply> {
        let snapshot = durable::snapshot(control).ok_or_else(unavailable)?;
        self.require_context(&snapshot)?;
        let allowed = || durable::matches(control, &snapshot);
        let cancelled = || !allowed() || progress.stopped();
        if self.record.phase == Phase::Discard {
            return Err(invalid("The cancelled upload is pending cleanup").into());
        }
        let context = &snapshot.1;
        let store = Store::new(&context.profile);
        let mut lease = store.lease_upload(&self.record.id, true)?;
        require_receipt(&store, &self.record)?;
        require_parent(client, &self.record)?;
        let local = LocalSource::reopen_upload(&self.record.source, &|| !allowed())?;
        let stage = self.record.stage()?;
        let directory = Location::Remote(self.record.parent()?.to_vec());
        let offset = Cell::new(0);
        let writing = Cell::new(false);
        let result = (|| -> Result<u64> {
            if already_published(client, &self.record, &local, &allowed)? {
                store.remove_upload(&self.record.id, &allowed)?;
            } else {
                require_absent(client, &self.record.remote)?;
                client.upload_private_verified(
                    &stage,
                    UploadStage::Resume(self.record.owner()),
                    self.record.source.size,
                    Stream {
                        file: local.file(),
                        progress,
                        writing: &writing,
                    },
                    |_, position| {
                        local.verify()?;
                        offset.set(position);
                        progress.set_position(position)?;
                        writing.set(true);
                        Ok(())
                    },
                    cancelled,
                )?;
                publish(client, &mut self.record, &local, &store, &allowed, progress)?;
            }
            lease.retain(false);
            Ok(self.record.source.size)
        })();
        match result {
            Ok(count) => changed(
                client,
                &directory,
                format!("Resumed {} · {count} bytes", self.record.title),
            ),
            Err(error) => {
                let mut interrupted = InterruptedUpload {
                    record: self.record,
                    store: &store,
                    lease: &mut lease,
                    control,
                    snapshot: &snapshot,
                };
                Err(interrupted.finish(client, &local, progress, error, offset.get()))
            }
        }
    }

    pub(super) fn discard(&self, client: &mut Client, control: &Control) -> Result<()> {
        let snapshot = durable::snapshot(control).ok_or_else(unavailable)?;
        self.require_context(&snapshot)?;
        let allowed = || durable::matches(control, &snapshot);
        let store = Store::new(&snapshot.1.profile);
        let mut lease = store.lease_upload(&self.record.id, true)?;
        require_receipt(&store, &self.record)?;
        discard_record(client, &self.record, &store, None, &allowed)?;
        lease.retain(false);
        Ok(())
    }
}

struct InterruptedUpload<'a> {
    record: UploadRecord,
    store: &'a Store,
    lease: &'a mut Lease,
    control: &'a Control,
    snapshot: &'a (u64, durable::Context),
}

impl InterruptedUpload<'_> {
    fn finish(
        &mut self,
        client: &mut Client,
        local: &LocalSource,
        progress: &Progress,
        mut error: Failure,
        offset: u64,
    ) -> Failure {
        let retain = self
            .control
            .resume_preferences()
            .retention(progress.stopped());
        let policy = ClipboardPolicy::load();
        let context_current = || {
            durable::matches_context(self.control, self.snapshot)
                && self.control.transfer_allowed(&policy)
        };
        if retain {
            let saved = self.store.uploads().ok().and_then(|records| {
                records.into_iter().find(|record| {
                    *record
                        == UploadRecord {
                            phase: record.phase,
                            ..self.record.clone()
                        }
                })
            });
            let can_offer = saved.is_some() && context_current();
            return failure(
                error,
                saved.unwrap_or_else(|| self.record.clone()),
                &self.snapshot.1,
                offset,
                can_offer,
            );
        }
        let cleanup = || !self.control.is_closed() && context_current();
        if !error.disconnected && cleanup() {
            match discard_record(client, &self.record, self.store, Some(local), &cleanup) {
                Ok(()) => {
                    self.lease.retain(false);
                    return error.context(" · Partial upload removed");
                }
                Err(cleanup) => {
                    error.disconnected |= cleanup.disconnected;
                    error = error.context(format!(" · Stage cleanup failed ({})", cleanup.message));
                }
            }
        }
        // On a lost channel, defer removal until the same verified route can
        // inspect the private stage. A cancelled upload must not be resumed.
        self.record.phase = Phase::Discard;
        if let Err(mark) = self
            .store
            .save_upload(self.record.clone(), &context_current)
        {
            error = error.context(format!(" · Could not mark the stage for removal ({mark})"));
        }
        failure(error, self.record.clone(), &self.snapshot.1, offset, false)
    }
}

fn discard_record(
    client: &mut Client,
    record: &UploadRecord,
    store: &Store,
    local: Option<&LocalSource>,
    allowed: &impl Fn() -> bool,
) -> Result<()> {
    require_allowed(allowed)?;
    require_parent(client, record)?;
    let stage = record.stage()?;
    match client.lstat(&stage) {
        Ok(_) => {
            let reopened;
            let local = if let Some(local) = local {
                local
            } else {
                reopened = LocalSource::reopen_upload(&record.source, &|| !allowed())?;
                &reopened
            };
            local.verify()?;
            client.verify_private_upload(
                &stage,
                record.owner(),
                record.source.size,
                local.file(),
                || !allowed(),
            )?;
            local.verify()?;
            require_parent(client, record)?;
            require_allowed(allowed)?;
            client.remove_file(&stage)?;
        }
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    store.remove_upload(&record.id, allowed)?;
    Ok(())
}

pub(super) struct Found {
    pub(super) offer: Option<Offer>,
    pub(super) completed: usize,
    pub(super) discarded: usize,
    pub(super) skipped: usize,
}

pub(super) fn find(
    client: &mut Client,
    control: &Control,
    snapshot: &(u64, durable::Context),
) -> Result<Found> {
    let context = &snapshot.1;
    let allowed = || durable::matches(control, snapshot);
    let store = Store::new(&context.profile);
    let mut found = Found {
        offer: None,
        completed: 0,
        discarded: 0,
        skipped: 0,
    };
    for record in store.uploads()? {
        if record.route != context.route
            || HostKey::from_openssh(&record.host_key)? != context.host_key
        {
            continue;
        }
        require_allowed(&allowed)?;
        match inspect(client, &record, &store, &allowed) {
            Ok(Some(offset)) => {
                found.offer = Some(Offer::upload(
                    Saved::new(record, context),
                    &context.host_key,
                    offset,
                ));
                break;
            }
            Ok(None) if record.phase == Phase::Discard => found.discarded += 1,
            Ok(None) => found.completed += 1,
            Err(error) if error.disconnected => return Err(error),
            Err(_) => found.skipped += 1,
        }
    }
    Ok(found)
}

fn inspect(
    client: &mut Client,
    record: &UploadRecord,
    store: &Store,
    allowed: &impl Fn() -> bool,
) -> Result<Option<u64>> {
    let mut lease = store.lease_upload(&record.id, true)?;
    require_receipt(store, record)?;
    if record.phase == Phase::Discard {
        discard_record(client, record, store, None, allowed)?;
        lease.retain(false);
        return Ok(None);
    }
    require_parent(client, record)?;
    let local = LocalSource::reopen_upload(&record.source, &|| !allowed())?;
    if already_published(client, record, &local, allowed)? {
        store.remove_upload(&record.id, allowed)?;
        lease.retain(false);
        return Ok(None);
    }
    let offset = client.verify_private_upload(
        &record.stage()?,
        record.owner(),
        record.source.size,
        local.file(),
        || !allowed(),
    )?;
    local.verify()?;
    require_allowed(allowed)?;
    Ok(Some(offset))
}

fn publish(
    client: &mut Client,
    record: &mut UploadRecord,
    local: &LocalSource,
    store: &Store,
    allowed: &impl Fn() -> bool,
    progress: &Progress,
) -> Result<()> {
    let active = || allowed() && !progress.stopped();
    local.verify()?;
    require_parent(client, record)?;
    require_absent(client, &record.remote)?;
    record.phase = Phase::Ready;
    store.save_upload(record.clone(), &active)?;
    verify_complete(client, &record.stage()?, record, local, &active)?;
    require_allowed(&active)?;
    // Standard v3 rename rejects a newly occupied destination.
    client.rename(record.stage()?, &record.remote)?;
    store.remove_upload(&record.id, allowed)?;
    Ok(())
}

fn already_published(
    client: &mut Client,
    record: &UploadRecord,
    local: &LocalSource,
    allowed: &impl Fn() -> bool,
) -> Result<bool> {
    if record.phase != Phase::Ready {
        return Ok(false);
    }
    match client.lstat(&record.stage()?) {
        Ok(_) => Ok(false),
        Err(error) if error.is_not_found() => {
            verify_complete(client, &record.remote, record, local, allowed)?;
            Ok(true)
        }
        Err(error) => Err(error.into()),
    }
}

fn verify_complete(
    client: &mut Client,
    path: &[u8],
    record: &UploadRecord,
    local: &LocalSource,
    allowed: &impl Fn() -> bool,
) -> Result<()> {
    local.verify()?;
    let mut digest = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    let size = client.verify_private_upload(
        path,
        record.owner(),
        record.source.size,
        Observed {
            source: local.file(),
            observe: |data: &[u8]| digest.update(data),
        },
        || !allowed(),
    )?;
    local.verify()?;
    let mut compared = String::with_capacity(64);
    for byte in digest.finalize() {
        let _ = write!(compared, "{byte:02x}");
    }
    if size != record.source.size || compared != record.source.sha256 {
        return Err(invalid("The complete upload could not be verified").into());
    }
    Ok(())
}

fn require_parent(client: &mut Client, record: &UploadRecord) -> Result<()> {
    if client.canonicalize(record.parent()?)? != record.parent()? {
        return Err(invalid("The saved upload destination folder changed").into());
    }
    Ok(())
}

fn require_receipt(store: &Store, expected: &UploadRecord) -> io::Result<()> {
    if store.uploads()?.contains(expected) {
        Ok(())
    } else {
        Err(invalid(
            "The saved upload receipt changed. Check saved transfers again.",
        ))
    }
}

fn require_absent(client: &mut Client, target: &[u8]) -> Result<()> {
    match client.lstat(target) {
        Err(error) if error.is_not_found() => Ok(()),
        Ok(_) => {
            Err(invalid("The upload destination is occupied. The existing file was kept.").into())
        }
        Err(error) => Err(error.into()),
    }
}

fn failure(
    mut failure: Failure,
    record: UploadRecord,
    context: &durable::Context,
    offset: u64,
    can_offer: bool,
) -> Failure {
    if let Ok(stage) = record.stage() {
        failure.message.push_str(&format!(
            " · staging path {}",
            files::display(&String::from_utf8_lossy(&stage))
        ));
    }
    if can_offer {
        failure.partial = Some(Offer::upload(
            Saved::new(record, context),
            &context.host_key,
            offset,
        ));
    }
    failure
}

fn require_allowed(allowed: &impl Fn() -> bool) -> io::Result<()> {
    if allowed() {
        Ok(())
    } else {
        Err(io::ErrorKind::Interrupted.into())
    }
}

fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Saved uploads are unavailable in this window",
    )
}

fn invalid(message: &str) -> io::Error {
    io::Error::other(message.to_owned())
}

struct Stream<'a> {
    file: &'a File,
    progress: &'a Progress,
    writing: &'a Cell<bool>,
}

struct Observed<R, F> {
    source: R,
    observe: F,
}

impl<R: Read, F: FnMut(&[u8])> Read for Observed<R, F> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.source.read(output)?;
        (self.observe)(&output[..count]);
        Ok(count)
    }
}

impl<R: Seek, F> Seek for Observed<R, F> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.source.seek(position)
    }
}

impl Read for Stream<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.file.read(output)?;
        if self.writing.get() {
            self.progress.reserve_more(count)?;
            self.progress
                .bytes
                .fetch_add(count as u64, Ordering::Relaxed);
        }
        Ok(count)
    }
}

impl Seek for Stream<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.writing.set(false);
        self.file.seek(position)
    }
}
