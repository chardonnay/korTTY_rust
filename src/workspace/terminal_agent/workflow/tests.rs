use std::io::Write as _;

use iced::futures::{StreamExt as _, executor::block_on};

use super::super::{Command, Elevation, Outcome, Phase, engine, flow::tests::Fixture};
use super::*;
use crate::local_ai::test_support::{accept, exclusive_run, fixture_response, listener, request};

fn send(fixture: &mut Fixture, id: window::Id, message: Message) -> Task<Desktop> {
    fixture.send(id, RunMessage::Workflow(message))
}

fn no_task(task: Task<Desktop>) {
    assert!(iced_runtime::task::into_stream(task).is_none());
}

fn reply(task: Task<Desktop>, id: window::Id) -> Message {
    let mut stream = iced_runtime::task::into_stream(task).expect("workflow reply task");
    while let Some(action) = block_on(stream.next()) {
        if let iced_runtime::Action::Output(Desktop::TerminalAgent(
            actual,
            RunMessage::Workflow(message @ Message::Reply(..)),
        )) = action
        {
            assert_eq!(actual, id);
            return message;
        }
    }
    panic!("missing workflow reply");
}

#[test]
fn private_workflow_sends_only_reviewed_commands_and_keeps_editing_scoped() {
    let _exclusive = exclusive_run();
    let (listener, _) = listener();
    let mut fixture = Fixture::with_endpoint_and_preset(
        false,
        true,
        &listener.local_addr().unwrap().to_string(),
        "qwen",
    );
    let run_id = fixture.open(Mode::Run);
    let run = fixture.run_mut(run_id);
    run.goal = "Inspect the worker password=fixture-goal-secret".into();
    run.activity.push_back("Unreviewed conversation".into());
    run.epoch = 3;
    run.final_evidence.start(
        run.epoch,
        &Command {
            command: "printf observation".into(),
            purpose: "Read worker status".into(),
            risk: engine::Risk::ReadOnly,
            elevation: Elevation::None,
        },
    );
    run.final_evidence.record_output(
        run.epoch,
        Phase::Running,
        &Outcome {
            status: engine::Status::Exited(7),
            stdout: Default::default(),
            stderr: Default::default(),
        },
        "Worker is stopped\nAPI_KEY=fixture-output-secret".into(),
        String::new(),
    );
    run.phase = Phase::Finished;
    let epoch = run.epoch;

    no_task(send(&mut fixture, run_id, Message::Open(epoch - 1)));
    assert!(fixture.workspace.terminal_agents.workflows.is_empty());
    drop(send(&mut fixture, run_id, Message::Open(epoch)));
    let id = fixture
        .workspace
        .terminal_agents
        .workflows
        .keys()
        .next()
        .copied()
        .unwrap_or_else(|| {
            panic!(
                "could not open workflow: {}",
                fixture.run_mut(run_id).notice
            )
        });
    no_task(send(&mut fixture, id, Message::Header(HeaderChoice::None)));
    no_task(send(&mut fixture, id, Message::Suggestions(2)));
    let saved_hardening = fixture.workspace.window_settings.snippet_hardening.clone();
    let saved_single_language = fixture.workspace.window_settings.workflow_single_language;
    let saved_font = fixture.workspace.window_settings.workflow_script_font;
    no_task(send(&mut fixture, id, Message::AllHardening(false)));
    no_task(send(&mut fixture, id, Message::SaveHardening));
    no_task(send(&mut fixture, id, Message::GuardEnabled(true)));
    no_task(send(&mut fixture, id, Message::AllGuards(false)));
    no_task(send(&mut fixture, id, Message::SaveGuards));
    no_task(send(&mut fixture, id, Message::SingleLanguage(true)));
    assert_eq!(
        fixture.workspace.window_settings.snippet_hardening,
        saved_hardening
    );
    assert_eq!(
        fixture.workspace.window_settings.workflow_single_language,
        saved_single_language
    );
    no_task(send(
        &mut fixture,
        id,
        Message::Instructions("Report the status API_KEY=fixture-instructions-secret".into()),
    ));
    no_task(send(&mut fixture, id, Message::Review));
    let dialog = &fixture.workspace.terminal_agents.workflows[&id];
    assert!(dialog.private);
    let Stage::Review(review) = &dialog.stage else {
        panic!("could not review workflow: {}", dialog.notice);
    };
    assert_eq!(review.requests.len(), 2);
    assert_eq!(review.requests[0].suggestion, 1);
    assert_eq!(review.requests[1].suggestion, 2);
    let reviewed = &review.requests[0];
    let input: Value = serde_json::from_str(&reviewed.input).unwrap();
    assert_eq!(input["goal"], "Inspect the worker password=***");
    assert_eq!(
        input["executed_commands"][0]["command"],
        "printf observation"
    );
    assert_eq!(input["executed_commands"][0]["exitStatus"], 7);
    assert!(
        input["executed_commands"][0]["stdoutTail"]
            .as_str()
            .unwrap()
            .contains("Worker is stopped")
    );
    assert!(
        input["extra_instructions"]
            .as_str()
            .unwrap()
            .contains("Report the status")
    );
    assert!(!reviewed.input.contains("fixture-"));
    assert!(!reviewed.input.contains("Unreviewed conversation"));
    assert!(reviewed.header.text().is_none());
    assert!(reviewed.system.contains("Use only the selected language"));
    assert!(!reviewed.system.contains("strict"));
    assert_eq!(reviewed.system.matches("<kortty_model_preset>").count(), 1);
    assert!(reviewed.system.contains("Do not emit <think>"));
    let messages: Vec<_> = review
        .requests
        .iter()
        .map(|request| {
            let input: Value = serde_json::from_str(&request.input).unwrap();
            let instructions = input["extra_instructions"].as_str().unwrap();
            assert!(instructions.contains(&format!(
                "Provide alternative approach {} of 2",
                request.suggestion
            )));
            assert!(!request.input.contains("fixture-"));
            json!([
                {"role":"system", "content":request.system},
                {"role":"user", "content":request.input},
            ])
        })
        .collect();
    assert_ne!(messages[0], messages[1]);
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );

    let server = std::thread::spawn(move || {
        for (content, messages) in [
            "#!/usr/bin/env bash\n# API_KEY=fixture-draft-secret\nprintf '%s\\n' stopped",
            "```bash\n#!/usr/bin/env bash\nprintf '%s\\n' stopped\n```",
        ]
        .into_iter()
        .zip(messages)
        {
            let mut socket = accept(&listener);
            let body = request(&mut socket);
            assert_eq!(body["messages"], messages);
            assert!(body.get("response_format").is_none());
            socket
                .get_mut()
                .write_all(fixture_response("200 OK", "", content).as_bytes())
                .unwrap();
        }
    });
    let first = reply(send(&mut fixture, id, Message::Generate), id);
    let second_task = send(&mut fixture, id, first.clone());
    no_task(send(&mut fixture, id, first));
    let second = reply(second_task, id);
    no_task(send(&mut fixture, id, second.clone()));
    server.join().unwrap();

    let dialog = &fixture.workspace.terminal_agents.workflows[&id];
    let Stage::Results(results) = &dialog.stage else {
        panic!("missing workflow drafts: {}", dialog.notice);
    };
    assert_eq!(results.items.len(), 2);
    assert!(results.items[0].content.is_none());
    assert!(
        results.items[0]
            .error
            .as_deref()
            .unwrap()
            .contains("requires masking")
    );
    assert_eq!(results.selected, 1);
    let code = results.items[1].content.as_ref().unwrap().text();
    assert!(code.starts_with("#!/usr/bin/env bash"));
    assert!(code.contains("printf"));
    assert!(!code.contains("```"));
    no_task(send(&mut fixture, id, Message::ScriptFont(20)));
    assert_eq!(
        fixture.workspace.terminal_agents.workflows[&id]
            .options
            .script_font,
        20
    );
    assert_eq!(
        fixture.workspace.window_settings.workflow_script_font,
        saved_font
    );
    no_task(send(&mut fixture, id, Message::OpenSnippet));
    no_task(send(&mut fixture, id, Message::Diagram));

    no_task(send(
        &mut fixture,
        id,
        Message::Edit(text_editor::Action::Edit(text_editor::Edit::Paste(
            Arc::new("# API_KEY=fixture-edit-secret\n".into()),
        ))),
    ));
    no_task(send(&mut fixture, id, Message::Copy));
    assert!(
        fixture.workspace.terminal_agents.workflows[&id]
            .notice
            .contains("sensitive text")
    );

    fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap()
        .stream_generation += 1;
    drop(fixture.workspace.reconcile_agent_workflows());
    assert!(fixture.workspace.terminal_agents.workflows.is_empty());
    no_task(send(&mut fixture, id, second));
    assert!(fixture.workspace.terminal_agents.workflows.is_empty());
}

#[test]
fn reviewed_workflow_header_is_applied_after_the_wire_request() {
    let _exclusive = exclusive_run();
    let (listener, transport) = listener();
    let language = Language::Bash;
    let options = Options::new(language, &WindowSettings::default());
    let header = Header::custom(language, "# Local workflow header fixture").unwrap();
    let system = options.system(language, &header).unwrap();
    let input = "Reviewed command evidence".to_owned();
    let messages = json!([
        {"role":"system", "content":system},
        {"role":"user", "content":input},
    ]);
    let server = std::thread::spawn(move || {
        let mut socket = accept(&listener);
        let body = request(&mut socket);
        assert_eq!(body["messages"], messages);
        assert!(!body.to_string().contains("Local workflow header fixture"));
        socket
            .get_mut()
            .write_all(fixture_response("200 OK", "", "#!/usr/bin/env bash\necho ready").as_bytes())
            .unwrap();
    });
    let mut dialog = Dialog {
        run: window::Id::unique(),
        run_epoch: 1,
        generation: 1,
        goal: "Check worker".into(),
        profile_name: "Fixture".into(),
        source_shell: "Bash".into(),
        choices: vec![],
        options,
        review_selected: 0,
        diagram: diagram::State::default(),
        stage: Stage::Running(Pending {
            review: Arc::new(Review {
                requests: vec![Request {
                    language,
                    suggestion: 1,
                    system,
                    input,
                    guidance: SkillGuidance::default(),
                    header,
                }],
                headers: vec![],
                model: Model {
                    name: "Fixture".into(),
                    destination: String::new(),
                    transport,
                    limit: 4096,
                },
                destination: String::new(),
                goal: "Check worker".into(),
                policy: Policy::default(),
            }),
            completed: 0,
            total: 1,
            items: vec![],
            cancel: None,
            skill_usage: None,
        }),
        notice: String::new(),
        private: true,
    };
    let id = window::Id::unique();
    let Message::Reply(1, 0, result) =
        reply(dialog.send_next(id, &Configuration::default()).unwrap(), id)
    else {
        panic!("workflow reply did not preserve its generation and index");
    };
    server.join().unwrap();
    assert_eq!(
        result.as_ref().as_ref().unwrap().code(),
        "#!/usr/bin/env bash\n# Local workflow header fixture\necho ready",
    );
}

#[test]
fn workflow_loads_shared_preferences_and_keeps_optional_rules_conditional() {
    let settings = WindowSettings {
        snippet_hardening: Preferences {
            classic: vec![HardeningOption::HelpUsage],
            input_enabled: true,
            input: vec![InputOption::FileSizeLimit],
            max_file_size_mib: 7,
        },
        workflow_single_language: true,
        workflow_script_font: 18,
        ..WindowSettings::default()
    };
    let mut options = Options::new(Language::Bash, &settings);
    assert_eq!(options.script_font, 18);
    let bash = options.system(Language::Bash, &Header::NONE).unwrap();
    assert!(bash.contains("Accept --help"));
    assert!(bash.contains("7340032"));
    assert!(bash.contains("Use only the selected language"));
    assert!(!bash.contains("strict"));
    assert!(!bash.contains("cleanup"));

    let ansible = options.system(Language::Ansible, &Header::NONE).unwrap();
    assert!(ansible.contains("--extra-vars"));
    assert!(!ansible.contains("MAX_FILE_SIZE"));

    options.max_file_mib = "1025".into();
    assert!(options.system(Language::Bash, &Header::NONE).is_err());
    options.language = Language::Ansible;
    let additional = options.system(Language::Bash, &Header::NONE).unwrap();
    assert!(!additional.contains("MAX_FILE_SIZE"));
}
