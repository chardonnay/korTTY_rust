use std::io::{ErrorKind, Write as _};
use std::thread;
use std::time::Duration;

use iced::futures::{FutureExt as _, StreamExt as _};
use serde_json::json;

use crate::local_ai::test_support::{accept, exclusive_run, fixture_response, listener, request};
use crate::snippets::{Input, tests::Profile};

use super::*;

#[test]
fn reviewed_split_generates_modules_first_and_publishes_the_selected_files() {
    let _exclusive = exclusive_run();
    let profile = Profile::new();
    let original = "print(42)\n";
    let mut library = Library::open(profile.path()).unwrap();
    let snippet = library
        .save(
            None,
            Input {
                name: "Source".into(),
                language: "python".into(),
                category: String::new(),
                content: original.into(),
            },
        )
        .unwrap();
    let subject = Subject::Snippet(snippet);
    let window = SnippetWindow::open(profile.path()).unwrap();
    let store = store(&window).unwrap();
    store
        .append(
            subject,
            Record::new(
                Purpose::Analysis,
                None,
                Source::new(original, "python", "en", "en", "Source").unwrap(),
                Provenance {
                    profile_id: ProfileRef::Numeric(7),
                    ..Provenance::default()
                },
                Analysis::default(),
            )
            .unwrap(),
        )
        .unwrap();

    let (listener, _) = listener();
    let endpoint = listener.local_addr().unwrap().to_string();
    let mut workspace = Workspace::default();
    workspace.set_test_ai_configuration(serde_json::from_value(json!({
        "profiles": {
            "profiles": [{"id": 7, "name": "Local test", "endpoint": endpoint, "model": "reviewed-model"}],
            "default": 7, "last_id": 7
        }
    })).unwrap());
    let (owner, _) = workspace.open_window(false);
    let (id, _) = workspace.install_snippet_surface(owner, window, true);
    let _ = workspace.update_snippets(id, SnippetMessage::Open(snippet));
    let load = workspace.open_snippet_analysis(id, subject);
    let _ = complete(&mut workspace, load);
    let update = |workspace: &mut Workspace, action| {
        workspace.update_snippet_analysis(id, super::super::Action::Modularization(action))
    };
    let _ = update(&mut workspace, Action::Plan);
    let page = workspace.snippets[&id].analysis.page(subject).unwrap();
    assert!(
        matches!(&page.state, State::Modularization(pane) if matches!(pane.as_ref(), Pane::Preview(_)))
    );
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

    let provider = thread::spawn(move || {
        let answers = [
            json!({"recommended": true, "rationale": "Extract the value.", "files": [
                {"path":"entry.py", "purpose":"Entry point", "symbols":[], "executable":true, "entryPoint":true},
                {"path":"helper.py", "purpose":"Helper", "symbols":["VALUE"], "executable":false, "entryPoint":false}
            ]}),
            json!({"lines":["VALUE = 42"]}),
            json!({"lines":["from helper import VALUE", "print(VALUE)"]}),
        ];
        answers
            .into_iter()
            .map(|answer| {
                let mut stream = accept(&listener);
                let received = request(&mut stream);
                let response = fixture_response("200 OK", "", &answer.to_string());
                stream.get_mut().write_all(response.as_bytes()).unwrap();
                received
            })
            .collect::<Vec<_>>()
    });

    let plan = update(&mut workspace, Action::Confirm);
    let load = complete(&mut workspace, plan);
    let _ = complete(&mut workspace, load);
    assert!(
        store
            .load(subject)
            .unwrap()
            .current()
            .unwrap()
            .modularization
            .is_some()
    );
    let _ = update(&mut workspace, Action::Generate);
    let generate = update(&mut workspace, Action::Confirm);
    let load = complete(&mut workspace, generate);
    let _ = complete(&mut workspace, load);
    let requests = provider.join().unwrap();
    let prompt = |index: usize| requests[index]["messages"][1]["content"].as_str().unwrap();
    assert!(prompt(0).contains("print(42)"));
    let (_, first) = prompt(1)
        .split_once("Generate this planned file\n")
        .unwrap();
    assert!(first.contains("\"path\": \"helper.py\""));
    let (_, second) = prompt(2)
        .split_once("Generate this planned file\n")
        .unwrap();
    assert!(second.contains("\"path\": \"entry.py\""));
    assert!(second.contains("VALUE = 42"));

    let _ = update(&mut workspace, Action::Review);
    let _ = update(&mut workspace, Action::File(true));
    let _ = update(&mut workspace, Action::Include(1, false));
    let page = workspace.snippets[&id].analysis.page(subject).unwrap();
    assert!(
        matches!(&page.state, State::Modularization(pane) if matches!(pane.as_ref(), Pane::Review(review) if review.diagnostics.iter().any(|warning| warning.contains("unselected new file helper.py"))))
    );
    let _ = update(&mut workspace, Action::Include(1, true));
    let publish = update(&mut workspace, Action::Publish);
    let _ = complete(&mut workspace, publish);
    let library = Library::open(profile.path()).unwrap();
    assert_eq!(library.entries().len(), 2);
    assert_eq!(
        library.get(snippet).unwrap().content(),
        "from helper import VALUE\nprint(VALUE)\n"
    );
    assert!(
        store
            .load(subject)
            .unwrap()
            .current()
            .unwrap()
            .modularization
            .as_ref()
            .unwrap()
            .published
    );
}

#[test]
fn reviewed_cli_split_reopens_at_the_next_unapproved_module() {
    use crate::local_ai::cli::{Program, fixture_executable};
    use std::process::Command;

    const FIXTURE: &str = "KORTTY_TEST_MODULARIZATION_CLI_HOME";
    const TEST: &str = "workspace::snippet_access::analysis::modularization::tests::reviewed_cli_split_reopens_at_the_next_unapproved_module";
    const PRIVATE_VALUE: &str = "fixture-private-source-value";
    const SOURCE: &str = "# API_KEY='fixture-private-source-value'\nprint(1)\n";
    let Some(home) = std::env::var_os(FIXTURE).map(std::path::PathBuf::from) else {
        let scratch = Profile::new();
        let home = scratch.0.canonicalize().unwrap();
        let rustc = std::env::var_os("RUSTC").or_else(|| {
            let tool = Command::new("rustup")
                .args(["which", "rustc"])
                .output()
                .ok()?;
            tool.status
                .success()
                .then(|| std::ffi::OsString::from(String::from_utf8_lossy(&tool.stdout).trim()))
        });
        let mut child = Command::new(std::env::current_exe().unwrap());
        if let Some(rustc) = rustc {
            child.env("RUSTC", rustc);
        }
        let child = child
            .args(["--exact", TEST, "--nocapture"])
            .env(FIXTURE, &home)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("LOCALAPPDATA", &home)
            .env("XDG_CONFIG_HOME", &home)
            .env("TEMP", &home)
            .env("TMP", &home)
            .env("TMPDIR", &home)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
        return;
    };
    let _exclusive = exclusive_run();
    let root = home.join("private-profile");
    std::fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    crate::profile::set_runtime_root(root.clone());
    let (executable, fingerprint) = Program::inspect(&fixture_executable(&root)).unwrap();
    let config: Configuration = serde_json::from_value(json!({
        "profiles": {"profiles": [
            {"id": 1, "name": "Local analysis", "endpoint": "127.0.0.1:12345", "model": "fixture"},
            {"id": 7, "name": "Reviewed split CLI", "connection_mode": "cli", "endpoint": "",
                "vision": "disabled", "cli": {"path": executable, "fingerprint": fingerprint,
                    "arguments": ["fixture_modularization", "{promptFile}"]}}
        ], "default": 1, "last_id": 7}
    }))
    .unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();

    let library_path = root.join("snippets.json");
    let mut library = Library::open(library_path.clone()).unwrap();
    let snippet = library
        .save(
            None,
            Input {
                name: "Source".into(),
                language: "python".into(),
                category: String::new(),
                content: SOURCE.into(),
            },
        )
        .unwrap();
    let subject = Subject::Snippet(snippet);
    let window = SnippetWindow::open(library_path.clone()).unwrap();
    let store = store(&window).unwrap();
    store
        .append(
            subject,
            Record::new(
                Purpose::Analysis,
                None,
                Source::new(SOURCE, "python", "en", "en", "Source").unwrap(),
                Provenance {
                    profile_id: ProfileRef::Numeric(1),
                    ..Provenance::default()
                },
                Analysis::default(),
            )
            .unwrap(),
        )
        .unwrap();

    let mut workspace = Workspace::default();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let (owner, _) = workspace.open_window(false);
    let (id, _) = workspace.install_snippet_surface(owner, window, true);
    let _ = workspace.update_snippets(id, SnippetMessage::Open(snippet));
    let load = workspace.open_snippet_analysis(id, subject);
    let _ = complete(&mut workspace, load);
    let update = |workspace: &mut Workspace, action| {
        workspace.update_snippet_analysis(id, super::super::Action::Modularization(action))
    };
    let receipt = |stage: &str| root.join(format!("fixture-modularization-{stage}"));
    let job = || {
        store
            .load(subject)
            .unwrap()
            .current()
            .unwrap()
            .modularization
            .as_deref()
            .unwrap()
            .clone()
    };
    let approve = |workspace: &mut Workspace, module, stage, stale| {
        let page = workspace.snippets[&id].analysis.page(subject).unwrap();
        let State::Modularization(pane) = &page.state else {
            panic!(
                "expected outbound modularization preview: {:?}",
                page.status
            );
        };
        let Phase::Modularization(View::OutboundPreview(preview)) = pane.view() else {
            panic!("expected outbound modularization preview");
        };
        assert_eq!(preview.profile.id, 7);
        assert_eq!(preview.module, module);
        assert!(preview.masked_source);
        assert_eq!(preview.pages, 1);
        assert!(matches!(preview.destination, OutboundDestination::Cli));
        let (command, prompt) = preview
            .prompt
            .split_once("\n\nExact UTF-8 prompt.txt\n")
            .unwrap();
        let (_, arguments) = command.rsplit_once("\nArguments: ").unwrap();
        let arguments: Vec<String> = serde_json::from_str(arguments).unwrap();
        let [mode, path] = arguments.as_slice() else {
            panic!("expected the mode and the expanded prompt path");
        };
        assert_eq!(mode, "fixture_modularization");
        assert!(!std::path::Path::new(path).exists());
        assert!(prompt.contains("API_KEY=***"));
        assert!(!prompt.contains(PRIVATE_VALUE));
        let prompt = prompt.to_owned();
        let approval = preview.approval;
        assert!(!receipt(stage).exists());
        assert_eq!(update(workspace, Action::Confirm).units(), 0);
        if let Some(stale) = stale {
            assert_ne!(approval, stale);
            assert_eq!(update(workspace, Action::ConfirmOutbound(stale)).units(), 0);
        }
        assert!(!receipt(stage).exists());

        let task = update(workspace, Action::ConfirmOutbound(approval));
        let load = complete(workspace, task);
        let _ = complete(workspace, load);
        assert_eq!(std::fs::read_to_string(receipt(stage)).unwrap(), prompt);
        assert_eq!(
            std::fs::read_to_string(receipt(&format!("{stage}-path"))).unwrap(),
            *path
        );
        assert!(!std::path::Path::new(path).exists());
        assert_eq!(
            update(workspace, Action::ConfirmOutbound(approval)).units(),
            0
        );
        approval
    };

    let _ = update(&mut workspace, Action::Profile(ProfileRef::Numeric(7)));
    let _ = update(&mut workspace, Action::Plan);
    let planning = approve(&mut workspace, None, "plan", None);
    let planned = job();
    assert_eq!(planned.outbound.unwrap().profile_id, ProfileRef::Numeric(7));
    assert!(planned.generation.is_none());
    assert!(!receipt("helper").exists());
    assert!(!receipt("entry").exists());

    let _ = update(&mut workspace, Action::Generate);
    let helper = approve(&mut workspace, Some("helper.py"), "helper", Some(planning));
    let checkpoint = job();
    let progress = &checkpoint.generation.as_ref().unwrap().progress;
    assert_eq!(progress.outputs.len(), 1);
    assert_eq!(progress.outputs[0].path, "helper.py");
    assert_eq!(progress.outputs[0].content, "VALUE = 1\n");
    assert!(!progress.ready());
    let helper_path = std::fs::read_to_string(receipt("helper-path")).unwrap();

    let _ = workspace.update_snippet_analysis(id, super::super::Action::Close);
    assert!(workspace.snippets[&id].analysis.page(subject).is_none());
    let load = workspace.open_snippet_analysis(id, subject);
    let _ = complete(&mut workspace, load);
    assert!(!receipt("entry").exists());
    let _ = update(&mut workspace, Action::Generate);
    approve(&mut workspace, Some("entry.py"), "entry", Some(helper));
    assert!(
        std::fs::read_to_string(receipt("entry"))
            .unwrap()
            .contains("VALUE = 1")
    );
    assert_eq!(
        std::fs::read_to_string(receipt("helper-path")).unwrap(),
        helper_path
    );
    let completed = job();
    assert_eq!(completed.id, checkpoint.id);
    assert!(completed.generation.unwrap().progress.ready());
    assert_eq!(
        Library::open(library_path.clone())
            .unwrap()
            .get(snippet)
            .unwrap()
            .content(),
        SOURCE
    );

    let _ = update(&mut workspace, Action::Review);
    let publish = update(&mut workspace, Action::Publish);
    let _ = complete(&mut workspace, publish);
    let library = Library::open(library_path).unwrap();
    assert_eq!(library.entries().len(), 2);
    assert_eq!(
        library.get(snippet).unwrap().content(),
        "from helper import VALUE\nprint(VALUE)\n"
    );
    assert!(job().published);
}

#[test]
fn reviewed_auto_split_waits_for_saved_pre_and_runs_before_the_flowchart() {
    automatic_split_fixture(false);
}

#[test]
fn saved_https_analysis_opens_a_separate_split_review_and_revokes_on_vault_change() {
    use std::process::Command;

    const HOME: &str = "KORTTY_TEST_HTTPS_SPLIT_FOLLOWUP_HOME";
    const TEST: &str = "workspace::snippet_access::analysis::modularization::tests::saved_https_analysis_opens_a_separate_split_review_and_revokes_on_vault_change";
    let Some(root) = std::env::var_os(HOME).map(std::path::PathBuf::from) else {
        let scratch = Profile::new();
        let root = scratch.0.canonicalize().unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(HOME, &root)
            .env("HOME", &root)
            .env("LOCALAPPDATA", &root)
            .env("XDG_CONFIG_HOME", &root)
            .output()
            .unwrap();
        assert!(
            child.status.success() && String::from_utf8_lossy(&child.stdout).contains("1 passed"),
            "{}\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        return;
    };
    crate::profile::set_runtime_root(root.clone());
    let (listener, _) = listener();
    let endpoint = format!("https://{}/v1", listener.local_addr().unwrap());
    let config: Configuration = serde_json::from_value(json!({
        "profiles": {
            "profiles": [{"id": 1, "name": "HTTPS", "model": "fixture",
                "connection_mode": "remote-open-ai", "endpoint": endpoint,
                "trusted_remote_endpoint": true}],
            "default": 1, "last_id": 1
        }
    }))
    .unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let mut library = Library::open(root.join("snippets.json")).unwrap();
    let input = "print('already saved')\n";
    let snippet = library
        .save(
            None,
            Input {
                name: "Source".into(),
                language: "python".into(),
                category: String::new(),
                content: input.into(),
            },
        )
        .unwrap();
    let subject = Subject::Snippet(snippet);
    let window = SnippetWindow::open(library.path().to_owned()).unwrap();
    let history = store(&window).unwrap();
    let record = Record::new(
        Purpose::Analysis,
        None,
        Source::new(input, "python", "en", "en", "Source").unwrap(),
        Provenance {
            profile_id: ProfileRef::Numeric(1),
            ..Provenance::default()
        },
        Analysis::default(),
    )
    .unwrap();
    history.append(subject, record.clone()).unwrap();
    let mut workspace = Workspace::default();
    workspace
        .window_settings
        .snippet_analysis_auto_modularization = true;
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let (owner, _) = workspace.open_window(false);
    let (id, _) = workspace.install_snippet_surface(owner, window, true);
    let _ = workspace.update_snippets(id, SnippetMessage::Open(snippet));
    let load = workspace.open_snippet_analysis(id, subject);
    let _ = complete(&mut workspace, load);
    let config = workspace.ai_configuration().unwrap();
    let vault_revision = workspace.vault_change_revision();
    let surface = workspace.snippets.get_mut(&id).unwrap();
    let page = surface.analysis.page_mut(subject).unwrap();
    let capture = Capture::for_record(&surface.window, page, &record).unwrap();
    page.outbound_followup = Some(automatic::OutboundFollowup {
        id: record.id,
        profile: page.profiles[0].clone(),
        transport: config.snippet_analysis_transport(1).unwrap().0,
        source_hash: capture.source_sha256,
        rendered_hash: capture.rendered_sha256,
        config,
        guidance: SkillGuidance::default(),
        instructions: "Keep reviewed behavior.".into(),
        vault_revision,
    });
    let _ = workspace
        .start_outbound_split_followup(id, subject)
        .unwrap();
    let page = workspace.snippets[&id].analysis.page(subject).unwrap();
    let State::Modularization(pane) = &page.state else {
        panic!("expected outbound preview: {:?}", page.status);
    };
    let Phase::Modularization(View::OutboundPreview(preview)) = pane.view() else {
        panic!("expected the separately approved HTTPS preview");
    };
    assert!(matches!(preview.destination, OutboundDestination::Https));
    assert!(preview.prompt.contains("Keep reviewed behavior."));
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    let approval = preview.approval;
    workspace.vault_change_revision = Arc::new(());
    let task = workspace.update_snippet_modularization(id, Action::ConfirmOutbound(approval));
    assert_eq!(task.units(), 0);
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    assert!(
        history
            .load(subject)
            .unwrap()
            .current()
            .unwrap()
            .modularization
            .is_none()
    );
}

#[test]
fn disabling_auto_split_before_pre_completes_keeps_the_report_without_sending_the_plan() {
    automatic_split_fixture(true);
}

fn automatic_split_fixture(disable: bool) {
    let _exclusive = exclusive_run();
    let profile = Profile::new();
    let original = "print(42)\n";
    let mut library = Library::open(profile.path()).unwrap();
    let snippet = library
        .save(
            None,
            Input {
                name: "Reviewed automatic split".into(),
                language: "python".into(),
                category: String::new(),
                content: original.into(),
            },
        )
        .unwrap();
    let subject = Subject::Snippet(snippet);
    let window = SnippetWindow::open(profile.path()).unwrap();
    let history = store(&window).unwrap();
    let (listener, _) = listener();
    let endpoint = listener.local_addr().unwrap().to_string();
    let mut workspace = Workspace::default();
    assert!(
        !workspace
            .window_settings
            .snippet_analysis_auto_modularization
    );
    workspace
        .window_settings
        .snippet_analysis_auto_modularization = true;
    workspace.window_settings.snippet_analysis_auto_diagram = !disable;
    workspace.set_test_ai_configuration(serde_json::from_value(json!({
        "profiles": {
            "profiles": [{"id": 7, "name": "Local test", "endpoint": endpoint, "model": "reviewed-model"}],
            "default": 7, "last_id": 7
        }
    })).unwrap());
    let (owner, _) = workspace.open_window(false);
    let (id, _) = workspace.install_snippet_surface(owner, window, true);
    let _ = workspace.update_snippets(id, SnippetMessage::Open(snippet));
    let load = workspace.open_snippet_analysis(id, subject);
    let _ = complete(&mut workspace, load);
    let _ = workspace.update_snippet_analysis(id, super::super::Action::RequestStart);
    let page = workspace.snippets[&id].analysis.page(subject).unwrap();
    let State::Review(review) = &page.state else {
        panic!(
            "The analysis must show the combined preview: {:?}",
            page.status
        );
    };
    let plan = &review
        .plan
        .as_ref()
        .expect("reviewed automatic plan")
        .request;
    assert!(
        review
            .parts
            .iter()
            .any(|part| matches!(part.prompt, super::super::Prompt::PlanUser))
    );
    assert!(
        review
            .parts
            .iter()
            .any(|part| matches!(part.prompt, super::super::Prompt::PlanSystem))
    );
    let reviewed = [plan.system().to_owned(), plan.input().to_owned()];
    assert!(reviewed[1].contains("print(42)"));
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

    let observed = history.clone();
    let provider = thread::spawn(move || {
        let answers = [
            json!({"summary":"The analysis is ready."}),
            json!({"recommended": true, "rationale": "Extract the value.", "files": [
                {"path":"entry.py", "purpose":"Entry point", "symbols":[], "executable":true, "entryPoint":true},
                {"path":"helper.py", "purpose":"Helper", "symbols":["VALUE"], "executable":false, "entryPoint":false}
            ]}),
            json!({
                "mermaid": "flowchart TD\ns([Start])\na[Print 42]\ne([End])\ns --> a\na --> e",
                "codeReferences": []
            }),
        ];
        answers
            .into_iter()
            .take(if disable { 1 } else { 3 })
            .enumerate()
            .map(|(step, answer)| {
                let mut stream = accept(&listener);
                let received = request(&mut stream);
                if step > 0 {
                    let history = observed.load(subject).unwrap();
                    let pre = history
                        .current()
                        .expect("PRE is saved before any follow-up");
                    assert_eq!(pre.purpose, Purpose::Analysis);
                    assert_eq!(pre.modularization.is_some(), step == 2);
                }
                let response = fixture_response("200 OK", "", &answer.to_string());
                stream.get_mut().write_all(response.as_bytes()).unwrap();
                received
            })
            .collect::<Vec<_>>()
    });
    let analysis = workspace.update_snippet_analysis(id, super::super::Action::ConfirmStart);
    let save = complete(&mut workspace, analysis);
    if disable {
        workspace
            .window_settings
            .snippet_analysis_auto_modularization = false;
        let _ = complete(&mut workspace, save);
    } else {
        let split = complete(&mut workspace, save);
        let split_save = complete(&mut workspace, split);
        let diagram = complete(&mut workspace, split_save);
        let diagram_save = complete(&mut workspace, diagram);
        let _ = complete(&mut workspace, diagram_save);
    }
    let sent = provider.join().unwrap();
    let loaded = history.load(subject).unwrap();
    let pre = loaded.current().unwrap();
    assert_eq!(pre.analysis.summary, "The analysis is ready.");
    assert_eq!(pre.modularization.is_some(), !disable);
    assert_eq!(pre.diagram.is_some(), !disable);
    if disable {
        assert_eq!(sent.len(), 1);
    } else {
        assert_eq!(sent[1]["model"], "reviewed-model");
        for (index, prompt) in reviewed.iter().enumerate() {
            assert_eq!(
                sent[1]["messages"][index]["content"].as_str(),
                Some(prompt.as_str())
            );
        }
    }
    assert_eq!(
        Library::open(profile.path())
            .unwrap()
            .get(snippet)
            .unwrap()
            .content(),
        original
    );
}

fn complete(workspace: &mut Workspace, task: Task<Message>) -> Task<Message> {
    let mut stream = iced_runtime::task::into_stream(task).expect("scheduled modularization step");
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match stream.next().now_or_never() {
            Some(Some(iced_runtime::Action::Output(message))) => {
                let provisional = matches!(
                    &message,
                    Message::SnippetAnalysis(_, super::super::Event::Streaming(..))
                );
                let next = workspace.update(message);
                if !provisional {
                    return next;
                }
            }
            Some(None) => panic!("The modularization step ended without a result"),
            _ => {}
        }
        assert!(
            Instant::now() < deadline,
            "The modularization step did not complete"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
