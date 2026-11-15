use std::fs::File;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kortty_jobs::{Cancellation, RunRequest};
use kortty_server_archive::protocol::{CreateFormat, CreatedPayload, Reply, Request};
use kortty_ssh::{SshSession, SshTarget, TrustStore, sftp::FileKind};
use sha2::{Digest, Sha256};

use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::Store as ConnectionStore;
use crate::job_executor::connection;
use crate::profile;
use crate::scheduler_window::{Archive, ConnectionRef, RemotePath, SshCommand, sync};
use crate::sftp_window::{
    StagedDownload, exchange_server_archive, exchange_server_password_archive,
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
    let require_route = spec.output.is_some() || spec.report_payload || password.is_some();
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
    let local = archive
        .download
        .as_deref()
        .map(StagedDownload::create)
        .transpose()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                Failure::failed("The local archive destination already exists.")
            } else {
                Failure::failed("Could not create a private local archive stage.")
            }
        })?;
    let cancellation = request.cancellation.clone();
    let stopped = move || cancellation.is_requested();
    let mut session =
        super::sync::open_pinned_for_job(target, jumps, &pins, &stopped, request, policy)?;
    let result = (|| {
        let opener = session
            .exec_opener()
            .map(ExecOpener::Direct)
            .ok_or_else(|| {
                Failure::blocked("The verified SSH session does not support a command channel.")
            })?;
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
                Ok(copy_completion(request, result))
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
    stage: StagedDownload,
    cancellation: &Cancellation,
    authorize: &(impl Fn() -> io::Result<()> + Sync),
) -> Result<(), Failure> {
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
    let result = super::sync::supervise(session, &cancelled, |client, stopped, _| {
        let attributes = super::verify_entry(client, path.as_str())?;
        if attributes.kind() != FileKind::File || attributes.size != Some(payload.bytes) {
            return Err(Failure::failed(
                "The published server archive no longer matches its local-copy receipt.",
            ));
        }
        publish_payload(stage, payload, cancellation, stopped, authorize, |output| {
            client
                .download_regular(path.as_str().as_bytes(), attributes, output)
                .map_err(|_| {
                    Failure::failed("Could not read the published server archive for a local copy.")
                })
        })
    })
    .and_then(|result| result);
    if result.is_err() {
        before_copy(cancellation, authorize)?;
    }
    result
}

fn before_copy(
    cancellation: &Cancellation,
    authorize: &impl Fn() -> io::Result<()>,
) -> Result<(), Failure> {
    check_cancelled(cancellation)?;
    authorize().map_err(|_| Failure::blocked("The local archive copy is no longer authorized."))
}

fn publish_payload(
    stage: StagedDownload,
    payload: CreatedPayload,
    cancellation: &Cancellation,
    stopped: &dyn Fn() -> bool,
    authorize: &impl Fn() -> io::Result<()>,
    download: impl FnOnce(&mut dyn Write) -> Result<u64, Failure>,
) -> Result<(), Failure> {
    before_copy(cancellation, authorize)?;
    let mut output = CheckedOutput {
        file: stage
            .writer()
            .map_err(|_| Failure::failed("The private local archive stage was unavailable."))?,
        hash: Sha256::new(),
        remaining: payload.bytes,
        stopped,
    };
    let count = download(&mut output);
    before_copy(cancellation, authorize)?;
    if stopped() {
        return Err(Failure::failed("The local archive copy stopped."));
    }
    if count? != payload.bytes
        || output.remaining != 0
        || <[u8; 32]>::from(output.hash.finalize()) != payload.sha256
    {
        return Err(Failure::failed(
            "The copied bytes do not match the published server archive.",
        ));
    }
    let verify = || {
        if stopped() {
            return Err(io::Error::other("The local archive transfer stopped."));
        }
        authorize()
    };
    if let Err(error) = stage.publish_checked(|_| verify()) {
        before_copy(cancellation, authorize)?;
        return Err(if error.kind() == io::ErrorKind::AlreadyExists {
            Failure::failed("The local archive destination already exists.")
        } else {
            Failure::failed(
                "The local archive copy could not be confirmed. Check the local destination before retrying.",
            )
        });
    }
    Ok(())
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
    use std::{cell::Cell, fs};

    use crate::job_executor::tests::scheduled_request;

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
            let stage = StagedDownload::create(&destination).unwrap();
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
        let partial = copy_completion(&request, Err(revoked));
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
        let stage = StagedDownload::create(&destination).unwrap();
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
        let completion = copy_completion(&request, result);
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
