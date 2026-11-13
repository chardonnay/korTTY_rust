use std::io;
use std::path::Path;
use std::sync::Arc;

use kortty_jobs::{RunOutcome, RunRequest};
use kortty_ssh::TrustStore;

use crate::scheduler_window::SshCommand;
use crate::ssh_dialog::unsafe_text;

use super::{
    BatchAuth, BorrowedArchivePassword, Completion, ExecutionPolicy, Failure, check_cancelled,
    remote,
};

const MAX_TARGET_DETAIL: usize = 400;

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
    policy.require_target_groups(action)?;
    policy.check_elevation(action)?;
    action
        .validate_target_selection()
        .map_err(Failure::blocked)?;
    let mut single = action.as_ref().clone();
    let any_route_allowed = policy.any_route_allowed(action);
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
    if action.target_groups.is_some() {
        scoped_policy.group_scope = Some(Arc::clone(action));
    }
    let credentials = credentials.map_err(Failure::blocked)?;
    let total = action.targets().count();
    if total < 2 || total != credentials.len() {
        return Err(Failure::blocked(
            "Multi-target SSH authentication is unavailable",
        ));
    }
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
    for (target, credentials) in action.targets().zip(credentials) {
        if request.cancellation.is_requested() {
            break;
        }
        if let Err(error) = scoped_policy.require_target_groups(action) {
            summary.record(target.id, &Completion::from_result(request.id, Err(error)));
            break;
        }
        single.connection = Some(target.clone());
        let target_request = RunRequest {
            id: request.id,
            job: request.job.clone(),
            action: Arc::new(single.clone()),
            trigger: request.trigger,
            started_at: request.started_at,
            cancellation: request.cancellation.clone(),
        };
        let completed = super::run(
            &target_request,
            None,
            None,
            &scoped_policy,
            credentials.map(Some),
            archive_password,
            &mut shared_trust,
        );
        summary.record(target.id, &completed);
        if completed.outcome == RunOutcome::Cancelled {
            break;
        }
    }
    Ok(summary.finish(request, total))
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
    fn record(&mut self, id: u64, completion: &Completion) {
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
            detail.truncate(detail.floor_char_boundary(MAX_TARGET_DETAIL));
        }
        self.targets.push(detail);
    }

    fn finish(self, request: &RunRequest<SshCommand>, total: usize) -> Completion {
        let outcome = if request.cancellation.is_requested() || self.cancelled > 0 {
            RunOutcome::Cancelled
        } else if self.failed > 0 {
            RunOutcome::Failed {
                exit_code: self.first_failure,
            }
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
            recording: None,
            swarm_report: None,
        }
    }
}
