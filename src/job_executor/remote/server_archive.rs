use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kortty_jobs::{Cancellation, RunRequest};
use kortty_server_archive::protocol::{CreateFormat, CreatedPayload, Reply, Request};
use kortty_server_archive::rsync_tree::{Inventory, Manifest};
use kortty_ssh::{SshSession, SshTarget, TrustStore, sftp::FileKind};
use sha2::{Digest, Sha256};

use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::Store as ConnectionStore;
use crate::job_executor::connection;
use crate::profile;
use crate::scheduler_window::{Archive, ConnectionRef, RemotePath, SshCommand, sync};
use crate::sftp_window::{
    ArchiveUpdateFailure, ReviewedFilePublication, ReviewedFileReceiver, StagedDownload,
    VacantLocalFilePublication, VacantLocalFileReceiver, exchange_archive_update,
    exchange_server_archive, exchange_server_password_archive,
};
use crate::ssh_connection::ExecOpener;

use super::{Completion, ExecutionPolicy, Failure, check_cancelled};

const POLICY_POLL: Duration = Duration::from_millis(250);

pub(super) fn run(
    request: &RunRequest<SshCommand>,
    archive: &Archive,
    policy: &ExecutionPolicy,
    target: SshTarget,
    jumps: Vec<SshTarget>,
    trust: &TrustStore,
    password: Option<&str>,
) -> Result<Completion, Failure> {
    let spec = archive.server_spec().map_err(Failure::blocked)?;
    if (spec.format == CreateFormat::ZipPassword) != password.is_some() {
        return Err(Failure::blocked(
            "The server archive password is unavailable.",
        ));
    }
    let reference = request.action.connection.as_ref().ok_or_else(|| {
        Failure::blocked("Choose one saved SSH route for server-only archive creation.")
    })?;
    let pins = sync::pins(reference, trust).map_err(|_| {
        Failure::blocked("Verify the SSH route host keys before creating an archive.")
    })?;
    let require_route =
        archive.reuse_output || spec.output.is_some() || spec.report_payload || password.is_some();
    let require_transfer = spec.report_payload;
    let action = Arc::clone(&request.action);
    let scoped_policy = policy.clone();
    let trust_check = trust.clone();
    let saved_reference = reference.clone();
    let current_pins = pins.clone();
    let cancellation = request.cancellation.clone();
    let authorize = move || {
        scoped_policy
            .require_target_groups(&action)
            .map_err(|failure| io::Error::other(failure.detail))?;
        let policy = ClipboardPolicy::load();
        if policy.problem.is_some()
            || !policy.minimum_isolation.is_none()
            || !connection::reference_allowed(&saved_reference, policy.server_access())
            || cancellation.is_requested()
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The server archive route is no longer allowed.",
            ));
        }
        if require_route
            && saved_reference.shared_identity().is_none()
            && (!sync::pins(&saved_reference, &trust_check)
                .is_ok_and(|current| current == current_pins)
                || !profile::connections_path()
                    .and_then(ConnectionStore::open)
                    .is_ok_and(|store| {
                        store
                            .get(saved_reference.id)
                            .map(ConnectionRef::from)
                            .as_ref()
                            == Some(&saved_reference)
                    }))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The SSH route or host keys for the saved archive output changed.",
            ));
        }
        if require_transfer && !policy.file_transfer_allowed {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Current policy forbids downloading the server archive.",
            ));
        }
        Ok(())
    };
    if spec.report_payload && !policy.file_transfer_allowed {
        return Err(Failure::blocked(
            "Application policy forbids downloading the server archive.",
        ));
    }
    authorize()
        .map_err(|_| Failure::blocked("Current policy forbids this server archive route."))?;
    let cancellation = request.cancellation.clone();
    let stopped = move || cancellation.is_requested();
    let local = archive
        .download
        .as_deref()
        .map(|path| LocalCopy::prepare(path, archive.reuse_output, &stopped))
        .transpose()?;
    let mut session =
        super::sync::open_pinned_for_job(target, jumps, &pins, &stopped, request, policy)?;
    let result = (|| {
        let opener = session.exec_opener().ok_or_else(|| {
            Failure::blocked("The verified SSH session does not support a command channel.")
        })?;
        if archive.reuse_output {
            let publication = match exchange_archive_update(
                &opener,
                &spec,
                password,
                stopped.clone(),
                authorize.clone(),
            ) {
                Ok(publication) => publication,
                Err(ArchiveUpdateFailure::Uncertain { recovery }) => {
                    return Ok(update_uncertain_completion(request, recovery.as_deref()));
                }
                Err(ArchiveUpdateFailure::BeforePublication | ArchiveUpdateFailure::Refused) => {
                    check_cancelled(&request.cancellation)?;
                    authorize().map_err(|_| {
                        Failure::blocked("The archive update is no longer authorized.")
                    })?;
                    return Err(Failure::failed(
                        "Could not publish the reviewed archive update. Check the Linux helper, source paths and protected output.",
                    ));
                }
            };
            let copy = local.map(|local| {
                download_archive(
                    &mut session,
                    &archive.target,
                    publication.payload,
                    local,
                    &request.cancellation,
                    &authorize,
                )
            });
            return Ok(update_completion(
                request,
                copy,
                publication.recovery.as_deref(),
            ));
        }
        let opener = ExecOpener::Direct(opener);
        let reply = exchange_server_archive(
            &opener,
            &Request::ReviewCreation { spec: spec.clone() },
            stopped.clone(),
            &authorize,
        );
        check_cancelled(&request.cancellation)?;
        let Ok(Reply::CreationReview { review }) = reply else {
            return Err(Failure::failed(
                "Could not review the server archive. Check the helper and protected source paths.",
            ));
        };
        review.validate_for(&spec).map_err(|_| {
            Failure::failed("The archive helper returned an invalid source inventory.")
        })?;
        let commit = Request::ApplyCreation {
            spec: spec.clone(),
            token: review.token,
        };
        let reply = if let Some(password) = password {
            exchange_server_password_archive(
                &opener,
                &commit,
                password,
                stopped.clone(),
                authorize.clone(),
            )
        } else {
            exchange_server_archive(&opener, &commit, stopped.clone(), &authorize)
        };
        match reply {
            Ok(Reply::Applied { destination }) if destination == spec.destination => {
                if local.is_some() {
                    return Ok(copy_completion(
                        request,
                        Err(Failure::failed(
                            "The server did not return a verified local-copy receipt.",
                        )),
                    ));
                }
                let mut completion = Completion::from_result(request.id, Ok(0));
                completion.detail = Some("Created the reviewed archive on the server.".into());
                Ok(completion)
            }
            Ok(Reply::CreationApplied {
                destination,
                payload,
            }) if destination == spec.destination && spec.report_payload => {
                let stage = local
                    .ok_or_else(|| Failure::failed("The local archive stage was unavailable."))?;
                let result = download_archive(
                    &mut session,
                    &archive.target,
                    payload,
                    stage,
                    &request.cancellation,
                    &authorize,
                );
                Ok(copy_completion(
                    request,
                    result.map(|_| ()).map_err(|error| error.failure),
                ))
            }
            Ok(Reply::Failed {
                outcome_unknown: false,
                ..
            }) => Err(Failure::failed(
                "The archive helper refused publication. Check the source and destination.",
            )),
            _ => Err(Failure::failed(
                "The server archive result is uncertain. Check the configured destination before retrying.",
            )),
        }
    })();
    let _ = session.close();
    result
}

fn append_recovery(
    mut detail: String,
    label: &str,
    recovery: Option<impl std::fmt::Display>,
) -> String {
    if let Some(recovery) = recovery {
        detail.push_str(&format!(" {label} · {recovery}"));
    }
    detail
}

pub(super) fn update_uncertain_completion(
    request: &RunRequest<SshCommand>,
    recovery: Option<&str>,
) -> Completion {
    let mut completion = Completion::from_result(
        request.id,
        Err(Failure::failed(
            "The archive update outcome is uncertain. Check the remote destination before retrying.",
        )),
    );
    if let Some(recovery) = recovery {
        completion.append_detail(&format!("Last reported remote recovery · {recovery}"));
    }
    completion
}

pub(super) fn update_completion(
    request: &RunRequest<SshCommand>,
    copy: Option<Result<Option<PathBuf>, LocalCopyFailure>>,
    remote_recovery: Option<&str>,
) -> Completion {
    let (outcome, detail) = match copy {
        Some(Ok(recovery)) => (
            Ok(0),
            append_recovery(
                "Published the reviewed server archive and its verified local copy.".into(),
                "Local recovery",
                recovery.as_deref().map(Path::display),
            ),
        ),
        Some(Err(LocalCopyFailure { failure, recovery })) => {
            let detail = append_recovery(
                format!("The server archive was published. {}", failure.detail),
                "Last reported local recovery",
                recovery.as_deref().map(Path::display),
            );
            (Err(failure), detail)
        }
        None => (Ok(0), "Published the reviewed server archive.".into()),
    };
    let mut completion = Completion::from_result(request.id, outcome);
    completion.detail = Some(append_recovery(detail, "Remote recovery", remote_recovery));
    completion
}

pub(super) struct LocalCopy {
    stage: StagedDownload,
    receiver: CopyReceiver,
}

enum CopyReceiver {
    Legacy,
    Vacant(VacantLocalFileReceiver),
    Existing(Box<ReviewedFileReceiver>),
}

pub(super) struct LocalCopyFailure {
    failure: Failure,
    recovery: Option<PathBuf>,
}

impl From<Failure> for LocalCopyFailure {
    fn from(failure: Failure) -> Self {
        Self {
            failure,
            recovery: None,
        }
    }
}

impl LocalCopy {
    pub(super) fn prepare(
        path: &Path,
        reuse: bool,
        stopped: &impl Fn() -> bool,
    ) -> Result<Self, Failure> {
        let receiver = if reuse {
            match fs::symlink_metadata(path) {
                Ok(_) => ReviewedFileReceiver::open(path, stopped)
                    .map(|receiver| CopyReceiver::Existing(Box::new(receiver)))
                    .map_err(|_| {
                        Failure::failed(
                            "Could not verify the existing local archive copy and its parent.",
                        )
                    })?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    VacantLocalFileReceiver::open(path, stopped)
                        .map(CopyReceiver::Vacant)
                        .map_err(|_| {
                            Failure::failed("Could not verify a vacant local archive path.")
                        })?
                }
                Err(_) => return Err(Failure::failed("Could not inspect the local archive path.")),
            }
        } else {
            CopyReceiver::Legacy
        };
        let stage = match &receiver {
            CopyReceiver::Existing(receiver) => receiver.stage(),
            CopyReceiver::Vacant(receiver) => receiver.stage(),
            CopyReceiver::Legacy => StagedDownload::create(path),
        }
        .map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                Failure::failed("The local archive destination already exists.")
            } else {
                Failure::failed("Could not create a private local archive stage.")
            }
        })?;
        Ok(Self { stage, receiver })
    }

    pub(super) fn writer(&self) -> Result<&File, Failure> {
        self.stage
            .writer()
            .map_err(|_| Failure::failed("The private local archive stage was unavailable."))
    }
}

pub(super) fn copy_completion(
    request: &RunRequest<SshCommand>,
    result: Result<(), Failure>,
) -> Completion {
    let (outcome, detail) = match result {
        Ok(()) => (
            Ok(0),
            "Created the reviewed archive on the server and saved its verified local copy.".into(),
        ),
        Err(error) => {
            let detail = format!("The server archive was created. {}", error.detail);
            (Err(error), detail)
        }
    };
    let mut completion = Completion::from_result(request.id, outcome);
    completion.detail = Some(detail);
    completion
}

fn download_archive(
    session: &mut SshSession,
    path: &RemotePath,
    payload: CreatedPayload,
    local: LocalCopy,
    cancellation: &Cancellation,
    authorize: &(impl Fn() -> io::Result<()> + Sync),
) -> Result<Option<PathBuf>, LocalCopyFailure> {
    payload.validate().map_err(|_| {
        Failure::failed("The server returned an invalid archive local-copy receipt.")
    })?;
    before_copy(cancellation, authorize)?;
    let live = Mutex::new((Instant::now(), true));
    let cancelled = || {
        if cancellation.is_requested() {
            return true;
        }
        let mut live = live.lock().unwrap_or_else(|error| error.into_inner());
        if live.1 && live.0.elapsed() >= POLICY_POLL {
            *live = (Instant::now(), authorize().is_ok());
        }
        !live.1
    };
    super::sync::supervise(session, &cancelled, |client, stopped, _| {
        let attributes = super::verify_entry(client, path.as_str())
            .map_err(|error| before_copy(cancellation, authorize).err().unwrap_or(error))?;
        if attributes.kind() != FileKind::File || attributes.size != Some(payload.bytes) {
            before_copy(cancellation, authorize)?;
            return Err(Failure::failed(
                "The published server archive no longer matches its local-copy receipt.",
            )
            .into());
        }
        publish_payload(local, payload, cancellation, stopped, authorize, |output| {
            client
                .download_regular(path.as_str().as_bytes(), attributes, output)
                .map_err(|_| {
                    Failure::failed("Could not read the published server archive for a local copy.")
                })
        })
    })
    .map_err(|error| before_copy(cancellation, authorize).err().unwrap_or(error))?
}

fn before_copy(
    cancellation: &Cancellation,
    authorize: &impl Fn() -> io::Result<()>,
) -> Result<(), Failure> {
    check_cancelled(cancellation)?;
    authorize().map_err(|_| Failure::blocked("The local archive copy is no longer authorized."))
}

fn publish_payload(
    local: LocalCopy,
    payload: CreatedPayload,
    cancellation: &Cancellation,
    stopped: &dyn Fn() -> bool,
    authorize: &impl Fn() -> io::Result<()>,
    download: impl FnOnce(&mut dyn Write) -> Result<u64, Failure>,
) -> Result<Option<PathBuf>, LocalCopyFailure> {
    before_copy(cancellation, authorize)?;
    let mut output = CheckedOutput {
        file: local.writer()?,
        hash: Sha256::new(),
        remaining: payload.bytes,
        stopped,
    };
    let count = download(&mut output);
    before_copy(cancellation, authorize)?;
    if stopped() {
        return Err(Failure::failed("The local archive copy stopped.").into());
    }
    if count? != payload.bytes
        || output.remaining != 0
        || <[u8; 32]>::from(output.hash.finalize()) != payload.sha256
    {
        return Err(
            Failure::failed("The copied bytes do not match the published server archive.").into(),
        );
    }
    publish_verified_copy(local, payload, cancellation, stopped, authorize)
}

pub(super) fn publish_verified_copy(
    LocalCopy { stage, receiver }: LocalCopy,
    payload: CreatedPayload,
    cancellation: &Cancellation,
    stopped: &dyn Fn() -> bool,
    authorize: &impl Fn() -> io::Result<()>,
) -> Result<Option<PathBuf>, LocalCopyFailure> {
    before_copy(cancellation, authorize)?;
    let verify = || {
        if stopped() {
            return Err(io::Error::other("The local archive transfer stopped."));
        }
        authorize()
    };
    if let CopyReceiver::Vacant(receiver) = receiver {
        return match receiver.publish(stage, copy_inventory(payload)?, &stopped, &verify, |_| {
            Ok(())
        }) {
            Ok(VacantLocalFilePublication::Published(_)) => Ok(None),
            Ok(VacantLocalFilePublication::Uncertain(_)) => Err(Failure::failed(
                "The local archive copy is uncertain. Check the local destination before retrying.",
            )
            .into()),
            Err(_) => {
                before_copy(cancellation, authorize)?;
                Err(
                    Failure::failed("The vacant archive destination or staged copy changed.")
                        .into(),
                )
            }
        };
    }
    if let CopyReceiver::Existing(receiver) = receiver {
        return match receiver.publish(stage, copy_inventory(payload)?, &stopped, &verify, |_| {
            Ok(())
        }) {
            Ok(ReviewedFilePublication::Published { recovery, .. }) => Ok(Some(recovery)),
            Ok(ReviewedFilePublication::Restored { .. }) => {
                before_copy(cancellation, authorize)?;
                Err(Failure::failed(
                    "The local archive replacement was interrupted. The original file was restored.",
                ).into())
            }
            Ok(ReviewedFilePublication::Uncertain { recovery, .. }) => Err(LocalCopyFailure {
                failure: Failure::failed(
                    "The local archive replacement is uncertain. Check the local destination before retrying.",
                ),
                recovery,
            }),
            Err(_) => {
                before_copy(cancellation, authorize)?;
                Err(
                    Failure::failed("The reviewed local archive changed or could not be replaced.")
                        .into(),
                )
            }
        };
    }
    if let Err(error) = stage.publish_checked(|_| verify()) {
        before_copy(cancellation, authorize)?;
        return Err((if error.kind() == io::ErrorKind::AlreadyExists {
            Failure::failed("The local archive destination already exists.")
        } else {
            Failure::failed(
                "The local archive copy could not be confirmed. Check the local destination before retrying.",
            )
        }).into());
    }
    Ok(None)
}

fn copy_inventory(payload: CreatedPayload) -> Result<Inventory, Failure> {
    let mut manifest = Manifest::default();
    manifest
        .add("", Some((payload.bytes, payload.sha256)))
        .map_err(|_| Failure::failed("Could not verify the staged local archive copy."))?;
    Ok(manifest.finish())
}

struct CheckedOutput<'a> {
    file: &'a File,
    hash: Sha256,
    remaining: u64,
    stopped: &'a dyn Fn() -> bool,
}

impl Write for CheckedOutput<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if (self.stopped)() || bytes.len() as u64 > self.remaining {
            return Err(io::Error::other(
                "The local archive transfer stopped or exceeded its size limit.",
            ));
        }
        let count = self.file.write(bytes)?;
        self.hash.update(&bytes[..count]);
        self.remaining -= count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Seek as _;
    use std::{cell::Cell, fs};

    use crate::job_executor::tests::scheduled_request;

    #[test]
    fn reused_archive_checks_staged_bytes_again_before_first_local_publication() {
        let profile = crate::snippets::tests::Profile::new();
        let directory = profile.0.canonicalize().unwrap().join("downloads");
        crate::private_files::Directory::open(&directory, true).unwrap();
        let destination = directory.join("checked.tar");
        let expected = b"expected archive";
        let payload = CreatedPayload {
            bytes: expected.len() as u64,
            sha256: Sha256::digest(expected).into(),
        };
        let (_, request) = scheduled_request();
        let local = LocalCopy::prepare(&destination, true, &|| false)
            .unwrap_or_else(|error| panic!("{}", error.detail));
        let mut file = local
            .writer()
            .unwrap_or_else(|error| panic!("{}", error.detail));
        file.write_all(expected).unwrap();
        file.rewind().unwrap();
        file.write_all(b"tampered archive").unwrap();
        assert!(
            publish_verified_copy(local, payload, &request.cancellation, &|| false, &|| Ok(()))
                .is_err()
        );
        assert!(!destination.exists());
        assert!(fs::read_dir(&directory).unwrap().next().is_none());
    }

    #[test]
    fn server_archive_local_copy_checks_bytes_authority_and_vacant_destination() {
        let profile = crate::snippets::tests::Profile::new();
        let destination = profile.0.join("download.zip");
        let known = b"archive payload";
        let receipt = CreatedPayload {
            bytes: known.len() as u64,
            sha256: Sha256::digest(known).into(),
        };
        let (scheduler, request) = scheduled_request();
        let approved = || Ok(());
        let unstopped = || false;
        let write = |output: &mut dyn Write, bytes: &[u8]| {
            output
                .write_all(bytes)
                .map_err(|_| Failure::failed("Scripted read failed."))?;
            Ok(bytes.len() as u64)
        };
        let run = |authorize: &dyn Fn() -> io::Result<()>, bytes: &[u8], competing: bool| {
            let stage = LocalCopy::prepare(&destination, false, &unstopped)
                .unwrap_or_else(|error| panic!("{}", error.detail));
            publish_payload(
                stage,
                receipt,
                &request.cancellation,
                &unstopped,
                &authorize,
                |output| {
                    let count = write(output, bytes)?;
                    assert!(!destination.exists());
                    if competing {
                        fs::write(&destination, b"competing bytes").unwrap();
                    }
                    Ok(count)
                },
            )
        };

        assert!(run(&approved, b"altered payload", false).is_err());
        assert!(fs::read_dir(&profile.0).unwrap().next().is_none());
        assert!(run(&approved, b"archive payload plus extra bytes", false).is_err());
        assert!(fs::read_dir(&profile.0).unwrap().next().is_none());

        let checks = Cell::new(0);
        let revoked = || {
            checks.set(checks.get() + 1);
            if checks.get() < 3 {
                Ok(())
            } else {
                Err(io::ErrorKind::PermissionDenied.into())
            }
        };
        let Err(revoked) = run(&revoked, known, false) else {
            panic!("a revoked local copy was published")
        };
        let partial = copy_completion(&request, Err(revoked.failure));
        assert_eq!(partial.outcome, kortty_jobs::RunOutcome::Blocked);
        assert!(
            partial
                .detail
                .unwrap()
                .starts_with("The server archive was created.")
        );
        assert!(fs::read_dir(&profile.0).unwrap().next().is_none());

        assert!(run(&approved, known, true).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"competing bytes");
        fs::remove_file(&destination).unwrap();
        assert!(run(&approved, known, false).is_ok());
        assert_eq!(fs::read(&destination).unwrap(), known);
        assert_eq!(fs::read_dir(&profile.0).unwrap().count(), 1);

        fs::remove_file(&destination).unwrap();
        let stage = LocalCopy::prepare(&destination, false, &unstopped)
            .unwrap_or_else(|error| panic!("{}", error.detail));
        let result = publish_payload(
            stage,
            receipt,
            &request.cancellation,
            &unstopped,
            &approved,
            |output| {
                let count = write(output, known)?;
                scheduler.cancel(request.id).unwrap();
                Ok(count)
            },
        );
        let completion =
            copy_completion(&request, result.map(|_| ()).map_err(|error| error.failure));
        assert_eq!(completion.outcome, kortty_jobs::RunOutcome::Cancelled);
        assert!(
            completion
                .detail
                .unwrap()
                .starts_with("The server archive was created.")
        );
        assert!(fs::read_dir(&profile.0).unwrap().next().is_none());
    }
}
