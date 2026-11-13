//! Ordered target results and target-local evidence for a scheduled firing.

use std::fmt::Write as _;
use std::sync::Arc;

use kortty_jobs::{
    MAX_SWARM_PARALLELISM, MAX_SWARM_QUESTION_BYTES, MAX_SWARM_TARGET_DETAIL_BYTES, RunOutcome,
    RunRequest, SwarmReport, SwarmTargetReport,
};

use crate::job_executor::{
    Completion,
    automation::{IterativeCapture, Plan},
};
use crate::scheduler_window::{
    SshCommand,
    agent::{
        Probe,
        proposal::{MAX_SWARM_COMMAND_ROUNDS, SwarmEvidence, SwarmRound},
    },
};

const MAX_NOTE_BYTES: usize = 64;

enum Phase {
    Queued,
    Ready,
    Claimed,
    ClosingJournal,
    Complete(TargetResult),
}

pub(in crate::workspace) struct Sequence {
    request: Arc<RunRequest<SshCommand>>,
    targets: Vec<Target>,
    parallelism: usize,
    iterative: bool,
    question: Option<String>,
    journals: Option<Journals>,
    cancelled: bool,
    completed: bool,
}

struct Target {
    id: u64,
    phase: Phase,
    swarm: Option<Swarm>,
    capture: Option<IterativeCapture>,
}

struct TargetResult {
    outcome: RunOutcome,
    detail: String,
}

impl Target {
    fn occupies_slot(&self) -> bool {
        matches!(
            self.phase,
            Phase::Ready | Phase::Claimed | Phase::ClosingJournal
        )
    }

    fn holds_journal_slot(&self) -> bool {
        self.capture.is_some() || matches!(self.phase, Phase::ClosingJournal)
    }

    fn finish(&mut self, outcome: RunOutcome, detail: String) {
        self.phase = Phase::Complete(TargetResult { outcome, detail });
        self.swarm = None;
    }
}

#[derive(Default)]
struct Swarm {
    environment: Option<SwarmEvidence>,
    prior: Vec<SwarmRound>,
}

#[derive(Default)]
struct Journals {
    initialized: bool,
    plan: Option<Plan>,
    warning: Option<String>,
}

impl Sequence {
    pub(in crate::workspace) fn new(request: RunRequest<SshCommand>, parallelism: usize) -> Self {
        let iterative = request
            .action
            .agent
            .as_ref()
            .is_some_and(|action| action.probe == Probe::SwarmIterative);
        let targets = request
            .action
            .targets()
            .map(|target| Target {
                id: target.id,
                phase: Phase::Queued,
                swarm: iterative.then(Swarm::default),
                capture: None,
            })
            .collect();
        let journals = (request.action.agent.as_ref().is_some_and(|action| {
            action.probe.is_firing() && action.probe.supports_target_journal(action.kind)
        }) && request
            .action
            .automation_journal
            .as_ref()
            .is_some_and(|options| options.enabled))
        .then(Journals::default);
        Self {
            request: Arc::new(request),
            targets,
            parallelism: if iterative {
                parallelism.clamp(1, MAX_SWARM_PARALLELISM)
            } else {
                1
            },
            iterative,
            question: None,
            journals,
            cancelled: false,
            completed: false,
        }
    }

    pub(super) fn ready_indices(&self) -> impl Iterator<Item = usize> + '_ {
        let running = !self.cancellation_requested() && !self.completed;
        let room = running
            && self
                .targets
                .iter()
                .filter(|target| target.occupies_slot())
                .count()
                < self.parallelism;
        let queued_candidates = if self.parallelism == 1 {
            1
        } else {
            self.targets.len()
        };
        self.targets
            .iter()
            .enumerate()
            .filter(move |(_, target)| running && matches!(target.phase, Phase::Ready))
            .chain(
                self.targets
                    .iter()
                    .enumerate()
                    .filter(move |(_, target)| room && matches!(target.phase, Phase::Queued))
                    .take(queued_candidates),
            )
            .map(|(index, _)| index)
    }

    pub(super) fn claim(&mut self, index: usize) -> Option<(usize, Arc<RunRequest<SshCommand>>)> {
        if !self.ready_indices().any(|ready| ready == index) {
            return None;
        }
        self.targets[index].phase = Phase::Claimed;
        Some((index, Arc::clone(&self.request)))
    }

    pub(super) fn is_current(&self, index: usize) -> bool {
        self.targets
            .get(index)
            .is_some_and(|target| matches!(target.phase, Phase::Claimed))
    }

    pub(super) fn cancellation_requested(&self) -> bool {
        self.cancelled || self.request.cancellation.is_requested()
    }

    pub(super) fn request(&self) -> &RunRequest<SshCommand> {
        &self.request
    }

    pub(super) fn current_round(&self, index: usize) -> Option<u8> {
        self.is_current(index).then(|| {
            self.swarm_prior(index)
                .map_or(1, |prior| prior.len() as u8 + 1)
        })
    }

    pub(super) fn swarm_prior(&self, index: usize) -> Option<&[SwarmRound]> {
        Some(self.targets.get(index)?.swarm.as_ref()?.prior.as_slice())
    }

    pub(super) fn swarm_environment(&self, index: usize) -> Option<&SwarmEvidence> {
        self.targets
            .get(index)?
            .swarm
            .as_ref()?
            .environment
            .as_ref()
    }

    pub(super) fn needs_initial_probe(&self, index: usize) -> bool {
        self.swarm_prior(index).is_some_and(<[_]>::is_empty)
            && self.swarm_environment(index).is_none()
            && self
                .request
                .action
                .agent
                .as_ref()
                .and_then(|agent| agent.approval.as_deref())
                .is_some_and(crate::scheduler_window::agent::Approval::has_initial_swarm_probe)
    }

    pub(super) fn accept_initial_probe(&mut self, index: usize, evidence: SwarmEvidence) -> bool {
        if self.current_round(index) != Some(1)
            || evidence.exit != 0
            || self.cancellation_requested()
        {
            return false;
        }
        let target = &mut self.targets[index];
        let Some(swarm) = &mut target.swarm else {
            return false;
        };
        if swarm.environment.is_some() {
            return false;
        }
        swarm.environment = Some(evidence);
        target.phase = Phase::Ready;
        true
    }

    pub(super) fn record_masked_question(&mut self, question: &str) -> bool {
        if !self.iterative || self.completed {
            return false;
        }
        let question = plain_text(question, MAX_SWARM_QUESTION_BYTES);
        if question.is_empty() {
            return false;
        }
        match &self.question {
            Some(current) => current == &question,
            None => {
                self.question = Some(question);
                true
            }
        }
    }

    pub(super) fn continue_swarm(
        &mut self,
        index: usize,
        round: u8,
        commands: Vec<SwarmEvidence>,
    ) -> bool {
        if self.current_round(index) != Some(round)
            || !(1..=MAX_SWARM_COMMAND_ROUNDS).contains(&round)
            || !(1..=3).contains(&commands.len())
            || self.cancellation_requested()
        {
            return false;
        }
        let target = &mut self.targets[index];
        let Some(swarm) = &mut target.swarm else {
            return false;
        };
        swarm.prior.push(SwarmRound { round, commands });
        target.phase = Phase::Ready;
        true
    }

    pub(super) fn defer(&mut self, index: usize) {
        if self.is_current(index) {
            self.targets[index].phase = Phase::Ready;
        }
    }

    pub(super) fn journal_plan_needed(&self) -> bool {
        self.journals
            .as_ref()
            .is_some_and(|journals| !journals.initialized)
    }

    pub(super) fn set_journal_plan(&mut self, result: Result<Option<Plan>, String>) {
        if let Some(journals) = &mut self.journals
            && !journals.initialized
        {
            journals.initialized = true;
            match result {
                Ok(plan) => journals.plan = plan,
                Err(warning) => journals.warning = Some(warning),
            }
        }
    }

    pub(super) fn journal_slot_count(&self) -> usize {
        self.targets
            .iter()
            .filter(|target| target.holds_journal_slot())
            .count()
    }

    pub(super) fn needs_journal_slot(&self, index: usize) -> bool {
        self.targets
            .get(index)
            .is_some_and(|target| !target.holds_journal_slot())
            && self
                .journals
                .as_ref()
                .is_some_and(|journals| !journals.initialized || journals.plan.is_some())
    }

    pub(super) fn needs_model_journal_slot(&self, index: usize) -> bool {
        !self.iterative && self.needs_journal_slot(index)
    }

    pub(super) fn capture_target(&mut self, index: usize) -> Option<IterativeCapture> {
        if !self.is_current(index) {
            return None;
        }
        let plan = self.journals.as_ref()?.plan.as_ref()?;
        Some(
            self.targets[index]
                .capture
                .get_or_insert_with(|| IterativeCapture::new(plan.clone()))
                .clone(),
        )
    }

    pub(super) fn begin_journal_close(&mut self, index: usize) -> Option<IterativeCapture> {
        if !self.is_current(index) {
            return None;
        }
        let target = &mut self.targets[index];
        let capture = target.capture.take()?;
        target.phase = Phase::ClosingJournal;
        Some(capture)
    }

    pub(super) fn journal_closed(&mut self, index: usize, complete: bool) -> bool {
        let Some(target) = self
            .targets
            .get_mut(index)
            .filter(|target| matches!(target.phase, Phase::ClosingJournal))
        else {
            return false;
        };
        if !complete && let Some(journals) = &mut self.journals {
            journals
                .warning
                .get_or_insert_with(|| "Could not complete a target session journal".into());
        }
        target.phase = Phase::Claimed;
        true
    }

    pub(super) fn claim_cancelled_journals(&mut self) -> Vec<usize> {
        if !self.cancellation_requested() {
            return Vec::new();
        }
        self.targets
            .iter_mut()
            .enumerate()
            .filter_map(|(index, target)| {
                if matches!(target.phase, Phase::Ready) && target.capture.is_some() {
                    target.phase = Phase::Claimed;
                    Some(index)
                } else {
                    None
                }
            })
            .collect()
    }

    pub(super) fn take_journals(&mut self) -> (Option<Plan>, Option<String>) {
        self.journals
            .take()
            .map_or((None, None), |journals| (journals.plan, journals.warning))
    }

    pub(in crate::workspace) fn finish(
        &mut self,
        index: usize,
        outcome: RunOutcome,
        detail: String,
    ) -> Option<Option<Completion>> {
        if !self.is_current(index) || self.targets[index].capture.is_some() {
            return None;
        }
        let outcome = match outcome {
            RunOutcome::Cancelled if !self.iterative => {
                self.cancelled = true;
                RunOutcome::Cancelled
            }
            RunOutcome::Cancelled if !self.cancellation_requested() => RunOutcome::Blocked,
            outcome => outcome,
        };
        self.targets[index].finish(outcome, detail);
        if self.cancellation_requested() {
            self.cancel_waiting();
        }
        Some(self.complete())
    }

    pub(in crate::workspace) fn cancel_ready(&mut self) -> Option<Completion> {
        if !self.cancellation_requested() {
            return None;
        }
        self.cancel_waiting();
        self.complete()
    }

    fn cancel_waiting(&mut self) {
        let singleton = self.targets.len() == 1;
        for target in &mut self.targets {
            if matches!(target.phase, Phase::Queued | Phase::Ready) && target.capture.is_none() {
                let detail = if singleton {
                    "Cancelled before the model request."
                } else if target
                    .swarm
                    .as_ref()
                    .is_some_and(|swarm| swarm.environment.is_some() || !swarm.prior.is_empty())
                {
                    "Before next round"
                } else {
                    "Before launch"
                };
                target.finish(RunOutcome::Cancelled, detail.into());
            }
        }
    }

    fn complete(&mut self) -> Option<Completion> {
        if self.completed || self.targets.is_empty() {
            return None;
        }
        let mut outcome = if self.cancellation_requested() {
            RunOutcome::Cancelled
        } else {
            RunOutcome::Success
        };
        let mut detail = String::new();
        let mut report_targets = Vec::new();
        for target in &self.targets {
            let Phase::Complete(result) = &target.phase else {
                return None;
            };
            outcome = combine(outcome, result.outcome);
            if self.targets.len() == 1 {
                detail = result.detail.clone();
            } else if self.targets.len() <= crate::scheduler_window::MAX_TARGETS {
                append(&mut detail, target.id, result.outcome, &result.detail);
            }
            if self.iterative {
                let note = plain_text(&result.detail, MAX_SWARM_TARGET_DETAIL_BYTES);
                report_targets.push(SwarmTargetReport {
                    target: target.id,
                    label: format!("Profile {}", target.id),
                    outcome: result.outcome,
                    detail: if note.is_empty() {
                        "No target summary was recorded.".into()
                    } else {
                        note
                    },
                });
            }
        }
        if self.iterative && self.targets.len() > crate::scheduler_window::MAX_TARGETS {
            detail = crate::job_executor::swarm_completion_summary(&report_targets, outcome);
        }
        self.completed = true;
        let swarm_report = self
            .question
            .take()
            .filter(|_| outcome != RunOutcome::Cancelled)
            .map(|question| {
                Box::new(SwarmReport {
                    question,
                    targets: report_targets,
                    comparison: None,
                })
            });
        Some(Completion {
            id: self.request.id,
            outcome,
            detail: Some(detail),
            recording: None,
            swarm_report,
        })
    }
}

fn append(detail: &mut String, target: u64, outcome: RunOutcome, target_detail: &str) {
    if !detail.is_empty() {
        detail.push_str(" | ");
    }
    let status = match outcome {
        RunOutcome::Success => "Success",
        RunOutcome::Failed { .. } => "Failed",
        RunOutcome::Blocked => "Blocked",
        RunOutcome::Cancelled => "Cancelled",
    };
    let _ = write!(detail, "Profile {target}: {status}");
    if let RunOutcome::Failed {
        exit_code: Some(code),
    } = outcome
    {
        let _ = write!(detail, " (exit {code})");
    }
    let note = plain_note(target_detail);
    if !note.is_empty() {
        let _ = write!(detail, " · {note}");
    }
}

fn combine(previous: RunOutcome, current: RunOutcome) -> RunOutcome {
    match (previous, current) {
        (RunOutcome::Cancelled, _) | (_, RunOutcome::Cancelled) => RunOutcome::Cancelled,
        (failed @ RunOutcome::Failed { .. }, _) | (_, failed @ RunOutcome::Failed { .. }) => failed,
        (RunOutcome::Blocked, _) | (_, RunOutcome::Blocked) => RunOutcome::Blocked,
        _ => RunOutcome::Success,
    }
}

fn plain_note(detail: &str) -> String {
    plain_text(detail, MAX_NOTE_BYTES)
}

fn plain_text(detail: &str, limit: usize) -> String {
    let visible: String = detail.chars().map(|ch| {
        if ch.is_control()
            || !crate::session_journal::is_display_character(ch)
            || matches!(ch, '|' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
        {
            ' '
        } else {
            ch
        }
    }).collect();
    let mut note = visible.split_whitespace().collect::<Vec<_>>().join(" ");
    note.truncate(note.floor_char_boundary(limit));
    note.truncate(note.trim_end().len());
    note
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use kortty_jobs::{Job, JobGraph, JobId, Scheduler};
    use serde_json::json;

    use super::*;

    fn firing(ids: &[u64]) -> (Scheduler<SshCommand>, Sequence) {
        fixture(ids, false, 8)
    }

    fn fixture(
        ids: &[u64],
        iterative: bool,
        parallelism: usize,
    ) -> (Scheduler<SshCommand>, Sequence) {
        let reference = |id| {
            json!({
                "id": id,
                "target": {"host": format!("target-{id}.example"), "port": 22, "username": "fixture"},
            })
        };
        let mut action = json!({
            "connection": reference(ids[0]),
            "additional_connections": ids[1..].iter().copied().map(reference).collect::<Vec<_>>(),
        });
        if iterative {
            action["working_directory"] = json!("/srv");
            action["agent"] =
                json!({"kind":"swarm", "probe":"swarm_iterative", "profile":1, "goal":"Fixture"});
        }
        let action: SshCommand = serde_json::from_value(action).unwrap();
        let id = JobId::new("sequence-fixture").unwrap();
        let job = Job::new(id.clone(), "Sequence fixture", action).unwrap();
        let mut scheduler = Scheduler::new(JobGraph::new(vec![job]).unwrap());
        let request = scheduler
            .start_manual(&id, SystemTime::now())
            .unwrap()
            .started
            .pop()
            .unwrap();
        (scheduler, Sequence::new(request, parallelism))
    }

    fn claim_next(sequence: &mut Sequence) -> Option<(usize, Arc<RunRequest<SshCommand>>)> {
        let index = sequence.ready_indices().next()?;
        sequence.claim(index)
    }

    fn round_evidence() -> Vec<SwarmEvidence> {
        vec![SwarmEvidence {
            command: "pwd".into(),
            exit: 0,
            stdout: "ephemeral target evidence".into(),
            stderr: String::new(),
            truncated: false,
        }]
    }

    #[test]
    fn scheduled_iterative_swarm_keeps_sixteen_active_targets_and_all_ordered_results() {
        let ids = (1..=kortty_jobs::MAX_SWARM_TARGETS as u64)
            .rev()
            .collect::<Vec<_>>();
        let (_, mut sequence) = fixture(&ids, true, MAX_SWARM_PARALLELISM);
        for index in 0..MAX_SWARM_PARALLELISM {
            assert_eq!(claim_next(&mut sequence).unwrap().0, index);
        }
        assert!(claim_next(&mut sequence).is_none());
        assert!(sequence.record_masked_question("Inspect the 32 targets"));
        let note = "wide result ü".repeat(100);
        assert!(matches!(
            sequence.finish(9, RunOutcome::Success, note.clone()),
            Some(None)
        ));
        assert_eq!(claim_next(&mut sequence).unwrap().0, 16);
        assert!(claim_next(&mut sequence).is_none());
        for index in 0..32 {
            if index == 9 {
                continue;
            }
            let completed = sequence
                .finish(index, RunOutcome::Success, note.clone())
                .unwrap();
            if index == 31 {
                let completion = completed.unwrap();
                assert!(
                    completion
                        .detail
                        .as_deref()
                        .unwrap()
                        .starts_with("32 of 32 targets succeeded")
                );
                assert_eq!(
                    completion
                        .swarm_report
                        .unwrap()
                        .targets
                        .iter()
                        .map(|row| row.target)
                        .collect::<Vec<_>>(),
                    ids
                );
            } else {
                assert!(completed.is_none());
                if let Some((next, _)) = claim_next(&mut sequence) {
                    assert!(next >= 17);
                }
            }
        }
    }

    #[test]
    fn parallel_targets_hold_slots_between_rounds_and_keep_evidence_separate() {
        let (_, mut sequence) = fixture(&[11, 22, 33], true, 2);
        assert_eq!(sequence.ready_indices().collect::<Vec<_>>(), [0, 1, 2]);
        let (_, first) = sequence.claim(0).unwrap();
        let (_, second) = sequence.claim(1).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(sequence.claim(2).is_none());
        assert_eq!(sequence.current_round(0), Some(1));
        assert_eq!(sequence.current_round(1), Some(1));

        let mut alpha_probe = round_evidence().pop().unwrap();
        alpha_probe.stdout = "Alpha environment".into();
        let mut beta_probe = alpha_probe.clone();
        beta_probe.stdout = "Beta environment".into();
        assert!(sequence.accept_initial_probe(0, alpha_probe));
        assert!(sequence.swarm_environment(1).is_none());
        assert!(sequence.accept_initial_probe(1, beta_probe));
        assert_eq!(sequence.ready_indices().collect::<Vec<_>>(), [0, 1]);
        assert!(sequence.claim(2).is_none());

        sequence.claim(0).unwrap();
        sequence.claim(1).unwrap();
        let mut alpha_commands = round_evidence();
        alpha_commands[0].stdout = "Alpha command evidence".into();
        assert!(sequence.continue_swarm(0, 1, alpha_commands));
        assert!(sequence.swarm_prior(1).unwrap().is_empty());
        assert_eq!(
            sequence.swarm_environment(0).unwrap().stdout,
            "Alpha environment"
        );
        assert_eq!(
            sequence.swarm_environment(1).unwrap().stdout,
            "Beta environment"
        );
        assert_eq!(sequence.ready_indices().collect::<Vec<_>>(), [0]);
        sequence.claim(0).unwrap();
        assert_eq!(sequence.current_round(0), Some(2));
        assert_eq!(sequence.current_round(1), Some(1));
        assert_eq!(
            sequence.swarm_prior(0).unwrap()[0].commands[0].stdout,
            "Alpha command evidence"
        );
        assert!(matches!(
            sequence.finish(1, RunOutcome::Success, "Beta completed".into()),
            Some(None)
        ));
        assert_eq!(sequence.ready_indices().collect::<Vec<_>>(), [2]);
        assert!(sequence.swarm_environment(2).is_none());
        assert!(sequence.swarm_prior(2).unwrap().is_empty());
    }

    #[test]
    fn parallel_completion_aggregates_once_in_saved_target_order() {
        let (_, mut sequence) = fixture(&[19, 2, 84], true, 3);
        for index in 0..3 {
            sequence.claim(index).unwrap();
        }
        assert!(sequence.record_masked_question("Inspect ***"));
        let first_failed = RunOutcome::Failed {
            exit_code: Some(17),
        };
        let second_failed = RunOutcome::Failed {
            exit_code: Some(91),
        };
        assert!(matches!(
            sequence.finish(1, second_failed, "Second target".into()),
            Some(None)
        ));
        assert!(
            sequence
                .finish(1, RunOutcome::Success, "Stale result".into())
                .is_none()
        );
        assert!(matches!(
            sequence.finish(2, RunOutcome::Blocked, "Third target".into()),
            Some(None)
        ));
        let completion = sequence
            .finish(0, first_failed, "First target".into())
            .unwrap()
            .unwrap();
        assert_eq!(completion.outcome, first_failed);
        assert_eq!(
            completion.detail.as_deref(),
            Some(
                "Profile 19: Failed (exit 17) · First target | Profile 2: Failed (exit 91) · Second target | Profile 84: Blocked · Third target"
            )
        );
        let report = completion.swarm_report.unwrap();
        assert_eq!(report.question, "Inspect ***");
        assert_eq!(
            report
                .targets
                .iter()
                .map(|target| (target.target, target.outcome))
                .collect::<Vec<_>>(),
            [
                (19, first_failed),
                (2, second_failed),
                (84, RunOutcome::Blocked),
            ]
        );
        assert!(sequence.cancel_ready().is_none());
        assert!(sequence.finish(0, first_failed, String::new()).is_none());
    }

    #[test]
    fn parallel_parent_cancellation_drains_claimed_and_closing_targets() {
        let (scheduler, mut sequence) = fixture(&[11, 22, 33], true, 2);
        sequence.journals = Some(Journals::default());
        sequence.set_journal_plan(Ok(Some(journal_plan())));
        let (_, request) = sequence.claim(0).unwrap();
        sequence.claim(1).unwrap();
        assert!(sequence.capture_target(0).is_some());
        assert!(sequence.capture_target(1).is_some());
        assert_eq!(sequence.journal_slot_count(), 2);
        assert!(sequence.continue_swarm(1, 1, round_evidence()));
        scheduler.cancel(request.id).unwrap();
        assert!(sequence.ready_indices().next().is_none());
        assert!(sequence.cancel_ready().is_none());
        assert_eq!(sequence.claim_cancelled_journals(), [1]);
        assert!(sequence.claim_cancelled_journals().is_empty());
        drop(sequence.begin_journal_close(1).unwrap());
        assert_eq!(sequence.journal_slot_count(), 2);
        assert!(!sequence.journal_closed(0, true));
        assert!(sequence.journal_closed(1, true));
        assert!(!sequence.journal_closed(1, true));
        assert!(matches!(
            sequence.finish(1, RunOutcome::Cancelled, "Between rounds".into()),
            Some(None)
        ));
        assert_eq!(sequence.journal_slot_count(), 1);
        assert!(sequence.is_current(0));
        assert!(
            sequence
                .finish(0, RunOutcome::Cancelled, String::new())
                .is_none()
        );
        drop(sequence.begin_journal_close(0).unwrap());
        assert!(sequence.cancel_ready().is_none());
        assert!(sequence.journal_closed(0, true));
        let completion = sequence
            .finish(0, RunOutcome::Cancelled, "During execution".into())
            .unwrap()
            .unwrap();
        assert_eq!(completion.outcome, RunOutcome::Cancelled);
        assert_eq!(
            completion.detail.as_deref(),
            Some(
                "Profile 11: Cancelled · During execution | Profile 22: Cancelled · Between rounds | Profile 33: Cancelled · Before launch"
            )
        );
        assert!(completion.swarm_report.is_none());
        assert_eq!(sequence.journal_slot_count(), 0);
        assert!(sequence.cancel_ready().is_none());
    }

    #[test]
    fn iterative_swarm_resets_history_per_target_and_saves_only_final_results() {
        let (_, mut sequence) = fixture(&[11, 22], true, 1);
        assert_eq!(claim_next(&mut sequence).unwrap().0, 0);
        let mut probe = round_evidence().pop().unwrap();
        probe.stdout = "Initial environment for the first target".into();
        let mut failed = probe.clone();
        failed.exit = 17;
        assert!(!sequence.accept_initial_probe(0, failed));
        assert!(!sequence.accept_initial_probe(1, probe.clone()));
        assert!(sequence.accept_initial_probe(0, probe.clone()));
        for round in 1..=MAX_SWARM_COMMAND_ROUNDS {
            assert_eq!(claim_next(&mut sequence).unwrap().0, 0);
            assert_eq!(sequence.current_round(0), Some(round));
            assert_eq!(sequence.swarm_environment(0).unwrap().stdout, probe.stdout);
            assert!(!sequence.accept_initial_probe(0, probe.clone()));
            assert!(sequence.record_masked_question("Inspect ***"));
            assert!(!sequence.continue_swarm(0, round + 1, round_evidence()));
            assert!(sequence.continue_swarm(0, round, round_evidence()));
            assert_eq!(sequence.swarm_prior(0).unwrap().len(), usize::from(round));
        }
        assert_eq!(claim_next(&mut sequence).unwrap().0, 0);
        assert_eq!(
            sequence.current_round(0),
            Some(MAX_SWARM_COMMAND_ROUNDS + 1)
        );
        assert!(!sequence.continue_swarm(0, MAX_SWARM_COMMAND_ROUNDS + 1, round_evidence()));
        assert!(matches!(
            sequence.finish(0, RunOutcome::Success, "First final result".into()),
            Some(None)
        ));
        assert_eq!(claim_next(&mut sequence).unwrap().0, 1);
        assert_eq!(sequence.current_round(1), Some(1));
        assert!(sequence.swarm_prior(1).unwrap().is_empty());
        assert!(sequence.swarm_environment(1).is_none());
        let report = sequence
            .finish(1, RunOutcome::Blocked, "Second final result".into())
            .unwrap()
            .unwrap()
            .swarm_report
            .unwrap();
        assert_eq!(report.question, "Inspect ***");
        assert_eq!(report.targets[0].detail, "First final result");
        assert_eq!(report.targets[1].outcome, RunOutcome::Blocked);
    }

    #[test]
    fn iterative_swarm_cancellation_between_rounds_omits_report() {
        let (scheduler, mut sequence) = fixture(&[11, 22], true, 1);
        let (_, request) = claim_next(&mut sequence).unwrap();
        assert!(sequence.record_masked_question("Inspect ***"));
        assert!(sequence.continue_swarm(0, 1, round_evidence()));
        scheduler.cancel(request.id).unwrap();
        let completion = sequence.cancel_ready().unwrap();
        assert_eq!(completion.outcome, RunOutcome::Cancelled);
        assert!(completion.swarm_report.is_none());
        assert_eq!(
            completion.detail.as_deref(),
            Some(
                "Profile 11: Cancelled · Before next round | Profile 22: Cancelled · Before launch"
            )
        );
    }

    #[test]
    fn cancelling_with_an_open_journal_waits_for_the_matching_close() {
        let (scheduler, mut sequence) = fixture(&[11, 22], true, 1);
        sequence.journals = Some(Journals::default());
        assert!(sequence.needs_journal_slot(0));
        let (index, request) = claim_next(&mut sequence).unwrap();
        sequence.set_journal_plan(Ok(Some(journal_plan())));
        assert!(sequence.capture_target(index).is_some());
        assert_eq!(sequence.journal_slot_count(), 1);
        assert!(!sequence.needs_journal_slot(index));
        assert!(sequence.continue_swarm(index, 1, round_evidence()));
        scheduler.cancel(request.id).unwrap();
        assert!(sequence.cancel_ready().is_none());
        assert_eq!(sequence.claim_cancelled_journals(), [index]);
        let capture = sequence.begin_journal_close(index).unwrap();
        assert_eq!(sequence.journal_slot_count(), 1);
        assert!(sequence.ready_indices().next().is_none());
        assert!(!sequence.journal_closed(index + 1, true));
        assert!(
            sequence
                .finish(index, RunOutcome::Cancelled, String::new())
                .is_none()
        );
        drop(capture);
        assert!(sequence.journal_closed(index, true));
        assert!(!sequence.journal_closed(index, true));
        let completion = sequence
            .finish(
                index,
                RunOutcome::Cancelled,
                "Cancelled before next round".into(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(completion.outcome, RunOutcome::Cancelled);
        assert!(completion.swarm_report.is_none());
        assert_eq!(sequence.journal_slot_count(), 0);
        assert!(sequence.take_journals().0.is_some());
    }

    fn journal_plan() -> Plan {
        use crate::session_journal::automation::{Caps, CaptureSettings, Kind, Options, Run};

        Plan::new(
            CaptureSettings {
                root: "unused".into(),
                format: Default::default(),
                max_parts: None,
                markers: Default::default(),
                stopped: Default::default(),
            },
            Run::new(
                Kind::Job,
                "fixture".into(),
                "Fixture".into(),
                "AI_SWARM",
                jiff::Timestamp::now(),
                Options {
                    enabled: true,
                    ..Options::default()
                },
                Caps::default(),
            )
            .unwrap(),
            Default::default(),
        )
    }

    #[test]
    fn agent_firing_reserves_one_target_journal_until_close() {
        let (_, seed) = firing(&[11, 22]);
        let original = &seed.request;
        let mut action = original.action.as_ref().clone();
        action.working_directory = Some("/srv".into());
        action.agent = Some(
            serde_json::from_value(json!({
                "kind": "agent", "probe": "per_firing", "profile": 1, "goal": "Fixture"
            }))
            .unwrap(),
        );
        action.automation_journal = Some(crate::session_journal::automation::Options {
            enabled: true,
            ..Default::default()
        });
        let mut sequence = Sequence::new(
            RunRequest {
                id: original.id,
                job: original.job.clone(),
                action: Arc::new(action),
                trigger: original.trigger,
                started_at: original.started_at,
                cancellation: original.cancellation.clone(),
            },
            8,
        );
        assert!(sequence.needs_model_journal_slot(0));
        assert!(sequence.claim(0).is_some());
        sequence.set_journal_plan(Ok(Some(journal_plan())));
        assert!(sequence.capture_target(0).is_some());
        assert!(!sequence.needs_model_journal_slot(0));
        assert_eq!(sequence.journal_slot_count(), 1);
        assert!(sequence.claim(1).is_none());

        assert!(sequence.begin_journal_close(0).is_some());
        assert_eq!(sequence.journal_slot_count(), 1);
        assert!(sequence.claim(1).is_none());
        assert!(sequence.journal_closed(0, true));
        assert!(matches!(
            sequence.finish(0, RunOutcome::Success, "No SSH commands needed.".into()),
            Some(None)
        ));
        assert_eq!(sequence.journal_slot_count(), 0);
        assert!(sequence.needs_model_journal_slot(1));
        assert!(sequence.claim(1).is_some());
    }

    #[test]
    fn claims_each_ordinal_once_and_keeps_the_first_failure() {
        let (_, mut sequence) = firing(&[19, 2, 84, 7]);
        assert_eq!(sequence.ready_indices().collect::<Vec<_>>(), [0]);
        assert!(
            sequence
                .finish(0, RunOutcome::Success, String::new())
                .is_none()
        );
        let (index, original) = claim_next(&mut sequence).unwrap();
        assert_eq!(index, 0);
        assert!(sequence.is_current(0));
        assert!(claim_next(&mut sequence).is_none());
        sequence.defer(1);
        assert!(sequence.is_current(0));
        sequence.defer(0);
        assert_eq!(claim_next(&mut sequence).unwrap().0, 0);
        assert!(
            sequence
                .finish(1, RunOutcome::Success, String::new())
                .is_none()
        );
        assert!(matches!(
            sequence.finish(0, RunOutcome::Blocked, "Review declined".into()),
            Some(None)
        ));
        let failed = RunOutcome::Failed {
            exit_code: Some(17),
        };
        let (index, next) = claim_next(&mut sequence).unwrap();
        assert_eq!(index, 1);
        assert!(Arc::ptr_eq(&original, &next));
        assert!(
            sequence
                .finish(0, RunOutcome::Success, String::new())
                .is_none()
        );
        assert!(matches!(
            sequence.finish(1, failed, "First failure".into()),
            Some(None)
        ));
        assert_eq!(claim_next(&mut sequence).unwrap().0, 2);
        assert!(matches!(
            sequence.finish(
                2,
                RunOutcome::Failed {
                    exit_code: Some(91)
                },
                String::new()
            ),
            Some(None)
        ));
        assert_eq!(claim_next(&mut sequence).unwrap().0, 3);
        let completion = sequence
            .finish(3, RunOutcome::Success, "Done".into())
            .unwrap()
            .unwrap();
        assert_eq!(completion.id, original.id);
        assert_eq!(completion.outcome, failed);
        assert_eq!(
            completion.detail.as_deref(),
            Some(
                "Profile 19: Blocked · Review declined | Profile 2: Failed (exit 17) · First failure | Profile 84: Failed (exit 91) | Profile 7: Success · Done"
            )
        );
        assert!(sequence.ready_indices().next().is_none());
        assert!(
            sequence
                .finish(3, RunOutcome::Success, String::new())
                .is_none()
        );
    }

    #[test]
    fn real_cancellation_keeps_completed_targets_and_marks_unstarted_targets() {
        let (scheduler, mut sequence) = firing(&[11, 22, 33]);
        assert!(sequence.cancel_ready().is_none());
        let (_, request) = claim_next(&mut sequence).unwrap();
        assert!(matches!(
            sequence.finish(0, RunOutcome::Success, "Done".into()),
            Some(None)
        ));
        scheduler.cancel(request.id).unwrap();
        let completion = sequence.cancel_ready().unwrap();
        assert_eq!(completion.outcome, RunOutcome::Cancelled);
        assert_eq!(
            completion.detail.as_deref(),
            Some(
                "Profile 11: Success · Done | Profile 22: Cancelled · Before launch | Profile 33: Cancelled · Before launch"
            )
        );
        assert!(sequence.cancel_ready().is_none());
    }

    #[test]
    fn worker_cancellation_stops_the_sequence_and_singleton_keeps_worker_detail() {
        let (_, mut sequence) = firing(&[11, 22]);
        assert_eq!(claim_next(&mut sequence).unwrap().0, 0);
        let completion = sequence
            .finish(0, RunOutcome::Cancelled, "Worker cancelled".into())
            .unwrap()
            .unwrap();
        assert_eq!(completion.outcome, RunOutcome::Cancelled);
        assert!(
            completion
                .detail
                .unwrap()
                .ends_with("Profile 22: Cancelled · Before launch")
        );
        assert!(claim_next(&mut sequence).is_none());

        let (_, mut singleton) = firing(&[11]);
        assert_eq!(claim_next(&mut singleton).unwrap().0, 0);
        let detail = "Command 2 of 3: Reviewed SSH command failed.";
        let failed = RunOutcome::Failed {
            exit_code: Some(17),
        };
        let completion = singleton.finish(0, failed, detail.into()).unwrap().unwrap();
        assert_eq!(completion.outcome, failed);
        assert_eq!(completion.detail.as_deref(), Some(detail));
        assert!(completion.swarm_report.is_none());
    }

    #[test]
    fn scheduled_iterative_swarm_with_one_target_keeps_its_swarm_report() {
        let (_, mut sequence) = fixture(&[11], true, MAX_SWARM_PARALLELISM);
        assert_eq!(claim_next(&mut sequence).unwrap().0, 0);
        assert!(sequence.record_masked_question("Inspect this target"));
        let detail = "Target completed.";
        let completion = sequence
            .finish(0, RunOutcome::Success, detail.into())
            .unwrap()
            .unwrap();
        assert_eq!(completion.detail.as_deref(), Some(detail));
        let report = completion.swarm_report.unwrap();
        assert_eq!(report.question, "Inspect this target");
        assert_eq!(report.targets.len(), 1);
        assert_eq!(report.targets[0].target, 11);
        assert_eq!(report.targets[0].detail, detail);
    }

    #[test]
    fn full_sequence_summary_keeps_every_target_within_1024_utf8_bytes() {
        let ids: Vec<_> = (0..8).map(|offset| u64::MAX - offset).collect();
        let (_, mut sequence) = firing(&ids);
        let detail = format!("heading\n\u{202e} | {}", "界".repeat(200));
        let mut final_result = None;
        for index in 0..ids.len() {
            assert_eq!(claim_next(&mut sequence).unwrap().0, index);
            final_result = sequence
                .finish(
                    index,
                    RunOutcome::Failed {
                        exit_code: Some(i32::MIN),
                    },
                    detail.clone(),
                )
                .unwrap();
        }
        let summary = final_result.unwrap().detail.unwrap();
        assert!(summary.len() <= 1024);
        assert!(!summary.contains(['\n', '\u{202e}']));
        for (id, note) in ids.into_iter().zip(summary.split(" | ")) {
            assert!(note.starts_with(&format!(
                "Profile {id}: Failed (exit {}) · heading ",
                i32::MIN
            )));
        }
        assert_eq!(summary.split(" | ").count(), 8);
    }
}
