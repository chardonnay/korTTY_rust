//! Executes one target's approved commands through a pinned, non-PTY SSH connection.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use kortty_ssh::{ConnectError, ExecConnection};

use super::{
    Cancellation, ClipboardPolicy, Completion, Context, Failure, Kind, Mask, Outcome, POLL, Policy,
    Probe, RedactionSecrets, Redactor, RunOutcome, RunRequest, RunTrigger, SshCommand, Status,
    Store, Target, TrustStore, check_cancelled, checked_proposal, current_route_policy,
    require_approved_profile, require_current_route, require_mask_unchanged, seal_targets,
    terminal_agent,
};
use crate::job_executor::automation::{IterativeCapture, IterativeTarget};
use crate::scheduler_window::{
    ConnectionRef,
    agent::{
        initial_swarm_probe,
        proposal::{MAX_SWARM_STREAM_BYTES, Prompt, SwarmEvidence},
    },
};
use crate::terminal_agent::swarm::headless::Review;

const EXECUTION_LIMIT: Duration = Duration::from_secs(125);
const MAX_COMMANDS: usize = 3;
const MAX_COMMAND_BYTES: usize = 1024;

#[derive(Clone, Copy)]
pub(crate) enum FiringPermit {
    Execute,
    Journal,
}

pub(crate) enum FiringWork {
    Commands(Vec<String>),
    InitialProbe(Arc<Prompt>),
}

#[derive(Clone)]
pub(crate) struct FiringResult {
    pub(crate) completion: Completion,
    pub(crate) evidence: Option<Vec<SwarmEvidence>>,
}

impl FiringResult {
    fn failed(id: kortty_jobs::RunId, failure: Failure) -> Self {
        Self {
            completion: Completion::from_result(id, Err(failure)),
            evidence: None,
        }
    }
}

pub(crate) fn spawn(
    request: Arc<RunRequest<SshCommand>>,
    target_index: usize,
    context: Result<Context, &'static str>,
    shared_secrets: Option<RedactionSecrets>,
    capture: Option<IterativeCapture>,
    work: FiringWork,
    still_current: impl Fn(FiringPermit) -> Result<(), &'static str> + Send + Sync + 'static,
) -> async_channel::Receiver<FiringResult> {
    let (sender, receiver) = async_channel::bounded(1);
    let id = request.id;
    let worker = sender.clone();
    if thread::Builder::new()
        .name("kortty-scheduled-agent-firing".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(
                    &request,
                    target_index,
                    context,
                    (shared_secrets, capture),
                    work,
                    still_current,
                    ClipboardPolicy::load,
                )
            }))
            .unwrap_or_else(|_| Err(Failure::failed("The scheduled Agent worker failed.")));
            let _ = worker
                .send_blocking(result.unwrap_or_else(|error| FiringResult::failed(id, error)));
        })
        .is_err()
    {
        let _ = sender.try_send(FiringResult::failed(
            id,
            Failure::failed("Could not start the scheduled Agent worker."),
        ));
    }
    receiver
}

fn run(
    request: &RunRequest<SshCommand>,
    target_index: usize,
    context: Result<Context, &'static str>,
    (shared_secrets, capture): (Option<RedactionSecrets>, Option<IterativeCapture>),
    work: FiringWork,
    still_current: impl Fn(FiringPermit) -> Result<(), &'static str> + Send + Sync + 'static,
    load_policy: impl Fn() -> ClipboardPolicy + Send + Sync + 'static,
) -> Result<FiringResult, Failure> {
    check_cancelled(&request.cancellation)?;
    if !matches!(request.trigger, RunTrigger::Manual | RunTrigger::Scheduled) {
        return Err(Failure::blocked(
            "Scheduled Agent requires Run Now or a calendar start.",
        ));
    }
    let action = request
        .action
        .agent
        .as_ref()
        .filter(|action| {
            matches!(
                (action.kind, action.probe),
                (Kind::Agent, Probe::PerFiring) | (Kind::Swarm, Probe::SwarmIterative)
            )
        })
        .ok_or_else(|| Failure::blocked("The reviewed scheduled Agent action is unavailable."))?;
    action.validate(&request.action).map_err(Failure::blocked)?;
    request
        .action
        .validate_target_selection()
        .map_err(Failure::blocked)?;
    let approval = action
        .approval
        .as_ref()
        .ok_or_else(|| Failure::blocked("The scheduled Agent job scope is unavailable."))?;
    if approval.profile.id != action.profile {
        return Err(Failure::blocked(
            "The reviewed scheduled Agent profile changed.",
        ));
    }
    let approved = reviewed_target(&request.action, &approval.targets, target_index)?;
    let reference = &approved.reference;
    let directory = request
        .action
        .working_directory
        .as_deref()
        .ok_or_else(route_unavailable)?;
    let Context {
        transport,
        targets,
        target_vault_revisions,
        connection_store,
        trust,
        log_policy,
        mask,
        ..
    } = context.map_err(Failure::blocked)?;
    require_approved_profile(&approval.profile, &transport).map_err(Failure::blocked)?;
    if targets.len() != approval.targets.len() {
        return Err(route_unavailable());
    }
    let secrets = command_secrets(shared_secrets, &targets)?;
    let execution = targets
        .into_iter()
        .nth(target_index)
        .ok_or_else(route_unavailable)?
        .map_err(Failure::blocked)?;
    if execution.review.directory() != directory
        || execution.review.reference() != reference
        || &execution.current != reference
    {
        return Err(route_unavailable());
    }
    let initial_probe = matches!(&work, FiringWork::InitialProbe(_));
    let commands = match work {
        FiringWork::Commands(commands) => {
            prepare_commands(commands, directory, &secrets, &log_policy, mask)?
        }
        FiringWork::InitialProbe(prompt) => {
            if action.probe != Probe::SwarmIterative
                || !approval.has_initial_swarm_probe()
                || !approval.generation_matches_for(target_index, &prompt)
            {
                return Err(Failure::blocked(
                    "The scheduled Swarm environment probe is no longer reviewed.",
                ));
            }
            let redactor = Redactor::for_journal(secrets.clone(), log_policy.clone());
            vec![prepare_initial_probe(
                directory,
                prompt.swarm_probe_command(),
                &redactor,
                mask,
            )?]
        }
    };
    let capture = capture.map(|capture| {
        let target = IterativeTarget {
            approved: approved.clone(),
            connection_store: connection_store.clone(),
            trust: trust.clone(),
            vault_revision: target_vault_revisions.get(target_index).cloned().flatten(),
        };
        (capture, target)
    });
    let authority = Arc::new(Authority {
        cancellation: request.cancellation.clone(),
        action: Arc::clone(&request.action),
        kind: action.kind,
        reference: reference.clone(),
        approved: approved.clone(),
        review: execution.review,
        connection_store,
        trust,
        log_policy,
        secrets,
        mask,
        still_current,
        load_policy,
        denied: Mutex::new(None),
    });
    let cancel = terminal_agent::Cancel::default();
    let deadline = Instant::now() + EXECUTION_LIMIT;
    let result = thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let cancelled = || cancel.requested() || request.cancellation.is_requested();
            authority.fresh(None)?;
            let (target, jumps) = execution
                .credentials
                .into_swarm_route()
                .map_err(Failure::blocked)?;
            if !authority.review.matches_route(&target, &jumps) {
                return Err(route_unavailable());
            }
            let entry = jumps.first().unwrap_or(&target);
            let tcp = crate::ssh_connection::connect_transport_with_options(
                &entry.endpoint,
                execution.startup,
                cancelled,
            )
            .map_err(|_| Failure::failed("Could not connect to the reviewed SSH route."))?;
            let pins = authority
                .review
                .trust_snapshot(&authority.reference, &authority.trust)
                .map_err(|_| route_unavailable())?;
            let connection = ExecConnection::connect_preconnected(
                tcp,
                (target, jumps),
                pins,
                execution.startup,
                cancelled,
                || authority.io_guard(None),
            )
            .map_err(|error| match error {
                ConnectError::UnknownHostKey { .. } | ConnectError::ChangedHostKey { .. } => {
                    route_unavailable()
                }
                _ => authority.take_failure("Could not authenticate the reviewed SSH route."),
            })?;
            let opener = connection
                .exec_opener()
                .ok_or_else(|| Failure::failed("The reviewed SSH command connection closed."))?;
            let total = commands.len();
            let mut evidence = (action.probe == Probe::SwarmIterative).then(Vec::new);
            let mut transcript = Vec::new();
            let result = run_sequence(commands, |ReviewedCommand { raw, wire }| {
                check_cancelled(&request.cancellation)?;
                let evidence_command = evidence.as_ref().map(|_| raw.clone());
                let journal_command = capture.as_ref().map(|_| raw.clone());
                let current = Arc::clone(&authority);
                let cancelled = cancel.clone();
                let cancellation = request.cancellation.clone();
                let lease = opener
                    .open_guarded(
                        &wire,
                        move || cancelled.requested() || cancellation.is_requested(),
                        move || current.io_guard(Some(&raw)),
                    )
                    .map_err(|_| {
                        authority.take_failure("Could not open the reviewed SSH command channel.")
                    })?;
                let output = terminal_agent::capture_ssh_lease(lease, &cancel).ok();
                let status = output.as_ref().map(|output| output.status);
                if let (Some(command), Some(output), Some(Status::Exited(exit))) =
                    (evidence_command, &output, status)
                {
                    evidence = evidence.take().and_then(|mut items| {
                        items.push(authority.evidence(command, exit, output)?);
                        Some(items)
                    });
                }
                if let Some(command) = journal_command {
                    transcript.push((command, output));
                }
                status.ok_or_else(|| {
                    Failure::failed("The reviewed SSH command streams are unavailable.")
                })
            });
            // A nonzero exit is usable evidence. An interrupted channel, denied
            // command or masking failure leaves the batch ineligible to continue.
            let completed = match &result {
                Ok(()) => total,
                Err((ordinal, _)) => *ordinal,
            };
            let evidence = evidence.filter(|items| items.len() == completed);
            let (outcome, detail) = match result {
                Ok(()) if initial_probe => (
                    RunOutcome::Success,
                    "The reviewed environment probe completed.".into(),
                ),
                Ok(()) => (
                    RunOutcome::Success,
                    format!("Reviewed commands completed ({total} of {total})."),
                ),
                Err((_, failure)) if initial_probe => (
                    failure.outcome,
                    format!("Environment probe: {}", failure.detail),
                ),
                Err((ordinal, failure)) => (
                    failure.outcome,
                    format!("Command {ordinal} of {total}: {}", failure.detail),
                ),
            };
            Ok((
                FiringResult {
                    completion: Completion {
                        id: request.id,
                        outcome,
                        detail: Some(detail),
                        recording: None,
                        swarm_report: None,
                    },
                    evidence,
                },
                transcript,
            ))
        });
        while !worker.is_finished() {
            if request.cancellation.is_requested() || Instant::now() >= deadline {
                cancel.request();
                break;
            }
            thread::sleep(POLL);
        }
        worker.join()
    });
    let result =
        result.unwrap_or_else(|_| Err(Failure::failed("The scheduled Agent execution failed.")));
    if let (Some((capture, target)), Ok((_, transcript))) = (&capture, &result)
        && !transcript.is_empty()
    {
        if (authority.still_current)(FiringPermit::Journal).is_ok() {
            // The journal never holds the SSH lease or spends the command deadline.
            for (command, output) in transcript {
                capture.command(
                    target,
                    &authority.secrets,
                    &(authority.load_policy)(),
                    command,
                );
                capture.output(
                    target,
                    &authority.secrets,
                    &(authority.load_policy)(),
                    output.as_ref(),
                );
            }
        } else {
            capture.stop();
        }
    }
    check_cancelled(&request.cancellation)?;
    if cancel.requested() {
        return Err(Failure::failed(
            "The reviewed scheduled Agent firing timed out.",
        ));
    }
    result.map(|(result, _)| result)
}

fn reviewed_target<'a>(
    action: &SshCommand,
    approved: &'a [Target],
    target_index: usize,
) -> Result<&'a Target, Failure> {
    if !approved
        .iter()
        .map(|target| &target.reference)
        .eq(action.targets())
    {
        return Err(route_unavailable());
    }
    approved.get(target_index).ok_or_else(route_unavailable)
}

fn command_secrets(
    shared: Option<RedactionSecrets>,
    targets: &[Result<super::Execution, &'static str>],
) -> Result<RedactionSecrets, Failure> {
    match shared {
        Some(secrets) => Ok(secrets),
        None if targets.len() == 1 => seal_targets(targets).map_err(Failure::blocked),
        None => Err(Failure::blocked(
            "The shared scheduled Agent secret mask is unavailable.",
        )),
    }
}

struct ReviewedCommand {
    raw: String,
    wire: String,
}

fn prepare_initial_probe(
    directory: &str,
    reviewed_wire: Option<&str>,
    redactor: &Redactor,
    mask: Mask,
) -> Result<ReviewedCommand, Failure> {
    let wire = initial_swarm_probe(directory)
        .filter(|wire| wire.len() <= kortty_ssh::MAX_EXEC_COMMAND_BYTES)
        .filter(|wire| reviewed_wire == Some(wire.as_str()))
        .ok_or_else(|| Failure::blocked("The exact reviewed Swarm environment probe changed."))?;
    require_mask_unchanged(redactor, mask, &wire, directory)?;
    Ok(ReviewedCommand {
        raw: wire.clone(),
        wire,
    })
}

fn prepare_commands(
    commands: Vec<String>,
    directory: &str,
    secrets: &RedactionSecrets,
    policy: &Policy,
    mask: Mask,
) -> Result<Vec<ReviewedCommand>, Failure> {
    if !(1..=MAX_COMMANDS).contains(&commands.len()) {
        return Err(Failure::blocked(
            "Review one to three commands for this Agent firing.",
        ));
    }
    let redactor = Redactor::for_journal(secrets.clone(), policy.clone());
    commands
        .into_iter()
        .map(|raw| {
            if raw.trim().is_empty()
                || raw.len() > MAX_COMMAND_BYTES
                || !terminal_agent::valid_target_label(&raw)
                || raw.contains("***")
            {
                return Err(Failure::blocked(
                    "The exact reviewed SSH command is unavailable.",
                ));
            }
            require_mask_unchanged(&redactor, mask, &raw, directory)?;
            let wire =
                terminal_agent::wrap_ssh_command(directory, &checked_proposal(&raw, directory))
                    .map_err(|_| {
                        Failure::blocked("The reviewed SSH command exceeds the execution limit.")
                    })?;
            if wire.len() > kortty_ssh::MAX_EXEC_COMMAND_BYTES {
                return Err(Failure::blocked(
                    "The reviewed SSH command exceeds the execution limit.",
                ));
            }
            Ok(ReviewedCommand { raw, wire })
        })
        .collect()
}

struct Authority<Guard, LoadPolicy> {
    cancellation: Cancellation,
    action: Arc<SshCommand>,
    kind: Kind,
    reference: ConnectionRef,
    approved: Target,
    review: Arc<Review>,
    connection_store: PathBuf,
    trust: TrustStore,
    log_policy: Policy,
    secrets: RedactionSecrets,
    mask: Mask,
    still_current: Guard,
    load_policy: LoadPolicy,
    denied: Mutex<Option<Failure>>,
}

impl<Guard, LoadPolicy> Authority<Guard, LoadPolicy>
where
    Guard: Fn(FiringPermit) -> Result<(), &'static str>,
    LoadPolicy: Fn() -> ClipboardPolicy,
{
    fn fresh(&self, command: Option<&str>) -> Result<Policy, Failure> {
        check_cancelled(&self.cancellation)?;
        (self.still_current)(FiringPermit::Execute).map_err(Failure::blocked)?;
        check_cancelled(&self.cancellation)?;
        let policy = current_route_policy(self.kind, &[&self.reference], &self.load_policy)
            .map_err(Failure::blocked)?;
        if !policy
            .terminal_logging
            .equivalent_redactions(&self.log_policy)
        {
            return Err(Failure::blocked("The managed AI redaction rules changed."));
        }
        let store = Store::open(self.connection_store.clone()).map_err(|_| route_unavailable())?;
        self.action
            .require_current_target_groups(&store)
            .map_err(Failure::blocked)?;
        require_current_route(&store, &self.reference, &self.approved, &self.trust)?;
        self.review
            .trust_snapshot(&self.reference, &self.trust)
            .map_err(|_| route_unavailable())?;
        if let Some(command) = command {
            let redactor =
                Redactor::for_journal(self.secrets.clone(), policy.terminal_logging.clone());
            require_mask_unchanged(&redactor, self.mask, command, self.review.directory())?;
        }
        check_cancelled(&self.cancellation)?;
        Ok(policy.terminal_logging)
    }

    fn io_guard(&self, command: Option<&str>) -> io::Result<()> {
        self.fresh(command).map(drop).map_err(|failure| {
            let error = io::Error::new(io::ErrorKind::PermissionDenied, failure.detail);
            *self
                .denied
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(failure);
            error
        })
    }

    fn evidence(&self, command: String, exit: u32, output: &Outcome) -> Option<SwarmEvidence> {
        let policy = self.fresh(None).ok()?;
        let redactor = Redactor::for_journal(self.secrets.clone(), policy);
        let (stdout, stdout_limited) = masked_stream(&redactor, self.mask, &output.stdout.text())?;
        let (stderr, stderr_limited) = masked_stream(&redactor, self.mask, &output.stderr.text())?;
        Some(SwarmEvidence {
            command,
            exit,
            stdout,
            stderr,
            truncated: stdout_limited
                || stderr_limited
                || output.stdout.truncated()
                || output.stderr.truncated(),
        })
    }

    fn take_failure(&self, fallback: &'static str) -> Failure {
        self.denied
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .unwrap_or_else(|| Failure::failed(fallback))
    }
}

fn masked_stream(redactor: &Redactor, mask: Mask, raw: &str) -> Option<(String, bool)> {
    let mut text = mask(redactor, raw).ok()?;
    let truncated = text.len() > MAX_SWARM_STREAM_BYTES;
    text.truncate(text.floor_char_boundary(MAX_SWARM_STREAM_BYTES));
    Some((text, truncated))
}

fn route_unavailable() -> Failure {
    Failure::blocked("The reviewed SSH route or host key changed.")
}

fn run_sequence(
    commands: Vec<ReviewedCommand>,
    mut execute: impl FnMut(ReviewedCommand) -> Result<Status, Failure>,
) -> Result<(), (usize, Failure)> {
    for (index, command) in commands.into_iter().enumerate() {
        let status = execute(command).map_err(|failure| (index + 1, failure))?;
        let failure = match status {
            Status::Exited(0) => continue,
            Status::Exited(code) => Failure {
                outcome: RunOutcome::Failed {
                    exit_code: i32::try_from(code).ok(),
                },
                detail: "Reviewed SSH command failed.",
            },
            Status::Cancelled => Failure {
                outcome: RunOutcome::Cancelled,
                detail: "Reviewed SSH command cancelled.",
            },
            Status::TimedOut => Failure::failed("The reviewed SSH command timed out."),
            Status::TransportFailed | Status::MissingExitCode => {
                Failure::failed("The reviewed SSH command ended unexpectedly.")
            }
        };
        return Err((index + 1, failure));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_logging::Secrets;

    fn fixture_mask(_: &Redactor, text: &str) -> Result<String, String> {
        Ok(text.replace("fixture-secret", "***"))
    }

    fn prepare(raw: &[&str]) -> Result<Vec<ReviewedCommand>, Failure> {
        prepare_commands(
            raw.iter().map(|command| (*command).into()).collect(),
            "/srv/example directory",
            &Secrets::default().seal().unwrap(),
            &Policy::default(),
            fixture_mask,
        )
    }

    #[test]
    fn reviewed_firing_preserves_order_and_stops_at_the_first_nonzero_exit() {
        let first = "printf '%s' 'first step'";
        let second = "false";
        let commands = prepare(&[first, second, "printf 'unreached'"])
            .unwrap_or_else(|failure| panic!("{}", failure.detail));
        let mut sent = Vec::new();
        let (ordinal, failure) = run_sequence(commands, |command| {
            assert!(
                command
                    .wire
                    .starts_with("cd '/srv/example directory' && exec /bin/sh -c ")
            );
            sent.push(command.raw);
            Ok(Status::Exited(if sent.len() == 1 { 0 } else { 19 }))
        })
        .expect_err("the second command returned a nonzero exit");
        assert_eq!(sent, [first, second]);
        assert_eq!(ordinal, 2);
        assert_eq!(
            failure.outcome,
            RunOutcome::Failed {
                exit_code: Some(19)
            }
        );
    }

    #[test]
    fn reviewed_firing_blocks_invalid_or_masked_commands_before_dispatch() {
        for commands in [
            &[][..],
            &["a", "b", "c", "d"],
            &[" "],
            &["printf '***'"],
            &["printf 'fixture-secret'"],
            &["first\nsecond"],
            &[&"x".repeat(MAX_COMMAND_BYTES + 1)],
        ] {
            assert!(
                matches!(prepare(commands), Err(failure) if failure.outcome == RunOutcome::Blocked)
            );
        }
    }

    #[test]
    fn initial_probe_uses_the_exact_bound_wire_and_rejects_a_mask_change() {
        let directory = "/srv/reviewed directory";
        let wire = initial_swarm_probe(directory).unwrap();
        let redactor = Redactor::for_journal(Secrets::default().seal().unwrap(), Policy::default());
        let Ok(prepared) = prepare_initial_probe(directory, Some(&wire), &redactor, fixture_mask)
        else {
            panic!("the reviewed probe should be prepared");
        };
        assert_eq!(prepared.raw, wire);
        assert_eq!(prepared.wire, wire);
        assert!(prepare_initial_probe(directory, None, &redactor, fixture_mask).is_err());
        assert!(prepare_initial_probe(directory, Some("true"), &redactor, fixture_mask).is_err());
        assert!(prepare_initial_probe("/srv/other", Some(&wire), &redactor, fixture_mask).is_err());

        let directory = "/srv/fixture-secret";
        let wire = initial_swarm_probe(directory).unwrap();
        assert!(matches!(
            prepare_initial_probe(directory, Some(&wire), &redactor, fixture_mask),
            Err(error) if error.outcome == RunOutcome::Blocked
        ));
    }

    #[test]
    fn firing_selects_the_reviewed_ordinal_and_rejects_reordered_receipts() {
        let reference = |id, host| {
            serde_json::json!({
                "id": id, "target": {"host": host, "port": 22, "username": "fixture"}
            })
        };
        let action: SshCommand = serde_json::from_value(serde_json::json!({
            "connection": reference(1, "first.example.test"),
            "additional_connections": [reference(2, "second.example.test")],
        }))
        .unwrap();
        let mut approved: Vec<Target> = action
            .targets()
            .map(|reference| {
                serde_json::from_value(serde_json::json!({
                    "reference": reference, "pins": ["fixture"], "revision": vec![0; 32]
                }))
                .unwrap()
            })
            .collect();
        assert_eq!(
            reviewed_target(&action, &approved, 1)
                .ok()
                .map(|target| target.reference.id),
            Some(2)
        );
        assert!(reviewed_target(&action, &approved, 2).is_err());
        assert!(reviewed_target(&action, &approved[..1], 0).is_err());
        approved.swap(0, 1);
        assert!(reviewed_target(&action, &approved, 0).is_err());
    }

    #[test]
    fn multi_target_firing_requires_and_applies_the_shared_credential_mask() {
        fn mask(redactor: &Redactor, command: &str) -> Result<String, String> {
            redactor
                .journal_text(command, Instant::now() + Duration::from_secs(1))
                .map(|text| text.to_string())
                .ok_or_else(|| "fixture mask failed".into())
        }

        let slots = [Err("Other target unavailable"), Err("Fixture route")];
        assert!(command_secrets(None, &slots[..1]).is_ok());
        assert!(
            matches!(command_secrets(None, &slots), Err(error) if error.outcome == RunOutcome::Blocked)
        );

        let sibling_secret = "other-target-authentication-value";
        let shared = RedactionSecrets::with_secret(None, sibling_secret).unwrap();
        let Ok(secrets) = command_secrets(Some(shared), &slots) else {
            panic!("the reviewed shared secret mask should be accepted");
        };
        assert!(matches!(
            prepare_commands(
                vec![format!("printf '%s' '{sibling_secret}'")],
                "/srv",
                &secrets,
                &Policy::default(),
                mask,
            ),
            Err(error) if error.outcome == RunOutcome::Blocked
        ));
    }

    #[test]
    fn swarm_capture_masks_a_credential_crossing_the_output_limit_before_truncating() {
        fn mask(redactor: &Redactor, value: &str) -> Result<String, String> {
            redactor
                .journal_text(value, Instant::now() + Duration::from_secs(1))
                .map(|text| text.to_string())
                .ok_or_else(|| "fixture mask failed".into())
        }

        let secret = "shared-ssh-fixture-password";
        let secrets = RedactionSecrets::with_secret(None, secret).unwrap();
        let redactor = Redactor::for_journal(secrets, Policy::default());
        let raw = format!("{}{secret}", "a".repeat(MAX_SWARM_STREAM_BYTES - 2));
        let (masked, truncated) = masked_stream(&redactor, mask, &raw).unwrap();
        assert!(truncated);
        assert_eq!(masked.len(), MAX_SWARM_STREAM_BYTES);
        assert!(masked.ends_with("**"));
        assert!(!masked.contains("sh"));

        let (unicode, truncated) =
            masked_stream(&redactor, mask, &"界".repeat(MAX_SWARM_STREAM_BYTES)).unwrap();
        assert!(truncated);
        assert!(unicode.len() <= MAX_SWARM_STREAM_BYTES);
        assert!(unicode.chars().all(|ch| ch == '界'));
        assert!(masked_stream(&redactor, |_, _| Err("no mask".into()), "fixture").is_none());
    }
}
