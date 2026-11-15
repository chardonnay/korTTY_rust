use super::proposal_web::{
    research_input, research_request, unattended_workspace_for_probe, write_lm_message,
    write_research_json,
};
use super::*;
use crate::job_executor::agent::{Resend, probe_fixture};
use crate::scheduler_window::agent::proposal::SwarmEvidence;
use crate::workspace::scheduler_access::firing_web::{
    Event as WebEvent, round::Event as RoundEvent,
};

const ROOT: &str = "KORTTY_SWARM_ENVIRONMENT_WEB_TEST_ROOT";
const TEST: &str = "workspace::scheduler_access::firing::tests::environment_web::approved_probe_adds_only_public_environment_research";
const ADVISORY_TEST: &str = "workspace::scheduler_access::firing::tests::environment_web::read_only_swarm_researches_only_its_reviewed_probe";
const ROUND_TEST: &str = "workspace::scheduler_access::firing::tests::environment_web::later_swarm_round_requires_query_and_excerpt_reviews";
const ROUND_PAGE_TEST: &str = "workspace::scheduler_access::firing::tests::environment_web::later_swarm_round_reviews_an_exact_page_and_its_excerpts";
const DIRECTORY: &str = "/srv/internal";
const TOPIC: &str = "Public documentation on kernel support";
const BASE_QUERY: &str = "Operating systems kernel maintenance public reference";
const ENVIRONMENT_QUERY: &str = "Linux 6.8 upstream kernel maintenance public reference";
const BASE_NOTE: &str = "The public support policy describes stable release branches.";
const ENVIRONMENT_NOTE: &str = "The Linux 6.8 maintainer guide lists stable updates.";
const ROUND_QUERY: &str = "Linux kernel upstream stable diagnostic guide";
const ROUND_NOTE: &str = "The upstream guide describes which kernel log messages need attention.";
const ROUND_PAGE: &str = "https://docs.example.com/release-notes";
const ROUND_PAGE_NOTE: &str = "The public release guide describes how to interpret kernel logs.";
const PAGE_MODEL_TOKEN_ENV: &str = "KORTTY_ROUND_PAGE_TEST_MODEL_TOKEN";
const PAGE_TOKEN_ENV: &str = "KORTTY_ROUND_PAGE_TEST_TOKEN";
const PAGE_TOKEN: &str = "fixture-swarm-page-bearer-key";
const PRIVATE_ROUND_PATH: &str = "/private/swarm-round/diagnostic";
const SUMMARY: &str = "Verified the research sources.";
const ADVISORY_MEMORY: &str = "Use the private maintenance window for this host.";
const PROBE_OUTPUT: &str =
    "Linux 6.8.12 x86_64\nprivate-ssh-account\n9012\n/private/physical/directory\n";
const ADVISORY_PROBE_OUTPUT: &str = "Linux 6.8.12 x86_64\nprivate-ssh-account\n9012\n/private/physical/directory/scheduled-agent-fixture-secret\n";
const MASKED_ADVISORY_PROBE_OUTPUT: &str =
    "Linux 6.8.12 x86_64\nprivate-ssh-account\n9012\n/private/physical/directory/***\n";

#[derive(Clone, Copy)]
enum SearchProvider {
    Tavily,
    Searxng,
}

#[derive(Clone, Copy)]
enum ResearchReview {
    Environment,
    Search,
    Page,
}

#[test]
fn approved_probe_adds_only_public_environment_research() {
    let Some(root) = private_profile(TEST) else {
        return;
    };

    let (model, _) = test_support::listener();
    let (ssh, _) = test_support::listener();
    let (mut workspace, view, target, jobs) = unattended_workspace_for_probe(
        &root,
        &model,
        &ssh,
        json!({"mode":"tavily"}),
        "",
        agent::Probe::SwarmIterative,
    );
    approve_research(&mut workspace, view, &jobs, ResearchReview::Environment);
    let (id, task) = run_baseline_research(&mut workspace, view, &model, SearchProvider::Tavily);
    let task = run_initial_probe(
        &mut workspace,
        id,
        task,
        &model,
        &ssh,
        SearchProvider::Tavily,
    );
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::ModelDispatch(
        ticket,
        permit,
    ))))) = block_on(stream.next())
    else {
        panic!("Expected the isolated Swarm diagnostics request");
    };
    assert_eq!(ticket.run(), id);
    let _ = workspace.update_firing(Event(Progress::ModelDispatch(ticket.clone(), permit)));
    let (mut socket, diagnostics) = research_request(&model, "/api/v1/chat");
    let input = research_input(&diagnostics);
    assert_eq!(input["round"], 1);
    assert_eq!(input["targets"], json!([target]));
    assert_eq!(input["environment_probe"]["stdout"], PROBE_OUTPUT);
    assert_combined_evidence(&input);
    write_lm_message(
        &mut socket,
        &json!({"state":"done","summary":SUMMARY}).to_string(),
    );
    drop(socket);
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::Generated(
        completed,
        result,
    ))))) = block_on(stream.next())
    else {
        panic!("Expected the diagnostics conclusion");
    };
    assert_eq!(completed, ticket);
    assert!(matches!(&result, Ok(Decision::SwarmDone { summary }) if summary == SUMMARY));
    let _ = workspace.update_firing(Event(Progress::Generated(completed, result)));
    assert_no_connection(&model);
    assert_no_connection(&ssh);
    assert_public_state(&jobs);
}

#[test]
fn read_only_swarm_researches_only_its_reviewed_probe() {
    let Some(root) = private_profile(ADVISORY_TEST) else {
        return;
    };
    let (model, _) = test_support::listener();
    let (ssh, _) = test_support::listener();
    let (mut workspace, view, target, jobs) = unattended_workspace_for_probe(
        &root,
        &model,
        &ssh,
        json!({"mode":"tavily"}),
        "",
        agent::Probe::SwarmReadOnly,
    );
    let memory = crate::private_files::Directory::open(&root.join("memory"), true).unwrap();
    memory
        .private_child("connections", true)
        .unwrap()
        .publish_copy(&format!("{target}.md"), ADVISORY_MEMORY.as_bytes())
        .unwrap();
    let _ = workspace.update_scheduler(view, SchedulerMessage::ReviewAgent);
    approve_research(&mut workspace, view, &jobs, ResearchReview::Environment);
    // Preserve the real worker and replace only the response from the checked SSH probe.
    probe_fixture::replace_next(target, DIRECTORY, ADVISORY_PROBE_OUTPUT);
    let (id, task) = run_baseline_research(&mut workspace, view, &model, SearchProvider::Tavily);
    let mut worker = iced_runtime::task::into_stream(task).unwrap();
    let event = block_on(worker.next());
    let Some(Action::Output(Message::JobAgentResend(Resend::Environment(research)))) = event else {
        panic!(
            "Expected the read-only worker's research request for the successful probe, got {event:?}"
        );
    };
    assert_eq!(research.scope.request.id, id);
    assert_eq!(research.scope.evidence_targets, [target]);
    assert_eq!(research.index, 0);
    let baseline_scope = Arc::clone(&research.scope);
    let task = workspace.answer_scheduler_agent_resend(Resend::Environment(research));
    assert!(
        workspace
            .require_read_only_environment_web(&baseline_scope)
            .is_err()
    );
    let _ = run_environment_research(&mut workspace, id, task, &model, SearchProvider::Tavily);
    assert!(
        workspace
            .require_read_only_environment_web(&baseline_scope)
            .is_err()
    );
    assert_no_connection(&model);
    let Some(Action::Output(Message::JobAgentResend(Resend::Model(advice)))) =
        block_on(worker.next())
    else {
        panic!("The read-only worker must ask for authority before sending advice");
    };
    assert_eq!(advice.scope.request.id, id);
    assert_eq!(advice.scope.evidence_targets, [target]);
    assert!(
        workspace
            .require_read_only_environment_web(&advice.scope)
            .is_ok()
    );
    let _ = workspace.answer_scheduler_agent_resend(Resend::Model(advice));
    let (mut socket, advisory) = research_request(&model, "/api/v1/chat");
    assert!(
        advisory["system_prompt"]
            .as_str()
            .unwrap()
            .contains(ADVISORY_MEMORY)
    );
    let input = research_input(&advisory);
    let diagnostics = input["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["target"], target);
    assert!(
        diagnostics[0]["output"]
            .as_str()
            .unwrap()
            .contains(MASKED_ADVISORY_PROBE_OUTPUT)
    );
    assert!(!advisory.to_string().contains(PASSWORD));
    assert_combined_evidence(&input);
    write_lm_message(
        &mut socket,
        &json!({"status":"done","summary":SUMMARY,"userMessage":SUMMARY}).to_string(),
    );
    drop(socket);
    loop {
        match block_on(worker.next()) {
            Some(Action::Output(Message::JobAgentResend(Resend::Model(accept)))) => {
                assert_eq!(accept.scope.request.id, id);
                assert_eq!(accept.scope.evidence_targets, [target]);
                let _ = workspace.answer_scheduler_agent_resend(Resend::Model(accept));
            }
            Some(Action::Output(Message::JobFinished(completion, journals))) => {
                assert_eq!(completion.id, id);
                assert_eq!(completion.outcome, RunOutcome::Success, "{completion:?}");
                let report = completion.swarm_report.as_ref().unwrap();
                assert_eq!(report.targets.len(), 1);
                assert!(report.targets[0].detail.contains(SUMMARY));
                let _ = workspace.update(Message::JobFinished(completion, journals));
                break;
            }
            event => panic!("Expected an advisory permit or completed job, got {event:?}"),
        }
    }
    assert_no_connection(&model);
    assert_no_connection(&ssh);
    assert_public_state(&jobs);
}

#[test]
fn later_swarm_round_requires_query_and_excerpt_reviews() {
    let Some(root) = private_profile(ROUND_TEST) else {
        return;
    };

    let (model, _) = test_support::listener();
    let (ssh, _) = test_support::listener();
    let (mut workspace, view, target, jobs) = unattended_workspace_for_probe(
        &root,
        &model,
        &ssh,
        json!({"mode":"tavily"}),
        "",
        agent::Probe::SwarmIterative,
    );
    approve_research(&mut workspace, view, &jobs, ResearchReview::Search);
    let (id, task) = run_baseline_research(&mut workspace, view, &model, SearchProvider::Tavily);
    let task = run_initial_probe(
        &mut workspace,
        id,
        task,
        &model,
        &ssh,
        SearchProvider::Tavily,
    );
    let output = format!("Linux 6.8.12 reported a diagnostic at {PRIVATE_ROUND_PATH}/***\n");
    let (first, task) =
        run_first_command(&mut workspace, view, task, (&model, &ssh), target, &output);
    let (research, result) = run_swarm_decision(
        &mut workspace,
        task,
        &model,
        json!({"state":"research","query":ROUND_QUERY}),
        |input| {
            assert_eq!(input["round"], 2);
            assert_eq!(input["targets"], json!([target]));
            assert_eq!(input["public_research_topic"], TOPIC);
            assert_eq!(input["prior_rounds"][0]["commands"][0]["stdout"], output);
            assert_combined_evidence(input);
            assert!(!input.to_string().contains(ROUND_NOTE));
        },
    );
    assert!(
        matches!(&result, Ok(Decision::SwarmResearch { lookup: web::Lookup::Search, input }) if input == ROUND_QUERY)
    );
    let _ = workspace.update_firing(Event(Progress::Generated(research.clone(), result)));
    assert!(workspace.swarm_round_web_request(&research).is_some());
    assert_no_connection(&model);
    let update = |workspace: &mut Workspace, revision, message| {
        workspace.update_firing_web(WebEvent::Round(RoundEvent::View(
            research.clone(),
            revision,
            message,
        )))
    };
    let _ = update(&mut workspace, 0, web::Message::Send);
    let _ = update(
        &mut workspace,
        0,
        web::Message::Edit(format!("Public search {PASSWORD}")),
    );
    let _ = update(&mut workspace, 0, web::Message::Review);
    let _ = update(&mut workspace, 1, web::Message::Send);
    assert_no_connection(&model);
    let _ = update(&mut workspace, 0, web::Message::Edit(ROUND_QUERY.into()));
    let _ = update(&mut workspace, 0, web::Message::Review);
    let _ = update(&mut workspace, 0, web::Message::Send);
    assert_no_connection(&model);
    let task = update(&mut workspace, 1, web::Message::Send);
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let Some(Action::Output(Message::ScheduledFiringWeb(WebEvent::Round(RoundEvent::Authorize(
        ticket,
        nonce,
        permit,
    ))))) = block_on(stream.next())
    else {
        panic!("Expected the target's exact reviewed web query permit");
    };
    assert_eq!(ticket, research);
    for (wrong_ticket, wrong_nonce) in [(first, nonce), (ticket.clone(), nonce.wrapping_add(1))] {
        let (wrong, refused) = async_channel::bounded(1);
        let _ = workspace.update_firing_web(WebEvent::Round(RoundEvent::Authorize(
            wrong_ticket,
            wrong_nonce,
            wrong,
        )));
        assert_eq!(refused.try_recv(), Err(async_channel::TryRecvError::Closed));
    }
    assert_no_connection(&model);
    let _ = workspace.update_firing_web(WebEvent::Round(RoundEvent::Authorize(
        ticket.clone(),
        nonce,
        permit,
    )));
    lookup(&model, ROUND_QUERY, ROUND_NOTE, SearchProvider::Tavily);
    let Some(Action::Output(Message::ScheduledFiringWeb(WebEvent::Round(RoundEvent::Authorize(
        accepted,
        accepted_nonce,
        permit,
    ))))) = block_on(stream.next())
    else {
        panic!("Expected the target's search response to require current authority");
    };
    assert_eq!(accepted, ticket);
    assert_eq!(accepted_nonce, nonce);
    let _ = workspace.update_firing_web(WebEvent::Round(RoundEvent::Authorize(
        accepted,
        accepted_nonce,
        permit,
    )));
    let Some(Action::Output(Message::ScheduledFiringWeb(
        event @ WebEvent::Round(RoundEvent::Worker(_, web::Message::Finished(_, _))),
    ))) = block_on(stream.next())
    else {
        panic!("Expected the target's reviewed search results");
    };
    let _ = workspace.update_firing_web(event);
    let _ = update(&mut workspace, 2, web::Message::Include);
    assert!(workspace.swarm_round_web_request(&research).is_some());
    assert_no_connection(&model);
    let task = update(&mut workspace, 3, web::Message::Include);
    let (resumed, result) = run_swarm_decision(
        &mut workspace,
        task,
        &model,
        json!({"state":"commands","commands":[
            {"target":target,"command":"uname -s","purpose":"Confirm operating system"}
        ]}),
        |input| {
            assert_eq!(input["round"], 2);
            assert_eq!(input["targets"], json!([target]));
            assert_eq!(input["prior_rounds"][0]["commands"][0]["stdout"], output);
            assert!(input.get("public_research_topic").is_none());
            assert_combined_evidence(input);
            assert!(input["web_evidence"].as_str().unwrap().contains(ROUND_NOTE));
        },
    );
    assert_ne!(resumed, research);
    assert_eq!(resumed.run(), id);
    assert_eq!(resumed.target_index(), research.target_index());
    assert!(matches!(&result, Ok(Decision::Commands { commands }) if commands.len() == 1));
    let _ = workspace.update_firing(Event(Progress::Generated(resumed.clone(), result)));
    select(&mut workspace, view, &resumed);
    let task = workspace.update_scheduler(view, SchedulerMessage::ApproveRunProposal(resumed));
    let command = take_ssh_execution(task, &model, &ssh);
    let task = complete_ssh(&mut workspace, command, "uname -s", "Linux\n");
    let (skip, result) = run_swarm_decision(
        &mut workspace,
        task,
        &model,
        json!({"state":"research","query":ROUND_QUERY}),
        |input| {
            assert_eq!(input["round"], 3);
            assert_eq!(input["public_research_topic"], TOPIC);
            assert_combined_evidence(input);
            assert!(!input.to_string().contains(ROUND_NOTE));
        },
    );
    let _ = workspace.update_firing(Event(Progress::Generated(skip.clone(), result)));
    assert_no_connection(&model);
    let task = workspace.update_firing_web(WebEvent::Round(RoundEvent::Skip(skip, 0)));
    let (completed, result) = run_swarm_decision(
        &mut workspace,
        task,
        &model,
        json!({"state":"done","summary":SUMMARY}),
        |input| {
            assert_eq!(input["round"], 3);
            assert!(input.get("public_research_topic").is_none());
            assert_combined_evidence(input);
            assert!(!input.to_string().contains(ROUND_NOTE));
        },
    );
    assert!(matches!(&result, Ok(Decision::SwarmDone { summary }) if summary == SUMMARY));
    let _ = workspace.update_firing(Event(Progress::Generated(completed.clone(), result)));
    assert_finished(&jobs, &completed, RunOutcome::Success, 1);
    assert_no_connection(&model);
    assert_no_connection(&ssh);
    assert_public_state(&jobs);
}

#[test]
fn later_swarm_round_reviews_an_exact_page_and_its_excerpts() {
    let Some(root) = private_profile(ROUND_PAGE_TEST) else {
        return;
    };
    let (model, _) = test_support::listener();
    let (ssh, _) = test_support::listener();
    let (mcp, _) = test_support::listener();
    let (mut workspace, view, target, jobs) = unattended_workspace_for_probe(
        &root,
        &model,
        &ssh,
        json!({"mode":"searxng", "plugin":"mcp/searxng", "searxng_page":{
            "endpoint":format!("http://{}/mcp", mcp.local_addr().unwrap()),
            "bearer_env":PAGE_TOKEN_ENV,
        }}),
        PAGE_MODEL_TOKEN_ENV,
        agent::Probe::SwarmIterative,
    );
    approve_research(&mut workspace, view, &jobs, ResearchReview::Page);
    let (id, task) = run_baseline_research(&mut workspace, view, &model, SearchProvider::Searxng);
    let task = run_initial_probe(
        &mut workspace,
        id,
        task,
        &model,
        &ssh,
        SearchProvider::Searxng,
    );
    let output = format!("Linux 6.8.12 reported a diagnostic at {PRIVATE_ROUND_PATH}/***\n");
    let (first, task) =
        run_first_command(&mut workspace, view, task, (&model, &ssh), target, &output);
    let (research, result) = run_swarm_decision(
        &mut workspace,
        task,
        &model,
        json!({"state":"research_page", "url":format!("{ROUND_PAGE}?discard=query-text#fragment-text")}),
        |input| {
            assert_eq!(input["round"], 2);
            assert_eq!(input["targets"], json!([target]));
            assert_eq!(input["public_research_topic"], TOPIC);
            assert_eq!(input["prior_rounds"][0]["commands"][0]["stdout"], output);
            assert_combined_evidence(input);
        },
    );
    assert!(
        matches!(&result, Ok(Decision::SwarmResearch { lookup: web::Lookup::Page, input }) if input == ROUND_PAGE)
    );
    let _ = workspace.update_firing(Event(Progress::Generated(research.clone(), result)));
    assert!(workspace.swarm_round_web_request(&research).is_some());
    assert_no_connection(&mcp);
    let update = |workspace: &mut Workspace, revision, message| {
        workspace.update_firing_web(WebEvent::Round(RoundEvent::View(
            research.clone(),
            revision,
            message,
        )))
    };
    let _ = update(&mut workspace, 0, web::Message::Send);
    let encoded_credential = format!("%66{}", PAGE_TOKEN.strip_prefix('f').unwrap());
    let _ = update(
        &mut workspace,
        0,
        web::Message::Edit(format!("{ROUND_PAGE}/{encoded_credential}")),
    );
    let _ = update(&mut workspace, 0, web::Message::Review);
    let _ = update(&mut workspace, 1, web::Message::Send);
    assert_no_connection(&mcp);
    let _ = update(
        &mut workspace,
        0,
        web::Message::Edit(format!("{ROUND_PAGE}?discard=query-text#fragment-text")),
    );
    let _ = update(&mut workspace, 0, web::Message::Review);
    let _ = update(&mut workspace, 0, web::Message::Send);
    assert_no_connection(&mcp);
    let task = update(&mut workspace, 1, web::Message::Send);
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let mut page_nonce = None;
    for method in ["initialize", "notifications/initialized", "tools/call"] {
        let (ticket, nonce, permit) = take_round_page_permit(block_on(stream.next()));
        assert_eq!(ticket, research);
        if page_nonce.is_none() {
            for (wrong_ticket, wrong_nonce) in [(&first, nonce), (&ticket, nonce.wrapping_add(1))] {
                let (wrong, refused) = async_channel::bounded(1);
                let _ = workspace.update_firing_web(WebEvent::Round(RoundEvent::Authorize(
                    wrong_ticket.clone(),
                    wrong_nonce,
                    wrong,
                )));
                assert_eq!(refused.try_recv(), Err(async_channel::TryRecvError::Closed));
            }
        }
        assert_eq!(*page_nonce.get_or_insert(nonce), nonce);
        assert_no_connection(&mcp);
        let _ = workspace.update_firing_web(WebEvent::Round(RoundEvent::Authorize(
            ticket, nonce, permit,
        )));
        serve_round_page_method(&mcp, method);
    }
    let (ticket, nonce, permit) = take_round_page_permit(block_on(stream.next()));
    assert_eq!(ticket, research);
    assert_eq!(Some(nonce), page_nonce);
    let _ = workspace.update_firing_web(WebEvent::Round(RoundEvent::Authorize(
        ticket, nonce, permit,
    )));
    let Some(Action::Output(Message::ScheduledFiringWeb(
        event @ WebEvent::Round(RoundEvent::Worker(_, web::Message::Finished(_, _))),
    ))) = block_on(stream.next())
    else {
        panic!("Expected the reviewed page result");
    };
    let _ = workspace.update_firing_web(event);
    let _ = update(&mut workspace, 2, web::Message::Include);
    assert!(workspace.swarm_round_web_request(&research).is_some());
    assert_no_connection(&model);
    let task = update(&mut workspace, 3, web::Message::Include);
    let (completed, result) = run_swarm_decision(
        &mut workspace,
        task,
        &model,
        json!({"state":"done","summary":SUMMARY}),
        |input| {
            assert_eq!(input["round"], 2);
            assert_eq!(input["targets"], json!([target]));
            assert_eq!(input["prior_rounds"][0]["commands"][0]["stdout"], output);
            assert!(input.get("public_research_topic").is_none());
            assert_combined_evidence(input);
            let excerpts = input["web_evidence"].as_str().unwrap();
            assert!(excerpts.contains(ROUND_PAGE));
            assert!(excerpts.contains(&format!("{ROUND_PAGE_NOTE} Returned ***.")));
            assert!(!excerpts.contains("query-text") && !excerpts.contains("fragment-text"));
        },
    );
    assert_ne!(completed, research);
    assert_eq!(completed.run(), id);
    assert_eq!(completed.target_index(), research.target_index());
    assert!(matches!(&result, Ok(Decision::SwarmDone { summary }) if summary == SUMMARY));
    let _ = workspace.update_firing(Event(Progress::Generated(completed.clone(), result)));
    assert_finished(&jobs, &completed, RunOutcome::Success, 1);
    assert_no_connection(&model);
    assert_no_connection(&mcp);
    assert_no_connection(&ssh);
    assert_public_state(&jobs);
}

fn take_round_page_permit(action: Option<Action<Message>>) -> (Ticket, u64, Sender<()>) {
    let Some(Action::Output(Message::ScheduledFiringWeb(WebEvent::Round(RoundEvent::Authorize(
        ticket,
        nonce,
        permit,
    ))))) = action
    else {
        panic!("Expected a page provider permit, got {action:?}");
    };
    (ticket, nonce, permit)
}

fn serve_round_page_method(listener: &TcpListener, method: &str) {
    let (mut socket, request) = research_request(listener, "/mcp");
    assert_eq!(request["method"], method);
    assert_private_absent(&request);
    let payload = request.to_string();
    assert!(
        !payload.contains(PAGE_TOKEN)
            && !payload.contains("query-text")
            && !payload.contains("fragment-text")
    );
    match method {
        "initialize" => write_research_json(
            &mut socket,
            &json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}}}}),
        ),
        "notifications/initialized" => socket
            .get_mut()
            .write_all(test_support::fixture_http_response("202 Accepted", "", "").as_bytes())
            .unwrap(),
        "tools/call" => {
            assert_eq!(
                request["params"],
                json!({"name":"web_url_read","arguments":{"url":ROUND_PAGE,"maxLength":4000}})
            );
            write_research_json(
                &mut socket,
                &json!({"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":format!("{ROUND_PAGE_NOTE} Returned {PASSWORD}.")}]}}),
            );
        }
        _ => unreachable!(),
    }
}

fn private_profile(test: &str) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os(ROOT) {
        return Some(root.into());
    }
    let scratch = Profile::new();
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(
            ROOT,
            scratch
                .0
                .canonicalize()
                .unwrap()
                .join("environment-profile"),
        )
        .env("TAVILY_API_KEY", "swarm-environment-test-tavily-key")
        .env(PAGE_MODEL_TOKEN_ENV, "fixture-swarm-page-model-key")
        .env(PAGE_TOKEN_ENV, PAGE_TOKEN)
        .env_remove("KORTTY_AI_MEMORY")
        .output()
        .unwrap();
    let output = String::from_utf8_lossy(&child.stdout);
    assert!(
        child.status.success() && output.contains("1 passed"),
        "{output}\n{}",
        String::from_utf8_lossy(&child.stderr)
    );
    None
}

fn approve_research(
    workspace: &mut Workspace,
    view: window::Id,
    jobs: &Path,
    research: ResearchReview,
) {
    let review_ticket = |workspace: &Workspace| {
        workspace.scheduler_views[&view]
            .firing_review_ticket()
            .unwrap()
            .to_owned()
    };
    let ticket = review_ticket(workspace);
    let _ = workspace.update_scheduler(
        view,
        SchedulerMessage::EditUnattendedWebTopic(ticket.clone(), TOPIC.into()),
    );
    let _ = workspace.update_scheduler(
        view,
        SchedulerMessage::EditUnattendedWebEnvironment(ticket.clone(), true),
    );
    if matches!(research, ResearchReview::Search | ResearchReview::Page) {
        let ticket = review_ticket(workspace);
        let _ = workspace.update_scheduler(
            view,
            SchedulerMessage::EditUnattendedWebRounds(ticket, true),
        );
    }
    if matches!(research, ResearchReview::Page) {
        let ticket = review_ticket(workspace);
        let _ = workspace.update_scheduler(
            view,
            SchedulerMessage::EditUnattendedWebRoundPages(ticket, true),
        );
    }
    let ticket = review_ticket(workspace);
    let _ = workspace.update_scheduler(
        view,
        SchedulerMessage::ReviewUnattendedWeb(ticket, TOPIC.into()),
    );
    let ticket = review_ticket(workspace);
    let _ = workspace.update_scheduler(view, SchedulerMessage::ApproveFiringRequest(ticket));
    let _ = workspace.update_scheduler(view, SchedulerMessage::Save);
    let version = match research {
        ResearchReview::Environment => "scheduled-web-lookup-v3",
        ResearchReview::Search => "scheduled-web-lookup-v4",
        ResearchReview::Page => "scheduled-web-lookup-v5",
    };
    assert!(std::fs::read_to_string(jobs).unwrap().contains(version));
}

fn run_baseline_research(
    workspace: &mut Workspace,
    view: window::Id,
    model: &TcpListener,
    provider: SearchProvider,
) -> (RunId, Task<Message>) {
    let task = workspace.update_scheduler(view, SchedulerMessage::Run(false));
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    assert_no_connection(model);
    let (id, query_nonce) = authorize_base(workspace, block_on(stream.next()));
    let (mut socket, planner) = research_request(model, "/api/v1/chat");
    assert_eq!(
        research_input(&planner),
        json!({"public_research_topic":TOPIC})
    );
    write_lm_message(&mut socket, &json!({"query":BASE_QUERY}).to_string());
    drop(socket);
    assert_eq!(
        authorize_base(workspace, block_on(stream.next())),
        (id, query_nonce)
    );
    let Some(Action::Output(Message::ScheduledFiringWeb(event @ WebEvent::UnattendedQuery(..)))) =
        block_on(stream.next())
    else {
        panic!("Expected the baseline research query");
    };
    let task = workspace.update_firing_web(event);
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    assert_no_connection(model);
    let (search_id, search_nonce) = authorize_base(workspace, block_on(stream.next()));
    assert_eq!(search_id, id);
    lookup(model, BASE_QUERY, BASE_NOTE, provider);
    assert_eq!(
        authorize_base(workspace, block_on(stream.next())),
        (id, search_nonce)
    );
    let Some(Action::Output(Message::ScheduledFiringWeb(event @ WebEvent::UnattendedResults(..)))) =
        block_on(stream.next())
    else {
        panic!("Expected the baseline research excerpts");
    };
    (id, workspace.update_firing_web(event))
}

fn run_initial_probe(
    workspace: &mut Workspace,
    id: RunId,
    task: Task<Message>,
    model: &TcpListener,
    ssh: &TcpListener,
    provider: SearchProvider,
) -> Task<Message> {
    let ticket = take_ssh_execution(task, model, ssh);
    let review = workspace
        .scheduler
        .as_ref()
        .unwrap()
        .firing_reviews
        .by_ticket(&ticket)
        .unwrap();
    assert_eq!(ticket.run(), id);
    assert_eq!(ticket.target_index(), 0);
    assert_eq!(review.phase(), Phase::Executing);
    assert!(review.context().initial_probe);
    let command = agent::initial_swarm_probe(DIRECTORY).unwrap();
    assert_eq!(
        review.context().prompt.swarm_probe_command(),
        Some(command.as_str())
    );
    let task = complete_ssh(workspace, ticket, &command, PROBE_OUTPUT);
    run_environment_research(workspace, id, task, model, provider)
}

fn run_first_command(
    workspace: &mut Workspace,
    view: window::Id,
    task: Task<Message>,
    (model, ssh): (&TcpListener, &TcpListener),
    target: u64,
    output: &str,
) -> (Ticket, Task<Message>) {
    let (first, result) = run_swarm_decision(
        workspace,
        task,
        model,
        json!({"state":"commands","commands":[
            {"target":target,"command":"uname -r","purpose":"Inspect the kernel version"}
        ]}),
        |input| {
            assert_eq!(input["round"], 1);
            assert!(input.get("public_research_topic").is_none());
            assert_combined_evidence(input);
        },
    );
    assert!(matches!(&result, Ok(Decision::Commands { commands }) if commands.len() == 1));
    let _ = workspace.update_firing(Event(Progress::Generated(first.clone(), result)));
    let _ = workspace.update_scheduler(view, SchedulerMessage::Page(Page::Journal));
    select(workspace, view, &first);
    let task =
        workspace.update_scheduler(view, SchedulerMessage::ApproveRunProposal(first.clone()));
    let command = take_ssh_execution(task, model, ssh);
    assert_eq!(command, first);
    (first, complete_ssh(workspace, command, "uname -r", output))
}

fn take_ssh_execution(task: Task<Message>, model: &TcpListener, ssh: &TcpListener) -> Ticket {
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let event = block_on(stream.next());
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::ExecutionPermit(
        ticket,
        FiringPermit::Execute,
        permit,
    ))))) = event
    else {
        panic!("Expected a reviewed SSH execution permit, got {event:?}");
    };
    assert_no_connection(model);
    assert_no_connection(ssh);
    // Dropping the real permit prevents network access before replacing the completion.
    drop(permit);
    ticket
}

fn complete_ssh(
    workspace: &mut Workspace,
    ticket: Ticket,
    command: &str,
    stdout: &str,
) -> Task<Message> {
    let id = ticket.run();
    workspace.update_firing(Event(Progress::Executed(
        ticket,
        FiringResult {
            completion: Completion {
                id,
                outcome: RunOutcome::Success,
                detail: None,
                recording: None,
                output: None,
                swarm_report: None,
            },
            evidence: Some(vec![SwarmEvidence {
                command: command.into(),
                exit: 0,
                stdout: stdout.into(),
                stderr: String::new(),
                truncated: false,
            }]),
        },
    )))
}

fn run_swarm_decision(
    workspace: &mut Workspace,
    task: Task<Message>,
    model: &TcpListener,
    response: serde_json::Value,
    check_input: impl FnOnce(&serde_json::Value),
) -> (Ticket, Result<Decision, String>) {
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let event = block_on(stream.next());
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::ModelDispatch(
        ticket,
        permit,
    ))))) = event
    else {
        panic!("Expected a reviewed Swarm diagnostics request, got {event:?}");
    };
    assert_no_connection(model);
    let _ = workspace.update_firing(Event(Progress::ModelDispatch(ticket.clone(), permit)));
    let (mut socket, request) = research_request(model, "/api/v1/chat");
    assert!(!request.to_string().contains(PASSWORD));
    check_input(&research_input(&request));
    write_lm_message(&mut socket, &response.to_string());
    drop(socket);
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::Generated(
        completed,
        result,
    ))))) = block_on(stream.next())
    else {
        panic!("Expected the Swarm's reviewed decision");
    };
    assert_eq!(completed, ticket);
    assert!(block_on(stream.next()).is_none());
    (ticket, result)
}

fn run_environment_research(
    workspace: &mut Workspace,
    id: RunId,
    task: Task<Message>,
    model: &TcpListener,
    provider: SearchProvider,
) -> Task<Message> {
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    assert_no_connection(model);
    let (plan_nonce, permit) = take_environment_permit(block_on(stream.next()), id);
    for (index, nonce) in [(0, plan_nonce.wrapping_add(1)), (1, plan_nonce)] {
        let (wrong, refused) = async_channel::bounded(1);
        let _ = workspace.update_firing(Event(Progress::EnvironmentLookup(
            id,
            index,
            nonce,
            environment_research::Progress::Authorize(wrong),
        )));
        assert_eq!(refused.try_recv(), Err(async_channel::TryRecvError::Closed));
    }
    assert_no_connection(model);
    authorize_environment(workspace, id, plan_nonce, permit);
    let (mut socket, planner) = research_request(model, "/api/v1/chat");
    assert_eq!(
        research_input(&planner),
        json!({
            "public_research_topic": TOPIC,
            "environment": {"os_family":"linux", "kernel_major":6, "kernel_minor":8}
        })
    );
    assert_private_absent(&planner);
    write_lm_message(&mut socket, &json!({"query":ENVIRONMENT_QUERY}).to_string());
    drop(socket);
    let planned = loop {
        let Some(Action::Output(Message::ScheduledAgent(Event(Progress::EnvironmentLookup(
            run,
            index,
            nonce,
            stage,
        ))))) = block_on(stream.next())
        else {
            panic!("Expected the approved environment query");
        };
        assert_eq!((run, index, nonce), (id, 0, plan_nonce));
        match stage {
            environment_research::Progress::Authorize(permit) => {
                authorize_environment(workspace, id, nonce, permit);
            }
            stage @ environment_research::Progress::Planned(_) => break stage,
            _ => panic!("The search must wait for the environment query"),
        }
    };
    assert_no_connection(model);
    let task = workspace.update_firing(Event(Progress::EnvironmentLookup(
        id, 0, plan_nonce, planned,
    )));
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let (search_nonce, permit) = take_environment_permit(block_on(stream.next()), id);
    assert_no_connection(model);
    authorize_environment(workspace, id, search_nonce, permit);
    lookup(model, ENVIRONMENT_QUERY, ENVIRONMENT_NOTE, provider);
    let searched = loop {
        let Some(Action::Output(Message::ScheduledAgent(Event(Progress::EnvironmentLookup(
            run,
            index,
            nonce,
            stage,
        ))))) = block_on(stream.next())
        else {
            panic!("Expected the environment research excerpts");
        };
        assert_eq!((run, index, nonce), (id, 0, search_nonce));
        match stage {
            environment_research::Progress::Authorize(permit) => {
                authorize_environment(workspace, id, nonce, permit);
            }
            stage @ environment_research::Progress::Searched(_) => break stage,
            _ => panic!("Expected search completion after the query"),
        }
    };
    assert_no_connection(model);
    workspace.update_firing(Event(Progress::EnvironmentLookup(
        id,
        0,
        search_nonce,
        searched,
    )))
}

fn assert_combined_evidence(input: &serde_json::Value) {
    let evidence = input["web_evidence"].as_str().unwrap();
    assert!(evidence.contains(BASE_NOTE) && evidence.contains(ENVIRONMENT_NOTE));
}

fn assert_public_state(jobs: &Path) {
    let persisted = std::fs::read_to_string(jobs).unwrap();
    assert!(persisted.contains(TOPIC));
    for value in [
        BASE_QUERY,
        ENVIRONMENT_QUERY,
        BASE_NOTE,
        ENVIRONMENT_NOTE,
        ROUND_QUERY,
        ROUND_NOTE,
        ROUND_PAGE,
        ROUND_PAGE_NOTE,
        PRIVATE_ROUND_PATH,
        "private-ssh-account",
    ] {
        assert!(!persisted.contains(value));
    }
}

fn authorize_base(workspace: &mut Workspace, action: Option<Action<Message>>) -> (RunId, u64) {
    let Some(Action::Output(Message::ScheduledFiringWeb(WebEvent::Authorize(run, nonce, permit)))) =
        action
    else {
        panic!("Expected a baseline web permit");
    };
    let _ = workspace.update_firing_web(WebEvent::Authorize(run, nonce, permit));
    (run, nonce)
}

fn take_environment_permit(action: Option<Action<Message>>, id: RunId) -> (u64, Sender<()>) {
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::EnvironmentLookup(
        run,
        index,
        nonce,
        environment_research::Progress::Authorize(permit),
    ))))) = action
    else {
        panic!("Expected a target-specific environment research permit");
    };
    assert_eq!((run, index), (id, 0));
    (nonce, permit)
}

fn authorize_environment(workspace: &mut Workspace, id: RunId, nonce: u64, permit: Sender<()>) {
    let _ = workspace.update_firing(Event(Progress::EnvironmentLookup(
        id,
        0,
        nonce,
        environment_research::Progress::Authorize(permit),
    )));
}

fn lookup(listener: &TcpListener, query: &str, expected_note: &str, provider: SearchProvider) {
    let (mut socket, search) = research_request(listener, "/api/v1/chat");
    assert_eq!(search["input"], query);
    let (tool, info) = match provider {
        SearchProvider::Tavily => {
            assert_eq!(search["integrations"].as_array().unwrap().len(), 1);
            (
                "tavily-search",
                json!({"type":"ephemeral_mcp","server_label":"tavily"}),
            )
        }
        SearchProvider::Searxng => {
            assert_eq!(
                search["integrations"],
                json!([{"type":"plugin","id":"mcp/searxng","allowed_tools":["searxng_web_search"]}])
            );
            (
                "searxng_web_search",
                json!({"type":"plugin","plugin_id":"mcp/searxng"}),
            )
        }
    };
    assert_eq!(search["integrations"][0]["allowed_tools"], json!([tool]));
    assert_private_absent(&search);
    write_research_json(
        &mut socket,
        &json!({
            "output": [{
                "type":"tool_call",
                "tool":tool,
                "provider_info":info,
                "output":expected_note
            }]
        }),
    );
}

fn assert_private_absent(request: &serde_json::Value) {
    let request = request.to_string();
    for private in [
        DIRECTORY,
        PASSWORD,
        GOAL,
        "6.8.12",
        "x86_64",
        "private-ssh-account",
        "9012",
        "/private/physical/directory",
        PRIVATE_ROUND_PATH,
    ] {
        assert!(
            !request.contains(private),
            "Private probe field reached the web request: {private}"
        );
    }
}
