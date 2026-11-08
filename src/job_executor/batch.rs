use std::io;
use std::path::Path;
use std::sync::Arc;

use kortty_jobs::{RunOutcome, RunRequest};
use kortty_ssh::TrustStore;

use crate::scheduler_window::SshCommand;
use crate::ssh_dialog::unsafe_text;

use super::{BatchAuth, BorrowedArchivePassword, Completion, Failure, check_cancelled, remote};

const MAX_TARGET_DETAIL: usize = 400;

pub(super) fn run(
    request: &RunRequest<SshCommand>,
    snippets: Option<&Path>,
    file_transfer_allowed: bool,
    credentials: BatchAuth,
    archive_password: BorrowedArchivePassword<'_>,
    trust_store: impl FnOnce() -> io::Result<TrustStore>,
) -> Completion {
    execute(
        request,
        snippets,
        file_transfer_allowed,
        credentials,
        archive_password,
        trust_store,
    )
    .unwrap_or_else(|failure| Completion::from_result(request.id, Err(failure)))
}

fn execute(
    request: &RunRequest<SshCommand>,
    snippets: Option<&Path>,
    file_transfer_allowed: bool,
    credentials: BatchAuth,
    archive_password: BorrowedArchivePassword<'_>,
    trust_store: impl FnOnce() -> io::Result<TrustStore>,
) -> Result<Completion, Failure> {
    let action = &request.action;
    check_cancelled(&request.cancellation)?;
    action
        .validate_target_selection()
        .map_err(Failure::blocked)?;
    let mut single = action.as_ref().clone();
    if action.remote.is_some() {
        let operation = action.remote_action().map_err(Failure::blocked)?;
        if operation.transfers_files() && !file_transfer_allowed {
            return Err(Failure::blocked(
                "Application policy forbids scheduled file transfers",
            ));
        }
        remote::require_archive_password(operation, archive_password)?;
    } else {
        // Freeze the resolved command for every target in this run.
        single.command = action
            .command_for_exec(snippets)
            .map_err(Failure::blocked)?
            .into_owned();
        single.script = None;
        single.snippet = None;
    }
    single.additional_connections.clear();
    let credentials = credentials.map_err(Failure::blocked)?;
    let total = action.targets().count();
    if total < 2 || total != credentials.len() {
        return Err(Failure::blocked(
            "Multi-target SSH authentication is unavailable",
        ));
    }
    let trust =
        trust_store().map_err(|_| Failure::failed("Could not access SSH host-key trust store"))?;
    let mut summary = Summary::default();
    for (target, credentials) in action.targets().zip(credentials) {
        if request.cancellation.is_requested() {
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
            file_transfer_allowed,
            credentials.map(Some),
            archive_password,
            || Ok(trust.clone()),
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
        }
    }
}
