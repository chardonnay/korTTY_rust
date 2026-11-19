use serde_json::json;

use super::*;
use crate::snippet_analysis::model::hardening::{HardeningOption, InputOption};
use crate::snippets::SnippetId;

fn classic(option: HardeningOption) -> Requirement {
    Requirement::Hardening {
        option,
        declarative: false,
    }
}

fn request(language: &str, requirements: Vec<Requirement>) -> Result<Request, String> {
    Request::new(
        Snapshot::snippet("test.sh".into(), language.into(), "echo work\n".into()),
        &Analysis::default(),
        Selection {
            requirements,
            ..Selection::default()
        },
        String::new(),
        String::new(),
        "en",
        CodeTextLanguage::English,
    )
}

fn answer(lines: &[&str], items: &[WorkItem]) -> String {
    let changes: Vec<_> = items
        .iter()
        .map(|item| json!({"id": item.id, "reason": "Applied the option."}))
        .collect();
    json!({"lines": lines, "changes": changes}).to_string()
}

#[test]
fn legacy_selection_keeps_its_serialized_form() {
    let legacy = r#"{"findings":["SEC-1"],"dependencies":[]}"#;
    let selected: Selection = serde_json::from_str(legacy).unwrap();
    assert!(selected.requirements.is_empty());
    assert_eq!(serde_json::to_string(&selected).unwrap(), legacy);
}

#[test]
fn option_only_apply_rechecks_earlier_literals_after_restart() {
    let profile = Profile::new("127.0.0.1:12345", "fixture")
        .unwrap()
        .with_prompt_preset(local_ai::PromptPreset::Llama);
    let request = request(
        "bash",
        vec![
            classic(HardeningOption::SafeMode),
            classic(HardeningOption::HelpUsage),
            classic(HardeningOption::LoggingVerbose),
            classic(HardeningOption::StrictMode),
        ],
    )
    .unwrap()
    .with_profile(&profile);
    let first = &request.items[..3];
    let next = &request.items[3..];
    let guarded = "case $1 in --dry-run|--yes|--help|--verbose|-v) exit 0;; esac";
    let answer_with_ids_only = answer(&["echo changed"], first);
    assert!(
        request
            .parse_stage(&answer_with_ids_only, request.source(), first, 0)
            .is_err()
    );
    let first_result = request
        .parse_stage(
            &answer(&[guarded, "echo work"], first),
            request.source(),
            first,
            0,
        )
        .unwrap();
    let recovery = ApplyRecovery {
        revision: SnippetId::new().unwrap(),
        started_at: 1,
        elapsed_millis: 2,
        profile_name: "Reviewed local endpoint".into(),
        model: "test".into(),
        profile_revision: None,
        request: request.recovery_snapshot().unwrap(),
        checkpoint: Some(Arc::new(Checkpoint {
            code_sha256: content_hash(&first_result.code).unwrap(),
            code: first_result.code.into(),
            code_omitted: false,
            reasons: first_result.changes,
            completed: 1,
            migration_notes: Vec::new(),
        })),
    };
    let restored =
        Request::from_recovery(request.snapshot.clone(), &Analysis::default(), &recovery).unwrap();
    assert_eq!(restored.system, request.system);
    restored.require_current_preset(&profile).unwrap();
    assert!(
        restored
            .require_current_preset(&profile.with_prompt_preset(local_ai::PromptPreset::Qwen))
            .is_err()
    );
    let checkpoint = recovery.checkpoint.as_deref().unwrap();
    let remaining_preview = restored.preview_from(Some(checkpoint)).unwrap();
    assert!(remaining_preview.contains("Structured requirements completed in earlier stages"));
    let error = restored
        .parse_stage(
            &answer(&["set -eu", "echo work"], next),
            &checkpoint.code,
            next,
            1,
        )
        .err()
        .unwrap();
    assert!(error.contains("HARDENING-SAFE-MODE"));
    assert!(
        restored
            .parse_stage(
                &answer(&["set -eu", guarded, "echo work"], next),
                &checkpoint.code,
                next,
                1
            )
            .is_ok()
    );
}

#[test]
fn unsupported_declarative_input_guard_cannot_reach_the_preview() {
    let result = request(
        "Ansible",
        vec![Requirement::Input {
            option: InputOption::FileChecks,
            max_file_size_mib: 0,
        }],
    );
    assert!(result.err().unwrap().contains("unavailable"));
}

#[test]
fn analysis_apply_persists_migration_before_sending_selected_work() {
    use std::io::Write as _;

    use crate::local_ai::test_support::{
        accept, fixture_response, listener, request as wire_request,
    };
    use crate::snippet_analysis::model::Finding;

    let original = "#!/usr/bin/env bash\npython3 -c 'print(\"hello\")'";
    let migrated = "#!/usr/bin/env python3\nprint(\"hello\")";
    let fixed = "#!/usr/bin/env python3\nimport sys\nprint(\"hello\", file=sys.stderr)";
    let snapshot = Snapshot::snippet("fixture.sh".into(), "bash".into(), original.into());
    let report = Analysis {
        improvements: vec![Finding {
            id: "I1".into(),
            title: "Separate diagnostics".into(),
            line: Some(2),
            ..Finding::default()
        }],
        ..Analysis::default()
    };
    let plan = migration::Plan::WholeScript(migration::Language::Python);
    let request = Request::for_plan(
        snapshot.clone(),
        &report,
        Selection {
            findings: vec!["I1".into()],
            ..Selection::default()
        },
        String::new(),
        String::new(),
        ("en", CodeTextLanguage::English),
        Some(plan),
    )
    .unwrap();
    assert_eq!(request.stage_count(), 2);
    let preview = request.preview().unwrap();
    assert!(preview.contains("Migration stage"));
    assert!(preview.contains("Stage 2 uses the previous stage"));

    let (listener, profile) = listener();
    let server = |listener, response: serde_json::Value| {
        std::thread::spawn(move || {
            let mut connection = accept(&listener);
            let wire = wire_request(&mut connection);
            let reply = fixture_response("200 OK", "", &response.to_string());
            connection.get_mut().write_all(reply.as_bytes()).unwrap();
            wire["messages"][1]["content"].as_str().unwrap().to_owned()
        })
    };
    let worker = server(
        listener.try_clone().unwrap(),
        json!({"text": migrated, "notes": ["Now uses Python."]}),
    );
    let (updates, _progress) = async_channel::bounded(1);
    let observer = Observer::new(updates, None);
    let client = Client::new(profile);
    let mut point = request.initial();
    let error = request
        .run_stages(&client, &mut point, &observer, &|_| Ok(()), |_| {
            Err("Stop after checkpoint".into())
        })
        .unwrap_err();
    assert_eq!(error, "Stop after checkpoint");
    let wire = worker.join().unwrap();
    assert!(wire.contains("Migration stage"));
    assert!(!wire.contains("Separate diagnostics"));
    assert_eq!(point.completed(), 1);
    assert_eq!(point.code.as_ref(), migrated);
    assert_eq!(point.migration_notes, ["Now uses Python."]);

    let recovery = ApplyRecovery {
        revision: SnippetId::new().unwrap(),
        started_at: 1,
        elapsed_millis: 2,
        profile_name: "Fixture".into(),
        model: "Fixture".into(),
        profile_revision: None,
        request: serde_json::from_value(
            serde_json::to_value(request.recovery_snapshot().unwrap()).unwrap(),
        )
        .unwrap(),
        checkpoint: Some(Arc::new(point.clone())),
    };
    let restored = Request::from_recovery(snapshot, &report, &recovery).unwrap();
    let partial = restored.partial(&point, 1).unwrap();
    partial.validate(&report).unwrap();
    assert!(partial.selected.is_empty());
    assert_eq!(partial.migration, Some(plan));
    assert_eq!(partial.after, migrated);
    let remaining = restored.preview_from(Some(&point)).unwrap();
    assert!(remaining.contains("Stage 2 user prompt"));
    assert!(remaining.contains("Snippet language: python"));
    let worker = server(
        listener,
        answer(&fixed.lines().collect::<Vec<_>>(), &restored.items)
            .parse()
            .unwrap(),
    );
    restored
        .run_stages(&client, &mut point, &observer, &|_| Ok(()), |_| Ok(()))
        .unwrap();
    let wire = worker.join().unwrap();
    assert!(wire.contains("Snippet language: python"));
    assert!(wire.contains("Separate diagnostics"));
    assert!(!wire.contains("python3 -c"));
    let run = restored.completed(&point, 1).unwrap();
    run.validate(&report).unwrap();
    assert_eq!(run.migration, Some(plan));
    assert_eq!(run.migration_notes, ["Now uses Python."]);
    assert_eq!(run.before, original);
    assert_eq!(run.after, fixed);
}

#[test]
fn apply_format_retry_rechecks_dispatch_authority() {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::local_ai::test_support::{accept, fixture_http_response, listener, request};

    let (listener, profile) = listener();
    let allowed = Arc::new(AtomicBool::new(true));
    let worker_allowed = Arc::clone(&allowed);
    let server = std::thread::spawn(move || {
        let mut connection = accept(&listener);
        assert_eq!(
            request(&mut connection)["response_format"]["type"],
            "json_object"
        );
        worker_allowed.store(false, Ordering::Release);
        let refusal = fixture_http_response(
            "400 Bad Request",
            "",
            r#"{"error":{"code":"unsupported_response_format"}}"#,
        );
        connection.get_mut().write_all(refusal.as_bytes()).unwrap();
        listener
    });
    let client = Client::new(profile);
    let (updates, _progress) = async_channel::bounded(1);
    let observer = Observer::new(updates, None);
    let result = send(
        &client,
        "system",
        "exact reviewed apply input",
        &mut true,
        &|_| {
            allowed
                .load(Ordering::Acquire)
                .then_some(())
                .ok_or_else(|| "Managed header revoked".into())
        },
        observer.attempt(1, 1, &[], false),
    );
    assert!(result.unwrap_err().contains("Managed header revoked"));
    assert!(matches!(
        server.join().unwrap().accept(),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
}
