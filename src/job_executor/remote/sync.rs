use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use kortty_jobs::{RunOutcome, RunRequest, RunTrigger};
use kortty_ssh::{Endpoint, HostKey, SshSession, SshTarget, TrustStore, sftp::Sftp};

use crate::clipboard_policy::ClipboardPolicy;
use crate::job_executor::{Credentials, ExecutionPolicy, SavedAuth, connection};
use crate::scheduler_window::{ConnectionRef, SshCommand, sync};
use crate::sftp_window::scheduled::{Client, StandingLimits};

use super::{CANCELLATION_POLL, Completion, Failure, check_cancelled, open_subsystem};

const TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CLEANUP_GRACE: Duration = Duration::from_secs(2);
const REVIEW_REQUIRED: &str = "This scheduled SFTP copy needs a fresh folder review.";
const STANDING_STOPPED: &str = "The recurring SFTP approval expired or is no longer allowed.";
const REPLACEMENT_STOPPED: &str =
    "The scheduled replacement is no longer allowed or its reviewed SSH route changed.";

enum Authority<'a> {
    OneUse(sync::Receipt),
    Standing(&'a sync::standing::Approval, sync::standing::Firing),
}

fn plan_limits(limits: sync::standing::Limits) -> StandingLimits {
    StandingLimits {
        actions: usize::from(limits.max_actions),
        bytes: limits.max_bytes,
    }
}

fn sync_allowed(reference: &ConnectionRef) -> bool {
    let policy = ClipboardPolicy::load();
    policy.problem.is_none()
        && policy.file_transfer_allowed
        && policy.minimum_isolation.is_none()
        && connection::reference_allowed(reference, policy.server_access())
}

pub(crate) fn preview(
    request: sync::PreviewRequest,
    credentials: Credentials,
) -> async_channel::Receiver<Result<sync::Review, String>> {
    preview_with_trust(request, credentials, crate::ssh_connection::trust_store)
}

pub(in crate::job_executor) fn preview_with_trust(
    request: sync::PreviewRequest,
    credentials: Credentials,
    trust_store: impl FnOnce() -> io::Result<TrustStore> + Send + 'static,
) -> async_channel::Receiver<Result<sync::Review, String>> {
    let (sender, receiver) = async_channel::bounded(1);
    let _ = thread::Builder::new()
        .name("kortty-job-sftp-review".into())
        .spawn(move || {
            let result = (|| {
                if request.copy.standing.is_some() && !sync_allowed(&request.connection) {
                    return Err(STANDING_STOPPED.into());
                }
                if request.copy.replace_changed && !sync_allowed(&request.connection) {
                    return Err(REPLACEMENT_STOPPED.into());
                }
                let trust = trust_store().map_err(|error| error.to_string())?;
                let pins =
                    sync::pins(&request.connection, &trust).map_err(|error| error.to_string())?;
                if request.sudo != crate::scheduler_window::SudoMode::Disabled {
                    return super::privileged::preview_tree(request, credentials, &pins);
                }
                let (target, jumps) = credentials.into_route().map_err(|error| error.detail)?;
                let cancelled = || request.cancelled.load(Ordering::Acquire);
                let mut session =
                    open_pinned(target, jumps, &pins, cancelled).map_err(|error| error.detail)?;
                let scanned = supervise(&mut session, &cancelled, |client, stopped, _| {
                    let comparison = request.copy.comparison();
                    if let Some(settings) = request.copy.standing {
                        comparison
                            .preview_standing(client, plan_limits(settings.limits), &stopped)
                            .map(|preview| (preview.current, Some(preview.root_binding)))
                    } else {
                        comparison
                            .preview(client, &stopped)
                            .map(|preview| (preview, None))
                    }
                })
                .map_err(|error| error.detail)?;
                let (preview, standing_roots) = scanned?;
                Ok(sync::Review {
                    preview,
                    pins: sync::fingerprints(&pins),
                    standing_roots,
                })
            })();
            let _ = sender.send_blocking(result);
        });
    receiver
}

pub(super) fn run(
    request: &RunRequest<SshCommand>,
    policy: &ExecutionPolicy,
    copy: &sync::Copy,
    saved_auth: SavedAuth,
    trust_store: impl FnOnce() -> io::Result<TrustStore>,
) -> Result<(Completion, Option<String>), Failure> {
    let context = policy
        .sync
        .as_ref()
        .filter(|context| context.permits(request))
        .ok_or_else(|| {
            Failure::blocked(if request.trigger == RunTrigger::Cascade {
                "This cascade could not use the saved SFTP folder approval."
            } else {
                REVIEW_REQUIRED
            })
        })?;
    let reference = request
        .action
        .connection
        .as_ref()
        .ok_or_else(|| Failure::blocked(REVIEW_REQUIRED))?;
    let authority = match (copy.standing.is_some(), context.standing_approval()) {
        (true, Some((approval, firing))) if sync_allowed(reference) => {
            Authority::Standing(approval, *firing)
        }
        (false, None) => Authority::OneUse(
            context
                .load_receipt(copy.approval.as_deref())
                .map_err(|_| Failure::blocked(REVIEW_REQUIRED))?,
        ),
        _ => return Err(Failure::blocked(STANDING_STOPPED)),
    };
    let expected_pins = match &authority {
        Authority::OneUse(receipt) => &receipt.pins,
        Authority::Standing(approval, _) => &approval.pins,
    };
    let trust =
        trust_store().map_err(|_| Failure::failed("Could not access SSH host-key trust store"))?;
    let pins = sync::pins(reference, &trust).map_err(|_| Failure::blocked(REVIEW_REQUIRED))?;
    if &sync::fingerprints(&pins) != expected_pins {
        return Err(Failure::blocked(
            "The reviewed SSH host keys changed. Compare the folders again.",
        ));
    }
    let saved_auth = saved_auth.map_err(Failure::blocked)?;
    let (target, jumps) = connection::route(&request.action, saved_auth, &policy.server_access)?;
    let cancelled = || request.cancellation.is_requested();
    let live_policy = Mutex::new((Instant::now(), true));
    let stopped = || {
        if cancelled() {
            return true;
        }
        let Authority::Standing(approval, _) = &authority else {
            return false;
        };
        if !approval.active_at(SystemTime::now()) {
            return true;
        }
        let Ok(mut state) = live_policy.lock() else {
            return true;
        };
        if state.1 && state.0.elapsed() >= CANCELLATION_POLL {
            *state = (
                Instant::now(),
                sync_allowed(reference) && policy.require_target_groups(&request.action).is_ok(),
            );
        }
        !state.1
    };
    check_cancelled(&request.cancellation)?;
    policy.require_target_groups(&request.action)?;
    if let Authority::Standing(approval, firing) = &authority {
        let reserved =
            context
                .store
                .claim_standing(&request.job, approval, *firing, 0, 0, &stopped);
        check_cancelled(&request.cancellation)?;
        reserved.map_err(|_| {
            Failure::blocked("The recurring SFTP firing could not reserve an unused approval slot.")
        })?;
        if stopped()
            || !sync_allowed(reference)
            || !sync::pins(reference, &trust).is_ok_and(|current| current == pins)
        {
            return Err(Failure::blocked(STANDING_STOPPED));
        }
    }
    let replacement_allowed = || {
        policy.require_target_groups(&request.action).is_ok()
            && sync_allowed(reference)
            && sync::pins(reference, &trust).is_ok_and(|current| current == pins)
    };
    if copy.replace_changed && !replacement_allowed() {
        return Err(Failure::blocked(REPLACEMENT_STOPPED));
    }
    let opened = open_pinned_for_job(target, jumps, &pins, stopped, request, policy);
    check_cancelled(&request.cancellation)?;
    policy.require_target_groups(&request.action)?;
    if stopped() {
        return Err(Failure::blocked(STANDING_STOPPED));
    }
    let mut session = opened?;
    let claimed = AtomicBool::new(false);
    let applied = supervise(&mut session, &stopped, |client, stopped, disconnected| {
        let comparison = copy.comparison();
        match &authority {
            Authority::OneUse(receipt) => {
                let claim = || {
                    policy
                        .require_target_groups(&request.action)
                        .map_err(|error| io::Error::other(error.detail))?;
                    if copy.replace_changed && !replacement_allowed() {
                        return Err(io::Error::other(REPLACEMENT_STOPPED));
                    }
                    context
                        .store
                        .claim_with_wait(&request.job, receipt, stopped)?;
                    claimed.store(true, Ordering::Release);
                    Ok(())
                };
                if copy.replace_changed {
                    comparison.apply_authorized(
                        client,
                        receipt.proof,
                        &stopped,
                        &disconnected,
                        claim,
                        &|| {
                            if !replacement_allowed() {
                                return Err(io::Error::other(REPLACEMENT_STOPPED));
                            }
                            context.store.require_claimed(&request.job, receipt)
                        },
                    )
                } else {
                    comparison.apply(client, receipt.proof, &stopped, &disconnected, claim)
                }
            }
            Authority::Standing(approval, firing) => comparison.apply_standing(
                client,
                approval.anchors,
                plan_limits(approval.limits),
                &stopped,
                &disconnected,
                |actions, bytes| {
                    if stopped()
                        || policy.require_target_groups(&request.action).is_err()
                        || !sync_allowed(reference)
                        || !sync::pins(reference, &trust).is_ok_and(|current| current == pins)
                        || !approval.limits.allows(actions, bytes)
                    {
                        return Err(io::Error::other(STANDING_STOPPED));
                    }
                    context.store.require_standing_claimed(
                        &request.job,
                        approval,
                        *firing,
                        SystemTime::now(),
                    )?;
                    claimed.store(true, Ordering::Release);
                    Ok(())
                },
            ),
        }
    })?;
    let replacement_report = |applied: &crate::sftp_window::scheduled::Applied| {
        if !copy.replace_changed {
            return String::new();
        }
        let mut detail = format!("Replaced {} local files. ", applied.replaced);
        if let Some(prefix) = &applied.recovery_prefix {
            detail.push_str(&format!("Any retained originals are in sibling folders named {prefix} under the local destination. "));
        }
        detail
    };
    // Keep protocol diagnostics and staging names in the scheduler result.
    // Journal capture uses only confirmed publication counts and fixed text.
    let journal_note = match &applied {
        Ok(applied) if applied.error.is_some() => Some(format!(
            "Confirmed {} file publications and {} created folders. Review the scheduler result for the remaining state.",
            applied.copied + applied.replaced,
            applied.created,
        )),
        Err(_) => Some("The reviewed SFTP folders could not be verified.".into()),
        _ => None,
    };
    let (outcome, detail) = match applied {
        Ok(applied) if applied.error.is_none() => (
            RunOutcome::Success,
            format!(
                "{}Copied {} new files and created {} folders.",
                replacement_report(&applied),
                applied.copied,
                applied.created
            ),
        ),
        Ok(applied) => {
            let outcome = if cancelled() {
                RunOutcome::Cancelled
            } else if claimed.load(Ordering::Acquire) {
                RunOutcome::Failed { exit_code: None }
            } else {
                RunOutcome::Blocked
            };
            let detail = format!(
                "{}Published {} files and created {} folders. {}",
                replacement_report(&applied),
                applied.copied + applied.replaced,
                applied.created,
                applied.error.as_deref().expect("handled success"),
            );
            (outcome, detail)
        }
        Err(error) => (
            if cancelled() {
                RunOutcome::Cancelled
            } else {
                RunOutcome::Blocked
            },
            error,
        ),
    };
    Ok((
        Completion {
            id: request.id,
            outcome,
            detail: Some(detail),
            recording: None,
            swarm_report: None,
        },
        journal_note,
    ))
}

pub(in crate::job_executor) fn open_pinned(
    target: SshTarget,
    jumps: Vec<SshTarget>,
    pins: &[(Endpoint, HostKey)],
    cancelled: impl Fn() -> bool,
) -> Result<SshSession, Failure> {
    open_pinned_with(target, jumps, pins, |target, jumps, trust| {
        open_subsystem(target, jumps, trust, cancelled)
    })
}

pub(in crate::job_executor) fn open_pinned_for_job(
    target: SshTarget,
    jumps: Vec<SshTarget>,
    pins: &[(Endpoint, HostKey)],
    cancelled: impl Fn() -> bool,
    request: &RunRequest<SshCommand>,
    policy: &ExecutionPolicy,
) -> Result<SshSession, Failure> {
    open_pinned_with(target, jumps, pins, |target, jumps, trust| {
        super::open_group_subsystem(target, jumps, trust, cancelled, &request.action, policy)
    })
}

fn open_pinned_with(
    target: SshTarget,
    jumps: Vec<SshTarget>,
    pins: &[(Endpoint, HostKey)],
    open: impl FnOnce(SshTarget, Vec<SshTarget>, TrustStore) -> Result<SshSession, Failure>,
) -> Result<SshSession, Failure> {
    let expected = pins
        .iter()
        .find(|(endpoint, _)| endpoint == &target.endpoint)
        .map(|(_, key)| key)
        .ok_or_else(|| Failure::blocked(REVIEW_REQUIRED))?;
    let trust =
        TrustStore::from_pins(pins.to_vec()).map_err(|_| Failure::blocked(REVIEW_REQUIRED))?;
    let session = open(target, jumps, trust)?;
    if session.verified_host_key() == Some(expected) {
        Ok(session)
    } else {
        Err(Failure::blocked(REVIEW_REQUIRED))
    }
}

pub(in crate::job_executor) fn supervise<T: Send>(
    session: &mut SshSession,
    cancelled: &(impl Fn() -> bool + Sync),
    operation: impl FnOnce(&mut Client, &dyn Fn() -> bool, &dyn Fn() -> bool) -> T + Send,
) -> Result<T, Failure> {
    let unavailable = || Failure::failed("SFTP subsystem streams were unavailable");
    let reader = session.take_reader().ok_or_else(unavailable)?;
    let writer = session.take_writer().ok_or_else(unavailable)?;
    let abort = session.abort_handle();
    let stopped = AtomicBool::new(false);
    let disconnected = AtomicBool::new(false);
    let (sender, receiver) = mpsc::sync_channel(1);
    let deadline = Instant::now() + TIMEOUT;
    let result = thread::scope(|scope| {
        let stopped = &stopped;
        let disconnected = &disconnected;
        let worker = thread::Builder::new()
            .name("kortty-scheduled-copy".into())
            .spawn_scoped(scope, move || {
                let run = |client: &mut Client| {
                    operation(
                        client,
                        &|| stopped.load(Ordering::Acquire) || cancelled(),
                        &|| disconnected.load(Ordering::Acquire),
                    )
                };
                let result = Sftp::new(Box::new(reader) as _, Box::new(writer) as _)
                    .map(|mut client| run(&mut client))
                    .map_err(|_| Failure::failed("Could not initialize the SFTP subsystem"));
                let _ = sender.send(result);
            })
            .map_err(|_| Failure::failed("Could not start the SFTP copy worker"))?;
        let mut stop_deadline = None;
        let result = loop {
            match receiver.recv_timeout(CANCELLATION_POLL) {
                Ok(result) => break result,
                Err(RecvTimeoutError::Disconnected) => {
                    break Err(Failure::failed("The SFTP copy worker ended unexpectedly"));
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            let now = Instant::now();
            if stop_deadline.is_none() && (cancelled() || now >= deadline) {
                stopped.store(true, Ordering::Release);
                stop_deadline = Some(now + CLEANUP_GRACE);
            }
            if stop_deadline.is_some_and(|limit| now >= limit) {
                disconnected.store(true, Ordering::Release);
                abort.abort();
                break receiver
                    .recv()
                    .unwrap_or_else(|_| Err(Failure::failed("SFTP copy stopped")));
            }
        };
        worker
            .join()
            .map_err(|_| Failure::failed("SFTP copy worker failed"))?;
        result
    });
    let _ = session.close();
    result
}
