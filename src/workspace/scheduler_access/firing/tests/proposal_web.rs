use super::*;
use crate::workspace::terminal_ai::web;

const ROOT: &str = "KORTTY_SCHEDULER_WEB_TEST_ROOT";
const TEST: &str = "workspace::scheduler_access::firing::tests::proposal_web::scheduled_web_review_saves_exact_masked_evidence_and_reuses_it_without_another_search";
const FRESH_ROOT: &str = "KORTTY_SCHEDULER_FRESH_WEB_TEST_ROOT";
const FRESH_TEST: &str = "workspace::scheduler_access::firing::tests::proposal_web::fresh_scheduled_web_evidence_is_reviewed_and_bound_to_one_firing";
const UNATTENDED_ROOT: &str = "KORTTY_SCHEDULER_UNATTENDED_WEB_TEST_ROOT";
const UNATTENDED_TEST: &str = "workspace::scheduler_access::firing::tests::proposal_web::unattended_scheduled_web_search_is_bound_to_reviewed_topic_and_run";
const RESEARCH_ROOT: &str = "KORTTY_SCHEDULER_BOUNDED_WEB_RESEARCH_ROOT";
const RESEARCH_TEST: &str = "workspace::scheduler_access::firing::tests::proposal_web::unattended_research_reads_only_a_returned_page_and_keeps_its_evidence";
const TOPIC: &str = "Public HTTP status code documentation";
const QUERY: &str = "RFC HTTP status code registry";
const GOAL: &str = "Inspect the local deployment";
const PUBLIC_NOTE: &str = "Public deployments use a versioned manifest.";
const VAULT_NOTE: &str = "scheduler-web-unrelated-vault-value";

#[test]
fn scheduled_web_review_saves_exact_masked_evidence_and_reuses_it_without_another_search() {
    let Some(root) = std::env::var_os(ROOT).map(PathBuf::from) else {
        let scratch = Profile::new();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(ROOT, scratch.0.canonicalize().unwrap().join("web-profile"))
            .env("TAVILY_API_KEY", "scheduler-web-provider-fixture-key")
            .output()
            .unwrap();
        let output = String::from_utf8_lossy(&child.stdout);
        assert!(
            child.status.success() && output.contains("1 passed"),
            "{output}\n{}",
            String::from_utf8_lossy(&child.stderr)
        );
        return;
    };
    std::fs::create_dir(&root).unwrap();
    profile::set_runtime_root(root.clone());
    let (model, _) = test_support::listener();
    let (ssh, _) = test_support::listener();
    let config = json!({"profiles": {"profiles": [{
        "id": 1, "name": "Local LM Studio", "model": "fixture:local",
        "connection_mode": "lm-studio", "trusted_local_endpoint": true,
        "endpoint": model.local_addr().unwrap().to_string(), "lm_web": {"mode": "tavily"}
    }], "default": 1, "last_id": 1}});
    let settings: Configuration = serde_json::from_value(config.clone()).unwrap();
    settings
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let (reference, vault) = saved_target(ssh.local_addr().unwrap().port());
    drop(vault);
    let vault_path = profile::vault_path().unwrap();
    let mut vault = Vault::unlock(&vault_path, b"scheduled-agent-fixture-master").unwrap();
    vault
        .save(
            CredentialInput {
                label: "Unrelated fixture credential".into(),
                username: "fixture".into(),
                server_pattern: "example.invalid".into(),
                ..Default::default()
            },
            Secret::new(VAULT_NOTE.into()),
        )
        .unwrap();
    let vault = VaultWindow::with_unlocked_fixture(vault_path, vault);
    let target = reference.id;
    let job_id = JobId::new("web-review").unwrap();
    let jobs = profile::jobs_path().unwrap();
    let action: SshCommand = serde_json::from_value(json!({
        "connection":reference, "working_directory":"/srv",
        "agent":{"kind":"agent","probe":"per_firing","profile":1,"goal":GOAL}
    }))
    .unwrap();
    SchedulerStore::open(&jobs)
        .unwrap()
        .commit_with_actions(
            &JobGraph::new(vec![
                Job::new(job_id.clone(), "Search and inspect", action).unwrap(),
            ])
            .unwrap(),
            &[],
            &[],
        )
        .unwrap();
    let mut scheduler = SchedulerWindow::open(jobs.clone()).unwrap();
    scheduler.set_execution_allowed(true);
    let view = scheduler.new_view();
    let id = window::Id::unique();
    let mut workspace = Workspace {
        scheduler: Some(scheduler),
        vault: Some(vault),
        ..Workspace::default()
    };
    workspace.scheduler_views.insert(id, view);
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let _ = workspace.update_scheduler(id, SchedulerMessage::Page(Page::Action));
    let _ = workspace.update_scheduler(id, SchedulerMessage::ReviewAgent);
    let original = workspace.scheduler_views[&id]
        .firing_review_ticket()
        .unwrap()
        .to_owned();
    {
        let mut search = |message| {
            workspace.update_scheduler(id, SchedulerMessage::ProposalWeb(original.clone(), message))
        };
        let _ = search(web::Message::Open);
        let _ = search(web::Message::Edit(format!(
            "Find deployment notes for {PASSWORD} and {VAULT_NOTE}"
        )));
        assert_eq!(search(web::Message::Send).units(), 0);
        assert_no_connection(&model);
        let _ = search(web::Message::Review);
    }
    assert!(
        workspace.scheduler_views[&id]
            .firing_review_ticket()
            .is_none()
    );
    let _ =
        workspace.update_scheduler(id, SchedulerMessage::ApproveFiringRequest(original.clone()));
    let task = workspace.update_scheduler(
        id,
        SchedulerMessage::ProposalWeb(original.clone(), web::Message::Send),
    );
    let mut socket = test_support::accept(&model);
    let search_wire: serde_json::Value = serde_json::from_slice(&test_support::wire_request(
        &mut socket,
        "POST /api/v1/chat HTTP/1.1",
    ))
    .unwrap();
    assert_eq!(
        search_wire["input"],
        "Find deployment notes for *** and ***"
    );
    assert_eq!(
        search_wire["integrations"][0]["allowed_tools"],
        json!(["tavily-search"])
    );
    assert!(!search_wire.to_string().contains(GOAL));
    socket
        .get_mut()
        .write_all(
            test_support::fixture_http_response(
                "200 OK",
                "",
                &json!({
                    "output":[{"type":"tool_call","tool":"tavily-search",
                        "provider_info":{"type":"ephemeral_mcp","server_label":"tavily"},
                        "output":format!("{PUBLIC_NOTE} Returned {PASSWORD} and {VAULT_NOTE}.")}]
                })
                .to_string(),
            )
            .as_bytes(),
        )
        .unwrap();
    drop(socket);
    let mut actions = iced_runtime::task::into_stream(task).unwrap();
    let Some(Action::Output(Message::Scheduler(window, finished))) = block_on(actions.next())
    else {
        panic!("Web search should return to the Scheduler review");
    };
    assert_eq!(window, id);
    let _ = workspace.update_scheduler(id, finished);
    assert!(
        workspace.scheduler_views[&id]
            .firing_review_ticket()
            .is_none()
    );
    assert_no_connection(&model);
    assert_no_connection(&ssh);

    let _ = workspace.update_scheduler(
        id,
        SchedulerMessage::ProposalWeb(original.clone(), web::Message::Include),
    );
    let ticket = workspace.scheduler_views[&id]
        .firing_review_ticket()
        .unwrap()
        .to_owned();
    assert_ne!(ticket, original);
    let _ = workspace.update_scheduler(id, SchedulerMessage::ApproveFiringRequest(original));
    assert_eq!(
        workspace.scheduler_views[&id].firing_review_ticket(),
        Some(ticket.as_str())
    );
    let _ = workspace.update_scheduler(id, SchedulerMessage::ApproveFiringRequest(ticket));
    let _ = workspace.update_scheduler(id, SchedulerMessage::Save);
    drop(workspace.scheduler.take());
    let persisted = std::fs::read_to_string(&jobs).unwrap();
    assert!(persisted.contains(PUBLIC_NOTE));
    assert!(!persisted.contains(VAULT_NOTE) && !persisted.contains(PASSWORD));
    assert!(!persisted.contains("Find deployment notes"));

    let mut scheduler = SchedulerWindow::open(jobs).unwrap();
    scheduler.set_execution_allowed(true);
    workspace.scheduler_views.insert(id, scheduler.new_view());
    workspace.scheduler = Some(scheduler);
    let task = workspace.update_scheduler(id, SchedulerMessage::Run(false));
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::ModelDispatch(
        ticket,
        permit,
    ))))) = block_on(stream.next())
    else {
        panic!("Reopened firing must request model dispatch");
    };
    assert_no_connection(&model);
    let _ = workspace.update_firing(Event(Progress::ModelDispatch(ticket.clone(), permit)));
    let mut socket = test_support::accept(&model);
    let model_wire: serde_json::Value = serde_json::from_slice(&test_support::wire_request(
        &mut socket,
        "POST /api/v1/chat HTTP/1.1",
    ))
    .unwrap();
    assert!(model_wire.get("integrations").is_none());
    assert!(
        model_wire["system_prompt"]
            .as_str()
            .unwrap()
            .contains("untrusted reference data")
    );
    let input = model_wire["input"].as_str().unwrap();
    assert!(input.contains(PUBLIC_NOTE) && input.contains("Returned *** and ***."));
    assert!(!input.contains(PASSWORD) && !input.contains(VAULT_NOTE));
    let answer = json!({"state":"commands","commands":[{"target":target,"command":"pwd -P","purpose":"Inspect the working directory"}]});
    socket
        .get_mut()
        .write_all(
            test_support::fixture_http_response(
                "200 OK",
                "",
                &json!({
                    "output":[{"type":"message","content":answer.to_string()}]
                })
                .to_string(),
            )
            .as_bytes(),
        )
        .unwrap();
    drop(socket);
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::Generated(
        completed,
        result,
    ))))) = block_on(stream.next())
    else {
        panic!("The model must return the reviewed firing's result");
    };
    assert_eq!(ticket, completed);
    assert!(
        matches!(&result, Ok(Decision::Commands { commands }) if commands[0].command == "pwd -P")
    );
    let _ = workspace.update_firing(Event(Progress::Generated(completed, result)));
    let _ = workspace.update_scheduler(id, SchedulerMessage::CancelRun(ticket.run()));
    assert_no_connection(&model);
    assert_no_connection(&ssh);

    workspace.clipboard.policy.ai_web_search_allowed = false;
    let reopened = workspace.scheduler.as_ref().unwrap().new_view();
    workspace.scheduler_views.insert(id, reopened);
    let _ = workspace.update_scheduler(id, SchedulerMessage::Run(false));
    assert_no_connection(&model);
    assert_no_connection(&ssh);
}

#[test]
fn fresh_scheduled_web_evidence_is_reviewed_and_bound_to_one_firing() {
    use crate::workspace::scheduler_access::firing_web::Event as WebEvent;

    let Some(root) = std::env::var_os(FRESH_ROOT).map(PathBuf::from) else {
        let scratch = Profile::new();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", FRESH_TEST, "--nocapture"])
            .env(
                FRESH_ROOT,
                scratch.0.canonicalize().unwrap().join("fresh-web-profile"),
            )
            .env("TAVILY_API_KEY", "scheduler-web-provider-fixture-key")
            .output()
            .unwrap();
        let output = String::from_utf8_lossy(&child.stdout);
        assert!(
            child.status.success() && output.contains("1 passed"),
            "{output}\n{}",
            String::from_utf8_lossy(&child.stderr)
        );
        return;
    };
    std::fs::create_dir(&root).unwrap();
    profile::set_runtime_root(root.clone());
    let (model, _) = test_support::listener();
    let (ssh, _) = test_support::listener();
    let config: Configuration = serde_json::from_value(json!({"profiles": {"profiles": [{
        "id": 1, "name": "Local LM Studio", "model": "fixture:local",
        "connection_mode": "lm-studio", "trusted_local_endpoint": true,
        "endpoint": model.local_addr().unwrap().to_string(), "lm_web": {"mode": "tavily"}
    }], "default": 1, "last_id": 1}}))
    .unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let (reference, vault) = saved_target(ssh.local_addr().unwrap().port());
    let target = reference.id;
    let job_id = JobId::new("fresh-web-review").unwrap();
    let jobs = profile::jobs_path().unwrap();
    let action: SshCommand = serde_json::from_value(json!({
        "connection":reference, "working_directory":"/srv",
        "agent":{"kind":"agent","probe":"per_firing","profile":1,"goal":GOAL}
    }))
    .unwrap();
    SchedulerStore::open(&jobs)
        .unwrap()
        .commit_with_actions(
            &JobGraph::new(vec![
                Job::new(job_id.clone(), "Review fresh web", action).unwrap(),
            ])
            .unwrap(),
            &[],
            &[],
        )
        .unwrap();
    let mut scheduler = SchedulerWindow::open(jobs.clone()).unwrap();
    scheduler.set_execution_allowed(true);
    let id = window::Id::unique();
    let view = scheduler.new_view();
    let mut workspace = Workspace {
        scheduler: Some(scheduler),
        vault: Some(vault),
        ..Workspace::default()
    };
    workspace.scheduler_views.insert(id, view);
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let _ = workspace.update_scheduler(id, SchedulerMessage::Page(Page::Action));
    let _ = workspace.update_scheduler(id, SchedulerMessage::ReviewAgent);
    let ticket = workspace.scheduler_views[&id]
        .firing_review_ticket()
        .unwrap()
        .to_owned();
    let _ = workspace.update_scheduler(id, SchedulerMessage::RequireFreshWeb(ticket, true));
    let ticket = workspace.scheduler_views[&id]
        .firing_review_ticket()
        .unwrap()
        .to_owned();
    let _ = workspace.update_scheduler(id, SchedulerMessage::ApproveFiringRequest(ticket));
    let _ = workspace.update_scheduler(id, SchedulerMessage::Save);
    assert!(
        std::fs::read_to_string(&jobs)
            .unwrap()
            .contains("\"fresh_web\":true")
    );
    drop(workspace.scheduler.take());
    let mut scheduler = SchedulerWindow::open(jobs.clone()).unwrap();
    scheduler.set_execution_allowed(true);
    workspace.scheduler_views.insert(id, scheduler.new_view());
    workspace.scheduler = Some(scheduler);

    let start = |workspace: &mut Workspace| {
        let _ = workspace.update_scheduler(id, SchedulerMessage::Run(false));
        let run = workspace.scheduler_queue.back().unwrap().id;
        assert!(workspace.scheduler_firing_web.waiting(run));
        run
    };
    let edit = |workspace: &mut Workspace, run, message| {
        let revision = workspace.scheduler_firing_web.review_revision(run).unwrap();
        workspace.update_firing_web(WebEvent::View(run, revision, message))
    };
    let lookup = |workspace: &mut Workspace, run, note: &str| {
        let _ = edit(
            workspace,
            run,
            web::Message::Edit(format!("Find public notes {PASSWORD}")),
        );
        let _ = edit(workspace, run, web::Message::Send);
        assert_no_connection(&model);
        let _ = edit(workspace, run, web::Message::Review);
        let task = edit(workspace, run, web::Message::Send);
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        let Some(Action::Output(Message::ScheduledFiringWeb(WebEvent::Authorize(
            current,
            ticket,
            permit,
        )))) = block_on(stream.next())
        else {
            panic!("The fresh search must wait for its own dispatch permit");
        };
        assert_eq!(current, run);
        assert_no_connection(&model);
        let _ = workspace.update_firing_web(WebEvent::Authorize(current, ticket, permit));
        let mut socket = test_support::accept(&model);
        let payload: serde_json::Value = serde_json::from_slice(&test_support::wire_request(
            &mut socket,
            "POST /api/v1/chat HTTP/1.1",
        ))
        .unwrap();
        assert_eq!(payload["input"], "Find public notes ***");
        assert_eq!(
            payload["integrations"][0]["allowed_tools"],
            json!(["tavily-search"])
        );
        assert!(!payload.to_string().contains(GOAL));
        socket.get_mut().write_all(test_support::fixture_http_response("200 OK", "", &json!({
            "output":[{"type":"tool_call","tool":"tavily-search", "provider_info":{"type":"ephemeral_mcp","server_label":"tavily"},
                       "output":format!("{note} Returned {PASSWORD}.")}]
        }).to_string()).as_bytes()).unwrap();
        drop(socket);
        let Some(Action::Output(Message::ScheduledFiringWeb(WebEvent::Authorize(
            current,
            ticket,
            permit,
        )))) = block_on(stream.next())
        else {
            panic!("The fresh search must recheck permission before delivering its results");
        };
        assert_eq!(current, run);
        let _ = workspace.update_firing_web(WebEvent::Authorize(current, ticket, permit));
        let Some(Action::Output(Message::ScheduledFiringWeb(WebEvent::Worker(current, finished)))) =
            block_on(stream.next())
        else {
            panic!("The fresh search must return results to its run");
        };
        assert_eq!(current, run);
        assert_no_connection(&model);
        assert_no_connection(&ssh);
        finished
    };

    let expired = start(&mut workspace);
    let old_reply = lookup(&mut workspace, expired, "Old expired excerpt.");
    workspace.scheduler_firing_web.expire_fixture(expired);
    let _ = workspace.expire_pending_firing_web();
    assert!(!workspace.scheduler_firing_web.contains(expired));
    let run = start(&mut workspace);
    assert_ne!(run, expired);
    let intermediate = lookup(&mut workspace, run, "Unselected excerpt.");
    let _ = workspace.update_firing_web(WebEvent::Worker(expired, old_reply));
    let _ = edit(&mut workspace, run, web::Message::Include);
    assert!(workspace.scheduler_firing_web.waiting(run));
    let _ = workspace.update_firing_web(WebEvent::Worker(run, intermediate));
    let old_revision = workspace.scheduler_firing_web.review_revision(run).unwrap();
    let outdated_include = WebEvent::View(run, old_revision, web::Message::Include);
    let _ = edit(&mut workspace, run, web::Message::Close);
    let _ = edit(&mut workspace, run, web::Message::Open);
    let finished = lookup(&mut workspace, run, "Fresh second excerpt.");
    let _ = workspace.update_firing_web(WebEvent::Worker(run, finished));
    let _ = workspace.update_firing_web(outdated_include);
    assert!(workspace.scheduler_firing_web.waiting(run));
    assert_no_connection(&model);
    let task = edit(&mut workspace, run, web::Message::Include);
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::ModelDispatch(
        ticket,
        permit,
    ))))) = block_on(stream.next())
    else {
        panic!("Including excerpts must start the separately guarded model request");
    };
    assert_eq!(ticket.run(), run);
    assert_no_connection(&model);
    let _ = workspace.update_firing(Event(Progress::ModelDispatch(ticket.clone(), permit)));
    let mut socket = test_support::accept(&model);
    let model_wire: serde_json::Value = serde_json::from_slice(&test_support::wire_request(
        &mut socket,
        "POST /api/v1/chat HTTP/1.1",
    ))
    .unwrap();
    assert!(model_wire.get("integrations").is_none());
    assert!(
        model_wire["system_prompt"]
            .as_str()
            .unwrap()
            .contains("untrusted reference data")
    );
    let input = model_wire["input"].as_str().unwrap();
    assert!(input.contains("Fresh second excerpt. Returned ***."));
    assert!(
        !input.contains("Old expired excerpt")
            && !input.contains("Unselected excerpt")
            && !input.contains(PASSWORD)
    );
    let answer = json!({"state":"commands","commands":[{"target":target,"command":"pwd -P","purpose":"Inspect the working directory"}]});
    socket
        .get_mut()
        .write_all(
            test_support::fixture_http_response(
                "200 OK",
                "",
                &json!({
                    "output":[{"type":"message","content":answer.to_string()}]
                })
                .to_string(),
            )
            .as_bytes(),
        )
        .unwrap();
    drop(socket);
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::Generated(
        completed,
        result,
    ))))) = block_on(stream.next())
    else {
        panic!("The fresh firing should return its model proposal");
    };
    assert_eq!(ticket, completed);
    assert!(
        matches!(&result, Ok(Decision::Commands { commands }) if commands[0].command == "pwd -P")
    );
    let _ = workspace.update_firing(Event(Progress::Generated(completed, result)));
    workspace
        .scheduler
        .as_ref()
        .unwrap()
        .cancel_firing_web_run(ticket.run());
    let _ = workspace.expire_firing();
    assert_no_connection(&model);
    assert_no_connection(&ssh);
    let persisted = std::fs::read_to_string(&jobs).unwrap();
    assert!(
        !persisted.contains("Fresh second excerpt") && !persisted.contains("Old expired excerpt")
    );
}

#[test]
fn unattended_scheduled_web_search_is_bound_to_reviewed_topic_and_run() {
    use crate::workspace::scheduler_access::firing_web::Event as WebEvent;

    let Some(root) = std::env::var_os(UNATTENDED_ROOT).map(PathBuf::from) else {
        let scratch = Profile::new();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", UNATTENDED_TEST, "--nocapture"])
            .env(
                UNATTENDED_ROOT,
                scratch
                    .0
                    .canonicalize()
                    .unwrap()
                    .join("unattended-web-profile"),
            )
            .env("TAVILY_API_KEY", "scheduler-unattended-web-fixture-key")
            .output()
            .unwrap();
        let output = String::from_utf8_lossy(&child.stdout);
        assert!(
            child.status.success() && output.contains("1 passed"),
            "{output}\n{}",
            String::from_utf8_lossy(&child.stderr)
        );
        return;
    };
    let (model, _) = test_support::listener();
    let (ssh, _) = test_support::listener();
    let (mut workspace, id, target, jobs) =
        unattended_workspace(&root, &model, &ssh, json!({"mode":"tavily"}), "");
    let review_ticket = |workspace: &Workspace| {
        workspace.scheduler_views[&id]
            .firing_review_ticket()
            .unwrap()
            .to_owned()
    };
    let old = review_ticket(&workspace);
    for topic in [PASSWORD, TOPIC] {
        let _ = workspace.update_scheduler(
            id,
            SchedulerMessage::EditUnattendedWebTopic(old.clone(), topic.into()),
        );
        let _ = workspace.update_scheduler(
            id,
            SchedulerMessage::ReviewUnattendedWeb(old.clone(), topic.into()),
        );
        if topic == PASSWORD {
            assert_eq!(review_ticket(&workspace), old);
        }
    }
    let current = review_ticket(&workspace);
    assert_ne!(current, old);
    let _ = workspace.update_scheduler(id, SchedulerMessage::ApproveFiringRequest(old));
    assert!(
        workspace.scheduler_views[&id]
            .new_unattended_web_lookup()
            .is_none()
    );
    assert!(!std::fs::read_to_string(&jobs).unwrap().contains(TOPIC));
    let _ = workspace.update_scheduler(id, SchedulerMessage::ApproveFiringRequest(current));
    let _ = workspace.update_scheduler(id, SchedulerMessage::Save);
    assert!(std::fs::read_to_string(&jobs).unwrap().contains(TOPIC));
    assert_no_connection(&model);

    let authorize = |workspace: &mut Workspace, message| {
        let Some(Action::Output(Message::ScheduledFiringWeb(WebEvent::Authorize(
            run,
            ticket,
            permit,
        )))) = message
        else {
            panic!("The unattended lookup must wait for a dispatch permit: {message:?}");
        };
        let _ = workspace.update_firing_web(WebEvent::Authorize(run, ticket, permit));
        (run, ticket)
    };
    let planner = |workspace: &mut Workspace, query: &str| {
        let task = workspace.update_scheduler(id, SchedulerMessage::Run(false));
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        assert_no_connection(&model);
        let (run, ticket) = authorize(workspace, block_on(stream.next()));
        let mut socket = test_support::accept(&model);
        let payload: serde_json::Value = serde_json::from_slice(&test_support::wire_request(
            &mut socket,
            "POST /api/v1/chat HTTP/1.1",
        ))
        .unwrap();
        assert!(payload.get("integrations").is_none());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(payload["input"].as_str().unwrap()).unwrap(),
            json!({"public_research_topic":TOPIC})
        );
        assert!(!payload.to_string().contains(PASSWORD) && !payload.to_string().contains(GOAL));
        write_lm_message(&mut socket, &json!({"query":query}).to_string());
        drop(socket);
        assert_eq!(authorize(workspace, block_on(stream.next())), (run, ticket));
        let Some(Action::Output(Message::ScheduledFiringWeb(WebEvent::UnattendedQuery(
            completed_run,
            completed_ticket,
            result,
        )))) = block_on(stream.next())
        else {
            panic!("The guarded planner must return its automatic query");
        };
        assert_eq!((completed_run, completed_ticket), (run, ticket));
        assert_eq!(result.as_ref().map(String::as_str), Ok(query));
        assert_no_connection(&model);
        (run, ticket, result)
    };

    let (blocked, stale_ticket, rejected) = planner(&mut workspace, PASSWORD);
    let _ = workspace.update_firing_web(WebEvent::UnattendedQuery(blocked, stale_ticket, rejected));
    assert!(!workspace.scheduler_firing_web.contains(blocked));
    assert_no_connection(&model);
    assert_no_connection(&ssh);

    let (run, query_ticket, query) = planner(&mut workspace, QUERY);
    assert_ne!(run, blocked);
    let _ = workspace.update_firing_web(WebEvent::UnattendedQuery(
        run,
        stale_ticket,
        Ok("unreviewed query".into()),
    ));
    let _ = workspace.update_firing_web(WebEvent::UnattendedResults(
        run,
        query_ticket,
        Err("stale phase".into()),
    ));
    assert_no_connection(&model);
    let task = workspace.update_firing_web(WebEvent::UnattendedQuery(run, query_ticket, query));
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    assert_no_connection(&model);
    let (search_run, search_ticket) = authorize(&mut workspace, block_on(stream.next()));
    assert_eq!(search_run, run);
    assert_ne!(search_ticket, query_ticket);
    let mut socket = test_support::accept(&model);
    let search: serde_json::Value = serde_json::from_slice(&test_support::wire_request(
        &mut socket,
        "POST /api/v1/chat HTTP/1.1",
    ))
    .unwrap();
    assert_eq!(search["input"], QUERY);
    assert_eq!(search["integrations"].as_array().unwrap().len(), 1);
    assert_eq!(
        search["integrations"][0]["allowed_tools"],
        json!(["tavily-search"])
    );
    assert!(!search.to_string().contains(PASSWORD) && !search.to_string().contains(GOAL));
    socket.get_mut().write_all(test_support::fixture_http_response("200 OK", "", &json!({
        "output":[{"type":"tool_call","tool":"tavily-search", "provider_info":{"type":"ephemeral_mcp","server_label":"tavily"},
                    "output":format!("{PUBLIC_NOTE} Returned {PASSWORD}.")}]
    }).to_string()).as_bytes()).unwrap();
    drop(socket);
    let (stale_permit, refused) = async_channel::bounded(1);
    let _ = workspace.update_firing_web(WebEvent::Authorize(run, query_ticket, stale_permit));
    assert_eq!(refused.try_recv(), Err(async_channel::TryRecvError::Closed));
    assert_eq!(
        authorize(&mut workspace, block_on(stream.next())),
        (run, search_ticket)
    );
    let Some(Action::Output(Message::ScheduledFiringWeb(WebEvent::UnattendedResults(
        completed_run,
        completed_ticket,
        result,
    )))) = block_on(stream.next())
    else {
        panic!("The scoped automatic search must return its excerpts");
    };
    assert_eq!((completed_run, completed_ticket), (run, search_ticket));
    assert_no_connection(&model);
    let task = workspace.update_firing_web(WebEvent::UnattendedResults(run, search_ticket, result));
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::ModelDispatch(
        ticket,
        permit,
    ))))) = block_on(stream.next())
    else {
        panic!("The approved excerpts must start the guarded diagnostics model");
    };
    assert_eq!(ticket.run(), run);
    assert_no_connection(&model);
    let _ = workspace.update_firing(Event(Progress::ModelDispatch(ticket.clone(), permit)));
    let mut socket = test_support::accept(&model);
    let diagnostic: serde_json::Value = serde_json::from_slice(&test_support::wire_request(
        &mut socket,
        "POST /api/v1/chat HTTP/1.1",
    ))
    .unwrap();
    assert!(diagnostic.get("integrations").is_none());
    assert!(
        diagnostic["system_prompt"]
            .as_str()
            .unwrap()
            .contains("untrusted reference data")
    );
    let input: serde_json::Value =
        serde_json::from_str(diagnostic["input"].as_str().unwrap()).unwrap();
    let evidence = input["web_evidence"].as_str().unwrap();
    assert!(evidence.contains(PUBLIC_NOTE) && evidence.contains("Returned ***."));
    assert!(!input.to_string().contains(PASSWORD));
    write_lm_message(&mut socket, &json!({"state":"commands","commands":[{"target":target,"command":"pwd -P","purpose":"Inspect the working directory"}]}).to_string());
    drop(socket);
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::Generated(
        completed,
        result,
    ))))) = block_on(stream.next())
    else {
        panic!("The unattended firing must complete its diagnostics proposal");
    };
    assert_eq!(ticket, completed);
    assert!(
        matches!(&result, Ok(Decision::Commands { commands }) if commands[0].command == "pwd -P")
    );
    let _ = workspace.update_firing(Event(Progress::Generated(completed, result)));
    workspace
        .scheduler
        .as_ref()
        .unwrap()
        .cancel_firing_web_run(run);
    let _ = workspace.expire_firing();
    assert_no_connection(&model);
    assert_no_connection(&ssh);
    let persisted = std::fs::read_to_string(&jobs).unwrap();
    assert!(persisted.contains(TOPIC));
    for private in [PASSWORD, QUERY, PUBLIC_NOTE] {
        assert!(!persisted.contains(private));
    }
}

pub(super) fn write_lm_message(
    socket: &mut std::io::BufReader<std::net::TcpStream>,
    content: &str,
) {
    socket
        .get_mut()
        .write_all(
            test_support::fixture_http_response(
                "200 OK",
                "",
                &json!({
                    "output":[{"type":"message","content":content}]
                })
                .to_string(),
            )
            .as_bytes(),
        )
        .unwrap();
}

fn unattended_workspace(
    root: &Path,
    model: &TcpListener,
    ssh: &TcpListener,
    web: serde_json::Value,
    credential_env: &str,
) -> (Workspace, window::Id, u64, PathBuf) {
    unattended_workspace_for_probe(
        root,
        model,
        ssh,
        web,
        credential_env,
        agent::Probe::PerFiring,
    )
}

pub(super) fn unattended_workspace_for_probe(
    root: &Path,
    model: &TcpListener,
    ssh: &TcpListener,
    web: serde_json::Value,
    credential_env: &str,
    probe: agent::Probe,
) -> (Workspace, window::Id, u64, PathBuf) {
    std::fs::create_dir(root).unwrap();
    profile::set_runtime_root(root.to_owned());
    let config: Configuration = serde_json::from_value(json!({"profiles": {"profiles": [{
        "id": 1, "name": "Local LM Studio", "model": "fixture:local",
        "connection_mode": "lm-studio", "trusted_local_endpoint": true,
        "endpoint": model.local_addr().unwrap().to_string(), "lm_web": web,
        "credential_env":credential_env
    }], "default": 1, "last_id": 1}}))
    .unwrap();
    config
        .save_fixture_at(root, &Configuration::default())
        .unwrap();
    let (reference, vault) = saved_target(ssh.local_addr().unwrap().port());
    let target = reference.id;
    let action: SshCommand = serde_json::from_value(json!({
        "connection":reference, "working_directory":"/srv/internal",
        "agent":{
            "kind": if matches!(probe, agent::Probe::SwarmIterative | agent::Probe::SwarmReadOnly) { "swarm" } else { "agent" },
            "probe":probe, "profile":1, "goal":GOAL
        }
    }))
    .unwrap();
    let jobs = profile::jobs_path().unwrap();
    let job = Job::new(
        JobId::new("unattended-web-review").unwrap(),
        "Research public docs",
        action,
    )
    .unwrap();
    SchedulerStore::open(&jobs)
        .unwrap()
        .commit_with_actions(&JobGraph::new(vec![job]).unwrap(), &[], &[])
        .unwrap();
    let mut scheduler = SchedulerWindow::open(jobs.clone()).unwrap();
    scheduler.set_execution_allowed(true);
    let id = window::Id::unique();
    let view = scheduler.new_view();
    let mut workspace = Workspace {
        scheduler: Some(scheduler),
        vault: Some(vault),
        ..Workspace::default()
    };
    workspace.scheduler_views.insert(id, view);
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(root).unwrap());
    let _ = workspace.update_scheduler(id, SchedulerMessage::Page(Page::Action));
    let _ = workspace.update_scheduler(id, SchedulerMessage::ReviewAgent);
    (workspace, id, target, jobs)
}

#[test]
fn unattended_research_reads_only_a_returned_page_and_keeps_its_evidence() {
    use crate::workspace::scheduler_access::firing_web::Event as WebEvent;
    const PAGE: &str = "https://public.example.com/docs";
    const UNLISTED: &str = "https://public.example.com/not-reviewed";
    const LM_TOKEN: &str = "KORTTY_RESEARCH_FIXTURE_LM_TOKEN";
    const PAGE_TOKEN: &str = "KORTTY_RESEARCH_FIXTURE_PAGE_TOKEN";
    let Some(root) = std::env::var_os(RESEARCH_ROOT).map(PathBuf::from) else {
        let scratch = Profile::new();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", RESEARCH_TEST, "--nocapture"])
            .env(
                RESEARCH_ROOT,
                scratch.0.canonicalize().unwrap().join("research-profile"),
            )
            .env(LM_TOKEN, "fixture-research-lm-token")
            .env(PAGE_TOKEN, "fixture-research-page-token")
            .output()
            .unwrap();
        let output = String::from_utf8_lossy(&child.stdout);
        assert!(
            child.status.success() && output.contains("1 passed"),
            "{output}\n{}",
            String::from_utf8_lossy(&child.stderr)
        );
        return;
    };
    let (model, _) = test_support::listener();
    let (ssh, _) = test_support::listener();
    let (mcp, _) = test_support::listener();
    let (mut workspace, id, target, jobs) = unattended_workspace(
        &root,
        &model,
        &ssh,
        json!({
            "mode":"searxng", "plugin":"mcp/searxng", "searxng_page":{
                "endpoint":format!("http://{}/mcp", mcp.local_addr().unwrap()), "bearer_env":PAGE_TOKEN
            }
        }),
        LM_TOKEN,
    );
    let ticket = workspace.scheduler_views[&id]
        .firing_review_ticket()
        .unwrap()
        .to_owned();
    let _ = workspace.update_scheduler(
        id,
        SchedulerMessage::EditUnattendedWebTopic(ticket.clone(), TOPIC.into()),
    );
    let _ = workspace.update_scheduler(
        id,
        SchedulerMessage::EditUnattendedWebFollowups(ticket.clone(), true),
    );
    let ticket = workspace.scheduler_views[&id]
        .firing_review_ticket()
        .unwrap()
        .to_owned();
    let _ = workspace.update_scheduler(
        id,
        SchedulerMessage::ReviewUnattendedWeb(ticket, TOPIC.into()),
    );
    let ticket = workspace.scheduler_views[&id]
        .firing_review_ticket()
        .unwrap()
        .to_owned();
    let _ = workspace.update_scheduler(id, SchedulerMessage::ApproveFiringRequest(ticket));
    let _ = workspace.update_scheduler(id, SchedulerMessage::Save);
    assert!(std::fs::read_to_string(&jobs).unwrap().contains(TOPIC));

    let authorize = |workspace: &mut Workspace, action| {
        let Some(Action::Output(Message::ScheduledFiringWeb(WebEvent::Authorize(
            run,
            ticket,
            permit,
        )))) = action
        else {
            panic!("Expected an exact research permit: {action:?}");
        };
        let _ = workspace.update_firing_web(WebEvent::Authorize(run, ticket, permit));
        (run, ticket)
    };
    let mut completed = None;
    for requested in [UNLISTED, PAGE] {
        let task = workspace.update_scheduler(id, SchedulerMessage::Run(false));
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        let (run, query_ticket) = authorize(&mut workspace, block_on(stream.next()));
        let (mut socket, plan) = research_request(&model, "/api/v1/chat");
        assert_eq!(
            research_input(&plan),
            json!({"public_research_topic":TOPIC})
        );
        write_lm_message(&mut socket, &json!({"query":QUERY}).to_string());
        drop(socket);
        assert_eq!(
            authorize(&mut workspace, block_on(stream.next())),
            (run, query_ticket)
        );
        let Some(Action::Output(Message::ScheduledFiringWeb(
            query @ WebEvent::UnattendedQuery(..),
        ))) = block_on(stream.next())
        else {
            panic!("Expected query result");
        };
        let mut stream =
            iced_runtime::task::into_stream(workspace.update_firing_web(query)).unwrap();
        let (search_run, search_ticket) = authorize(&mut workspace, block_on(stream.next()));
        assert_eq!(search_run, run);
        assert_ne!(search_ticket, query_ticket);
        let (mut socket, search) = research_request(&model, "/api/v1/chat");
        assert_eq!(search["input"], QUERY);
        assert_eq!(
            search["integrations"],
            json!([{"type":"plugin","id":"mcp/searxng","allowed_tools":["searxng_web_search"]}])
        );
        write_research_json(
            &mut socket,
            &json!({"output":[{"type":"tool_call","tool":"searxng_web_search", "provider_info":{"type":"plugin","plugin_id":"mcp/searxng"},
            "output":json!({"results":[{"title":"Public documentation","url":format!("{PAGE}?query=discard#part"),"content":format!("{PUBLIC_NOTE} Returned {PASSWORD}.")}]}).to_string()}]}),
        );
        drop(socket);
        assert_eq!(
            authorize(&mut workspace, block_on(stream.next())),
            (run, search_ticket)
        );
        let Some(Action::Output(Message::ScheduledFiringWeb(
            found @ WebEvent::UnattendedResults(..),
        ))) = block_on(stream.next())
        else {
            panic!("Expected search results");
        };
        let mut stream =
            iced_runtime::task::into_stream(workspace.update_firing_web(found)).unwrap();
        let (follow_run, follow_ticket) = authorize(&mut workspace, block_on(stream.next()));
        assert_eq!(follow_run, run);
        let (mut socket, plan) = research_request(&model, "/api/v1/chat");
        let input = research_input(&plan);
        assert_eq!(input.as_object().unwrap().len(), 3);
        assert_eq!(input["public_research_topic"], TOPIC);
        assert_eq!(input["page_candidates"], json!([PAGE]));
        let excerpts = input["web_evidence"].as_str().unwrap();
        assert!(excerpts.contains(PUBLIC_NOTE) && !excerpts.contains(PASSWORD));
        write_lm_message(
            &mut socket,
            &json!({"action":"page","url":requested}).to_string(),
        );
        drop(socket);
        assert_eq!(
            authorize(&mut workspace, block_on(stream.next())),
            (run, follow_ticket)
        );
        let Some(Action::Output(Message::ScheduledFiringWeb(
            choice @ WebEvent::UnattendedFollowUp(..),
        ))) = block_on(stream.next())
        else {
            panic!("Expected page choice");
        };
        let next = workspace.update_firing_web(choice);
        assert_no_connection(&mcp);
        if requested != PAGE {
            assert!(!workspace.scheduler_firing_web.contains(run));
            assert_no_connection(&model);
            continue;
        }
        let mut stream = iced_runtime::task::into_stream(next).unwrap();
        let mut page_ticket = None;
        for method in ["initialize", "notifications/initialized", "tools/call"] {
            let (page_run, token) = authorize(&mut workspace, block_on(stream.next()));
            assert_eq!(page_run, run);
            assert_eq!(*page_ticket.get_or_insert(token), token);
            let (mut socket, request) = research_request(&mcp, "/mcp");
            assert_eq!(request["method"], method);
            match method {
                "initialize" => write_research_json(
                    &mut socket,
                    &json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}}}}),
                ),
                "notifications/initialized" => socket
                    .get_mut()
                    .write_all(
                        test_support::fixture_http_response("202 Accepted", "", "").as_bytes(),
                    )
                    .unwrap(),
                _ => {
                    assert_eq!(
                        request["params"],
                        json!({"name":"web_url_read","arguments":{"url":PAGE,"maxLength":4000}})
                    );
                    write_research_json(
                        &mut socket,
                        &json!({"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":format!("Full page returned {PASSWORD}.")}]}}),
                    );
                }
            }
        }
        assert_eq!(
            authorize(&mut workspace, block_on(stream.next())),
            (run, page_ticket.unwrap())
        );
        let Some(Action::Output(Message::ScheduledFiringWeb(
            found @ WebEvent::UnattendedResults(..),
        ))) = block_on(stream.next())
        else {
            panic!("Expected fetched page");
        };
        let mut stream =
            iced_runtime::task::into_stream(workspace.update_firing_web(found)).unwrap();
        let finish = authorize(&mut workspace, block_on(stream.next()));
        let (mut socket, plan) = research_request(&model, "/api/v1/chat");
        let input = research_input(&plan);
        assert_eq!(input["page_candidates"], json!([]));
        let excerpts = input["web_evidence"].as_str().unwrap();
        assert!(excerpts.contains("Full page returned ***.") && excerpts.contains(PUBLIC_NOTE));
        write_lm_message(&mut socket, r#"{"action":"finish"}"#);
        drop(socket);
        assert_eq!(authorize(&mut workspace, block_on(stream.next())), finish);
        let Some(Action::Output(Message::ScheduledFiringWeb(
            choice @ WebEvent::UnattendedFollowUp(..),
        ))) = block_on(stream.next())
        else {
            panic!("Expected finish choice");
        };
        completed = Some((run, workspace.update_firing_web(choice)));
    }

    let (run, task) = completed.unwrap();
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::ModelDispatch(
        ticket,
        permit,
    ))))) = block_on(stream.next())
    else {
        panic!("Expected diagnostics authorization");
    };
    assert_eq!(ticket.run(), run);
    let _ = workspace.update_firing(Event(Progress::ModelDispatch(ticket.clone(), permit)));
    let (mut socket, wire) = research_request(&model, "/api/v1/chat");
    let input = research_input(&wire);
    let excerpts = input["web_evidence"].as_str().unwrap();
    assert!(
        excerpts.contains(PAGE)
            && excerpts.contains("Full page returned ***.")
            && excerpts.contains(PUBLIC_NOTE)
    );
    assert!(!input.to_string().contains(PASSWORD));
    write_lm_message(&mut socket, &json!({"state":"commands","commands":[{"target":target,"command":"pwd -P","purpose":"Inspect the working directory"}]}).to_string());
    drop(socket);
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::Generated(_, result))))) =
        block_on(stream.next())
    else {
        panic!("Expected diagnostics result");
    };
    assert!(matches!(result, Ok(Decision::Commands { .. })));
    assert_no_connection(&ssh);
    assert_no_connection(&model);
    assert_no_connection(&mcp);
    let persisted = std::fs::read_to_string(jobs).unwrap();
    assert!(
        !persisted.contains(PAGE) && !persisted.contains(QUERY) && !persisted.contains(PUBLIC_NOTE)
    );
}

pub(super) fn research_request(
    listener: &TcpListener,
    path: &str,
) -> (std::io::BufReader<std::net::TcpStream>, serde_json::Value) {
    let mut socket = test_support::accept(listener);
    let body = test_support::wire_request(&mut socket, &format!("POST {path} HTTP/1.1"));
    (socket, serde_json::from_slice(&body).unwrap())
}

pub(super) fn research_input(request: &serde_json::Value) -> serde_json::Value {
    assert!(request.get("integrations").is_none());
    serde_json::from_str(request["input"].as_str().unwrap()).unwrap()
}

pub(super) fn write_research_json(
    socket: &mut std::io::BufReader<std::net::TcpStream>,
    value: &serde_json::Value,
) {
    socket
        .get_mut()
        .write_all(
            test_support::fixture_http_response(
                "200 OK",
                "Content-Type: application/json\r\n",
                &value.to_string(),
            )
            .as_bytes(),
        )
        .unwrap();
}
