use std::net::TcpListener;
use std::path::Path;

use serde_json::Value;

use super::*;
use crate::local_ai::test_support::fixture_http_response;
use crate::workspace::saved_chats::store::{Attachment, AttachmentKind};

const SECRET: &str = "masked-fixture-credential";
const LOCAL_GUIDANCE: &str = "Prefer a read-only diagnostic.";
const HYBRID_GUIDANCE: &str = "Confirm the symptom with a smoke check.";
const HYBRID_RULE: &str = "Use a reversible checkpoint.";
const ATTACHMENT: &str = "Attachment-only fixture evidence.";

fn configuration(server: &TcpListener, budget: bool) -> Configuration {
    let mut profile = json!({
        "id": 7,
        "name": "Local fixture",
        "endpoint": server.local_addr().unwrap().to_string(),
        "model": "test",
        "max_output_tokens": 256
    });
    if budget {
        profile["usage_budget"] = json!({"token_limit": 500});
    }
    serde_json::from_value(json!({
        "profiles": {"profiles": [profile], "default": 7, "last_id": 7},
        "skills": {
            "last_id": 55,
            "skills": {
                "51": {"name": "Database", "tags": ["database"], "content": LOCAL_GUIDANCE, "enabled": true, "target": "CHAT"},
                "52": {"name": "Workflow", "description": format!("Notes for {SECRET}"), "content": format!("{HYBRID_GUIDANCE} {SECRET}"), "rules": [HYBRID_RULE], "enabled": true, "target": "BOTH"},
                "53": {"name": "Agent only", "content": "Agent body.", "enabled": true, "target": "AGENT"},
                "54": {"name": "Connection only", "content": "Connection body.", "enabled": true, "target": "CONNECTION"},
                "55": {"name": "Hidden", "content": "Hidden body.", "enabled": true, "hidden": true}
            }
        }
    }))
    .unwrap()
}

fn chat_fixture(server: &TcpListener, saved: Option<&Path>) -> (Workspace, window::Id) {
    let mut config = configuration(server, saved.is_some());
    if let Some(root) = saved {
        config
            .save_fixture_at(root, &Configuration::default())
            .unwrap();
        config = Configuration::load_fixture_from(root).unwrap();
    }
    let secrets = Secrets::default();
    secrets.add(SECRET);
    let redactor = Redactor::for_journal(secrets.seal().unwrap(), Default::default());
    let mut chat = Chat::new(
        &config,
        Action::Ask,
        &format!("Terminal-only fixture evidence {SECRET}"),
        redactor,
        None,
    )
    .unwrap();
    chat.include_memory = false;
    chat.storage = None;
    chat.knowledge_root = None;
    chat.question = text_editor::Content::with_text("How can I diagnose the database?");
    let mut workspace = Workspace::default();
    workspace.set_test_ai_configuration(config);
    let id = window::Id::unique();
    workspace.terminal_chats.insert(id, chat);
    (workspace, id)
}

fn reviewed(workspace: &Workspace, id: window::Id) -> &Review {
    workspace.terminal_chats[&id].review.as_ref().unwrap()
}

fn classifier_preview(workspace: &Workspace, id: window::Id) -> Value {
    let classifier = reviewed(workspace, id)
        .skill_classification
        .as_ref()
        .unwrap();
    serde_json::from_str(&classifier.preview.text()).unwrap()
}

fn no_connection(server: &TcpListener) {
    assert_eq!(server.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
}

#[test]
fn terminal_skill_classification_requires_separate_reviews_and_accounts_each_request_once() {
    const QUESTION: &str = "From now on always diagnose the database with a read-only query.";
    let _exclusive = exclusive_run();
    let (server, _) = listener();
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let account = local_ai::UsageAccount::new(
        root.clone(),
        7,
        local_ai::UsageBudget {
            token_limit: Some(500),
            ..Default::default()
        },
    )
    .unwrap();
    let (mut workspace, id) = chat_fixture(&server, Some(&root));
    let chat = workspace.terminal_chats.get_mut(&id).unwrap();
    chat.storage = Some(root);
    chat.question = text_editor::Content::with_text(QUESTION);
    let attachment = Attachment::masked(
        AttachmentKind::Local,
        "fixture.txt",
        "Local fixture",
        ATTACHMENT,
        &chat.redactor,
    )
    .unwrap();
    chat.conversation = conversation::State::with_archived_attachment(Some(attachment));

    let task = workspace.update_terminal_ai(id, Message::Review);
    finish(&mut workspace, task);
    let initial = reviewed(&workspace, id);
    assert!(initial.system().contains(LOCAL_GUIDANCE));
    assert!(!initial.system().contains(HYBRID_GUIDANCE));
    assert!(initial.request.input.contains(ATTACHMENT));
    let _ = workspace.update_terminal_ai(id, Message::IncludeRuleSuggestion(true));
    assert!(reviewed(&workspace, id).automatic.as_ref().unwrap().showing);
    no_connection(&server);
    assert!(
        iced_runtime::task::into_stream(workspace.update_terminal_ai(id, Message::ClassifySkills))
            .is_none()
    );
    no_connection(&server);

    let task = workspace.update_terminal_ai(id, Message::ReviewSkills);
    finish(&mut workspace, task);
    let preview = classifier_preview(&workspace, id);
    assert_eq!(preview.as_array().unwrap().len(), 2);
    assert_eq!(preview[0]["role"], "system");
    assert_eq!(preview[1]["role"], "user");
    let payload: Value = serde_json::from_str(preview[1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(payload["target"], "CHAT");
    assert_eq!(payload["requestContext"]["question"], QUESTION);
    assert_eq!(
        payload["requestContext"]["terminalSelection"],
        "Terminal-only fixture evidence ***"
    );
    let candidates = payload["skills"].as_array().unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(candidates.iter().any(|candidate| candidate == &json!({
        "id": "52", "name": "Workflow", "description": "Notes for ***", "tags": [], "target": "BOTH"
    })));
    let rendered = preview.to_string();
    for excluded in [
        SECRET,
        ATTACHMENT,
        HYBRID_GUIDANCE,
        HYBRID_RULE,
        LOCAL_GUIDANCE,
        "Agent only",
        "Connection only",
        "Hidden",
    ] {
        assert!(
            !rendered.contains(excluded),
            "classifier included {excluded:?}"
        );
    }
    assert!(
        iced_runtime::task::into_stream(workspace.update_terminal_ai(id, Message::Send)).is_none()
    );
    assert_eq!(account.snapshot().unwrap().counts.requests, 0);
    no_connection(&server);

    let classifier = workspace.update_terminal_ai(id, Message::ClassifySkills);
    let mut socket = accept(&server);
    let wire = request(&mut socket);
    assert_eq!(wire["messages"], preview);
    assert_eq!(wire["max_tokens"], 128);
    assert!(wire.get("tools").is_none());
    assert!(wire.get("response_format").is_none());
    assert!(
        iced_runtime::task::into_stream(workspace.update_terminal_ai(id, Message::ClassifySkills))
            .is_none()
    );
    no_connection(&server);
    let response = fixture_http_response(
        "200 OK", "", &json!({
            "choices": [{"message": {"content": r#"{"skillIds":["53","52","52"]}"#}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 9, "completion_tokens": 4}
        }).to_string(),
    );
    socket.get_mut().write_all(response.as_bytes()).unwrap();
    drop(socket);
    let refresh = outcome(&mut workspace, classifier);
    finish(&mut workspace, refresh);
    let review = reviewed(&workspace, id);
    assert!(review.system().contains(HYBRID_GUIDANCE));
    assert!(review.system().contains(HYBRID_RULE));
    assert!(!review.system().contains(LOCAL_GUIDANCE));
    assert!(!review.system().contains(SECRET));
    let automatic = review.automatic.as_ref().unwrap();
    assert!(automatic.included && !automatic.showing);
    assert!(
        review
            .quota
            .label()
            .unwrap()
            .contains("13 of 500 tokens reported")
    );
    let estimated = quota::estimate_input([review.system(), review.request.outbound_input()]);
    assert!(
        review
            .quota
            .label()
            .unwrap()
            .contains(&format!("about {estimated} input tokens"))
    );
    assert!(workspace.terminal_chats[&id].pending.is_none());
    assert!(workspace.terminal_chats[&id].turns.is_empty());
    assert_eq!(account.snapshot().unwrap().counts.requests, 1);
    no_connection(&server);

    let main_preview: Value = serde_json::from_str(&review.preview.text()).unwrap();
    let _ = workspace.update_terminal_ai(id, Message::IncludeRuleSuggestion(false));
    let send = workspace.update_terminal_ai(id, Message::Send);
    let mut socket = accept(&server);
    let wire = request(&mut socket);
    assert_eq!(wire["messages"], main_preview);
    assert_eq!(wire["max_tokens"], 256);
    assert!(
        wire["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains(ATTACHMENT)
    );
    let response = fixture_http_response(
        "200 OK",
        "",
        r#"{"choices":[{"message":{"content":"Fixture answer."}}],"usage":{"prompt_tokens":18,"completion_tokens":6}}"#,
    );
    socket.get_mut().write_all(response.as_bytes()).unwrap();
    drop(socket);
    finish(&mut workspace, send);
    assert_eq!(
        workspace.terminal_chats[&id].turns[0].answer(),
        "Fixture answer."
    );
    let usage = account.snapshot().unwrap();
    assert_eq!(
        (
            usage.counts.requests,
            usage.counts.input_tokens,
            usage.counts.output_tokens,
            usage.in_flight
        ),
        (2, 27, 10, 0)
    );
    no_connection(&server);
}

#[test]
fn terminal_skill_classification_keeps_local_guidance_after_invalid_or_ineligible_results() {
    let _exclusive = exclusive_run();
    let (server, _) = listener();
    let (mut workspace, id) = chat_fixture(&server, None);
    workspace
        .terminal_chats
        .get_mut(&id)
        .unwrap()
        .turns
        .push(Arc::new(Turn::from_parts(
            Some("Past terminal evidence"),
            "Previous question".into(),
            "Previous answer".into(),
        )));

    for content in ["invalid reply", r#"{"skillIds":["53","54","55","999"]}"#] {
        let _ = workspace.update_terminal_ai(id, Message::Review);
        let _ = workspace.update_terminal_ai(id, Message::ReviewSkills);
        let preview = classifier_preview(&workspace, id);
        let rendered = preview.to_string();
        for excluded in [
            "Past terminal evidence",
            "Previous question",
            "Previous answer",
        ] {
            assert!(!rendered.contains(excluded));
        }
        let classifier = workspace.update_terminal_ai(id, Message::ClassifySkills);
        let mut socket = accept(&server);
        assert_eq!(request(&mut socket)["messages"], preview);
        socket
            .get_mut()
            .write_all(fixture_response("200 OK", "", content).as_bytes())
            .unwrap();
        drop(socket);
        finish(&mut workspace, classifier);
        let review = reviewed(&workspace, id);
        assert!(review.system().contains(LOCAL_GUIDANCE));
        assert!(!review.system().contains(HYBRID_GUIDANCE));
        assert!(review.classified_skills.is_empty());
        assert!(workspace.terminal_chats[&id].pending.is_none());
        assert_eq!(workspace.terminal_chats[&id].turns.len(), 1);
        no_connection(&server);
        let _ = workspace.update_terminal_ai(id, Message::Back);
    }
}

#[test]
fn terminal_skill_classification_cancel_closes_the_socket_and_ignores_late_results() {
    let _exclusive = exclusive_run();
    let (server, _) = listener();
    let (mut workspace, id) = chat_fixture(&server, None);
    let _ = workspace.update_terminal_ai(id, Message::Review);
    let _ = workspace.update_terminal_ai(id, Message::ReviewSkills);
    let task = workspace.update_terminal_ai(id, Message::ClassifySkills);
    let mut socket = accept(&server);
    let _ = request(&mut socket);
    let _ = workspace.update_terminal_ai(id, Message::CancelSkillClassification);
    match socket.read(&mut [0_u8; 1]) {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
            ) => {}
        result => panic!("classifier cancel left the HTTP connection open: {result:?}"),
    }
    let mut actions = iced_runtime::task::into_stream(task).unwrap();
    let token = loop {
        if let iced_runtime::Action::Output(Desktop::TerminalAi(
            _,
            Message::SkillClassificationFinished(token, _),
        )) = block_on(actions.next()).expect("scheduled classifier completion")
        {
            break token;
        }
    };
    let _ = workspace.update_terminal_ai(
        id,
        Message::SkillClassificationFinished(token, Ok(vec![52])),
    );
    let review = reviewed(&workspace, id);
    assert!(review.system().contains(LOCAL_GUIDANCE));
    assert!(!review.system().contains(HYBRID_GUIDANCE));
    assert!(review.classified_skills.is_empty());
    no_connection(&server);
}

#[test]
fn terminal_skill_classification_rechecks_the_saved_catalogue_before_dispatch() {
    let _exclusive = exclusive_run();
    let (server, _) = listener();
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let (mut workspace, id) = chat_fixture(&server, Some(&root));
    let task = workspace.update_terminal_ai(id, Message::Review);
    finish(&mut workspace, task);
    let task = workspace.update_terminal_ai(id, Message::ReviewSkills);
    finish(&mut workspace, task);
    assert!(classifier_preview(&workspace, id).is_array());

    let baseline = Configuration::load_fixture_from(&root).unwrap();
    let mut changed = serde_json::to_value(&baseline).unwrap();
    changed["skills"]["skills"]["52"]["description"] = json!("Changed after review");
    let changed: Configuration = serde_json::from_value(changed).unwrap();
    changed.save_fixture_at(&root, &baseline).unwrap();

    let classifier = workspace.update_terminal_ai(id, Message::ClassifySkills);
    let _ = outcome(&mut workspace, classifier);
    let review = reviewed(&workspace, id);
    assert!(review.classified_skills.is_empty());
    assert!(review.system().contains(LOCAL_GUIDANCE));
    assert!(!review.system().contains(HYBRID_GUIDANCE));
    assert!(!workspace.terminal_chats[&id].notice.is_empty());
    let account = local_ai::UsageAccount::new(root, 7, Default::default()).unwrap();
    assert_eq!(account.snapshot().unwrap().counts.requests, 0);
    no_connection(&server);
}
