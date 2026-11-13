use crate::ai_manager::{Configuration, ProfileRef};
use crate::terminal_logging::Secrets;

use super::*;

fn redactor(secret: Option<&str>) -> Redactor {
    let secrets = Secrets::default();
    if let Some(secret) = secret {
        secrets.add(secret);
    }
    Redactor::for_journal(secrets.seal().unwrap(), Default::default())
}

fn step(output: Vec<String>) -> Step {
    Step {
        kind: StepKind::Command,
        title: "Fixture action".into(),
        detail: "Reviewed evidence".into(),
        command: None,
        output,
        exit_code: Some(0),
        status: Some(StepStatus::Completed),
        elapsed_seconds: None,
        truncated: None,
    }
}

#[test]
fn agent_step_remasks_pem_as_one_output_block() {
    let header = "-----BEGIN OPENSSH PRIVATE KEY-----";
    let end = "-----END OPENSSH PRIVATE KEY-----";
    let step = step(vec![header.into(), "private key body".into(), end.into()]);
    let masked = step.remasked(&redactor(None)).unwrap();
    assert_eq!(masked.output, [header, "***", end]);
    assert_eq!(
        masked.remasked(&redactor(None)).unwrap().output,
        masked.output
    );
}

#[test]
fn existing_exact_agent_chat_remasks_history_and_keeps_composer_without_rebinding() {
    const ORIGIN: &str = "03684aee2b4c4bcfbe6dcfe70e0d025d";
    const LEARNED: &str = "fixture-sudo-phrase";
    let config: Arc<Configuration> = Arc::new(serde_json::from_value(serde_json::json!({
        "profiles": {
            "profiles": [{"id": 7, "name": "Fixture", "endpoint": "127.0.0.1:9", "model": "test"}],
            "default": 7,
            "last_id": 7
        }
    })).unwrap());
    let weak = redactor(None);
    let mut chat = Chat::new(
        &config,
        Action::Ask,
        "",
        weak.clone(),
        Some(&ProfileRef::Numeric(7)),
    )
    .unwrap();
    chat.agent_run_id = Some(ORIGIN.into());
    chat.agent_source_exact = true;
    chat.turns = vec![
        Arc::new(
            Turn::from_agent(
                "Check the fixture",
                "The fixture is ready.",
                &[step(vec![LEARNED.into()])],
                &weak,
            )
            .unwrap(),
        ),
        Arc::new(
            Turn::from_parts(None, "What was printed?".into(), LEARNED.into())
                .with_reasoning(Some("Keep ordinary reasoning.".into())),
        ),
    ];
    chat.question = text_editor::Content::with_text(&format!("Explain {LEARNED} in context."));
    chat.review = Some(chat.make_review(&config).unwrap());
    let mut workspace = Workspace::default();
    let id = window::Id::unique();
    workspace.terminal_chats.insert(id, chat);

    workspace
        .apply_agent_chat_mask(ORIGIN, &redactor(Some(LEARNED)))
        .unwrap();
    let chat = &workspace.terminal_chats[&id];
    assert!(chat.agent_has_learned_sudo && !chat.agent_mask_unavailable);
    assert!(chat.review.is_none() && chat.pending.is_none());
    assert_eq!(chat.question.text().trim_end(), "Explain *** in context.");
    assert_eq!(chat.turns[0].agent_steps()[0].output, ["***"]);
    assert_eq!(chat.turns[1].answer(), "***");
    assert_eq!(chat.turns[1].reasoning(), Some("Keep ordinary reasoning."));
    assert!(chat.source_pane.is_none() && chat.source_stream.is_none());
    let previous_mask = Arc::clone(&chat.redactor);
    workspace
        .apply_agent_chat_mask(ORIGIN, &redactor(Some(LEARNED)))
        .unwrap();
    assert!(Arc::ptr_eq(
        &previous_mask,
        &workspace.terminal_chats[&id].redactor
    ));
}
