use std::io;
use std::path::Path;
use std::sync::Arc;

use kortty_jobs::{RunOutcome, RunRequest};
use kortty_ssh::TrustStore;

use crate::scheduler_window::{RemoteAction, SshCommand};
use crate::ssh_dialog::unsafe_text;

use super::{
    BatchAuth, BorrowedArchivePassword, Completion, ExecutionPolicy, Failure, check_cancelled,
    output, remote,
};

const MAX_TARGET_DETAIL: usize = 400;
// A reviewed local path can use 4 KiB. Keep the recovery suffix and status too.
const MAX_EXISTING_RSYNC_TARGET_DETAIL: usize = 4608;

pub(super) fn run(
    request: &RunRequest<SshCommand>,
    snippets: Option<&Path>,
    policy: &ExecutionPolicy,
    credentials: BatchAuth,
    archive_password: BorrowedArchivePassword<'_>,
    trust_store: impl FnOnce() -> io::Result<TrustStore>,
) -> Completion {
    execute(
        request,
        snippets,
        policy,
        credentials,
        archive_password,
        trust_store,
    )
    .unwrap_or_else(|failure| Completion::from_result(request.id, Err(failure)))
}

fn execute(
    request: &RunRequest<SshCommand>,
    snippets: Option<&Path>,
    policy: &ExecutionPolicy,
    credentials: BatchAuth,
    archive_password: BorrowedArchivePassword<'_>,
    trust_store: impl FnOnce() -> io::Result<TrustStore>,
) -> Result<Completion, Failure> {
    let action = &request.action;
    check_cancelled(&request.cancellation)?;
    action.require_executable().map_err(Failure::blocked)?;
    policy.require_target_groups(action)?;
    policy.check_elevation(action)?;
    action
        .validate_target_selection()
        .map_err(Failure::blocked)?;
    let mut single = action.as_ref().clone();
    let any_route_allowed = policy.any_route_allowed(action);
    let rsync = action
        .remote
        .as_ref()
        .and_then(RemoteAction::rsync_transfer);
    let existing_rsync = crate::scheduler_window::rsync_authority::uses_reviewed_receiver(action);
    if action.remote.is_some() {
        let operation = action.remote_action().map_err(Failure::blocked)?;
        if operation.transfers_files() && !policy.file_transfer_allowed {
            return Err(Failure::blocked(
                "Application policy forbids scheduled file transfers",
            ));
        }
        if any_route_allowed {
            remote::require_archive_password(operation, archive_password)?;
        }
    } else if any_route_allowed {
        // Freeze the resolved command for every target in this run.
        single.command = action
            .command_for_exec(snippets)
            .map_err(Failure::blocked)?
            .into_owned();
        single.script = None;
        single.snippet = None;
        single.working_directory = None;
    }
    single.additional_connections.clear();
    single.target_groups = None;
    let mut scoped_policy = policy.clone();
    if action.target_groups.is_some() || rsync.is_some() {
        scoped_policy.group_scope = Some(Arc::clone(action));
    }
    let credentials = credentials.map_err(Failure::blocked)?;
    let total = action.targets().count();
    if total < 2 || total != credentials.len() {
        return Err(Failure::blocked(
            "Multi-target SSH authentication is unavailable",
        ));
    }
    let output = if action.remote.is_none() && action.virtual_terminal.is_none() {
        output::Scope::new(
            action.journal_detail,
            &scoped_policy,
            total,
            credentials
                .iter()
                .filter_map(|credentials| credentials.as_ref().ok()),
        )
    } else {
        None
    };
    let mut captured = output::Combined::default();
    // Open the trust store only when an allowed target actually reaches SSH setup.
    let mut open_trust = Some(trust_store);
    let mut trust = None;
    let mut shared_trust = || {
        trust
            .get_or_insert_with(|| open_trust.take().expect("first trust lookup")())
            .as_ref()
            .cloned()
            .map_err(|_| io::Error::other("Could not access SSH host-key trust store"))
    };
    let mut summary = Summary::default();
    for (index, (target, credentials)) in action.targets().zip(credentials).enumerate() {
        if request.cancellation.is_requested() {
            break;
        }
        if let Err(error) = scoped_policy.require_target_groups(action) {
            summary.record(
                target.id,
                &Completion::from_result(request.id, Err(error)),
                existing_rsync,
            );
            break;
        }
        single.connection = Some(target.clone());
        if let Some((transfer, upload)) = rsync {
            let transfer = transfer
                .for_target(index, upload)
                .map_err(Failure::blocked)?;
            single.remote = Some(if upload {
                RemoteAction::RsyncUpload(transfer)
            } else {
                RemoteAction::RsyncDownload(transfer)
            });
        }
        let target_request = RunRequest {
            id: request.id,
            job: request.job.clone(),
            action: Arc::new(single.clone()),
            trigger: request.trigger,
            started_at: request.started_at,
            cancellation: request.cancellation.clone(),
        };
        let mut completed = super::run_with_output(
            &target_request,
            None,
            None,
            (&scoped_policy, output.as_ref()),
            credentials.map(Some),
            archive_password,
            &mut shared_trust,
        );
        if let (Some(scope), Some(streams)) = (&output, completed.output.take()) {
            scope.append(&mut captured, target.id, streams);
        }
        summary.record(target.id, &completed, existing_rsync);
        if completed.outcome == RunOutcome::Cancelled {
            break;
        }
    }
    let mut completion = summary.finish(request, total, existing_rsync);
    if completion.outcome != RunOutcome::Cancelled {
        completion.output = output.and_then(|scope| scope.finish_combined(captured));
    }
    Ok(completion)
}

#[derive(Default)]
struct Summary {
    success: usize,
    failed: usize,
    blocked: usize,
    cancelled: usize,
    first_failure: Option<i32>,
    targets: Vec<String>,
}

impl Summary {
    fn record(&mut self, id: u64, completion: &Completion, existing_rsync: bool) {
        let status = match completion.outcome {
            RunOutcome::Success => {
                self.success += 1;
                "success".into()
            }
            RunOutcome::Failed { exit_code } => {
                if self.failed == 0 {
                    self.first_failure = exit_code;
                }
                self.failed += 1;
                exit_code.map_or_else(|| "failed".into(), |code| format!("failed (exit {code})"))
            }
            RunOutcome::Blocked => {
                self.blocked += 1;
                "blocked".into()
            }
            RunOutcome::Cancelled => {
                self.cancelled += 1;
                "cancelled".into()
            }
        };
        let mut detail = format!("Target {} · Profile {id} {status}", self.targets.len() + 1);
        if let Some(note) = completion
            .detail
            .as_deref()
            .filter(|note| !unsafe_text(note))
        {
            detail.push_str(" – ");
            detail.push_str(note);
            let limit = if existing_rsync {
                MAX_EXISTING_RSYNC_TARGET_DETAIL
            } else {
                MAX_TARGET_DETAIL
            };
            detail.truncate(detail.floor_char_boundary(limit));
        }
        self.targets.push(detail);
    }

    fn finish(
        self,
        request: &RunRequest<SshCommand>,
        total: usize,
        existing_rsync: bool,
    ) -> Completion {
        let cancelled = request.cancellation.is_requested() || self.cancelled > 0;
        let outcome = if self.failed > 0 && (existing_rsync || !cancelled) {
            RunOutcome::Failed {
                exit_code: self.first_failure,
            }
        } else if cancelled {
            RunOutcome::Cancelled
        } else if self.blocked > 0 {
            RunOutcome::Blocked
        } else {
            RunOutcome::Success
        };
        let mut detail = format!(
            "{} of {total} targets succeeded · {} failed · {} blocked",
            self.success, self.failed, self.blocked
        );
        let not_started = total - self.targets.len();
        if self.cancelled > 0 || not_started > 0 {
            detail.push_str(&format!(
                " · {} cancelled · {not_started} not started",
                self.cancelled
            ));
        }
        for target in self.targets {
            detail.push_str(" · ");
            detail.push_str(&target);
        }
        Completion {
            id: request.id,
            outcome,
            detail: Some(detail),
            output: None,
            recording: None,
            swarm_report: None,
        }
    }
}

#[cfg(test)]
#[test]
fn existing_rsync_batch_keeps_all_long_recoveries_within_the_journal_budget() {
    use kortty_jobs::{Cancellation, JobId, RunTrigger};
    use std::time::SystemTime;

    let id = serde_json::from_value(serde_json::json!(1)).unwrap();
    let action = serde_json::from_value(serde_json::json!({})).unwrap();
    let request = RunRequest {
        id,
        job: JobId::new("recovery-batch").unwrap(),
        action: Arc::new(action),
        trigger: RunTrigger::Manual,
        started_at: SystemTime::now(),
        cancellation: Cancellation::default(),
    };
    let mut summary = Summary::default();
    let mut ordinary = Summary::default();
    let parent = format!("/{}", vec!["b".repeat(254); 16].join("/"));
    let mut recoveries = Vec::new();
    for target in 1..=8 {
        let recovery =
            format!("{parent}/target-{target}/.kortty-{target:032x}.kortty-part/original");
        let mut result = Completion::from_result(id, Err(Failure::failed("Publication uncertain")));
        result.detail = Some(format!(
            "Check the last reported recovery path {recovery} before retrying."
        ));
        summary.record(target, &result, true);
        ordinary.record(target, &result, false);
        recoveries.push(recovery);
    }
    let detail = summary.finish(&request, 8, true).detail.unwrap();
    assert!(detail.len() <= kortty_jobs::MAX_JOURNAL_DETAIL_BYTES);
    assert!(recoveries.iter().all(|path| detail.contains(path)));
    assert!(
        ordinary
            .targets
            .iter()
            .all(|target| target.len() == MAX_TARGET_DETAIL)
    );
}
