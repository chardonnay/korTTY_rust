use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use iced::futures::{FutureExt as _, StreamExt as _, executor::block_on, stream::BoxStream};
use serde_json::json;

use crate::ai_manager::Configuration;
use crate::local_ai::test_support::{accept, exclusive_run, listener, request};
use crate::session_journal;

use super::super::screenshot_access::tests::Fixture;
use super::*;

fn dispatch(workspace: &mut Workspace, id: window::Id, event: Message) -> Task<Desktop> {
    workspace.update(Desktop::SessionJournal(id, JournalMessage::Question(event)))
}

fn drain(workspace: &mut Workspace, task: Task<Desktop>) {
    let mut pending = vec![task];
    while let Some(task) = pending.pop() {
        let Some(mut actions) = iced_runtime::task::into_stream(task) else {
            continue;
        };
        while let Some(action) = block_on(actions.next()) {
            if let iced_runtime::Action::Output(message @ Desktop::SessionJournal(..)) = action {
                pending.push(workspace.update(message));
            }
        }
    }
}

fn prepare(workspace: &mut Workspace, id: window::Id) {
    let _ = dispatch(workspace, id, Message::Open);
    prepare_current(workspace, id, "Where was the failure?");
}

fn prepare_current(workspace: &mut Workspace, id: window::Id, question: &str) {
    let _ = dispatch(workspace, id, Message::Edit(question.into()));
    let task = dispatch(workspace, id, Message::Prepare);
    drain(workspace, task);
    assert!(matches!(
        workspace.journal_questions.dialogs[&id].stage,
        Stage::Review(..)
    ));
}

fn https_configuration(address: std::net::SocketAddr, model: &str) -> Configuration {
    serde_json::from_value(json!({
        "profiles": {
            "profiles": [
                {"id": 7, "name": "Journal HTTPS fixture", "model": model,
                 "endpoint": format!("https://{address}/v1"), "connection_mode": "remote-open-ai",
                 "trusted_remote_endpoint": true},
                {"id": 8, "name": "Journal local fixture", "model": "fixture:local",
                 "endpoint": address.to_string()}
            ],
            "default": 7, "last_id": 8
        },
        "local": {"roles": [null, null, null, 8]}
    }))
    .unwrap()
}

#[test]
fn journal_question_https_requires_saved_selection_and_exact_masked_approval() {
    use kortty_vault::{CredentialInput, Secret, Vault};

    let _exclusive = exclusive_run();
    let fixture = Fixture::new();
    let (mut workspace, _, id) = fixture.open(true);
    let (endpoint, _) = listener();
    let address = endpoint.local_addr().unwrap();
    configure(&mut workspace, id, address);

    let config = https_configuration(address, "fixture-model");
    workspace.set_test_ai_configuration(config.clone());
    let _ = dispatch(&mut workspace, id, Message::Open);
    let _ = dispatch(&mut workspace, id, Message::SelectAnswer(Some(7)));
    assert!(workspace.journal_questions.dialogs[&id].outbound.is_none());

    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let secret = r#"sensitive "quoted" journal vault value"#;
    let path = root.join("test-vault.bin");
    let mut vault = Vault::create(&path, b"journal-question-fixture").unwrap();
    vault
        .save(
            CredentialInput {
                label: "Synthetic journal credential".into(),
                ..CredentialInput::default()
            },
            Secret::new(secret.into()),
        )
        .unwrap();
    workspace.vault = Some(crate::vault_window::VaultWindow::with_unlocked_fixture(
        path, vault,
    ));
    let Mode::Viewer(viewer) = &workspace.journals[&id].mode else {
        panic!("expected journal viewer");
    };
    let log = viewer.source.directory().join("session-log-2.json");
    let prefix = "failure crossed boundary: ";
    let display_secret = secret.replacen("sensitive", "sen\u{200e}sitive", 1);
    let crossing = json!({
        "seq": 2, "t": "2026-10-01T08:00:01Z", "k": "out",
        "x": format!("{prefix}{} {display_secret}", "x".repeat(585 - prefix.len())),
    });
    let contents = fs::read_to_string(&log)
        .unwrap()
        .replace(
            r#""failure from part two""#,
            &json!(format!("failure from part two with {secret}")).to_string(),
        )
        .replace(
            r#"{"type":"end"}"#,
            &format!("{crossing}\n{{\"type\":\"end\"}}"),
        );
    fs::write(log, contents).unwrap();

    let _ = dispatch(&mut workspace, id, Message::Open);
    let _ = dispatch(&mut workspace, id, Message::SelectAnswer(Some(8)));
    let dialog = &workspace.journal_questions.dialogs[&id];
    assert!(dialog.outbound.is_none());
    assert!(!dialog.answer_route().unwrap().profile.is_remote());
    let _ = dispatch(&mut workspace, id, Message::SelectAnswer(Some(7)));
    let dialog = &workspace.journal_questions.dialogs[&id];
    assert!(dialog.answer_route().unwrap().profile.is_remote());
    assert_eq!(
        dialog.answer_route().unwrap().label,
        format!("Journal HTTPS fixture · fixture-model · https://{address}/v1/chat/completions")
    );
    let _ = dispatch(
        &mut workspace,
        id,
        Message::Edit(format!("{}{secret}", "a".repeat(MAX_INPUT - 10))),
    );
    assert!(
        iced_runtime::task::into_stream(dispatch(&mut workspace, id, Message::Prepare)).is_none()
    );
    assert!(workspace.journal_questions.dialogs[&id].input.is_empty());
    prepare_current(
        &mut workspace,
        id,
        &format!("Where was the failure? {secret}"),
    );
    let prepared = prepared(&workspace, id);
    let ticket = prepared.https_ticket().unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(prepared.review.answer_messages()[1]).unwrap();
    assert_eq!(payload["question"], "Where was the failure? ***");
    assert!(
        prepared
            .review
            .citations
            .iter()
            .any(|citation| { citation.excerpt == "failure from part two with ***" })
    );
    assert!(prepared.review.citations.iter().any(|citation| {
        citation.excerpt.starts_with(prefix) && citation.excerpt.ends_with("***")
    }));
    assert!(!prepared.review.answer_messages()[1].contains("sensitive"));
    for message in [
        Message::Answer(true),
        Message::SendHttps(ticket.wrapping_add(1)),
    ] {
        assert!(iced_runtime::task::into_stream(dispatch(&mut workspace, id, message)).is_none());
    }
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

    let peer = thread::spawn(move || {
        let mut socket = accept(&endpoint);
        socket
            .get_mut()
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut tls = [0; 2];
        socket.read_exact(&mut tls).unwrap();
        assert_eq!(tls, [22, 3]);
    });
    let approved = dispatch(&mut workspace, id, Message::SendHttps(ticket));
    drain(&mut workspace, approved);
    peer.join().unwrap();
    assert!(matches!(&workspace.journal_questions.dialogs[&id].stage,
        Stage::Review(_, Some(answer)) if !answer.ai_used));
}

#[test]
fn journal_question_https_revokes_prepared_review_on_vault_lock_and_saved_route_change() {
    use kortty_vault::Vault;

    let _exclusive = exclusive_run();
    let fixture = Fixture::new();
    let (mut workspace, _, id) = fixture.open(true);
    let (endpoint, _) = listener();
    let address = endpoint.local_addr().unwrap();
    configure(&mut workspace, id, address);
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    https_configuration(address, "reviewed-model")
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let saved = Configuration::load_fixture_from(&root).unwrap();
    workspace.set_test_ai_configuration(saved.clone());
    let vault_path = root.join("test-vault.bin");
    let vault = Vault::create(&vault_path, b"journal-revocation-fixture").unwrap();
    workspace.vault = Some(crate::vault_window::VaultWindow::with_unlocked_fixture(
        vault_path, vault,
    ));
    let _ = dispatch(&mut workspace, id, Message::Open);
    let _ = dispatch(&mut workspace, id, Message::SelectAnswer(Some(7)));
    prepare_current(&mut workspace, id, "Where was the failure?");
    let locked_ticket = prepared(&workspace, id).https_ticket().unwrap();
    let _ = workspace
        .vault
        .as_mut()
        .unwrap()
        .update(crate::vault_window::Message::Lock);
    let revoked = dispatch(&mut workspace, id, Message::SendHttps(locked_ticket));
    assert!(iced_runtime::task::into_stream(revoked).is_none());
    let dialog = &workspace.journal_questions.dialogs[&id];
    assert!(dialog.outbound.is_none());
    assert!(matches!(dialog.stage, Stage::Question));
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

    let _ = dispatch(&mut workspace, id, Message::SelectAnswer(Some(7)));
    prepare_current(&mut workspace, id, "Where was the failure?");
    let ticket = prepared(&workspace, id).https_ticket().unwrap();
    https_configuration(address, "changed-model")
        .save_fixture_at(&root, &saved)
        .unwrap();
    let revoked = dispatch(&mut workspace, id, Message::SendHttps(ticket));
    drain(&mut workspace, revoked);
    let dialog = &workspace.journal_questions.dialogs[&id];
    assert!(dialog.error.is_some());
    assert!(matches!(dialog.stage, Stage::Review(_, None)));
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    let _ = dispatch(&mut workspace, id, Message::Answer(false));
    assert!(matches!(&workspace.journal_questions.dialogs[&id].stage,
        Stage::Review(_, Some(answer)) if !answer.ai_used));
}

fn prepared(workspace: &Workspace, id: window::Id) -> &Prepared {
    let Stage::Review(prepared, _) = &workspace.journal_questions.dialogs[&id].stage else {
        panic!("expected prepared question");
    };
    prepared
}

fn configure(workspace: &mut Workspace, id: window::Id, endpoint: std::net::SocketAddr) {
    let Mode::Viewer(viewer) = &workspace.journals[&id].mode else {
        panic!("expected journal viewer");
    };
    let path = viewer.source.directory();
    workspace.window_settings.session_journal.directory =
        path.parent().unwrap().to_str().unwrap().into();
    workspace.window_settings.journal_ai.enabled = true;
    workspace.window_settings.journal_ai.endpoint = endpoint.to_string();
    workspace.window_settings.journal_ai.model = "fixture:local".into();
    let source = viewer.source.clone();
    let journal_id = source.id();
    fs::write(path.join("journal.xml"), format!(
        "<session-journal formatVersion=\"1\" id=\"{journal_id}\"><meta><title>Fixture</title><logParts>2</logParts></meta>\
         <entries><entry><id>screen</id><kind>USER_NOTE</kind><title>Observed failure</title><text>Build failure note</text></entry></entries></session-journal>"
    )).unwrap();
    for (part, name, text) in [
        (1, "session-log.json", "failure from part one"),
        (2, "session-log-2.json", "failure from part two"),
    ] {
        let header = json!({"type":"meta","formatVersion":1,"journalId":journal_id,"part":part});
        let record = json!({"seq":1,"t":"2026-10-01T08:00:00Z","k":"out","x":text});
        fs::write(
            path.join(name),
            format!("{header}\n{record}\n{{\"type\":\"end\"}}\n"),
        )
        .unwrap();
    }
    let Mode::Viewer(viewer) = &mut workspace.journals.get_mut(&id).unwrap().mode else {
        unreachable!();
    };
    viewer.document = source.read(None).unwrap().map(Arc::new);
}

#[test]
fn journal_question_cli_reviews_exact_masked_files_for_single_and_cross_journal_answers() {
    use std::process::Command;

    use kortty_vault::{CredentialInput, Secret, Vault};

    use crate::local_ai::cli::{Program, fixture_executable};
    use crate::snippets::tests::Profile as Scratch;

    const FIXTURE: &str = "KORTTY_TEST_JOURNAL_QUESTION_CLI_HOME";
    const TEST: &str = "workspace::journal_access::questions::route_tests::journal_question_cli_reviews_exact_masked_files_for_single_and_cross_journal_answers";
    const SECRET: &str = "private journal question fixture value";
    const FIRST_QUOTE: &str = "failure from part two";
    const SECOND_QUOTE: &str = "Build failure on backup node";
    let Some(home) = std::env::var_os(FIXTURE).map(PathBuf::from) else {
        let scratch = Scratch::new();
        let home = scratch.0.canonicalize().unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
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
    fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    crate::profile::set_runtime_root(root.clone());
    let journals = root.join("journals");
    let (executable, fingerprint) = Program::inspect(&fixture_executable(&root)).unwrap();
    let config: Configuration = serde_json::from_value(json!({
        "profiles": {"profiles": [{"id": 7, "name": "Journal question CLI fixture",
            "connection_mode": "cli", "endpoint": "", "vision": "disabled", "prompt_preset": "qwen", "cli": {
                "path": executable, "fingerprint": fingerprint,
                "arguments": ["fixture_journal_question", "{systemPromptFile}", "{userPromptFile}"]
            }}], "default": 7, "last_id": 7}
    }))
    .unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let mut workspace = Workspace::default();
    workspace.window_settings.open_tool_windows_as_tabs = true;
    workspace.window_settings.session_journal.directory = journals.to_str().unwrap().into();
    workspace.window_settings.journal_ai.enabled = true;
    workspace
        .window_settings
        .save(&root.join("window-settings.json"))
        .unwrap();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());

    let path = root.join("test-vault.bin");
    let mut vault = Vault::create(&path, b"journal-question-cli-fixture").unwrap();
    vault
        .save(
            CredentialInput {
                label: "Synthetic journal question credential".into(),
                ..CredentialInput::default()
            },
            Secret::new(SECRET.into()),
        )
        .unwrap();
    workspace.vault = Some(VaultWindow::with_unlocked_fixture(path, vault));

    let first = journals.join("first");
    let second = journals.join("second");
    for (directory, id, title, parts, note) in [
        (
            &first,
            "first-fixture",
            "First failure",
            2,
            "First failure note",
        ),
        (&second, "second-fixture", "Backup failure", 0, SECOND_QUOTE),
    ] {
        fs::create_dir_all(directory).unwrap();
        fs::write(directory.join("journal.xml"), format!(
            r#"<session-journal formatVersion="1" id="{id}"><meta><title>{title}</title><logParts>{parts}</logParts></meta><entries><entry><id>note</id><kind>USER_NOTE</kind><title>{note}</title><text>Observed in build output.</text></entry></entries></session-journal>"#,
        )).unwrap();
    }
    let capture = first.join("session-log-2.json");
    let header = json!({"type":"meta", "formatVersion":1, "journalId":"first-fixture", "part":2});
    let record = json!({"seq":1, "t":"2026-10-01T08:00:00Z", "k":"out",
        "x": format!("{FIRST_QUOTE} with {SECRET}")});
    let original = format!("{header}\n{record}\n{{\"type\":\"end\"}}\n");
    fs::write(&capture, &original).unwrap();
    let header = json!({"type":"meta", "formatVersion":1, "journalId":"first-fixture", "part":1});
    fs::write(
        first.join("session-log.json"),
        format!("{header}\n{{\"type\":\"end\"}}\n"),
    )
    .unwrap();

    let (owner, _) = workspace.open_window(false);
    let document = session_journal::read(&first).unwrap();
    let source = Source::catalog(&document.summary).unwrap();
    let task = workspace.open_journal_viewer(owner, source, "First failure".into());
    drain(&mut workspace, task);
    let viewer = *workspace.journals.keys().next().unwrap();
    let receipt = |name: &str| root.join(format!("fixture-journal-question-{name}"));
    let open = |workspace: &mut Workspace, id| {
        let _ = dispatch(workspace, id, Message::Open);
        let _ = dispatch(workspace, id, Message::SelectAnswer(Some(7)));
        assert!(
            workspace.journal_questions.dialogs[&id]
                .answer_route()
                .unwrap()
                .profile
                .is_cli()
        );
    };
    let prepare_across = |workspace: &mut Workspace, id, question: &str| {
        let _ = dispatch(workspace, id, Message::Edit(question.into()));
        let ranking = dispatch(workspace, id, Message::Prepare);
        drain(workspace, ranking);
        assert!(matches!(&workspace.journal_questions.dialogs[&id].stage,
            Stage::Ranking(ranking) if ranking.candidates.len() == 2));
        let evidence = dispatch(workspace, id, Message::ReadEvidence(false));
        drain(workspace, evidence);
    };
    let ticket = |workspace: &Workspace, id| {
        let Some(ReviewedAnswer::Cli { ticket, .. }) = &prepared(workspace, id).outbound else {
            panic!("expected a prepared CLI request");
        };
        *ticket
    };
    let approve = |workspace: &mut Workspace, id, previous| {
        let review = prepared(workspace, id);
        let Some(ReviewedAnswer::Cli { ticket, request }) = &review.outbound else {
            panic!("expected a prepared CLI request");
        };
        let ticket = *ticket;
        let (label, system) = request.primary();
        let user = request.secondary().unwrap();
        assert_eq!(label, "Exact UTF-8 system-prompt.txt");
        assert_eq!([system, user], review.review.answer_messages());
        assert!(system.contains("<kortty_model_preset>"));
        assert!(!user.contains(SECRET));
        assert!(user.contains("***"));
        let expected = [system.to_owned(), user.to_owned()];
        let arguments: Vec<_> = request.reviewed_arguments().collect();
        let ["fixture_journal_question", system_path, user_path] = arguments.as_slice() else {
            panic!("expected two resolved prompt paths");
        };
        let paths = [system_path.to_string(), user_path.to_string()];
        assert!(
            paths
                .iter()
                .all(|path| !std::path::Path::new(path).exists())
        );
        for ignored in [
            Message::Answer(true),
            Message::SendHttps(ticket),
            Message::SendCli(previous),
        ] {
            assert!(iced_runtime::task::into_stream(dispatch(workspace, id, ignored)).is_none());
        }
        assert!(!receipt("user").exists());
        let task = dispatch(workspace, id, Message::SendCli(ticket));
        drain(workspace, task);
        for (index, role) in ["system", "user"].into_iter().enumerate() {
            let path_receipt = receipt(&format!("{role}-path"));
            assert_eq!(fs::read_to_string(&path_receipt).unwrap(), paths[index]);
            assert!(!std::path::Path::new(&paths[index]).exists());
            let input_receipt = receipt(role);
            assert_eq!(fs::read_to_string(&input_receipt).unwrap(), expected[index]);
            fs::remove_file(path_receipt).unwrap();
            fs::remove_file(input_receipt).unwrap();
        }
        assert!(
            iced_runtime::task::into_stream(dispatch(workspace, id, Message::SendCli(ticket)))
                .is_none()
        );
        assert!(!receipt("user").exists());
        ticket
    };
    let question = format!("Where was the build failure? {SECRET}");
    open(&mut workspace, viewer);
    prepare_current(&mut workspace, viewer, &question);
    let stale = ticket(&workspace, viewer);
    fs::write(
        &capture,
        original.replace(FIRST_QUOTE, "failure changed since review"),
    )
    .unwrap();
    let rejected = dispatch(&mut workspace, viewer, Message::SendCli(stale));
    drain(&mut workspace, rejected);
    assert!(!receipt("user").exists());
    fs::write(&capture, &original).unwrap();

    open(&mut workspace, viewer);
    prepare_current(&mut workspace, viewer, &question);
    let denied = ticket(&workspace, viewer);
    workspace.clipboard.policy.minimum_isolation = crate::session_security::IsolationLevel::Process;
    let rejected = dispatch(&mut workspace, viewer, Message::SendCli(denied));
    drain(&mut workspace, rejected);
    assert!(!receipt("user").exists());
    workspace.clipboard.policy.minimum_isolation = crate::session_security::IsolationLevel::None;

    open(&mut workspace, viewer);
    prepare_current(&mut workspace, viewer, &question);
    let single = approve(&mut workspace, viewer, denied);
    let dialog = &workspace.journal_questions.dialogs[&viewer];
    let Stage::Review(review, Some(answer)) = &dialog.stage else {
        panic!("expected the single journal CLI answer: {:?}", dialog.error);
    };
    assert!(answer.ai_used);
    let [finding] = answer.findings.as_slice() else {
        panic!("expected one validated quotation");
    };
    assert_eq!(finding.quote, FIRST_QUOTE);
    let index = finding.citation - 1;
    assert!(matches!(&review.review.citations[index].target,
        Target::Capture(position) if position.part == 2 && position.sequence == 1));
    let jump = dispatch(&mut workspace, viewer, Message::Jump(index));
    drain(&mut workspace, jump);
    let Mode::Viewer(first_viewer) = &workspace.journals[&viewer].mode else {
        panic!("expected journal viewer");
    };
    assert!(first_viewer.capture_shown);
    assert!(
        first_viewer.capture.as_ref().unwrap().records[0]
            .text
            .contains(FIRST_QUOTE)
    );

    let task = workspace.update(Desktop::SessionJournal(owner, JournalMessage::Open));
    drain(&mut workspace, task);
    let manager = *workspace
        .journals
        .iter()
        .find_map(|(id, surface)| matches!(surface.mode, Mode::Manager(_)).then_some(id))
        .unwrap();
    open(&mut workspace, manager);
    prepare_across(
        &mut workspace,
        manager,
        &format!("{question} invented citation"),
    );
    let invalid = approve(&mut workspace, manager, single);
    assert!(
        matches!(&workspace.journal_questions.dialogs[&manager].stage,
        Stage::Review(_, Some(answer)) if !answer.ai_used)
    );

    open(&mut workspace, manager);
    prepare_across(&mut workspace, manager, &question);
    let _ = approve(&mut workspace, manager, invalid);
    let dialog = &workspace.journal_questions.dialogs[&manager];
    let Stage::Review(review, Some(answer)) = &dialog.stage else {
        panic!("expected the cross-journal CLI answer: {:?}", dialog.error);
    };
    assert!(answer.ai_used);
    assert_eq!(answer.findings.len(), 2);
    let index = answer
        .findings
        .iter()
        .find(|finding| finding.quote == SECOND_QUOTE)
        .unwrap()
        .citation
        - 1;
    assert_eq!(
        review.review.citations[index].source.directory(),
        second.as_path()
    );
    let jump = dispatch(&mut workspace, manager, Message::Jump(index));
    drain(&mut workspace, jump);
    assert!(workspace.journals.values().any(|surface| matches!(&surface.mode,
        Mode::Viewer(viewer) if viewer.source.directory() == second.as_path() && viewer.selected_entry == Some(0))));
}

#[test]
fn journal_question_viewer_opens_the_reviewed_physical_part_and_entry() {
    let fixture = Fixture::new();
    let (mut workspace, _, id) = fixture.open(true);
    let (endpoint, _) = listener();
    configure(&mut workspace, id, endpoint.local_addr().unwrap());
    prepare(&mut workspace, id);
    let index = prepared(&workspace, id).review.citations.iter().position(|citation| {
        matches!(&citation.target, Target::Capture(position) if position.part == 2 && position.sequence == 1)
    }).unwrap();
    let jump = dispatch(&mut workspace, id, Message::Jump(index));
    drain(&mut workspace, jump);
    let Mode::Viewer(viewer) = &workspace.journals[&id].mode else {
        panic!("expected viewer");
    };
    assert!(viewer.capture_shown);
    let records = &viewer.capture.as_ref().unwrap().records;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].text, "failure from part two");

    prepare(&mut workspace, id);
    let index = prepared(&workspace, id)
        .review
        .citations
        .iter()
        .position(|citation| matches!(&citation.target, Target::Entry(entry) if entry == "screen"))
        .unwrap();
    let jump = dispatch(&mut workspace, id, Message::Jump(index));
    drain(&mut workspace, jump);
    let Mode::Viewer(viewer) = &workspace.journals[&id].mode else {
        panic!("expected viewer");
    };
    assert!(!viewer.capture_shown);
    assert_eq!(viewer.selected_entry, Some(0));
    assert_eq!(
        endpoint.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn journal_question_opens_the_reviewed_recorded_chat() {
    let fixture = Fixture::new();
    let (mut workspace, _, id) = fixture.open(true);
    let (endpoint, _) = listener();
    configure(&mut workspace, id, endpoint.local_addr().unwrap());
    let Mode::Viewer(viewer) = &mut workspace.journals.get_mut(&id).unwrap().mode else {
        panic!("expected viewer");
    };
    let path = viewer.source.directory().join("journal.xml");
    let xml = fs::read_to_string(&path).unwrap().replace(
        "</entries>",
        r#"
      <entry><id>agent-card</id><kind>AGENT</kind><title>Agent card</title>
        <chatSnapshot><messages><message><role>assistant</role>
          <content>Artifact-ZX91 was cached.</content>
        </message></messages></chatSnapshot>
      </entry></entries>"#,
    );
    fs::write(path, xml).unwrap();
    viewer.document = viewer.source.read(None).unwrap().map(Arc::new);
    let _ = dispatch(&mut workspace, id, Message::Open);
    prepare_current(&mut workspace, id, "Where was Artifact-ZX91?");
    let index = prepared(&workspace, id).review.citations.iter().position(|citation| {
        matches!(&citation.target, Target::ChatEntry(entry) if entry == "agent-card")
    }).unwrap();
    let jump = dispatch(&mut workspace, id, Message::Jump(index));
    drain(&mut workspace, jump);
    let Mode::Viewer(viewer) = &workspace.journals[&id].mode else {
        panic!("expected viewer");
    };
    let selected = viewer.selected_entry.unwrap();
    let entry = &viewer.document.as_ref().unwrap().timeline[selected];
    assert_eq!(entry.id, "agent-card");
    let projection = super::super::Projection::new(entry);
    assert!(
        viewer
            .navigation
            .chat_open(selected, projection.first_chat_field())
    );
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
}

#[test]
fn journal_question_refuses_a_changed_reviewed_entry_even_with_a_cached_viewer() {
    let fixture = Fixture::new();
    let (mut workspace, _, id) = fixture.open(true);
    let (endpoint, _) = listener();
    configure(&mut workspace, id, endpoint.local_addr().unwrap());
    prepare(&mut workspace, id);
    let review = &prepared(&workspace, id).review;
    let index = review
        .citations
        .iter()
        .position(|citation| matches!(&citation.target, Target::Entry(entry) if entry == "screen"))
        .unwrap();
    let path = review.citations[index]
        .source
        .directory()
        .join("journal.xml");
    let replacement = fs::read_to_string(&path)
        .unwrap()
        .replace("Build failure note", "Changed entry with the same ID");
    fs::write(path, replacement).unwrap();

    let jump = dispatch(&mut workspace, id, Message::Jump(index));
    drain(&mut workspace, jump);
    let Mode::Viewer(viewer) = &workspace.journals[&id].mode else {
        panic!("expected viewer");
    };
    assert!(
        viewer
            .error
            .as_deref()
            .is_some_and(|error| error.contains("changed after review"))
    );
    assert_eq!(viewer.selected_entry, None);
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
}

#[test]
fn journal_question_revocation_cancels_a_queued_model_request_before_dispatch() {
    let _exclusive = exclusive_run();
    let fixture = Fixture::new();
    let (mut workspace, _, id) = fixture.open(true);
    let (endpoint, _) = listener();
    configure(&mut workspace, id, endpoint.local_addr().unwrap());
    prepare(&mut workspace, id);
    let request = dispatch(&mut workspace, id, Message::Answer(true));
    assert!(workspace.journal_questions.dialogs[&id].work.is_some());

    workspace.clipboard.policy.ai_allowed = false;
    let _ = workspace.update(Desktop::Noop);
    assert!(workspace.journal_questions.dialogs.is_empty());
    drain(&mut workspace, request);
    assert_eq!(
        endpoint.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn journal_question_refuses_a_replaced_reviewed_capture_before_dispatch() {
    let _exclusive = exclusive_run();
    let fixture = Fixture::new();
    let (mut workspace, _, id) = fixture.open(true);
    let (endpoint, _) = listener();
    configure(&mut workspace, id, endpoint.local_addr().unwrap());
    prepare(&mut workspace, id);
    let review = &prepared(&workspace, id).review;
    let cited = review
        .citations
        .iter()
        .find(
            |citation| matches!(&citation.target, Target::Capture(position) if position.part == 2),
        )
        .unwrap();
    let path = cited.source.directory().join("session-log-2.json");
    let changed = fs::read_to_string(&path)
        .unwrap()
        .replace("failure from part two", "failure replaced");
    fs::write(path, changed).unwrap();

    let task = dispatch(&mut workspace, id, Message::Answer(true));
    drain(&mut workspace, task);
    let dialog = &workspace.journal_questions.dialogs[&id];
    assert!(dialog.error.is_some());
    assert!(matches!(dialog.stage, Stage::Review(_, None)));
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
}

#[test]
fn journal_question_reviews_the_capped_quota_and_requires_approval_after_refresh() {
    use crate::ai_manager::Configuration;

    let _exclusive = exclusive_run();
    let fixture = Fixture::new();
    let (mut workspace, _, id) = fixture.open(true);
    let (endpoint, _) = listener();
    let address = endpoint.local_addr().unwrap();
    configure(&mut workspace, id, address);
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let configuration: Configuration = serde_json::from_value(json!({
        "profiles": {
            "profiles": [{
                "id": 7, "name": "Journal fixture", "model": "fixture:local",
                "endpoint": address.to_string(), "max_output_tokens": 1536,
                "usage_budget": {"token_limit": 1000}
            }],
            "default": 7, "last_id": 7
        },
        "local": {"roles": [null, null, null, 7]}
    }))
    .unwrap();
    configuration
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());

    prepare(&mut workspace, id);
    let messages = prepared(&workspace, id).review.answer_messages();
    let estimated_input = ai_quota::estimate_input(messages);
    let reviewed_messages = messages.map(str::to_owned);
    let quota = &mut workspace
        .journal_questions
        .dialogs
        .get_mut(&id)
        .unwrap()
        .quota;
    let forecast = quota.label().unwrap();
    assert!(forecast.contains("0 of 1000 tokens reported"));
    assert!(forecast.contains(&format!(
        "about {estimated_input} input tokens and up to 1024 output tokens"
    )));
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

    quota.expire();
    let refresh = dispatch(&mut workspace, id, Message::Answer(true));
    let dialog = &workspace.journal_questions.dialogs[&id];
    assert!(dialog.quota.loading());
    assert!(dialog.work.is_none());
    let _ = dispatch(
        &mut workspace,
        id,
        Message::QuotaFinished(u64::MAX, "Obsolete forecast".into()),
    );
    assert!(workspace.journal_questions.dialogs[&id].quota.loading());
    assert!(
        iced_runtime::task::into_stream(dispatch(&mut workspace, id, Message::Answer(true)))
            .is_none()
    );
    drain(&mut workspace, refresh);
    let dialog = &workspace.journal_questions.dialogs[&id];
    assert!(!dialog.quota.loading());
    assert!(!dialog.quota.stale());
    assert!(
        dialog
            .error
            .as_deref()
            .unwrap()
            .contains("refreshed quota forecast")
    );
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

    let server = thread::spawn(move || {
        let mut socket = accept(&endpoint);
        let body = request(&mut socket);
        assert_eq!(body["max_tokens"], 1024);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), reviewed_messages.len());
        for (message, reviewed) in messages.iter().zip(reviewed_messages) {
            assert_eq!(message["content"], reviewed);
        }
        socket
            .get_mut()
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        let reply = json!({
            "choices": [{"index": 0, "delta": {"content": r#"{"findings":[]}"#}, "finish_reason": "stop"}]
        });
        let usage = json!({
            "choices": [],
            "usage": {"prompt_tokens": 9, "completion_tokens": 4}
        });
        write!(
            socket.get_mut(),
            "data: {reply}\n\ndata: {usage}\n\ndata: [DONE]\n\n"
        )
        .unwrap();
    });
    let approved = dispatch(&mut workspace, id, Message::Answer(true));
    drain(&mut workspace, approved);
    server.join().unwrap();
    let dialog = &workspace.journal_questions.dialogs[&id];
    assert!(matches!(&dialog.stage, Stage::Review(_, Some(answer)) if answer.ai_used));
    prepare(&mut workspace, id);
    assert!(
        workspace.journal_questions.dialogs[&id]
            .quota
            .label()
            .unwrap()
            .contains("13 of 1000 tokens reported")
    );
}

#[test]
fn journal_question_stream_previews_exact_quotes_and_closes_on_cancel() {
    let _exclusive = exclusive_run();
    let fixture = Fixture::new();
    let (mut workspace, _, id) = fixture.open(true);
    let (endpoint, _) = listener();
    configure(&mut workspace, id, endpoint.local_addr().unwrap());
    prepare(&mut workspace, id);
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    let expected = "failure from part two";
    let number = prepared(&workspace, id)
        .review
        .citations
        .iter()
        .position(|citation| citation.excerpt.contains(expected))
        .unwrap()
        + 1;
    let (finish, wait) = mpsc::channel();
    let server = thread::spawn(move || {
        let prefix = format!(
            r#"{{"findings":[{},{}"#,
            json!({"citation":number,"quote":"invented failure"}),
            json!({"citation":number,"quote":expected})
        );
        for cancel in [false, true] {
            let mut socket = accept(&endpoint);
            let body = request(&mut socket);
            assert_eq!(body["stream"], true);
            assert_eq!(body["response_format"]["type"], "json_object");
            assert!(
                body["messages"][1]["content"]
                    .as_str()
                    .unwrap()
                    .contains(expected)
            );
            socket.get_mut().write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            ).unwrap();
            writeln!(
                socket.get_mut(),
                "data: {}\n",
                json!({
                    "choices":[{"index":0,"delta":{"content":prefix}}]
                })
            )
            .unwrap();
            if cancel {
                match socket.read(&mut [0_u8; 1]) {
                    Ok(0) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
                        ) => {}
                    result => panic!("expected cancellation to close the socket: {result:?}"),
                }
            } else {
                wait.recv_timeout(Duration::from_secs(4)).unwrap();
                socket.get_mut().write_all(
                    b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"]}\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                ).unwrap();
            }
        }
    });

    let next = |actions: &mut BoxStream<'static, iced_runtime::Action<Desktop>>| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match actions.next().now_or_never() {
                Some(Some(iced_runtime::Action::Output(event @ Desktop::SessionJournal(..)))) => {
                    return event;
                }
                Some(None) => panic!("the question task ended without an outcome"),
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "the question task did not progress"
            );
            thread::sleep(Duration::from_millis(5));
        }
    };
    let answer = dispatch(&mut workspace, id, Message::Answer(true));
    let mut actions = iced_runtime::task::into_stream(answer).unwrap();
    let live = next(&mut actions);
    assert!(matches!(
        &live,
        Desktop::SessionJournal(_, JournalMessage::Question(Message::AnswerProgress(..)))
    ));
    let _ = workspace.update(live);
    let dialog = &workspace.journal_questions.dialogs[&id];
    let quotes = &dialog.work.as_ref().unwrap().quotations;
    assert_eq!(quotes.len(), 1);
    assert_eq!(quotes[0].citation + 1, number);
    assert_eq!(
        &prepared(&workspace, id).review.citations[number - 1].excerpt[quotes[0].range.clone()],
        expected
    );
    assert!(matches!(dialog.stage, Stage::Review(_, None)));
    finish.send(()).unwrap();
    let _ = workspace.update(next(&mut actions));
    let dialog = &workspace.journal_questions.dialogs[&id];
    assert!(dialog.work.is_none());
    assert!(matches!(&dialog.stage, Stage::Review(_, Some(answer)) if !answer.ai_used));

    let _ = dispatch(&mut workspace, id, Message::Close);
    prepare(&mut workspace, id);
    let answer = dispatch(&mut workspace, id, Message::Answer(true));
    let mut actions = iced_runtime::task::into_stream(answer).unwrap();
    let late = next(&mut actions);
    assert!(matches!(
        &late,
        Desktop::SessionJournal(_, JournalMessage::Question(Message::AnswerProgress(..)))
    ));
    let _ = dispatch(&mut workspace, id, Message::Close);
    server.join().unwrap();
    let _ = workspace.update(late);
    let _ = workspace.update(next(&mut actions));
    assert!(workspace.journal_questions.dialogs.is_empty());
}
