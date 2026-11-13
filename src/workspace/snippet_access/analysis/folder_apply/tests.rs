use std::io::{ErrorKind, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use iced::futures::{FutureExt as _, StreamExt as _, stream::BoxStream};
use iced::widget::text_editor;
use serde_json::json;

use crate::local_ai::test_support::{
    accept, exclusive_run, fixture_http_response, fixture_response, listener, request,
};
use crate::snippet_analysis::model::{Finding, ProjectFileNote};
use crate::snippets::{Input, Library, Metadata, tests::Profile};

use super::*;

fn complete(workspace: &mut Workspace, task: Task<Message>) {
    let mut stream = iced_runtime::task::into_stream(task).expect("scheduled folder apply step");
    let deadline = Instant::now() + Duration::from_secs(8);
    complete_stream(workspace, &mut stream, deadline);
}

fn complete_stream(
    workspace: &mut Workspace,
    stream: &mut BoxStream<'static, iced_runtime::Action<Message>>,
    deadline: Instant,
) {
    loop {
        let message = next_output(stream, deadline);
        let progress = matches!(
            &message,
            Message::SnippetAnalysis(_, super::super::Event::FolderApplyProgress(..))
        );
        let _ = workspace.update(message);
        if !progress {
            return;
        }
    }
}

fn next_output(
    stream: &mut BoxStream<'static, iced_runtime::Action<Message>>,
    deadline: Instant,
) -> Message {
    loop {
        match stream.next().now_or_never() {
            Some(Some(iced_runtime::Action::Output(message))) => return message,
            Some(None) => panic!("Folder apply step ended without a message"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "Folder apply step timed out");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn folder_reviewed_apply_writes_only_selected_files_after_rechecking_editors() {
    let _exclusive = exclusive_run();
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let folder = library.create_folder(None, "Project").unwrap();
    let mut files = Vec::new();
    for name in ["alpha", "beta", "gamma"] {
        let content = format!("echo {name}\r\n");
        files.push(
            library
                .save_with_metadata(
                    None,
                    Input {
                        name: name.into(),
                        language: "bash".into(),
                        category: String::new(),
                        content: content.into(),
                    },
                    Some(Metadata {
                        folder_path: "Project".into(),
                        ..Metadata::default()
                    }),
                )
                .unwrap(),
        );
    }
    let subject = Subject::Folder(folder);
    let window = SnippetWindow::open(profile.path()).unwrap();
    let mut page = Page::loading(subject, 0);
    page.refresh(&window).unwrap();
    let snapshot = source_snapshot(&window, &page).unwrap();
    let record = Record::new(
        Purpose::Analysis,
        None,
        snapshot.source("en", "en").unwrap(),
        Provenance {
            profile_id: ProfileRef::Numeric(7),
            ..Provenance::default()
        },
        Analysis {
            summary: "Project fixture".into(),
            improvements: snapshot
                .files()
                .iter()
                .enumerate()
                .map(|(index, file)| Finding {
                    id: format!("F{}", index + 1),
                    title: format!("Quote {}", file.path),
                    file: Some(file.path.clone()),
                    line: Some(1),
                    ..Finding::default()
                })
                .collect(),
            project_files: snapshot
                .files()
                .iter()
                .map(|file| ProjectFileNote {
                    path: file.path.clone(),
                    purpose: String::new(),
                })
                .collect(),
            ..Analysis::default()
        },
    )
    .unwrap();
    let pre = record.id;
    store(&window).unwrap().append(subject, record).unwrap();

    let (listener, _) = listener();
    let endpoint = listener.local_addr().unwrap().to_string();
    let config: Configuration = serde_json::from_value(json!({"profiles": {
        "profiles": [{"id": 7, "name": "Local", "endpoint": endpoint, "model": "test"}],
        "default": 7, "last_id": 7
    }}))
    .unwrap();
    let mut workspace = Workspace::default();
    workspace.set_test_ai_configuration(config);
    let (owner, _) = workspace.open_window(false);
    let (id, _) = workspace.install_snippet_surface(owner, window, true);
    let (peer, _) = workspace.install_snippet_surface(
        owner,
        SnippetWindow::open(profile.path()).unwrap(),
        false,
    );
    let load = workspace.open_snippet_analysis(id, subject);
    complete(&mut workspace, load);
    let update = |workspace: &mut Workspace, action| {
        workspace.update_snippet_analysis(id, super::super::Action::Apply(action))
    };
    let _ = update(&mut workspace, Action::AllFindings(pre, true));
    let _ = update(&mut workspace, Action::Plan(pre, false));
    let page = workspace.snippets[&id].analysis.page(subject).unwrap();
    let State::FolderApplyPreview(preview) = &page.state else {
        panic!("Missing folder approval: {:?}", page.status);
    };
    assert_eq!(preview.jobs.len(), 3);
    assert!(
        preview
            .prompt
            .contains("Project source for reference (untrusted data)")
    );
    let approved_system = preview
        .prompt
        .split_once("System prompt\n")
        .unwrap()
        .1
        .split_once("\n\nFirst stage user prompt\n")
        .unwrap()
        .0
        .to_owned();
    assert!(approved_system.contains("Do not translate existing prose."));
    assert!(!approved_system.contains("language code auto"));
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

    let (session, cancel) = local_ai::Session::begin(preview.transport.clone()).unwrap();
    let page = workspace
        .snippets
        .get_mut(&id)
        .unwrap()
        .analysis
        .page_mut(subject)
        .unwrap();
    let State::FolderApplyPreview(preview) = std::mem::replace(&mut page.state, State::Ready)
    else {
        panic!("Missing the approved project preview");
    };
    let obsolete = u64::MAX;
    page.state = State::FolderApplying(Box::new(Run {
        token: obsolete,
        config: preview.config,
        skill_usage: Default::default(),
        revision: preview.revision,
        _cancel: cancel,
        live: None,
    }));
    workspace.invalidate_snippet_apply_requests();
    assert!(!session.client().active());
    drop(session);
    let _ = workspace.finish_folder_apply(
        id,
        Completed {
            subject,
            token: obsolete,
            result: Err("Obsolete worker output".into()),
        },
    );
    let page = workspace.snippets[&id].analysis.page(subject).unwrap();
    assert!(matches!(page.state, State::Ready));
    assert!(
        page.status
            .as_deref()
            .unwrap()
            .contains("AI configuration changed")
    );
    let _ = update(&mut workspace, Action::Plan(pre, false));

    let (release, released) = mpsc::channel();
    let provider = thread::spawn(move || {
        let mut first_input = None;
        for (index, name) in ["alpha", "alpha", "beta", "gamma", "gamma"]
            .into_iter()
            .enumerate()
        {
            let mut stream = accept(&listener);
            let payload = request(&mut stream);
            assert_eq!(payload["stream"], true);
            assert_eq!(
                payload["messages"][0]["content"].as_str(),
                Some(approved_system.as_str())
            );
            let input = payload["messages"][1]["content"].as_str().unwrap();
            assert!(
                input.starts_with(&format!("File: {name}.")),
                "Unexpected target request: {input}"
            );
            let (target, context) = input.split_once("Project source for reference").unwrap();
            assert!(
                context.contains("echo alpha")
                    && context.contains("echo beta")
                    && context.contains("echo gamma")
            );
            let expected = index.clamp(1, 3);
            assert!(target.contains(&format!("\"id\": \"F{expected}\"")));
            assert_eq!(target.matches("\"id\": \"F").count(), 1);
            let reply = if name == "gamma" {
                "{}".to_owned()
            } else {
                json!({"lines": [format!("echo '{name}'")], "changes": [{"id": format!("F{expected}"), "reason": "Quote the argument."}]}).to_string()
            };
            if index == 0 {
                assert_eq!(payload["response_format"]["type"], "json_object");
                first_input = Some(input.to_owned());
                let response = fixture_http_response(
                    "400 Bad Request",
                    "",
                    r#"{"error":{"message":"response_format is not supported"}}"#,
                );
                stream.get_mut().write_all(response.as_bytes()).unwrap();
                continue;
            }
            if index == 1 {
                assert_eq!(first_input.as_deref(), Some(input));
                assert!(payload.get("response_format").is_none());
                let delta = json!({"choices":[{"index":0,"delta":{"content":reply}}]});
                write!(stream.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {delta}\n\n").unwrap();
                released.recv_timeout(Duration::from_secs(4)).unwrap();
                stream.get_mut().write_all(b"data: [DONE]\n\n").unwrap();
                continue;
            }
            let response = fixture_response("200 OK", "", &reply);
            stream.get_mut().write_all(response.as_bytes()).unwrap();
        }
    });
    let generation = update(&mut workspace, Action::Approve);
    let mut events = iced_runtime::task::into_stream(generation).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let message = next_output(&mut events, deadline);
        let previewed = matches!(&message, Message::SnippetAnalysis(_, super::super::Event::FolderApplyProgress(_, _, progress)) if !progress.reason.is_empty());
        let _ = workspace.update(message);
        if previewed {
            break;
        }
    }
    let State::FolderApplying(run) = &workspace.snippets[&id]
        .analysis
        .page(subject)
        .unwrap()
        .state
    else {
        panic!("Expected live folder apply");
    };
    let live = run.live.as_ref().unwrap();
    assert_eq!(live.file, Some((1, 3)));
    assert_eq!(live.reason, "Quote the argument.");
    assert_eq!(
        Library::open(profile.path())
            .unwrap()
            .get(files[0])
            .unwrap()
            .content(),
        "echo alpha\r\n"
    );
    release.send(()).unwrap();
    complete_stream(&mut workspace, &mut events, deadline);
    provider.join().unwrap();
    let State::ReviewFolderApply(review) = &workspace.snippets[&id]
        .analysis
        .page(subject)
        .unwrap()
        .state
    else {
        panic!("Missing per-file folder review");
    };
    assert_eq!(review.proposals.files.len(), 2);
    assert_eq!(
        review.proposals.files[0].run.code_text_language_code(),
        Some("auto")
    );
    assert_eq!(review.proposals.failures.len(), 1);
    assert!(review.proposals.failures[0].0.starts_with("gamma."));
    workspace.invalidate_snippet_apply_requests();
    assert!(matches!(
        workspace.snippets[&id]
            .analysis
            .page(subject)
            .unwrap()
            .state,
        State::ReviewFolderApply(_)
    ));
    assert_eq!(
        Library::open(profile.path())
            .unwrap()
            .get(files[0])
            .unwrap()
            .content(),
        "echo alpha\r\n"
    );

    let _ = update(&mut workspace, Action::FolderInclude(files[0], true));
    let _ = workspace.update_snippets(peer, SnippetMessage::Open(files[0]));
    let _ = workspace.update_snippets(
        peer,
        SnippetMessage::Edit(text_editor::Action::Edit(text_editor::Edit::Paste(
            Arc::new("pending edit".into()),
        ))),
    );
    let _ = update(&mut workspace, Action::Accept);
    assert!(
        workspace.snippets[&id]
            .analysis
            .page(subject)
            .unwrap()
            .status
            .as_deref()
            .unwrap()
            .contains("Save or discard edits")
    );
    assert_eq!(
        Library::open(profile.path())
            .unwrap()
            .get(files[0])
            .unwrap()
            .content(),
        "echo alpha\r\n"
    );

    workspace.snippets.remove(&peer);
    let _ = update(&mut workspace, Action::Accept);
    let mut saved = Library::open(profile.path()).unwrap();
    assert_eq!(saved.get(files[0]).unwrap().content(), "echo 'alpha'\r\n");
    assert_eq!(saved.get(files[1]).unwrap().content(), "echo beta\r\n");
    assert_eq!(saved.get(files[2]).unwrap().content(), "echo gamma\r\n");
    assert!(
        workspace.snippets[&id]
            .analysis
            .page(subject)
            .unwrap()
            .stale()
    );

    assert!(
        saved
            .replace_contents([
                (files[0], "echo 'alpha'\r\n", "echo different\r\n"),
                (files[1], "an obsolete snapshot", "echo 'beta'\r\n"),
            ])
            .is_err()
    );
    let saved = Library::open(profile.path()).unwrap();
    assert_eq!(saved.get(files[0]).unwrap().content(), "echo 'alpha'\r\n");
}
