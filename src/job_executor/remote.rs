//! Runs unattended actions over the authenticated SFTP subsystem.

use std::io::{self, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use kortty_jobs::{Cancellation, RunOutcome, RunRequest};
use kortty_ssh::sftp::{Attributes, Error, FileKind, Sftp};
use kortty_ssh::{SshSession, SshTarget, TrustStore};

use crate::scheduler_window::{
    Archive, ArchiveFormat, RemoteAction, SshCommand, windows_drive_ancestors,
};
use crate::sftp_window::FolderTransfer;
use crate::ssh_connection;

use super::{
    BorrowedArchivePassword, CANCELLATION_POLL, COMMAND_TIMEOUT, Completion, ExecutionPolicy,
    Failure, SavedAuth, check_cancelled, connect_failure, connection,
};

mod archive;
mod delete;
mod privileged;
mod server_archive;
pub(super) mod sync;
pub(super) mod transfer;

pub(crate) use archive::{BrowserArchiveManifest, scan_browser_archive};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Effects {
    confirmed_deletions: usize,
    folder_remains: Option<transfer::FolderRemains>,
    archive_published: bool,
}

pub(super) fn run(
    request: &RunRequest<SshCommand>,
    policy: &ExecutionPolicy,
    saved_auth: SavedAuth,
    archive_password: BorrowedArchivePassword<'_>,
    trust_store: impl FnOnce() -> io::Result<TrustStore>,
) -> Result<(Completion, Option<String>), Failure> {
    let cancellation = &request.cancellation;
    check_cancelled(cancellation)?;
    policy.require_target_groups(&request.action)?;
    let action = request.action.remote_action().map_err(Failure::blocked)?;
    if action.transfers_files() && !policy.file_transfer_allowed {
        return Err(Failure::blocked(
            "Application policy forbids scheduled file transfers",
        ));
    }
    if let RemoteAction::Privileged(transfer) = action {
        return privileged::run(request, policy, transfer, saved_auth, trust_store)
            .map(|completion| (completion, None));
    }
    if let RemoteAction::PrivilegedFileAction(metadata) = action {
        return privileged::run_file_action(request, policy, metadata, saved_auth, trust_store)
            .map(|completion| (completion, None));
    }
    if let RemoteAction::PrivilegedArchive(archive) = action {
        return privileged::run_archive(request, policy, archive, saved_auth, trust_store)
            .map(|completion| (completion, None));
    }
    if let RemoteAction::PrivilegedTreeUpload(copy) | RemoteAction::PrivilegedTreeDownload(copy) =
        action
    {
        return privileged::run_tree(request, policy, copy, saved_auth, trust_store)
            .map(|completion| (completion, None));
    }
    if let RemoteAction::Sync(copy) = action {
        return sync::run(request, policy, copy, saved_auth, trust_store);
    }
    let archive_password = require_archive_password(action, archive_password)?;
    let saved_auth = saved_auth.map_err(Failure::blocked)?;
    let (target, jumps) = connection::route(&request.action, saved_auth, &policy.server_access)?;
    let trust =
        trust_store().map_err(|_| Failure::failed("Could not access SSH host-key trust store"))?;
    policy.require_target_groups(&request.action)?;
    let cancelled = || cancellation.is_requested();
    if let RemoteAction::ServerArchive(archive) = action {
        return server_archive::run(request, archive, policy, target, jumps, &trust)
            .map(|completion| (completion, None));
    }
    if let RemoteAction::RsyncDownload(transfer) | RemoteAction::RsyncUpload(transfer) = action {
        return super::rsync::run(
            request,
            transfer,
            matches!(action, RemoteAction::RsyncUpload(_)),
            policy,
            target,
            jumps,
            &trust,
        )
        .map(|completion| (completion, None));
    }
    let session = open_group_subsystem(target, jumps, trust, cancelled, &request.action, policy);
    check_cancelled(cancellation)?;
    policy.require_target_groups(&request.action)?;
    let mut session = session?;
    let detail = supervise(&mut session, action, archive_password, cancellation)?;
    let mut completion = Completion::from_result(request.id, Ok(0));
    completion.detail = Some(detail);
    Ok((completion, None))
}

fn open_group_subsystem(
    target: SshTarget,
    jumps: Vec<SshTarget>,
    trust: TrustStore,
    cancelled: impl Fn() -> bool,
    action: &SshCommand,
    policy: &ExecutionPolicy,
) -> Result<SshSession, Failure> {
    let transfer_helper =
        matches!(&action.remote, Some(RemoteAction::RsyncUpload(transfer)) if transfer.directory);
    if !transfer_helper && !policy.has_target_groups(action) {
        return open_subsystem(target, jumps, trust, cancelled);
    }
    policy.require_target_groups(action)?;
    let tcp =
        ssh_connection::connect_transport(&jumps.first().unwrap_or(&target).endpoint, &cancelled);
    policy.require_target_groups(action)?;
    let opened = SshSession::connect_preconnected_guarded(
        tcp.map_err(|error| connect_failure(error.into()))?,
        (target, jumps),
        trust,
        if transfer_helper {
            kortty_ssh::StartupChannel::SftpWithTransferHelper
        } else {
            kortty_ssh::StartupChannel::Subsystem("sftp")
        },
        cancelled,
        || {
            policy
                .require_target_groups(action)
                .map_err(|failure| io::Error::new(io::ErrorKind::PermissionDenied, failure.detail))
        },
    );
    policy.require_target_groups(action)?;
    opened.map_err(connect_failure)
}

fn open_subsystem(
    target: SshTarget,
    jumps: Vec<SshTarget>,
    trust: TrustStore,
    cancelled: impl Fn() -> bool,
) -> Result<SshSession, Failure> {
    let tcp =
        ssh_connection::connect_transport(&jumps.first().unwrap_or(&target).endpoint, &cancelled);
    SshSession::connect_subsystem_preconnected_cancellable(
        tcp.map_err(|error| connect_failure(error.into()))?,
        target,
        jumps,
        trust,
        "sftp",
        cancelled,
    )
    .map_err(connect_failure)
}

pub(super) fn require_archive_password<'a>(
    action: &RemoteAction,
    password: BorrowedArchivePassword<'a>,
) -> Result<Option<&'a str>, Failure> {
    if matches!(
        action,
        RemoteAction::Archive(Archive {
            format: ArchiveFormat::ZipPassword,
            ..
        })
    ) {
        let password = password
            .map_err(Failure::blocked)?
            .filter(|secret| kortty_vault::valid_archive_password(secret))
            .ok_or_else(|| {
                Failure::blocked("The encrypted ZIP job requires an unlocked vault password.")
            })?;
        Ok(Some(password))
    } else {
        Ok(None)
    }
}

fn supervise(
    session: &mut SshSession,
    action: &RemoteAction,
    archive_password: Option<&str>,
    cancellation: &Cancellation,
) -> Result<String, Failure> {
    let missing_stream = || Failure::failed("SFTP subsystem streams were unavailable");
    let reader = session.take_reader().ok_or_else(missing_stream)?;
    let writer = session.take_writer().ok_or_else(missing_stream)?;
    let abort = session.abort_handle();
    let (sender, result) = mpsc::sync_channel(1);
    let timeout = if action.uses_extended_timeout() {
        COMMAND_TIMEOUT
    } else {
        OPERATION_TIMEOUT
    };
    let deadline = Instant::now() + timeout;
    let outcome = thread::scope(|scope| {
        let worker = thread::Builder::new()
            .name("kortty-job-sftp".into())
            .spawn_scoped(scope, move || {
                let mut effects = Effects::default();
                let operation = (|| {
                    before_change(cancellation)?;
                    let mut client = Sftp::new(reader, writer)
                        .map_err(|_| Failure::failed("Could not initialize the SFTP subsystem"))?;
                    execute(
                        &mut client,
                        action,
                        archive_password,
                        cancellation,
                        &mut effects,
                    )
                })();
                let _ = sender.send(operation);
                effects
            })
            .map_err(|_| Failure::failed("Could not start the SFTP job worker"))?;
        wait_for_worker(&result, cancellation, deadline, || abort.abort(), worker)
    });
    // An acknowledged SFTP change is authoritative even if the server closes
    // the subsystem before receiving our disconnect.
    let _ = session.close();
    outcome
}

fn wait_for_worker(
    result: &Receiver<Result<String, Failure>>,
    cancellation: &Cancellation,
    deadline: Instant,
    abort: impl FnOnce(),
    worker: thread::ScopedJoinHandle<'_, Effects>,
) -> Result<String, Failure> {
    let mut outcome = wait(result, cancellation, deadline);
    if outcome.is_err() {
        abort();
    }
    // Cancellation may win before the worker can report its operation result.
    // Join after interrupting SSH reads to retain known file-system effects.
    let effects = worker
        .join()
        .map_err(|_| Failure::failed("SFTP job worker failed"))?;
    if let Err(failure) = &mut outcome {
        if failure.outcome == RunOutcome::Cancelled && effects.confirmed_deletions > 0 {
            failure.detail = "SFTP deletion cancelled after at least one confirmed removal. Other removals may have completed.";
        }
        if failure.outcome == RunOutcome::Cancelled && effects.archive_published {
            failure.detail = "SFTP job cancelled after the server confirmed archive publication.";
        }
        if let Some(remains) = effects.folder_remains {
            failure.detail = match (remains, failure.outcome) {
                (transfer::FolderRemains::RemotePartial, RunOutcome::Cancelled) => {
                    "SFTP folder upload cancelled. A private partial folder may remain on the server."
                }
                (transfer::FolderRemains::RemotePartial, _) => {
                    "SFTP folder upload stopped. A private partial folder may remain on the server."
                }
                (transfer::FolderRemains::RemoteCopyPartial, RunOutcome::Cancelled) => {
                    "SFTP folder copy cancelled. A partial destination folder may remain on the server."
                }
                (transfer::FolderRemains::RemoteCopyPartial, _) => {
                    "SFTP folder copy stopped. A partial destination folder may remain on the server."
                }
                (transfer::FolderRemains::LocalStage, _) => {
                    "SFTP folder download stopped. Its private local staging folder could not be removed."
                }
                (transfer::FolderRemains::LocalPublished, _) => {
                    "SFTP folder download stopped after the complete local folder was published."
                }
                (transfer::FolderRemains::LocalPublicationUncertain, _) => {
                    transfer::LOCAL_PUBLICATION_UNCERTAIN
                }
            };
        }
    }
    outcome
}

fn wait(
    result: &Receiver<Result<String, Failure>>,
    cancellation: &Cancellation,
    deadline: Instant,
) -> Result<String, Failure> {
    loop {
        if let Ok(acknowledged) = result.try_recv() {
            return acknowledged;
        }
        before_change(cancellation)?;
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(Failure::failed(
                "SFTP action timed out. A file-system change may have completed.",
            ));
        };
        match result.recv_timeout(remaining.min(CANCELLATION_POLL)) {
            Ok(acknowledged) => return acknowledged,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Failure::failed("SFTP job worker ended unexpectedly"));
            }
        }
    }
}

fn before_change(cancellation: &Cancellation) -> Result<(), Failure> {
    before_change_if(cancellation.is_requested())
}

fn before_change_if(stopped: bool) -> Result<(), Failure> {
    if stopped {
        Err(Failure {
            outcome: RunOutcome::Cancelled,
            detail: "SFTP job cancelled. A file-system change may have completed.",
        })
    } else {
        Ok(())
    }
}

fn execute(
    client: &mut Sftp<impl Read, impl Write>,
    action: &RemoteAction,
    archive_password: Option<&str>,
    cancellation: &Cancellation,
    effects: &mut Effects,
) -> Result<String, Failure> {
    let detail = match action {
        RemoteAction::Mkdir { path } => {
            let mut parent = path.root();
            for directory in path.prefixes() {
                before_change(cancellation)?;
                match client.lstat(directory) {
                    Ok(metadata) => verify_directory_metadata(client, directory, metadata)?,
                    Err(error) if error.is_not_found() => {
                        verify_directory(client, parent)?;
                        before_change(cancellation)?;
                        let created = client.mkdir(directory);
                        let verified = verify_directory(client, directory);
                        // A concurrent creator may have made the intended directory.
                        if created.is_err() && verified.is_err() {
                            return Err(Failure::failed("Could not create the remote directory"));
                        }
                        verified?;
                    }
                    Err(_) => return Err(Failure::failed("Could not read the remote path")),
                }
                parent = directory;
            }
            "Remote directory is ready"
        }
        RemoteAction::Rename { from, to } => {
            verify_entry(client, from.as_str())?;
            verify_directory(client, to.parent())?;
            match client.lstat(to.as_str()) {
                Ok(_) => {
                    return Err(Failure::failed(
                        "The remote rename destination already exists",
                    ));
                }
                Err(error) if error.is_not_found() => {}
                Err(_) => {
                    return Err(Failure::failed(
                        "Could not check the remote rename destination",
                    ));
                }
            }
            before_change(cancellation)?;
            client
                .rename(from.as_str(), to.as_str())
                .map_err(|_| Failure::failed("The server could not rename the remote entry"))?;
            "Remote entry renamed"
        }
        RemoteAction::Chmod { path, mode } => {
            verify_entry(client, path.as_str())?;
            before_change(cancellation)?;
            client
                .set_permissions(path.as_str(), u32::from(*mode))
                .map_err(|_| {
                    Failure::failed("The server could not change the remote permissions")
                })?;
            let current = verify_entry(client, path.as_str())?;
            if current.permissions.map(|permissions| permissions & 0o7777) != Some(u32::from(*mode))
            {
                return Err(Failure::failed(
                    "The server did not apply the requested permissions",
                ));
            }
            "Remote permissions changed"
        }
        RemoteAction::Chown { path, uid, gid } => {
            if !client.supports_lsetstat() {
                return Err(Failure::blocked(
                    "The SFTP server does not advertise no-follow ownership changes",
                ));
            }
            let matches_requested = |attrs: Attributes| {
                uid.is_none_or(|uid| attrs.uid == Some(uid))
                    && gid.is_none_or(|gid| attrs.gid == Some(gid))
            };
            let current = verify_entry(client, path.as_str())?;
            if !matches_requested(current) {
                before_change(cancellation)?;
                client
                    .set_owner_no_follow(path.as_str(), *uid, *gid)
                    .map_err(|_| {
                        Failure::failed("The server could not change the remote ownership")
                    })?;
                if !matches_requested(verify_entry(client, path.as_str())?) {
                    return Err(Failure::failed(
                        "The server did not apply the requested ownership",
                    ));
                }
                "Remote ownership changed"
            } else {
                "Remote ownership already matches"
            }
        }
        RemoteAction::CopyRemote { from, to } => {
            return transfer::copy_remote(
                client,
                from,
                to,
                cancellation,
                &mut effects.folder_remains,
            );
        }
        RemoteAction::Archive(request) => {
            return archive::run(
                client,
                request,
                archive_password,
                cancellation,
                &mut effects.archive_published,
            );
        }
        RemoteAction::ServerArchive(_) => {
            return Err(Failure::blocked(
                "Server-only archive creation requires its verified command channel.",
            ));
        }
        RemoteAction::Delete {
            path, recursive, ..
        } => {
            return delete::delete(
                client,
                path,
                *recursive,
                cancellation,
                &mut effects.confirmed_deletions,
            );
        }
        RemoteAction::Upload {
            local,
            remote,
            folder: true,
        } => {
            return transfer::folder(
                client,
                FolderTransfer::Upload {
                    local,
                    remote: remote.as_str().as_bytes(),
                },
                cancellation,
                &mut effects.folder_remains,
            );
        }
        RemoteAction::Download {
            local,
            remote,
            folder: true,
        } => {
            return transfer::folder(
                client,
                FolderTransfer::Download {
                    remote: remote.as_str().as_bytes(),
                    local,
                },
                cancellation,
                &mut effects.folder_remains,
            );
        }
        RemoteAction::Upload {
            local,
            remote,
            folder: false,
        } => {
            return transfer::upload(client, local, remote, cancellation);
        }
        RemoteAction::Download {
            local,
            remote,
            folder: false,
        } => {
            return transfer::download(client, remote, local, cancellation);
        }
        RemoteAction::Sync(_) => {
            return Err(Failure::blocked("Reviewed SFTP approval is required."));
        }
        RemoteAction::Privileged(_)
        | RemoteAction::PrivilegedFileAction(_)
        | RemoteAction::PrivilegedArchive(_)
        | RemoteAction::PrivilegedTreeUpload(_)
        | RemoteAction::PrivilegedTreeDownload(_) => {
            return Err(Failure::blocked(
                "Reviewed privileged transfer approval is required.",
            ));
        }
        RemoteAction::RsyncDownload(_) | RemoteAction::RsyncUpload(_) => {
            return Err(Failure::blocked(
                "Approved rsync configuration is required.",
            ));
        }
    };
    Ok(detail.into())
}

fn verify_directory(client: &mut Sftp<impl Read, impl Write>, path: &str) -> Result<(), Failure> {
    let metadata = client
        .lstat(path)
        .map_err(|_| Failure::failed("Could not read the remote path"))?;
    verify_directory_metadata(client, path, metadata)
}

fn verify_directory_metadata(
    client: &mut Sftp<impl Read, impl Write>,
    path: &str,
    metadata: Attributes,
) -> Result<(), Failure> {
    if metadata.kind() != FileKind::Directory {
        return Err(Failure::failed(
            "The remote path must be an ordinary directory",
        ));
    }
    verify_canonical(client, path)
}

pub(super) fn verify_entry(
    client: &mut Sftp<impl Read, impl Write>,
    path: &str,
) -> Result<Attributes, Failure> {
    verify_entry_with(client, path, |_, detail| Failure::failed(detail))
}

fn verify_entry_with<E: From<Failure>>(
    client: &mut Sftp<impl Read, impl Write>,
    path: &str,
    remote_error: impl Fn(Error, &'static str) -> E,
) -> Result<Attributes, E> {
    let metadata = client
        .lstat(path)
        .map_err(|error| remote_error(error, "Could not read the remote path"))?;
    if !matches!(metadata.kind(), FileKind::File | FileKind::Directory) {
        return Err(
            Failure::failed("The remote path must refer to an ordinary file or directory").into(),
        );
    }
    verify_canonical_with(client, path, remote_error)?;
    Ok(metadata)
}

pub(super) fn verify_canonical(
    client: &mut Sftp<impl Read, impl Write>,
    path: &str,
) -> Result<(), Failure> {
    verify_canonical_with(client, path, |_, detail| Failure::failed(detail))
}

fn verify_canonical_with<E: From<Failure>>(
    client: &mut Sftp<impl Read, impl Write>,
    path: &str,
    remote_error: impl Fn(Error, &'static str) -> E,
) -> Result<(), E> {
    if client
        .canonicalize(path)
        .map_err(|error| remote_error(error, "Could not verify the remote path"))?
        != path.as_bytes()
    {
        return Err(Failure::failed("The remote path must be canonical").into());
    }
    for ancestor in windows_drive_ancestors(path) {
        if client
            .lstat(ancestor)
            .map_err(|error| remote_error(error, "Could not check a Windows drive path ancestor"))?
            .kind()
            != FileKind::Directory
        {
            return Err(Failure::failed(
                "Windows drive path ancestors must be ordinary directories",
            )
            .into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job_executor::tests::scheduled_request;

    #[cfg(windows)]
    #[test]
    #[ignore = "requires the disposable Win32 OpenSSH SFTP fixture"]
    fn windows_openssh_scheduler_executes_drive_qualified_sftp_jobs() {
        use std::{fs, net::SocketAddr, path::PathBuf, sync::Arc};

        use kortty_ssh::{Endpoint, HostKey};
        use serde_json::json;

        let required =
            |key| std::env::var(key).unwrap_or_else(|_| panic!("set {key} for the fixture"));
        let fixture = PathBuf::from(required("KORTTY_WINDOWS_RELAY_FIXTURE"));
        let address: SocketAddr = required("KORTTY_WINDOWS_RELAY_ADDRESS").parse().unwrap();
        assert!(address.ip().is_loopback());
        let username = required("KORTTY_WINDOWS_RELAY_USER");
        let key = fs::read_to_string(fixture.join("host.pub")).unwrap();
        let host_key = HostKey::from_openssh(key.trim()).unwrap();
        let run_job = |remote, allowed| {
            let (_, mut request) = scheduled_request();
            request.action = Arc::new(
                serde_json::from_value(json!({
                    "host": address.ip().to_string(),
                    "port": address.port(),
                    "username": username,
                    "key_file": fixture.join("client"),
                    "remote": remote,
                }))
                .unwrap(),
            );
            run(
                &request,
                &ExecutionPolicy::unrestricted(allowed),
                Ok(None),
                Ok(None),
                || {
                    assert!(
                        allowed,
                        "policy must deny before reading host keys or connecting"
                    );
                    let endpoint =
                        Endpoint::new(&address.ip().to_string(), address.port()).unwrap();
                    Ok(TrustStore::from_pins(vec![(endpoint, host_key.clone())]).unwrap())
                },
            )
            .map(|(completion, _)| completion)
        };
        let directory = fixture.join(format!("drive-path-{}", std::process::id()));
        let nested = directory.join("new directory");
        let remote_path = |path: &std::path::Path| {
            let path = format!("/{}", path.to_str().unwrap().replace('\\', "/"));
            assert!(
                crate::scheduler_window::windows_drive_ancestors(&path)
                    .next()
                    .is_some()
            );
            path
        };
        let mkdir = json!({"kind": "mkdir", "path": remote_path(&nested)});
        assert_eq!(
            run_job(mkdir.clone(), true)
                .map_err(|failure| failure.detail)
                .unwrap()
                .outcome,
            RunOutcome::Success
        );
        assert!(nested.is_dir());
        assert_eq!(
            run_job(mkdir, true)
                .map_err(|failure| failure.detail)
                .unwrap()
                .outcome,
            RunOutcome::Success
        );

        let source = nested.join("source bytes");
        let destination = directory.join("downloaded bytes");
        fs::write(&source, b"drive-qualified SFTP download\n").unwrap();
        let download = json!({
            "kind": "download", "local": destination, "remote": remote_path(&source),
        });
        assert!(
            matches!(run_job(download.clone(), false), Err(failure) if failure.outcome == RunOutcome::Blocked)
        );
        assert!(!destination.exists());
        assert_eq!(
            run_job(download.clone(), true)
                .map_err(|failure| failure.detail)
                .unwrap()
                .outcome,
            RunOutcome::Success
        );
        assert_eq!(
            fs::read(&destination).unwrap(),
            b"drive-qualified SFTP download\n"
        );
        fs::write(&destination, b"keep this existing destination").unwrap();
        let failure = run_job(download, true).unwrap_err();
        assert_eq!(
            failure.detail,
            "The local download destination already exists"
        );
        assert_eq!(
            fs::read(destination).unwrap(),
            b"keep this existing destination"
        );
    }

    #[test]
    fn sftp_cancellation_retains_known_effects_after_aborting_the_worker() {
        for (effects, detail) in [
            (
                Effects::default(),
                "SFTP job cancelled. A file-system change may have completed.",
            ),
            (
                Effects {
                    confirmed_deletions: 1,
                    ..Effects::default()
                },
                "SFTP deletion cancelled after at least one confirmed removal. Other removals may have completed.",
            ),
            (
                Effects {
                    folder_remains: Some(transfer::FolderRemains::RemotePartial),
                    ..Effects::default()
                },
                "SFTP folder upload cancelled. A private partial folder may remain on the server.",
            ),
            (
                Effects {
                    folder_remains: Some(transfer::FolderRemains::RemoteCopyPartial),
                    ..Effects::default()
                },
                "SFTP folder copy cancelled. A partial destination folder may remain on the server.",
            ),
            (
                Effects {
                    archive_published: true,
                    ..Effects::default()
                },
                "SFTP job cancelled after the server confirmed archive publication.",
            ),
            (
                Effects {
                    folder_remains: Some(transfer::FolderRemains::LocalPublished),
                    ..Effects::default()
                },
                "SFTP folder download stopped after the complete local folder was published.",
            ),
            (
                Effects {
                    folder_remains: Some(transfer::FolderRemains::LocalPublicationUncertain),
                    ..Effects::default()
                },
                transfer::LOCAL_PUBLICATION_UNCERTAIN,
            ),
        ] {
            let (scheduler, request) = scheduled_request();
            scheduler.cancel(request.id).unwrap();
            thread::scope(|scope| {
                let (report, result) = mpsc::sync_channel(1);
                let (abort, aborted) = mpsc::sync_channel(1);
                // Test double for a worker whose read stays blocked until abort,
                // even though a previous mutation may already be acknowledged.
                let worker = scope.spawn(move || {
                    aborted.recv_timeout(Duration::from_secs(2)).unwrap();
                    report
                        .send(Err(Failure::failed("SFTP transport closed")))
                        .ok();
                    effects
                });
                let outcome = wait_for_worker(
                    &result,
                    &request.cancellation,
                    Instant::now() + Duration::from_secs(1),
                    || abort.send(()).unwrap(),
                    worker,
                )
                .unwrap_err();
                assert_eq!(outcome.outcome, RunOutcome::Cancelled);
                assert_eq!(outcome.detail, detail);
            });
        }
    }
}
