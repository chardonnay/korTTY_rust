use std::io::{ErrorKind, Read as _, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use iced::futures::{FutureExt as _, Stream, StreamExt as _};
use serde_json::json;

use crate::local_ai::test_support::{accept, exclusive_run, fixture_response, listener, request};
use crate::snippet_analysis::model::{ChangeReason, Dependency, Finding};
use crate::snippets::{Input, Library, tests::Profile};

use super::*;

fn complete(workspace: &mut Workspace, task: Task<Message>) -> Task<Message> {
    complete_step(workspace, task, "apply workflow step")
}

fn complete_step(workspace: &mut Workspace, task: Task<Message>, step: &str) -> Task<Message> {
    let mut stream = iced_runtime::task::into_stream(task)
        .unwrap_or_else(|| panic!("Expected a scheduled task for {step}"));
    let deadline = Instant::now() + Duration::from_secs(8);
    complete_stream(workspace, &mut stream, deadline)
}

fn complete_stream(
    workspace: &mut Workspace,
    stream: &mut (impl Stream<Item = iced_runtime::Action<Message>> + Unpin),
    deadline: Instant,
) -> Task<Message> {
    loop {
        let message = next_output(stream, deadline);
        let provisional = matches!(
            &message,
            Message::SnippetAnalysis(_, super::super::Event::Apply(Event::Progress(..)))
        );
        let next = workspace.update(message);
        if !provisional {
            return next;
        }
    }
}

fn next_output(
    stream: &mut (impl Stream<Item = iced_runtime::Action<Message>> + Unpin),
    deadline: Instant,
) -> Message {
    loop {
        match stream.next().now_or_never() {
            Some(Some(iced_runtime::Action::Output(message))) => return message,
            Some(None) => panic!("Apply workflow step ended without a message"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "Apply workflow did not complete");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn interrupted_apply_resumes_reviewed_stages_and_preserves_source_until_accept_and_save() {
    reviewed_staged_apply(false);
}

#[test]
fn interrupted_apply_reviews_completed_changes_locally_and_preserves_partial_after_evidence() {
    reviewed_staged_apply(true);
}

#[test]
fn zero_retention_keeps_one_live_apply_review_and_accepts_only_its_exact_code() {
    let _exclusive = exclusive_run();
    let profile = Profile::new();
    let before = "echo private-original\n";
    let after = "printf '%s\\n' private-replacement\n";
    let mut library = Library::open(profile.path()).unwrap();
    let snippet = library
        .save(
            None,
            Input {
                name: "Test snippet".into(),
                language: "bash".into(),
                category: String::new(),
                content: before.into(),
            },
        )
        .unwrap();
    let subject = Subject::Snippet(snippet);
    let window = SnippetWindow::open(profile.path()).unwrap();
    let root = window
        .analysis_library()
        .path()
        .parent()
        .unwrap()
        .to_path_buf();
    let _limit = crate::snippet_analysis::store::Store::test_retention_limit(root.clone(), 0);
    let history = store(&window).unwrap();
    let pre = Record::new(
        Purpose::Analysis,
        None,
        Source::new(before, "bash", "en", "en", "Test snippet").unwrap(),
        Provenance {
            profile_id: ProfileRef::Numeric(7),
            ..Provenance::default()
        },
        Analysis {
            improvements: vec![Finding {
                id: "I1".into(),
                title: "Use a predictable command".into(),
                ..Finding::default()
            }],
            ..Analysis::default()
        },
    )
    .unwrap();
    let pre_id = pre.id;
    history.append(subject, pre).unwrap();
    let (listener, _) = listener();
    let endpoint = listener.local_addr().unwrap().to_string();
    let mut workspace = Workspace::default();
    workspace.set_test_ai_configuration(serde_json::from_value(json!({
        "profiles": {"profiles": [{"id": 7, "name": "Local", "model": "fixture", "endpoint": endpoint}], "default": 7, "last_id": 7}
    })).unwrap());
    let (owner, _) = workspace.open_window(false);
    let (id, _) = workspace.install_snippet_surface(owner, window, true);
    let _ = workspace.update_snippets(id, SnippetMessage::Open(snippet));
    let load = workspace.open_snippet_analysis(id, subject);
    let _ = complete_step(&mut workspace, load, "the initial analysis history load");
    let page = workspace.snippets[&id].analysis.page(subject).unwrap();
    assert!(
        matches!(page.state, State::Ready),
        "History failed to load: {:?}",
        page.status
    );
    let update = |workspace: &mut Workspace, action| {
        workspace.update_snippet_analysis(id, super::super::Action::Apply(action))
    };
    let _ = update(
        &mut workspace,
        Action::Hardening(
            pre_id,
            crate::snippet_window::analysis::hardening::Edit::AllClassic(false),
        ),
    );
    let _ = update(&mut workspace, Action::AllFindings(pre_id, true));
    let _ = update(&mut workspace, Action::Plan(pre_id, false));
    let page = workspace.snippets[&id].analysis.page(subject).unwrap();
    let State::ApplyPreview(preview) = &page.state else {
        panic!("Expected the reviewed provider request: {:?}", page.status);
    };
    assert!(preview.prompt.contains(before.trim_end()));
    let worker = update(&mut workspace, Action::Approve);
    let page = workspace.snippets[&id].analysis.page(subject).unwrap();
    assert!(
        matches!(page.state, State::Applying(_)),
        "Provider request did not start: {:?}",
        page.status
    );
    let provider = thread::spawn(move || {
        let mut socket = accept(&listener);
        let sent = request(&mut socket);
        assert!(
            sent["messages"][1]["content"]
                .as_str()
                .unwrap()
                .contains(before.trim_end())
        );
        let content = json!({
            "lines": [after.trim_end()],
            "changes": [{"id": "I1", "reason": "Use the selected command", "anchor": after.trim_end()}]
        });
        socket
            .get_mut()
            .write_all(fixture_response("200 OK", "", &content.to_string()).as_bytes())
            .unwrap();
    });
    let save = complete_step(&mut workspace, worker, "model proposal generation");
    let page = workspace.snippets[&id].analysis.page(subject).unwrap();
    assert!(
        matches!(page.state, State::SavingApply(_)),
        "Model proposal did not reach receipt storage: {:?}",
        page.status
    );
    let _ = complete_step(&mut workspace, save, "storage of the proposal receipt");
    provider.join().unwrap();

    let page = workspace.snippets[&id].analysis.page(subject).unwrap();
    let State::ReviewApply(review) = &page.state else {
        panic!("Expected the live source review: {:?}", page.status);
    };
    assert_eq!(review.run.before, before);
    assert_eq!(review.run.after, after);
    let run = review.run.id;
    let saved = history.load(subject).unwrap();
    assert!(!saved.current().unwrap().apply_runs[0].has_review_source());
    let accept = update(&mut workspace, Action::Accept);
    let surface = &workspace.snippets[&id];
    let page = surface.analysis.page(subject).unwrap();
    assert!(
        matches!(page.state, State::DecidingApply(_)) && surface.window.analysis_apply_locked(),
        "Acceptance did not lock the reviewed editor revision: {:?}",
        page.status
    );
    let _ = complete_step(
        &mut workspace,
        accept,
        "acceptance of the reviewed proposal",
    );
    assert_eq!(
        workspace.snippets[&id]
            .window
            .analysis_snippet(snippet)
            .unwrap()
            .2,
        after
    );
    assert_eq!(
        Library::open(profile.path())
            .unwrap()
            .get(snippet)
            .unwrap()
            .content(),
        before
    );
    let saved = history.load(subject).unwrap();
    let accepted = &saved.current().unwrap().apply_runs[0];
    assert_eq!(accepted.status, ApplyStatus::Accepted);
    assert_eq!(accepted.after_sha256, content_hash(after).unwrap());
    assert!(!accepted.has_review_source());
    let path = root
        .join("snippet-analyses")
        .join(format!("{}.json", String::from(snippet)));
    let raw = std::fs::read_to_string(path).unwrap();
    assert!(!raw.contains("private-original") && !raw.contains("private-replacement"));

    // Acceptance selects the editor but keeps the analysis page. Select the
    // page again so Close removes it before checking a fresh history load.
    let reselect = workspace.open_snippet_analysis(id, subject);
    assert!(iced_runtime::task::into_stream(reselect).is_none());
    assert_eq!(workspace.snippets[&id].analysis.active, Some(subject));
    let _ = workspace.update_snippet_analysis(id, super::super::Action::Close);
    assert!(workspace.snippets[&id].analysis.page(subject).is_none());
    let reopen = workspace.open_snippet_analysis(id, subject);
    let _ = complete_step(
        &mut workspace,
        reopen,
        "reopening the saved analysis from disk",
    );
    let _ = update(&mut workspace, Action::Open(pre_id, run));
    let page = workspace.snippets[&id].analysis.page(subject).unwrap();
    let State::ReviewApply(review) = &page.state else {
        panic!(
            "Expected the reopened metadata-only proposal: {:?}",
            page.status
        );
    };
    assert!(!review.run.has_review_source());
    let restore = update(&mut workspace, Action::Restore);
    assert!(iced_runtime::task::into_stream(restore).is_none());
    let surface = &workspace.snippets[&id];
    assert!(
        surface
            .analysis
            .page(subject)
            .unwrap()
            .status
            .as_deref()
            .is_some_and(|status| status.contains("omitted"))
    );
    assert!(!surface.window.analysis_apply_locked());
}

fn reviewed_staged_apply(review_partial: bool) {
    let _exclusive = exclusive_run();
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let before = "#!/bin/sh\r\necho unsafe\r\n";
    let after = if review_partial {
        "#!/bin/sh\r\nprintf '%s\\n' safe\r\n# pending check\r\n"
    } else {
        "#!/bin/sh\r\nprintf '%s\\n' safe\r\n"
    };
    let snippet = library
        .save(
            None,
            Input {
                name: "Deploy".into(),
                language: "bash".into(),
                category: String::new(),
                content: before.into(),
            },
        )
        .unwrap();
    let subject = Subject::Snippet(snippet);
    let window = SnippetWindow::open(profile.path()).unwrap();
    let history = store(&window).unwrap();
    let pre = Record::new(
        Purpose::Analysis,
        None,
        Source::new(before, "bash", "en", "en", "Deploy").unwrap(),
        Provenance {
            profile_id: ProfileRef::Numeric(7),
            ..Provenance::default()
        },
        Analysis {
            improvements: vec![Finding {
                id: "SEC-1".into(),
                category: "security".into(),
                title: "Unsafe command".into(),
                ..Finding::default()
            }],
            dependencies: (1..=4)
                .map(|index| Dependency {
                    id: format!("DEP-{index}"),
                    name: format!("command-{index}"),
                    kind: "command".into(),
                    suggestion: "Replace echo with printf".into(),
                    ..Dependency::default()
                })
                .collect(),
            ..Analysis::default()
        },
    )
    .unwrap();
    let pre_id = pre.id;
    history.append(subject, pre).unwrap();

    let (listener, _) = listener();
    let endpoint = listener.local_addr().unwrap().to_string();
    let mut configuration = json!({
        "general": {"code_text_language": "de"},
        "profiles": {
            "profiles": [
                {"id": 7, "name": "Original", "endpoint": endpoint, "model": "original-model"},
                {"id": 8, "name": "Other", "endpoint": endpoint, "model": "other-model"}
            ],
            "default": 8,
            "last_id": 8
        }
    });
    let mut workspace = Workspace::default();
    workspace.set_test_ai_configuration(serde_json::from_value(configuration.clone()).unwrap());
    let (owner, _) = workspace.open_window(false);
    let (id, _) = workspace.install_snippet_surface(owner, window, true);
    let _ = workspace.update_snippets(id, SnippetMessage::Open(snippet));
    let load = workspace.open_snippet_analysis(id, subject);
    let _ = complete(&mut workspace, load);
    let update = |workspace: &mut Workspace, action| {
        let id = *workspace.snippets.keys().next().unwrap();
        workspace.update_snippet_analysis(id, super::super::Action::Apply(action))
    };
    let _ = update(
        &mut workspace,
        Action::Hardening(
            pre_id,
            crate::snippet_window::analysis::hardening::Edit::AllClassic(false),
        ),
    );
    for index in 1..=4 {
        let _ = update(
            &mut workspace,
            Action::Dependency(pre_id, format!("DEP-{index}"), true),
        );
    }
    let _ = update(&mut workspace, Action::AllFindings(pre_id, true));
    let _ = update(&mut workspace, Action::AllFindings(pre_id, false));
    let _ = update(&mut workspace, Action::Plan(pre_id, false));
    let page = workspace.snippets[&id].analysis.page(subject).unwrap();
    let State::ApplyPreview(preview) = &page.state else {
        panic!("Apply did not preview its request: {:?}", page.status);
    };
    assert_eq!(preview.approval.profile.id, 7);
    assert!(preview.prompt.contains("echo unsafe"));
    assert!(preview.prompt.contains("DEP-1"));
    assert!(!preview.prompt.contains("SEC-1"));
    assert!(!preview.prompt.contains("HARDENING-"));
    let approved_system = preview
        .prompt
        .strip_prefix("System prompt\n")
        .unwrap()
        .split_once("\n\nFirst stage user prompt\n")
        .unwrap()
        .0
        .to_owned();
    assert!(approved_system.contains("in report language code en."));
    assert!(approved_system.contains("log, and help strings in language code de."));
    assert!(approved_system.contains("Translate existing prose"));
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    let (in_stage, stage_started) = mpsc::channel();
    let (release_stage, released) = mpsc::channel();
    let interrupted_provider = thread::spawn(move || {
        let mut stream = accept(&listener);
        let first = request(&mut stream);
        let initial_response = fixture_response(
            "200 OK",
            "",
            r##"{"lines":["#!/bin/sh","printf '%s\\n' safe","# pending check"],"changes":[{"id":"DEP-1","reason":"Use printf for predictable output","anchor":"printf '%s\\n' safe"},{"id":"DEP-2","reason":"Prepare the check"},{"id":"DEP-3","reason":"Preserve the interpreter"}]}"##,
        );
        stream
            .get_mut()
            .write_all(initial_response.as_bytes())
            .unwrap();
        drop(stream);

        let mut stream = accept(&listener);
        let interrupted = request(&mut stream);
        assert_eq!(interrupted["stream"], true);
        let provisional = json!({"choices":[{"delta":{"content":r#"{"lines":["UNREVIEWED_CODE"],"changes":[{"id":"DEP-4","reason":"API_KEY=fixture-stream-secret"},"#},"finish_reason":null}]});
        write!(stream.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {provisional}\n\n").unwrap();
        in_stage.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(4)).unwrap();
        let mut byte = [0];
        assert!(match stream.get_mut().read(&mut byte) {
            Ok(0) => true,
            Err(error) => error.kind() == ErrorKind::ConnectionReset,
            _ => false,
        });
        (listener, first, interrupted)
    });
    let generate = update(&mut workspace, Action::Approve);
    stage_started.recv_timeout(Duration::from_secs(4)).unwrap();
    let mut events = iced_runtime::task::into_stream(generate).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let message = next_output(&mut events, deadline);
        let previewed = matches!(&message, Message::SnippetAnalysis(_, super::super::Event::Apply(Event::Progress(_, _, progress))) if !progress.reason.is_empty());
        let _ = workspace.update(message);
        if previewed {
            break;
        }
    }
    let page = workspace.snippets[&id].analysis.page(subject).unwrap();
    let State::Applying(run) = &page.state else {
        panic!("Expected live proposal generation");
    };
    let live = run.live.as_ref().unwrap();
    assert_eq!((live.stage, live.stages), (2, 2));
    assert_eq!(live.reason, "API_KEY=***");
    let disk_while_running = history.load(subject).unwrap();
    let saved = disk_while_running
        .current()
        .unwrap()
        .apply_recovery
        .as_ref()
        .unwrap();
    assert_eq!(saved.checkpoint.as_deref().unwrap().completed(), 1);
    let saved_revision = saved.revision;
    if review_partial {
        workspace.set_test_ai_configuration(
            serde_json::from_value(json!({"general": {"features_enabled": false}})).unwrap(),
        );
        workspace.invalidate_snippet_apply_requests();
    } else {
        let _ = workspace.update_snippet_analysis(id, super::super::Action::Abort);
    }
    let State::Applying(run) = &workspace.snippets[&id]
        .analysis
        .page(subject)
        .unwrap()
        .state
    else {
        panic!("The cancelled operation must keep its checkpoint claim until the worker stops");
    };
    assert!(run.live.is_none());
    release_stage.send(()).unwrap();
    let _ = complete_stream(&mut workspace, &mut events, deadline);
    let (listener, first, mut interrupted) = interrupted_provider.join().unwrap();
    let page = workspace.snippets[&id].analysis.page(subject).unwrap();
    let State::RecoverApply(recovery) = &page.state else {
        panic!(
            "Apply did not retain the interrupted request: {:?}",
            page.status
        );
    };
    assert_eq!(recovery.completed(), 1);
    assert_eq!(
        page.status.as_deref(),
        Some(if review_partial {
            "The AI settings changed. Review a new apply request."
        } else {
            "Proposal generation cancelled."
        })
    );
    assert!(
        history
            .load(subject)
            .unwrap()
            .current()
            .unwrap()
            .apply_runs
            .is_empty()
    );

    assert_eq!(first["model"], "original-model");
    assert_eq!(
        first["messages"][0]["content"].as_str(),
        Some(approved_system.as_str())
    );
    drop(workspace);
    let mut workspace = Workspace::default();
    if review_partial {
        let disabled =
            serde_json::from_value(json!({"general": {"features_enabled": false}})).unwrap();
        workspace.set_test_ai_configuration(disabled);
        assert!(workspace.ai_configuration().is_none());
    } else {
        configuration["profiles"]["profiles"][0]["model"] = json!("resumed-model");
        workspace.set_test_ai_configuration(serde_json::from_value(configuration).unwrap());
    }
    let (owner, _) = workspace.open_window(false);
    let window = SnippetWindow::open(profile.path()).unwrap();
    let (id, _) = workspace.install_snippet_surface(owner, window, true);
    let _ = workspace.update_snippets(id, SnippetMessage::Open(snippet));
    let load = workspace.open_snippet_analysis(id, subject);
    let _ = complete(&mut workspace, load);
    let page = workspace.snippets[&id].analysis.page(subject).unwrap();
    let State::RecoverApply(recovery) = &page.state else {
        panic!(
            "A fresh workspace did not restore the saved request: {:?}",
            page.status
        );
    };
    assert_eq!(recovery.revision, Some(saved_revision));
    assert_eq!(recovery.completed(), 1);
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    if review_partial {
        let persist = update(&mut workspace, Action::ReviewPartial);
        let _ = complete(&mut workspace, persist);
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    } else {
        let _ = update(&mut workspace, Action::Resume);
        let _ = update(&mut workspace, Action::Back);
        let _ = update(&mut workspace, Action::Resume);
        let page = workspace.snippets[&id].analysis.page(subject).unwrap();
        let State::ApplyPreview(preview) = &page.state else {
            panic!(
                "Apply did not preview the remaining request: {:?}",
                page.status
            );
        };
        assert_eq!(preview.approval.profile.model, "resumed-model");
        let remaining_input = preview
            .prompt
            .split_once("\n\nStage 2 user prompt\n")
            .unwrap()
            .1
            .split_once("\nIf a stage returns an invalid proposal")
            .unwrap()
            .0
            .to_owned();
        assert!(remaining_input.contains("# pending check"));
        assert!(remaining_input.contains("DEP-4"));
        assert!(!remaining_input.contains("DEP-1"));
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        let resumed_provider = thread::spawn(move || {
            let mut stream = accept(&listener);
            let resumed = request(&mut stream);
            let final_response = fixture_response(
                "200 OK",
                "",
                r##"{"lines":["#!/bin/sh","printf '%s\\n' safe"],"changes":[{"id":"DEP-4","reason":"Remove the interim check"}]}"##,
            );
            stream
                .get_mut()
                .write_all(final_response.as_bytes())
                .unwrap();
            resumed
        });
        let generate = update(&mut workspace, Action::Approve);
        let persist = complete(&mut workspace, generate);
        let _ = complete(&mut workspace, persist);
        let resumed = resumed_provider.join().unwrap();
        interrupted["model"] = json!("resumed-model");
        assert_eq!(resumed, interrupted);
        assert_eq!(
            resumed["messages"][1]["content"].as_str(),
            Some(remaining_input.as_str())
        );
    }
    let loaded = history.load(subject).unwrap();
    assert!(loaded.current().unwrap().apply_recovery.is_none());
    let proposed = &loaded.current().unwrap().apply_runs[0];
    assert_eq!(proposed.status, ApplyStatus::Proposed);
    assert_eq!(proposed.after, after);
    assert_eq!(proposed.reasons.len(), if review_partial { 3 } else { 4 });
    assert_eq!(proposed.selected.contains("DEP-4"), !review_partial);
    assert_eq!(proposed.partial.is_some(), review_partial);
    if let Some(partial) = proposed.partial {
        assert_eq!(partial.completed_stages, 1);
        assert_eq!(partial.total_stages, 2);
        assert_eq!(partial.total_items, 4);
    }
    assert_eq!(proposed.code_text_language_code(), Some("de"));
    assert_eq!(
        proposed.provenance.model,
        if review_partial {
            "original-model"
        } else {
            "resumed-model"
        },
    );
    assert_eq!(
        loaded.current().unwrap().source.code_text_language_code,
        "en"
    );
    assert_eq!(
        proposed.reasons[0].anchor.as_deref(),
        Some("printf '%s\\n' safe")
    );
    let _ = update(&mut workspace, Action::Back);
    let _ = workspace.update_snippet_analysis(id, super::super::Action::Close);
    let load = workspace.open_snippet_analysis(id, subject);
    let _ = complete(&mut workspace, load);
    let _ = update(&mut workspace, Action::Open(pre_id, proposed.id));
    let _ = update(&mut workspace, Action::ToggleExact);
    let _ = update(&mut workspace, Action::RevealReason(proposed.id, Some(0)));
    let State::ReviewApply(review) = &workspace.snippets[&id]
        .analysis
        .page(subject)
        .unwrap()
        .state
    else {
        panic!("Expected the reopened proposal review");
    };
    assert_eq!(
        review.locations[0].unwrap().label(),
        if review_partial {
            "Proposed lines 2–3"
        } else {
            "Proposed line 2"
        }
    );
    assert_eq!(review.focused_reason, Some(0));
    assert!(!review.exact);
    assert_eq!(
        workspace.snippets[&id]
            .window
            .analysis_snippet(snippet)
            .unwrap()
            .2,
        before
    );
    let decide = update(&mut workspace, Action::Accept);
    assert!(workspace.snippets[&id].window.analysis_apply_locked());
    let _ = workspace.update_snippets(id, SnippetMessage::Save);
    let _ = complete(&mut workspace, decide);
    assert_eq!(
        workspace.snippets[&id]
            .window
            .analysis_snippet(snippet)
            .unwrap()
            .2,
        after
    );
    assert_eq!(
        Library::open(profile.path())
            .unwrap()
            .get(snippet)
            .unwrap()
            .content(),
        before
    );
    let loaded = history.load(subject).unwrap();
    let accepted = &loaded.current().unwrap().apply_runs[0];
    assert_eq!(accepted.status, ApplyStatus::Accepted);
    assert_eq!(accepted.after_sha256, content_hash(after).unwrap());
    assert_eq!(accepted.partial, proposed.partial);
    if review_partial {
        assert_partial_after_report(&profile.0, &loaded, &accepted.after_sha256);
    }
    let _ = workspace.update_snippets(id, SnippetMessage::Save);
    assert_eq!(
        Library::open(profile.path())
            .unwrap()
            .get(snippet)
            .unwrap()
            .content(),
        after
    );
}

fn assert_partial_after_report(path: &std::path::Path, history: &History, after_hash: &str) {
    use crate::snippet_analysis::report::{self, Format, Item, Options, Packing};

    let target = path.canonicalize().unwrap().join("partial-after.json");
    let item = Item::batch(history, String::new(), after_hash.into()).unwrap();
    report::export(
        target.clone(),
        vec![item],
        Options {
            format: Format::Json,
            packing: Packing::Single,
            include_code: false,
            branding: Default::default(),
            exported_at: now_millis(),
            locale: "en".into(),
            skipped: Vec::new(),
        },
        &AtomicBool::new(false),
    )
    .unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(target).unwrap()).unwrap();
    assert_eq!(report["kind"], "POST_APPLY");
    assert_eq!(report["apply"]["partial"], true);
    assert_eq!(report["apply"]["completedStages"], 1);
    assert_eq!(report["apply"]["totalStages"], 2);
    assert_eq!(report["apply"]["completedItems"], 3);
    assert_eq!(report["apply"]["totalItems"], 4);
    assert_eq!(report["dependencies"][3]["selected"], false);
}

#[test]
fn legacy_reason_jump_uses_only_a_changed_line_from_the_matching_source() {
    let before = "start\nobsolete\nend\n";
    let mut record = Record::new(
        Purpose::Analysis,
        None,
        Source::new(before, "bash", "en", "en", "Script").unwrap(),
        Provenance::default(),
        Analysis {
            improvements: vec![Finding {
                id: "SEC-1".into(),
                title: "Old call".into(),
                line: Some(2),
                ..Finding::default()
            }],
            ..Analysis::default()
        },
    )
    .unwrap();
    let run = ApplyRun::new(
        before.into(),
        "start\nend\n".into(),
        Selection {
            findings: vec!["SEC-1".into()],
            ..Selection::default()
        },
        vec![ChangeReason {
            id: "SEC-1".into(),
            reason: "Removed the old call".into(),
            anchor: None,
        }],
        now_millis(),
    )
    .unwrap();
    assert_eq!(
        Review::new(&record, &run).locations[0].unwrap().label(),
        "Original line 2"
    );
    record.source = Source::new("start\nother\nend\n", "bash", "en", "en", "Script").unwrap();
    assert!(Review::new(&record, &run).locations[0].is_none());
}
