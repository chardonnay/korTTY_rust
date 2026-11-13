use crate::terminal_agent::design::{History as DesignHistory, Layout};
use crate::terminal_agent::usage::{Attempt, AttemptKind, AttemptOutcome};
use crate::terminal_logging::{RedactionSecrets, Secrets};
use crate::workspace::terminal_agent::{ProfileChoice, Purpose, RunSetup, binding, engine};

use super::super::{MAX_STEPS, SessionId};
use super::*;

const SECRET: &str = "private-run-fixture-value";

fn run(private: bool) -> Run {
    let profile = ProfileChoice {
        id: 1,
        reference: crate::ai_manager::ProfileRef::Numeric(1),
        label: "Fixture".into(),
        reviewed_https: None,
        reviewed_cli: None,
    };
    let secrets = Secrets::default();
    secrets.add(SECRET);
    let mut run = Run::from_setup(RunSetup {
        binding: binding::Binding::Live(binding::Live {
            session: SessionId(7),
            stream: 9,
            private,
        }),
        mode: engine::Mode::Run,
        label: "Fixture terminal".into(),
        context: "Initial terminal context is excluded".into(),
        manual_directory: None,
        profiles: vec![profile.clone()],
        purpose: Purpose::Agent(profile),
        swarm: false,
        design: Layout {
            history: DesignHistory::Transcript,
            ..Layout::default()
        },
        redactor: Redactor::for_journal(secrets.seal().unwrap(), Default::default()),
    });
    run.goal = format!("Inspect {SECRET}");
    run.next(Phase::Thinking, "Fixture decision");
    run
}

#[test]
fn settled_non_inline_run_keeps_masked_decisions_and_executed_commands_for_chat() {
    let mut run = run(false);
    assert!(run.inline_update(false).is_none());
    assert!(run.follow_up_trace().is_err());
    let detail = mask::prompt(&run.redactor, &format!("Inspecting {SECRET}")).unwrap();
    run.inline_decision(
        &engine::Action::Commands(vec![engine::Command {
            command: "printf unapproved".into(),
            purpose: "Unapproved command".into(),
            risk: engine::Risk::ReadOnly,
            elevation: engine::Elevation::None,
        }]),
        "Read the source",
        &detail,
    );
    run.record_model_attempts(
        run.epoch,
        &[Attempt {
            kind: AttemptKind::Initial,
            outcome: AttemptOutcome::Completed,
            elapsed: std::time::Duration::ZERO,
            input_tokens: Some(2),
            output_tokens: Some(3),
            total_tokens: Some(5),
        }],
    );
    run.inline_command("printf approved");
    let output = mask::prompt(&run.redactor, &format!("stdout {SECRET}")).unwrap();
    run.inline_result(engine::Status::Exited(7), &output, "stderr fixture");
    run.inline_decision(&engine::Action::Done, "Done", "Review the reported exit.");
    run.next(Phase::Finished, "Agent run finished.");

    let trace = run.follow_up_trace().unwrap();
    assert_eq!(trace.goal, "Inspect ***");
    assert_eq!(trace.answer, "Review the reported exit.");
    assert_eq!(trace.steps.len(), 2);
    assert!(matches!(trace.steps[0].kind, StepKind::Decision));
    assert_eq!(trace.steps[0].detail, "Inspecting ***");
    assert!(trace.steps[0].command.is_none());
    let command = &trace.steps[1];
    assert!(matches!(command.kind, StepKind::Command));
    assert_eq!(command.command.as_deref(), Some("printf approved"));
    assert_eq!(command.exit_code, Some(7));
    assert!(matches!(command.status, Some(StepStatus::Failed)));
    assert!(command.elapsed_seconds.is_some());
    assert_eq!(command.truncated, None);
    assert_eq!(command.output, ["stdout ***", "stderr fixture"]);
    assert!(run.inline_update(false).is_none());
}

#[test]
fn private_plan_ready_keeps_an_in_memory_snapshot_without_approval_authority() {
    let mut run = run(true);
    run.inline_command("printf no-confirmed-result");
    run.plan = Some(format!("Investigate {SECRET}"));
    run.next(Phase::PlanReady, "Plan ready.");

    let trace = run.follow_up_trace().unwrap();
    assert_eq!(trace.answer, "Investigate ***");
    assert_eq!(trace.steps.len(), 1);
    let step = &trace.steps[0];
    assert!(matches!(step.kind, StepKind::Warning));
    assert!(step.command.is_none());
    assert!(step.output.is_empty());
    assert!(run.inline_update(false).is_none());
}

#[test]
fn newly_learned_password_is_remasked_before_clipping_the_stored_trace() {
    const LEARNED: &str = r#"newly-"learned\credential"#;
    let mut run = run(false);
    let title = "a".repeat(MAX_TITLE_BYTES - 8);
    let detail = "b".repeat(MAX_DETAIL_BYTES - 8);
    let command = "c".repeat(MAX_COMMAND_BYTES - 8);
    let output = "d".repeat(MAX_OUTPUT_LINE_BYTES - 8);
    run.inline_decision(
        &engine::Action::Commands(Vec::new()),
        &format!("{title}{LEARNED}"),
        &format!("{detail}{LEARNED}"),
    );
    run.inline_command(&format!("{command}{LEARNED}"));
    run.inline_result(
        engine::Status::Exited(u32::MAX),
        &format!(
            "Dropped line\nRetained line 1\nRetained line 2\nRetained line 3\n{output}{LEARNED}"
        ),
        "",
    );
    run.inline.as_mut().unwrap().transcript.steps[1].elapsed_ms = 2_345;
    let learned = RedactionSecrets::with_secret(None, LEARNED).unwrap();
    run.redactor = run.redactor.with_overlay(&learned).unwrap();
    run.learned_sudo = Some(learned);
    run.next(Phase::Finished, "Agent run finished.");

    let trace = run.follow_up_trace().unwrap();
    assert_eq!(trace.steps[0].title, format!("{title}***"));
    assert_eq!(trace.steps[0].detail, format!("{detail}***"));
    let step = &trace.steps[1];
    assert_eq!(step.command.as_ref().unwrap(), &format!("{command}***"));
    assert_eq!(step.output.len(), MAX_OUTPUT_LINES);
    assert_eq!(step.output[0], "Retained line 1");
    assert_eq!(step.output.last().unwrap(), &format!("{output}***"));
    assert_eq!(step.exit_code, Some(i64::from(u32::MAX)));
    assert!(matches!(step.status, Some(StepStatus::Failed)));
    assert_eq!(step.elapsed_seconds, Some(2));
    assert_eq!(step.truncated, Some(true));
}

#[test]
fn newest_complete_entries_fit_the_utf8_budget_in_chronological_order() {
    let mut run = run(false);
    let long = "🧭".repeat(MAX_DETAIL_BYTES);
    for index in 0..MAX_STEPS + 4 {
        run.inline_decision(
            &engine::Action::Commands(Vec::new()),
            &format!("{index:02} {long}"),
            &long,
        );
    }
    run.next(Phase::Blocked, "Check the terminal and try again.");
    run.notice = "Close another chat tab and try the handoff again.".into();
    let trace = run.follow_up_trace().unwrap();
    assert_eq!(trace.answer, "Check the terminal and try again.");
    assert!(trace.steps.len() > 1 && trace.steps.len() < MAX_STEPS);
    let first = MAX_STEPS + 4 - trace.steps.len();
    for (index, step) in trace.steps.iter().enumerate() {
        assert!(step.title.starts_with(&format!("{:02} ", first + index)));
        assert!(step.title.ends_with('…') && step.detail.ends_with('…'));
        assert!(step.title.len() <= MAX_TITLE_BYTES && step.detail.len() <= MAX_DETAIL_BYTES);
    }
    let total = trace.goal.len()
        + trace.answer.len()
        + trace
            .steps
            .iter()
            .map(|step| step.title.len() + step.detail.len())
            .sum::<usize>();
    assert!(total <= MAX_TRACE_TEXT_BYTES);
}
