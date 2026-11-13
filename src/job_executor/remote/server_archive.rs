use std::io;

use kortty_jobs::RunRequest;
use kortty_server_archive::protocol::{Reply, Request};
use kortty_ssh::{SshTarget, TrustStore};

use crate::clipboard_policy::ClipboardPolicy;
use crate::job_executor::connection;
use crate::scheduler_window::{Archive, SshCommand, sync};
use crate::sftp_window::exchange_server_archive;
use crate::ssh_connection::ExecOpener;

use super::{Completion, ExecutionPolicy, Failure, check_cancelled};

pub(super) fn run(
    request: &RunRequest<SshCommand>,
    archive: &Archive,
    policy: &ExecutionPolicy,
    target: SshTarget,
    jumps: Vec<SshTarget>,
    trust: &TrustStore,
) -> Result<Completion, Failure> {
    let spec = archive.server_spec().map_err(Failure::blocked)?;
    let reference = request.action.connection.as_ref().ok_or_else(|| {
        Failure::blocked("Choose one saved SSH route for server-only archive creation.")
    })?;
    let pins = sync::pins(reference, trust).map_err(|_| {
        Failure::blocked("Verify the SSH route host keys before creating an archive.")
    })?;
    let authorize = || {
        policy
            .require_target_groups(&request.action)
            .map_err(|failure| io::Error::other(failure.detail))?;
        let policy = ClipboardPolicy::load();
        if policy.problem.is_some()
            || !policy.minimum_isolation.is_none()
            || !connection::reference_allowed(reference, policy.server_access())
            || request.cancellation.is_requested()
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The server archive route is no longer allowed.",
            ));
        }
        Ok(())
    };
    authorize()
        .map_err(|_| Failure::blocked("Current policy forbids this server archive route."))?;
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
            authorize,
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
        let reply = exchange_server_archive(
            &opener,
            &Request::ApplyCreation {
                spec: spec.clone(),
                token: review.token,
            },
            stopped,
            authorize,
        );
        match reply {
            Ok(Reply::Applied { destination }) if destination == spec.destination => {
                let mut completion = Completion::from_result(request.id, Ok(0));
                completion.detail = Some("Created the reviewed archive on the server.".into());
                Ok(completion)
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
