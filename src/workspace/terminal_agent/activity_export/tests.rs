use std::time::Duration;

use crate::terminal_agent::usage::{Attempt, AttemptKind, AttemptOutcome};
use crate::terminal_logging::Secrets;
use crate::workspace::terminal_agent::{ProfileChoice, Purpose, RunSetup, binding};

use super::*;

#[test]
fn completed_archive_keeps_usage_coverage_after_attempt_rows_are_evicted() {
    let source = LiveBinding {
        session: SessionId(4),
        stream: 7,
        private: false,
    };
    let profile = ProfileChoice {
        id: 1,
        reference: crate::ai_manager::ProfileRef::Numeric(1),
        label: "Fixture".into(),
        reviewed_https: None,
        reviewed_cli: None,
    };
    let mut run = Run::from_setup(RunSetup {
        binding: binding::Binding::Live(source),
        mode: Mode::Ask,
        label: "Fixture terminal".into(),
        context: String::new(),
        manual_directory: None,
        profiles: vec![profile.clone()],
        purpose: Purpose::Agent(profile),
        swarm: false,
        design: Default::default(),
        redactor: Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default()),
    });
    run.goal = "Inspect the source".into();
    run.activity_export.start();
    for turn in 0..20 {
        let epoch = run.next(Phase::Thinking, "Fixture decision");
        run.record_model_attempts(
            epoch,
            &[
                Attempt {
                    kind: AttemptKind::Initial,
                    outcome: AttemptOutcome::InvalidReply,
                    elapsed: Duration::from_millis(12),
                    input_tokens: Some(turn),
                    output_tokens: None,
                    total_tokens: None,
                },
                Attempt {
                    kind: AttemptKind::Repair,
                    outcome: AttemptOutcome::Completed,
                    elapsed: Duration::from_millis(50),
                    input_tokens: None,
                    output_tokens: Some(0),
                    total_tokens: Some(0),
                },
            ],
        );
    }
    run.record("Original activity".into());
    run.next(Phase::Finished, "Complete");
    let mut archive = Archive::default();
    archive.capture(&mut run);
    let mut retained = archive.matching(source);
    assert_eq!(retained.len(), 1);
    let snapshot = retained.pop().unwrap();
    assert!(Arc::ptr_eq(
        &snapshot,
        run.activity_export.snapshot.as_ref().unwrap()
    ));

    let record = serde_json::to_value(&*snapshot).unwrap();
    assert_eq!(record["activity"], serde_json::json!(["Original activity"]));
    assert_eq!(record["modelUsage"]["attempts"], 40);
    assert_eq!(record["modelUsage"]["promptTokens"]["reported"], 190);
    assert_eq!(record["modelUsage"]["totalTokens"]["reported"], 0);
    assert_eq!(record["modelUsage"]["totalTokens"]["attemptsWithCount"], 20);
    let attempts = record["modelAttempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 32);
    assert_eq!(attempts[0]["promptTokens"], 4);
    assert_eq!(attempts[0]["outcome"], "invalid reply");
    assert!(attempts[0]["totalTokens"].is_null());
    assert_eq!(attempts[31]["kind"], "repair");
    assert_eq!(attempts[31]["totalTokens"], 0);
    assert!(
        archive
            .matching(LiveBinding {
                stream: 8,
                ..source
            })
            .is_empty()
    );
    assert!(
        archive
            .matching(LiveBinding {
                private: true,
                ..source
            })
            .is_empty()
    );
}
