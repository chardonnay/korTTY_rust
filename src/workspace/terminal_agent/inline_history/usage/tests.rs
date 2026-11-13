use iced::advanced::widget::{Id, Operation, operation};
use iced::{Element, Font, Rectangle, Renderer, Size, Vector};
use iced_runtime::{UserInterface, user_interface::Cache};

use crate::terminal_agent::design::{History as DesignHistory, Layout, Status as DesignStatus};
use crate::terminal_logging::{Redactor, Secrets};
use crate::workspace::terminal_agent::{ProfileChoice, Purpose, RunSetup, binding, design};

use super::*;

fn run(history: DesignHistory) -> Run {
    let profile = ProfileChoice {
        id: 1,
        reference: crate::ai_manager::ProfileRef::Numeric(1),
        label: "Fixture".into(),
        reviewed_https: None,
        reviewed_cli: None,
    };
    let mut run = Run::from_setup(RunSetup {
        binding: binding::Binding::Live(binding::Live {
            session: SessionId(7),
            stream: 9,
            private: true,
        }),
        mode: engine::Mode::Ask,
        label: "Fixture terminal".into(),
        context: String::new(),
        manual_directory: None,
        profiles: vec![profile.clone()],
        purpose: Purpose::Agent(profile),
        swarm: false,
        design: Layout {
            history,
            ..Layout::default()
        },
        redactor: Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default()),
    });
    run.goal = "Inspect the source".into();
    run.next(Phase::Thinking, "Fixture decision");
    run.record("Canonical source evidence".into());
    run
}

fn attempts() -> [Attempt; 2] {
    [
        Attempt {
            kind: AttemptKind::Initial,
            outcome: AttemptOutcome::InvalidReply,
            elapsed: Duration::from_millis(12),
            input_tokens: Some(10),
            output_tokens: Some(2),
            total_tokens: Some(12),
        },
        Attempt {
            kind: AttemptKind::Repair,
            outcome: AttemptOutcome::Completed,
            elapsed: Duration::from_millis(50),
            input_tokens: Some(20),
            output_tokens: Some(8),
            total_tokens: Some(28),
        },
    ]
}

#[test]
fn accepted_attempts_project_typed_thinking_without_changing_model_evidence_or_authority() {
    for history in [DesignHistory::InlineBlocks, DesignHistory::Focus] {
        let mut run = run(history);
        let evidence = run.activity.clone();
        let epoch = run.epoch;
        run.record_model_attempts(epoch, &attempts());
        run.record_model_attempts(epoch, &attempts());
        assert_eq!(run.activity, evidence);
        assert_eq!(run.epoch, epoch);
        assert!(!run.cancel.requested());
        assert!(run.commands.is_empty());
        assert_eq!(run.presentation.model_usage.attempts().count(), 2);
        run.phase = Phase::Finished;
        let update = run.inline_update(false).unwrap();
        assert_eq!(update.transcript.tokens, Some(40));
        assert_eq!(update.transcript.steps.len(), 2);
        let initial = &update.transcript.steps[0];
        assert_eq!(initial.kind, Kind::Thinking);
        assert_eq!(initial.state, StepState::Failed);
        assert_eq!(initial.elapsed_ms, 12);
        assert_eq!(initial.tokens, Some(12));
        assert!(initial.detail.contains("Invalid reply · 0.012 s"));
        assert!(initial.command.is_none() && initial.output.is_empty());
        assert_eq!(update.transcript.steps[1].state, StepState::Completed);
    }
}

#[test]
fn live_repair_replaces_one_stable_step_and_counts_only_reported_attempts() {
    for history in [DesignHistory::InlineBlocks, DesignHistory::Focus] {
        let mut run = run(history);
        let evidence = run.activity.clone();
        let epoch = run.epoch;
        let reports = attempts();
        assert!(run.model_repair_started(epoch, &reports[0]));
        assert!(!run.model_repair_started(epoch, &reports[0]));
        assert_eq!(run.presentation.model_usage.attempts().count(), 1);
        assert_eq!(run.presentation.model_usage.summary().unwrap().attempts, 1);
        assert_eq!(run.presentation.model_usage.total(), Some(12));
        let active = run.inline_update(false).unwrap();
        assert_eq!(active.transcript.steps.len(), 2);
        assert_eq!(active.transcript.steps[0].state, StepState::Completed);
        assert!(
            active.transcript.steps[0]
                .detail
                .contains("The AI response needed repair.")
        );
        let repair = &active.transcript.steps[1];
        assert_eq!(repair.state, StepState::Running);
        assert_eq!(repair.title, "AI response repair");
        assert!(repair.tokens.is_none());
        assert!(repair.command.is_none() && repair.output.is_empty());
        assert_eq!(active.transcript.tokens, Some(12));

        run.record_model_attempts(epoch, &reports);
        run.finish_model_repair(epoch, false);
        run.record_model_attempts(epoch, &reports);
        assert!(!run.model_repair_started(epoch, &reports[0]));
        assert!(!run.presentation.model_usage.repairing(epoch));
        assert_eq!(run.presentation.model_usage.attempts().count(), 2);
        assert_eq!(run.presentation.model_usage.total(), Some(40));
        assert!(run.phase == Phase::Thinking);
        assert_eq!(run.epoch, epoch);
        assert_eq!(run.activity, evidence);
        assert!(run.commands.is_empty());
        assert!(!run.cancel.requested());
        run.phase = Phase::Finished;
        let completed = run.inline_update(false).unwrap();
        assert_eq!(completed.transcript.steps.len(), 2);
        for (before, after) in active
            .transcript
            .steps
            .iter()
            .zip(&completed.transcript.steps)
        {
            assert_eq!(before.id, after.id);
            assert_eq!(after.state, StepState::Completed);
        }
        assert_eq!(completed.transcript.steps[1].tokens, Some(28));
        assert_eq!(completed.transcript.steps[1].elapsed_ms, 50);
        assert_eq!(completed.transcript.tokens, Some(40));
    }
}

#[test]
fn abandoned_repair_keeps_its_step_without_inventing_metadata_and_can_accept_a_real_late_report() {
    for cancelled in [false, true] {
        let mut run = run(DesignHistory::InlineBlocks);
        let epoch = run.epoch;
        let mut reports = attempts();
        assert!(run.model_repair_started(epoch, &reports[0]));
        let active = run.inline_update(false).unwrap();
        run.finish_model_repair(epoch, cancelled);
        run.finish_model_repair(epoch, !cancelled);
        assert!(!run.presentation.model_usage.repairing(epoch));
        assert_eq!(run.presentation.model_usage.attempts().count(), 1);
        assert_eq!(run.presentation.model_usage.total(), Some(12));
        run.phase = if cancelled {
            Phase::Cancelled
        } else {
            Phase::Failed
        };
        let stopped = run.inline_update(false).unwrap();
        let repair = &stopped.transcript.steps[1];
        assert_eq!(repair.id, active.transcript.steps[1].id);
        assert_eq!(
            repair.state,
            if cancelled {
                StepState::Cancelled
            } else {
                StepState::Failed
            }
        );
        assert!(repair.tokens.is_none());
        assert_eq!(repair.elapsed_ms, 0);
        assert!(
            repair
                .detail
                .contains("No completed repair report is available.")
        );

        reports[1].outcome = AttemptOutcome::Cancelled;
        reports[1].total_tokens = None;
        run.record_model_attempts(epoch, &reports);
        let reconciled = run.inline_update(false).unwrap();
        assert_eq!(reconciled.transcript.steps.len(), 2);
        assert_eq!(reconciled.transcript.steps[1].id, repair.id);
        assert_eq!(reconciled.transcript.steps[1].elapsed_ms, 50);
        assert!(run.presentation.model_usage.total().is_none());
        assert_eq!(run.presentation.model_usage.attempts().count(), 2);
    }
}

#[test]
fn live_repair_ignores_old_or_invalid_progress_and_allocates_new_ids_for_the_next_turn() {
    let mut run = run(DesignHistory::InlineBlocks);
    let epoch = run.epoch;
    let reports = attempts();
    assert!(!run.model_repair_started(epoch, &reports[1]));
    let mut valid_initial = reports[0].clone();
    valid_initial.outcome = AttemptOutcome::Completed;
    assert!(!run.model_repair_started(epoch, &valid_initial));
    assert!(run.presentation.model_usage.summary().is_none());
    assert!(run.model_repair_started(epoch, &reports[0]));
    run.record_model_attempts(epoch, &reports);
    let first = run.inline.as_ref().unwrap().transcript.steps.clone();
    let next_epoch = run.next(Phase::Thinking, "Next decision");
    assert!(!run.model_repair_started(epoch, &reports[0]));
    assert!(run.model_repair_started(next_epoch, &reports[0]));
    run.finish_model_repair(epoch, true);
    let second = run.inline_update(false).unwrap();
    assert_eq!(second.transcript.steps.len(), 4);
    assert_eq!(second.transcript.steps[3].state, StepState::Running);
    assert!(
        second.transcript.steps[2..]
            .iter()
            .all(|step| first.iter().all(|old| step.id != old.id))
    );
    assert_eq!(run.presentation.model_usage.attempts().count(), 3);
    assert_eq!(run.presentation.model_usage.total(), Some(52));
}

#[derive(Default)]
struct Labels(Vec<String>);
impl Operation for Labels {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
        visit(self);
    }
    fn text(&mut self, _: Option<&Id>, _: Rectangle, value: &str) {
        self.0.push(value.to_owned());
    }
}

fn labels<M>(element: Element<'_, M>) -> String {
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut ui = UserInterface::build(
        element,
        Size::new(800.0, 600.0),
        Cache::default(),
        &mut renderer,
    );
    let mut labels = Labels::default();
    ui.operate(&renderer, &mut labels);
    labels.0.join("\n")
}

#[test]
fn every_native_history_and_visible_status_presents_real_attempt_metadata() {
    let mut run = run(DesignHistory::Transcript);
    run.record_model_attempts(run.epoch, &attempts());
    run.phase = Phase::Finished;
    for history in DesignHistory::ALL {
        run.design.history = history;
        let text = labels(design::history_view(&run, None));
        assert!(
            text.contains("Reported tokens · 30 input · 10 output · 40 total"),
            "{history}"
        );
        assert!(text.contains("AI decision · Invalid reply · 0.012 s"));
        assert!(text.contains("AI response repair · Completed · 0.050 s"));
    }
    let id = window::Id::unique();
    let mut state = crate::workspace::terminal_agent::State::default();
    state.runs.insert(id, run);
    for status in DesignStatus::ALL {
        state.runs.get_mut(&id).unwrap().design.status = status;
        let view = state.pane_agent_status(SessionId(7), 9);
        if status == DesignStatus::Hidden {
            assert!(view.is_none());
        } else {
            assert!(
                labels(view.unwrap()).contains("40 reported tokens"),
                "{status}"
            );
        }
    }
    assert!(state.pane_agent_status(SessionId(7), 10).is_none());
}

#[test]
fn every_native_history_and_visible_status_presents_live_repair_before_completion() {
    let mut run = run(DesignHistory::Transcript);
    assert!(run.model_repair_started(run.epoch, &attempts()[0]));
    for history in DesignHistory::ALL {
        run.design.history = history;
        let text = labels(design::history_view(&run, None));
        assert!(text.contains("AI response repair · Running"), "{history}");
        assert!(
            text.contains("Repairing the AI response format."),
            "{history}"
        );
        assert!(text.contains("Reported tokens · 10 input · 2 output · 12 total"));
        assert!(!text.contains("AI response repair · Completed"));
    }
    let id = window::Id::unique();
    let mut state = crate::workspace::terminal_agent::State::default();
    state.runs.insert(id, run);
    for status in DesignStatus::ALL {
        state.runs.get_mut(&id).unwrap().design.status = status;
        let view = state.pane_agent_status(SessionId(7), 9);
        if status == DesignStatus::Hidden {
            assert!(view.is_none());
        } else {
            assert!(
                labels(view.unwrap())
                    .to_lowercase()
                    .contains("repairing response"),
                "{status}"
            );
        }
    }
    assert!(state.pane_agent_status(SessionId(7), 10).is_none());
}

#[test]
fn failed_and_cancelled_partial_usage_stays_visible_without_inventing_a_total() {
    let mut run = run(DesignHistory::InlineBlocks);
    let mut records = attempts();
    records[0].outcome = AttemptOutcome::Failed;
    records[0].input_tokens = Some(0);
    records[0].output_tokens = None;
    records[0].total_tokens = None;
    records[1].outcome = AttemptOutcome::Cancelled;
    run.record_model_attempts(run.epoch, &records);
    run.phase = Phase::Cancelled;
    let update = run.inline_update(false).unwrap();
    assert!(update.transcript.tokens.is_none());
    assert_eq!(update.transcript.steps[1].state, StepState::Cancelled);
    let text = labels(design::history_view(&run, None));
    assert!(text.contains("0 input · output unavailable · total unavailable"));
    assert!(text.contains("28 total (reported subtotal; 1/2 attempts)"));
    assert!(text.contains("AI response repair · Cancelled"));
}

#[derive(Default)]
struct ScrollBounds(Vec<Rectangle>);
impl Operation for ScrollBounds {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
        visit(self);
    }
    fn scrollable(
        &mut self,
        _: Option<&Id>,
        bounds: Rectangle,
        _: Rectangle,
        _: Vector,
        _: &mut dyn operation::Scrollable,
    ) {
        self.0.push(bounds);
    }
}

#[test]
fn compact_native_history_keeps_most_space_for_transcript_below_usage() {
    let mut run = run(DesignHistory::Transcript);
    run.record_model_attempts(run.epoch, &attempts());
    for height in [48.0, 72.0, 120.0, 320.0, 600.0] {
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let mut ui = UserInterface::build(
            design::history_view(&run, None),
            Size::new(300.0, height),
            Cache::default(),
            &mut renderer,
        );
        let mut bounds = ScrollBounds::default();
        ui.operate(&renderer, &mut bounds);
        assert_eq!(bounds.0.len(), 2);
        let usage = bounds.0[0];
        let transcript = bounds.0[1];
        assert!(usage.height <= height * 0.28 + 0.01 && usage.height <= 104.01);
        assert!(
            transcript.height >= height * 0.68,
            "transcript lost room at {height} px"
        );
        assert!(transcript.y + transcript.height <= height + 0.01);
    }
}
