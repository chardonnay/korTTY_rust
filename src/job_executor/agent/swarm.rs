//! Isolates each scheduled read-only Swarm model request to one approved target.

use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::ai_manager::SkillUsageSurface;

use super::{
    AdviceInput, ClipboardPolicy, DispatchCheck, Failure, Kind, Mask, ModelAuthority, Outcome,
    Redactor, Resend, ResendScope, RunOutcome, Store, Target, TrustStore, advise, advisory,
    await_model_authority, check_cancelled, current_route_policy, probe_evidence, request_advice,
    require_current_route, send_model_permit,
};

// Read-only Swarms share the scheduler's model pool and a bounded probe pool.
// Target slots can remain active while waiting for either resource.
static PROBES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub(super) struct ProbePermit;

impl ProbePermit {
    pub(super) fn acquire(cancellation: &kortty_jobs::Cancellation) -> Result<Self, Failure> {
        use std::sync::atomic::Ordering;
        let deadline = Instant::now() + super::PROBE_LIMIT;
        loop {
            check_cancelled(cancellation)?;
            if PROBES
                .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    (current < kortty_jobs::MAX_SWARM_PARALLELISM).then_some(current + 1)
                })
                .is_ok()
            {
                return Ok(Self);
            }
            if Instant::now() >= deadline {
                return Err(Failure::failed(
                    "Timed out waiting for a scheduled Swarm SSH probe slot.",
                ));
            }
            std::thread::sleep(super::POLL);
        }
    }
}

impl Drop for ProbePermit {
    fn drop(&mut self) {
        PROBES.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

const WIRE_PERMIT_LIMIT: Duration = Duration::from_secs(5);
// The journal allows 4096 bytes, including labels for all supported targets.
pub(super) const MAX_SUMMARY_BYTES: usize = 384;

pub(super) fn check_target(
    approved: &Target,
    connection_store: &Path,
    trust: &TrustStore,
    vault_revision: Option<&Weak<()>>,
) -> Result<(), &'static str> {
    if vault_revision.is_some_and(|revision| revision.strong_count() == 0) {
        return Err("The SSH credential vault changed or was locked.");
    }
    let store = Store::open(connection_store.to_owned())
        .map_err(|_| "The saved SSH route is unavailable.")?;
    require_current_route(&store, &approved.reference, approved, trust)
        .map_err(|error| error.detail)
}

pub(super) fn advise_target(
    outcome: Outcome,
    approved: &Target,
    trust: &TrustStore,
    scope: Arc<ResendScope>,
    (goal, redactor, mask): (&str, &Redactor, Mask),
    (resends, load_policy): (
        &async_channel::Sender<Resend>,
        impl Fn() -> ClipboardPolicy + Clone + Send + 'static,
    ),
) -> (RunOutcome, String) {
    let target = approved.reference.id;
    let cancellation = &scope.request.cancellation;
    let (status, note, output) = probe_evidence(outcome, cancellation, redactor, mask);
    let Some(output) = output else {
        return (status, note.into());
    };
    let reviewed = approved.clone();
    let trust = trust.clone();
    let dispatch_scope = Arc::clone(&scope);
    let check_dispatch = move || {
        super::require_target_groups(
            &dispatch_scope.request.action,
            &dispatch_scope.connection_store,
        )
        .map_err(|error| error.detail)?;
        let kind = dispatch_scope
            .request
            .action
            .agent
            .as_ref()
            .map_or(Kind::Swarm, |agent| agent.kind);
        let policy = current_route_policy(kind, &[&reviewed.reference], &load_policy)?;
        if !policy
            .terminal_logging
            .equivalent_redactions(&dispatch_scope.log_policy)
        {
            return Err("The managed AI redaction rules changed.");
        }
        check_target(
            &reviewed,
            &dispatch_scope.connection_store,
            &trust,
            dispatch_scope.vault_revision.as_ref(),
        )?;
        if let Some(prompt) = &dispatch_scope.advisory_prompt {
            let unavailable = "The reviewed read-only Swarm request is unavailable.";
            let approval = dispatch_scope
                .request
                .action
                .agent
                .as_ref()
                .and_then(|agent| agent.approval.as_deref())
                .ok_or(unavailable)?;
            let grant = approval.advisory.as_ref().ok_or(unavailable)?;
            let index = approval
                .targets
                .iter()
                .position(|target| target == &reviewed)
                .ok_or(unavailable)?;
            return advisory::require_saved_guidance(
                &dispatch_scope.connection_store,
                &approval.profile,
                grant,
                index,
                prompt,
                || {
                    check_cancelled(&dispatch_scope.request.cancellation)
                        .map_err(|error| error.detail.to_owned())?;
                    if dispatch_scope
                        .vault_revision
                        .as_ref()
                        .is_some_and(|revision| revision.strong_count() == 0)
                    {
                        return Err("The credential vault changed.".into());
                    }
                    Ok(())
                },
            )
            .map(Some);
        }
        Ok(None)
    };
    let decision: Result<String, Failure> = (|| {
        let skill_usage =
            check_dispatch()
                .map_err(Failure::blocked)?
                .and_then(|(config, guidance)| {
                    config.skill_usage(&guidance, &scope.transport, SkillUsageSurface::Agent, false)
                });
        let dispatch = check_dispatch.clone();
        let wire_permits = resends.clone();
        let wire_scope = Arc::clone(&scope);
        let before_send = move || {
            await_model_authority(
                &wire_permits,
                &wire_scope,
                ModelAuthority::Send,
                Instant::now() + WIRE_PERMIT_LIMIT,
            )
            .map_err(|error| error.detail)?;
            dispatch()?;
            check_cancelled(&wire_scope.request.cancellation).map_err(|error| error.detail)
        };
        let model = (
            scope.transport.clone(),
            scope.context_limit,
            DispatchCheck::BeforeSend(before_send),
        );
        let authorize = |permit, _deadline| {
            // The format/repair handshake can advance only to another guarded send.
            check_cancelled(cancellation)?;
            check_dispatch().map_err(Failure::blocked)?;
            send_model_permit(permit)
        };
        let summary = if let Some(prompt) = &scope.advisory_prompt {
            let input = prompt.advisory_input(Some(&output)).map_err(|_| {
                Failure::blocked("The diagnostic input exceeds the selected AI context limit.")
            })?;
            request_advice(
                model,
                AdviceInput::Reviewed {
                    system: prompt.firing_system().into(),
                    input,
                },
                cancellation,
                redactor,
                mask,
                authorize,
            )
        } else {
            advise(
                model,
                goal.to_owned(),
                &mut vec![json!({"target": target, "output": output})],
                cancellation,
                redactor,
                mask,
                |_, permit, deadline| authorize(permit, deadline),
            )
        }?;
        check_cancelled(cancellation)?;
        check_dispatch().map_err(Failure::blocked)?;
        if scope
            .request
            .action
            .agent
            .as_ref()
            .is_some_and(|agent| agent.swarm_parallelism.is_some())
        {
            await_model_authority(
                resends,
                &scope,
                ModelAuthority::AcceptAdvice,
                Instant::now() + WIRE_PERMIT_LIMIT,
            )?;
            check_dispatch().map_err(Failure::blocked)?;
        }
        if let Some(usage) = skill_usage {
            usage.record();
        }
        Ok(summary)
    })();
    match decision {
        Ok(summary) => (RunOutcome::Success, format!("AI: {summary}")),
        Err(failure) => (failure.outcome, format!("AI: {}", failure.detail)),
    }
}

/// Fill a free target slot as soon as its previous target finishes. Store results
/// in input order even when a later target completes first.
pub(super) fn ordered_targets<T: Send, R: Send>(
    inputs: impl IntoIterator<Item = T>,
    parallelism: usize,
    run: impl Fn(T) -> R + Sync,
) -> Vec<R> {
    let inputs = inputs.into_iter().collect::<Vec<_>>();
    let workers = inputs.len().min(parallelism);
    if workers <= 1 {
        return inputs.into_iter().map(run).collect();
    }
    let pending = Mutex::new(inputs.into_iter().enumerate());
    let mut results = std::thread::scope(|scope| {
        let pending = &pending;
        let run = &run;
        (0..workers)
            .map(|_| {
                scope.spawn(move || {
                    let mut completed = Vec::new();
                    loop {
                        let next = pending.lock().expect("scheduled Swarm target queue").next();
                        let Some((index, input)) = next else { break };
                        completed.push((index, run(input)));
                    }
                    completed
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|worker| worker.join().expect("scheduled Swarm target worker"))
            .collect::<Vec<_>>()
    });
    results.sort_unstable_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}

pub(super) fn plain_text(summary: &str, limit: usize) -> String {
    let visible: String = summary
        .chars()
        .map(|ch| {
            if ch.is_control() || !crate::session_journal::is_display_character(ch)
                || matches!(ch, '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
            {
                ' '
            } else {
                ch
            }
        })
        .collect();
    let mut line = visible.split_whitespace().collect::<Vec<_>>().join(" ");
    line.truncate(line.floor_char_boundary(limit));
    line.truncate(line.trim_end().len());
    if line.is_empty() {
        return "No displayable text.".into();
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    use super::super::Report;

    #[test]
    fn scheduled_read_only_swarm_refills_sixteen_slots_and_returns_ordered_results() {
        let (entered, progress) = mpsc::channel();
        let held = (0..kortty_jobs::MAX_SWARM_TARGETS)
            .map(|id| {
                let (release, ready) = mpsc::channel();
                (release, (id, ready))
            })
            .collect::<Vec<_>>();
        let (releases, inputs): (Vec<_>, Vec<_>) = held.into_iter().unzip();
        let worker = std::thread::spawn(move || {
            ordered_targets(inputs, kortty_jobs::MAX_SWARM_PARALLELISM, |(id, ready)| {
                entered.send(id).unwrap();
                ready.recv_timeout(Duration::from_secs(5)).unwrap();
                id
            })
        });
        let mut first = (0..kortty_jobs::MAX_SWARM_PARALLELISM)
            .map(|_| progress.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect::<Vec<_>>();
        first.sort_unstable();
        assert_eq!(first, (0..16).collect::<Vec<_>>());
        assert!(progress.try_recv().is_err());
        releases[8].send(()).unwrap();
        assert_eq!(progress.recv_timeout(Duration::from_secs(5)).unwrap(), 16);
        for (id, release) in releases.into_iter().enumerate() {
            if id != 8 {
                release.send(()).unwrap();
            }
        }
        assert_eq!(worker.join().unwrap(), (0..32).collect::<Vec<_>>());
    }

    #[test]
    fn target_summary_is_one_line_and_fits_every_supported_journal_entry() {
        let message = format!("one\n\u{202e} two\t{}", "ü".repeat(400));
        let summary = plain_text(&message, MAX_SUMMARY_BYTES);
        assert!(summary.starts_with("one two ü"));
        assert!(summary.len() <= MAX_SUMMARY_BYTES);
        assert!(crate::terminal_agent::valid_target_label(&summary));
        let mut report = Report::default();
        for id in (0..crate::scheduler_window::MAX_TARGETS).rev() {
            report.record_swarm_target(
                u64::MAX - id as u64,
                RunOutcome::Success,
                &format!("AI: {summary}"),
            );
        }
        assert!(report.detail.len() <= 4096);
    }
}
