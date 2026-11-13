use std::io::Write as _;

use kortty_jobs::{MAX_SWARM_COMPARISON_BYTES, RunOutcome, SwarmReport, SwarmTargetReport};
use serde_json::{Value, json};

use super::{apply_comparison, cancel_completion};
use crate::job_executor::Completion;
use crate::local_ai::test_support;
use crate::terminal_agent::swarm::comparison::{ScheduledScope, ScheduledTemplate};
use crate::terminal_logging::{Policy, Redactor, Secrets};
use crate::workspace::terminal_ai::mask::prompt as mask;

const SECRET: &str = "scheduler-comparison-fixture-value";

fn completion() -> Completion {
    Completion {
        id: serde_json::from_value(json!(74)).unwrap(),
        outcome: RunOutcome::Blocked,
        detail: Some("Target 14 was blocked.".into()),
        recording: None,
        swarm_report: Some(Box::new(SwarmReport {
            question: "Compare the results for ***".into(),
            targets: vec![
                SwarmTargetReport {
                    target: 31,
                    label: "First target".into(),
                    outcome: RunOutcome::Success,
                    detail: "The service was healthy.".into(),
                },
                SwarmTargetReport {
                    target: 14,
                    label: "Second target".into(),
                    outcome: RunOutcome::Blocked,
                    detail: "The saved route changed.".into(),
                },
            ],
            comparison: None,
        })),
    }
}

#[test]
fn scheduled_comparison_keeps_the_completed_targets_and_attaches_masked_model_result() {
    let _worker = test_support::exclusive_run();
    let (listener, profile) = test_support::listener();
    let secrets = Secrets::default();
    secrets.add(SECRET);
    let redactor = Redactor::for_journal(secrets.seal().unwrap(), Policy::default());
    let mut completion = completion();
    let original = completion.swarm_report.as_deref().unwrap().clone();
    let template = ScheduledTemplate::prepare(
        ScheduledScope {
            profile: &profile,
            profile_revision: [47; 32],
            context_limit: 8192,
            targets: &[31, 14],
        },
        &format!("Compare the results for {SECRET}"),
        None,
        &redactor,
        mask,
    )
    .unwrap();
    let request = template.fill(&original.targets, &redactor, mask).unwrap();
    let [system, input] = request.messages().map(str::to_owned);
    let pending = request.start(profile, redactor, mask, || Ok(())).unwrap();
    let mut socket = test_support::accept(&listener);
    let wire = test_support::request(&mut socket);
    assert_eq!(wire["max_tokens"], 2048);
    assert_eq!(wire["messages"][0]["content"], system);
    assert_eq!(wire["messages"][1]["content"], input);
    let parsed: Value = serde_json::from_str(&input).unwrap();
    assert_eq!(parsed["question"], original.question);
    assert_eq!(
        parsed["targets"],
        json!([
            {"target":31,"label":"Profile 31","status":"Completed","detail":"The service was healthy."},
            {"target":14,"label":"Profile 14","status":"Blocked","detail":"The saved route changed."}
        ])
    );
    let markdown = format!(
        "| Profile | Result | Errors |\n| --- | --- | --- |\n| 31 | Healthy | - |\n| 14 | {SECRET} | Route changed |"
    );
    socket
        .get_mut()
        .write_all(test_support::fixture_response("200 OK", "", &markdown).as_bytes())
        .unwrap();
    drop(socket);
    let reply = pending.reply.recv_blocking().unwrap().unwrap();
    let masked = reply.markdown.unwrap();
    assert_eq!(masked, markdown.replace(SECRET, "***"));
    apply_comparison(&mut completion, Some(&masked));
    let report = completion.swarm_report.as_deref().unwrap();
    assert_eq!(report.comparison.as_deref(), Some(masked.as_str()));
    assert_eq!(report.targets, original.targets);
    assert_eq!(report.question, original.question);
    assert_eq!(completion.outcome, RunOutcome::Blocked);
}

#[test]
fn scheduled_comparison_fallback_keeps_the_target_report_and_cancellation_drops_it() {
    let mut completion = completion();
    let original = completion.swarm_report.clone();
    let over_limit = "x".repeat(MAX_SWARM_COMPARISON_BYTES + 1);
    for candidate in [None, Some(over_limit.as_str()), Some("Invalid\u{001b}")] {
        apply_comparison(&mut completion, candidate);
        assert_eq!(completion.swarm_report, original);
        assert_eq!(completion.outcome, RunOutcome::Blocked);
    }
    apply_comparison(
        &mut completion,
        Some("| Profile | Errors |\n| --- | --- |\n| 31 | - |\n| 14 | Route changed |"),
    );
    assert!(
        completion
            .swarm_report
            .as_ref()
            .unwrap()
            .comparison
            .is_some()
    );
    cancel_completion(&mut completion);
    assert_eq!(completion.outcome, RunOutcome::Cancelled);
    assert!(completion.swarm_report.is_none());
}
