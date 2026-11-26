use super::proposal_web::{unattended_workspace_with_profile, write_research_json};
use super::*;
use crate::job_executor::agent::{Resend, probe_fixture};
use crate::workspace::scheduler_access::firing_web::{
    Event as WebEvent, round::Event as RoundEvent,
};

const ROOT: &str = "KORTTY_SCHEDULED_MODEL_WEB_TEST_ROOT";
const TEST: &str = "workspace::scheduler_access::firing::tests::model_web::scheduled_tools_require_each_review_and_enforce_the_firing_budget";
const READ_ONLY_TEST: &str = "workspace::scheduler_access::firing::tests::model_web::read_only_swarm_aligns_tool_reviews_and_advisory_authority";
const TOPIC: &str = "Public documentation on portable development";
const PAGE: &str = "https://example.com/scheduled";

fn private_test_profile(test: &str) -> Option<PathBuf> {
    std::env::var_os(ROOT).map(PathBuf::from).or_else(|| {
        let scratch = Profile::new();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(ROOT, scratch.0.canonicalize().unwrap().join("profile"))
            .env("TAVILY_API_KEY", "scheduled-web-fixture-provider")
            .env("KORTTY_DIALOG_PREVIEW", "agent-web-tool")
            .output()
            .unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        None
    })
}

fn approved_fixture(
    root: &Path,
    model: &TcpListener,
    ssh: &TcpListener,
    probe: agent::Probe,
) -> (Workspace, window::Id, u64, PathBuf) {
    let config: Configuration = serde_json::from_value(json!({
      "general":{"agent_always_offer_web_tools":true,"agent_web_offer_revision":1},
      "profiles": {
        "profiles":[{"id":1,"name":"Scheduled tools","model":"fixture","tavily_search":true,
            "endpoint":model.local_addr().unwrap().to_string(),"selection_limit":40000}],
        "default":1,"last_id":1
    }}))
    .unwrap();
    let (mut workspace, view, target, jobs) =
        unattended_workspace_with_profile(root, ssh, config, probe, "Inspect the build settings");
    let review_ticket = |workspace: &Workspace| {
        workspace.scheduler_views[&view]
            .firing_review_ticket()
            .unwrap()
            .to_owned()
    };
    let ticket = review_ticket(&workspace);
    drop(workspace.update_scheduler(
        view,
        SchedulerMessage::EditUnattendedWebTopic(ticket.clone(), TOPIC.into()),
    ));
    drop(workspace.update_scheduler(
        view,
        SchedulerMessage::EditUnattendedWebModelTools(ticket, true),
    ));
    let ticket = review_ticket(&workspace);
    drop(workspace.update_scheduler(
        view,
        SchedulerMessage::ReviewUnattendedWeb(ticket, TOPIC.into()),
    ));
    let ticket = review_ticket(&workspace);
    drop(workspace.update_scheduler(view, SchedulerMessage::ApproveFiringRequest(ticket)));
    drop(workspace.update_scheduler(view, SchedulerMessage::Save));
    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&jobs).unwrap()).unwrap();
    assert_eq!(
        persisted["actions"]["unattended-web-review"]["agent"]["approval"]["web_lookup"]["model_tools"],
        true
    );
    drop(workspace.scheduler.take());
    let mut saved = SchedulerWindow::open(jobs.clone()).unwrap();
    saved.set_execution_allowed(true);
    workspace.scheduler_views.insert(view, saved.new_view());
    workspace.scheduler = Some(saved);
    drop(workspace.update_scheduler(view, SchedulerMessage::Page(Page::Journal)));
    (workspace, view, target, jobs)
}

#[test]
fn scheduled_tools_require_each_review_and_enforce_the_firing_budget() {
    let Some(root) = private_test_profile(TEST) else {
        return;
    };
    let (model, _) = test_support::listener();
    let (ssh, _) = test_support::listener();
    let (mut workspace, view, _, jobs) =
        approved_fixture(&root, &model, &ssh, agent::Probe::PerFiring);
    let firing = start_after_preflight(&mut workspace, view, &model);
    let mut stream = iced_runtime::task::into_stream(firing).unwrap();
    let ticket = permit_model(&mut workspace, block_on(stream.next()));
    let mut socket = test_support::accept(&model);
    let outbound = test_support::request(&mut socket);
    assert_eq!(outbound["tools"].as_array().unwrap().len(), 2);
    let call = |id, function, arguments: serde_json::Value| json!({"id":id,"type":"function","function":{"name":function,"arguments":arguments.to_string()}});
    write_model(
        &mut socket,
        json!({"content":null,"tool_calls":[
            call("find", "web_search", json!({"query":"Initial public search"})),
            call("read", "web_extract", json!({"url":PAGE})),
            call("beyond-budget", "web_search", json!({"query":"One more public search"}))
        ]}),
    );

    for input in ["Reviewed scheduler search", PAGE] {
        let Some(Action::Output(Message::ScheduledAgent(
            event @ Event(Progress::ModelUpdate(_, usage::Progress::WebTool(_))),
        ))) = after_model_response(&mut workspace, &mut stream)
        else {
            panic!("Expected the tool proposal for the active firing");
        };
        drop(workspace.update_firing(event));
        let draft = workspace.round_web_revision(&ticket).unwrap();
        let update = |workspace: &mut Workspace, revision, message| {
            workspace.update_firing_web(WebEvent::Round(RoundEvent::View(
                ticket.clone(),
                revision,
                message,
            )))
        };
        assert_eq!(update(&mut workspace, draft, web::Message::Send).units(), 0);
        drop(update(
            &mut workspace,
            draft,
            web::Message::Edit(input.into()),
        ));
        drop(update(&mut workspace, draft, web::Message::Review));
        let exact = workspace.round_web_revision(&ticket).unwrap();
        assert_ne!(draft, exact);
        assert_eq!(update(&mut workspace, draft, web::Message::Send).units(), 0);
        assert_no_connection(&model);
        let lookup = update(&mut workspace, exact, web::Message::Send);
        let mut lookup = iced_runtime::task::into_stream(lookup).unwrap();
        while let Some(action) = block_on(lookup.next()) {
            let Action::Output(Message::ScheduledFiringWeb(event)) = action else {
                panic!("Expected the reviewed provider worker");
            };
            drop(workspace.update_firing_web(event));
        }
        let results = workspace.round_web_revision(&ticket).unwrap();
        assert_ne!(exact, results);
        assert_eq!(
            update(&mut workspace, exact, web::Message::Include).units(),
            0
        );
        assert_no_connection(&model);
        drop(update(&mut workspace, results, web::Message::Include));
    }
    let Some(Action::Output(Message::ScheduledAgent(
        event @ Event(Progress::ModelUpdate(_, usage::Progress::WebTool(_))),
    ))) = after_model_response(&mut workspace, &mut stream)
    else {
        panic!("Expected the over-budget tool proposal");
    };
    drop(workspace.update_firing(event));
    assert!(workspace.round_web_revision(&ticket).is_none());
    assert_no_connection(&model);
    assert_eq!(
        permit_model(&mut workspace, block_on(stream.next())),
        ticket
    );
    let mut socket = test_support::accept(&model);
    let continuation = test_support::request(&mut socket);
    for (index, id, expected) in [
        (3, "find", "Reviewed scheduler search"),
        (4, "read", PAGE),
        (5, "beyond-budget", "tool_round_limit"),
    ] {
        let tool = &continuation["messages"][index];
        assert_eq!(tool["tool_call_id"], id);
        assert!(tool["content"].as_str().unwrap().contains(expected));
    }
    write_model(&mut socket, json!({"content":"{\"state\":\"done\"}"}));
    let Some(Action::Output(Message::ScheduledAgent(
        event @ Event(Progress::Generated(_, Ok(Decision::Done {}))),
    ))) = after_model_response(&mut workspace, &mut stream)
    else {
        panic!("Expected a completed scheduled decision");
    };
    drop(workspace.update_firing(event));
    assert_finished(&jobs, &ticket, RunOutcome::Success, 1);
    assert_no_connection(&model);
    assert_no_connection(&ssh);

    let firing = start_after_preflight(&mut workspace, view, &model);
    let mut stream = iced_runtime::task::into_stream(firing).unwrap();
    let next = permit_model(&mut workspace, block_on(stream.next()));
    assert_ne!(next.run(), ticket.run());
    let mut socket = test_support::accept(&model);
    let initial = test_support::request(&mut socket);
    assert_eq!(initial["tools"].as_array().unwrap().len(), 2);
    write_model(
        &mut socket,
        json!({"content":null,"tool_calls":[call("next", "web_search", json!({"query":"Public next firing"}))]}),
    );
    let Some(Action::Output(Message::ScheduledAgent(
        event @ Event(Progress::ModelUpdate(_, usage::Progress::WebTool(_))),
    ))) = after_model_response(&mut workspace, &mut stream)
    else {
        panic!("Expected a fresh proposal in the next firing");
    };
    drop(workspace.update_firing(event));
    let revision = workspace.round_web_revision(&next).unwrap();
    drop(
        workspace.update_firing_web(WebEvent::Round(RoundEvent::View(
            ticket,
            revision,
            web::Message::Close,
        ))),
    );
    assert_eq!(workspace.round_web_revision(&next), Some(revision));
    let changed = crate::clipboard_policy::parse(
        "[meta]\nschema-version = 1\n[[rule]]\n[[rule.session-journal.replace]]\npattern = 'Public next firing'\n",
    )
    .unwrap();
    let original = std::mem::replace(
        &mut workspace.clipboard.policy.terminal_logging,
        changed.terminal_logging,
    );
    assert_eq!(
        workspace.current_firing(&next, Phase::Generating).err(),
        Some("The masking rules changed after the AI request was reviewed. Review it again.")
    );
    assert_no_connection(&model);
    workspace.clipboard.policy.terminal_logging = original;
    drop(workspace.update_scheduler(view, SchedulerMessage::CancelRun(next.run())));
    assert!(workspace.round_web_revision(&next).is_none());
    assert_finished(&jobs, &next, RunOutcome::Cancelled, 2);
    assert_no_connection(&model);
    assert_no_connection(&ssh);
}

#[test]
fn read_only_swarm_aligns_tool_reviews_and_advisory_authority() {
    use crate::job_executor::agent::{WEB_MASK_CHANGED, WEB_POLICY_BLOCKED, WEB_VAULT_CHANGED};

    #[derive(Clone, Copy)]
    enum Change {
        None,
        Vault,
        Mask,
        InternetPolicy,
        InternetPolicyBeforeSend,
        PreferenceBeforeSend,
    }

    let Some(root) = private_test_profile(READ_ONLY_TEST) else {
        return;
    };
    let (model, _) = test_support::listener();
    let (ssh, _) = test_support::listener();
    let (mut workspace, view, target, _) =
        approved_fixture(&root, &model, &ssh, agent::Probe::SwarmReadOnly);

    let masked_policy = crate::clipboard_policy::parse(
        "[meta]\nschema-version = 1\n[[rule]]\n[[rule.session-journal.replace]]\npattern = 'Public page'\n",
    )
    .unwrap();
    let original_mask = workspace.clipboard.policy.terminal_logging.clone();
    for change in [
        Change::None,
        Change::Vault,
        Change::Mask,
        Change::InternetPolicy,
        Change::InternetPolicyBeforeSend,
        Change::PreferenceBeforeSend,
    ] {
        probe_fixture::replace_next(target, "/srv/internal", "owned read-only diagnostics");
        let firing = start_after_preflight(&mut workspace, view, &model);
        let mut worker = iced_runtime::task::into_stream(firing).unwrap();
        let action = block_on(worker.next());
        let Some(Action::Output(Message::JobAgentResend(Resend::Model(permit)))) = action else {
            panic!("Expected authority for the read-only advisory request, got {action:?}");
        };
        let id = permit.scope.request.id;
        assert_eq!(permit.scope.evidence_targets, [target]);
        drop(workspace.answer_scheduler_agent_resend(Resend::Model(permit)));

        let mut socket = test_support::accept(&model);
        let initial = test_support::request(&mut socket);
        assert_eq!(initial["tools"].as_array().unwrap().len(), 2);
        let input: serde_json::Value =
            serde_json::from_str(initial["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(input["diagnostics"].as_array().unwrap().len(), 1);
        assert_eq!(input["diagnostics"][0]["target"], target);
        write_model(
            &mut socket,
            json!({"content":null,"tool_calls":[{
                "id":"advisory-page","type":"function","function":{
                    "name":"web_extract","arguments":json!({"url":PAGE}).to_string()
                }
            }]}),
        );

        let review = loop {
            match block_on(worker.next()) {
                Some(Action::Output(Message::JobAgentResend(Resend::Model(permit)))) => {
                    drop(workspace.answer_scheduler_agent_resend(Resend::Model(permit)));
                }
                Some(Action::Output(Message::JobAgentResend(Resend::WebTool(review)))) => {
                    break review;
                }
                action => panic!("Expected a read-only tool review, got {action:?}"),
            }
        };
        assert_eq!(review.authority.scope.evidence_targets, [target]);
        assert_eq!(review.authority.scope.request.id, id);
        drop(workspace.answer_scheduler_agent_resend(Resend::WebTool(review)));
        let key = Key::new(id, 0);
        let ticket = workspace
            .model_web_ticket(key)
            .expect("read-only tool held for review");
        let draft = workspace.round_web_revision(&ticket).unwrap();
        let update = |workspace: &mut Workspace, revision, message| {
            workspace.update_firing_web(WebEvent::Round(RoundEvent::View(
                ticket.clone(),
                revision,
                message,
            )))
        };
        assert_eq!(update(&mut workspace, draft, web::Message::Send).units(), 0);
        drop(update(&mut workspace, draft, web::Message::Review));
        let reviewed = workspace.round_web_revision(&ticket).unwrap();
        assert_no_connection(&model);
        if matches!(change, Change::Vault) {
            workspace.vault_change_revision = Arc::new(());
            assert_eq!(
                update(&mut workspace, reviewed, web::Message::Send).units(),
                0
            );
        } else {
            let lookup = update(&mut workspace, reviewed, web::Message::Send);
            let mut lookup = iced_runtime::task::into_stream(lookup).unwrap();
            while let Some(action) = block_on(lookup.next()) {
                let Action::Output(Message::ScheduledFiringWeb(event)) = action else {
                    panic!("Expected a reviewed provider dispatch");
                };
                drop(workspace.update_firing_web(event));
            }
            let excerpts = workspace.round_web_revision(&ticket).unwrap();
            assert_ne!(reviewed, excerpts);
            assert_no_connection(&model);
            match change {
                Change::Mask => {
                    workspace.clipboard.policy.terminal_logging =
                        masked_policy.terminal_logging.clone()
                }
                Change::InternetPolicy => workspace.clipboard.policy.ai_web_search_allowed = false,
                _ => {}
            }
            drop(update(&mut workspace, excerpts, web::Message::Include));
        }
        if matches!(
            change,
            Change::None | Change::InternetPolicyBeforeSend | Change::PreferenceBeforeSend
        ) {
            let Some(Action::Output(Message::JobAgentResend(Resend::Model(permit)))) =
                block_on(worker.next())
            else {
                panic!("The continuation must recheck advisory authority");
            };
            if matches!(change, Change::InternetPolicyBeforeSend) {
                workspace.clipboard.policy.ai_web_search_allowed = false;
            }
            if matches!(change, Change::PreferenceBeforeSend) {
                let baseline = Configuration::load_fixture_from(&root).unwrap();
                let mut changed = serde_json::to_value(&baseline).unwrap();
                changed["general"]["agent_always_offer_web_tools"] = false.into();
                changed["general"]["agent_web_offer_revision"] = 2.into();
                let changed: Configuration = serde_json::from_value(changed).unwrap();
                changed.save_fixture_at(&root, &baseline).unwrap();
                workspace
                    .set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
            }
            drop(workspace.answer_scheduler_agent_resend(Resend::Model(permit)));
        }
        if matches!(change, Change::None) {
            let mut socket = test_support::accept(&model);
            let continuation = test_support::request(&mut socket);
            let result = &continuation["messages"][3];
            assert_eq!(result["tool_call_id"], "advisory-page");
            let content: serde_json::Value =
                serde_json::from_str(result["content"].as_str().unwrap()).unwrap();
            assert_eq!(content["approved_input"], PAGE);
            assert!(
                content["evidence"]
                    .as_str()
                    .unwrap()
                    .contains("Public page fixture excerpt for evidence review.")
            );
            write_model(
                &mut socket,
                json!({"content":json!({
                "status":"done","summary":"Reviewed advisory fixture","userMessage":"Complete."
            }).to_string()}),
            );
        }

        let expected = match change {
            Change::None => None,
            Change::Vault => Some(WEB_VAULT_CHANGED),
            Change::Mask => Some(WEB_MASK_CHANGED),
            Change::InternetPolicy | Change::InternetPolicyBeforeSend => Some(WEB_POLICY_BLOCKED),
            Change::PreferenceBeforeSend => Some(
                "The scheduled web review changed or ended. Start a new firing and review it again.",
            ),
        };
        assert!(workspace.model_web_ticket(key).is_none());
        loop {
            match block_on(worker.next()) {
                Some(Action::Output(Message::JobAgentResend(Resend::Model(permit)))) => {
                    assert!(
                        expected.is_none(),
                        "Revoked review must not reach the model"
                    );
                    drop(workspace.answer_scheduler_agent_resend(Resend::Model(permit)));
                }
                Some(Action::Output(Message::JobFinished(completion, journals))) => {
                    assert_eq!(completion.id, id);
                    assert_eq!(
                        completion.outcome,
                        if expected.is_some() {
                            RunOutcome::Blocked
                        } else {
                            RunOutcome::Success
                        },
                        "{completion:?}"
                    );
                    let report = completion.swarm_report.as_ref().unwrap();
                    assert_eq!(report.targets.len(), 1);
                    if let Some(reason) = expected {
                        assert_eq!(report.targets[0].detail, format!("AI: {reason}"));
                    } else {
                        assert!(
                            report.targets[0]
                                .detail
                                .contains("Reviewed advisory fixture")
                        );
                    }
                    drop(workspace.update(Message::JobFinished(completion, journals)));
                    break;
                }
                action => {
                    panic!("Expected a model permit or completed advisory job, got {action:?}")
                }
            }
        }
        workspace.clipboard.policy.ai_web_search_allowed = true;
        workspace.clipboard.policy.terminal_logging = original_mask.clone();
        assert_no_connection(&model);
        assert_no_connection(&ssh);
    }
}

fn start_after_preflight(
    workspace: &mut Workspace,
    view: window::Id,
    model: &TcpListener,
) -> Task<Message> {
    let task = workspace.update_scheduler(view, SchedulerMessage::Run(false));
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let authorize = |workspace: &mut Workspace, action| {
        let Some(Action::Output(Message::ScheduledFiringWeb(event @ WebEvent::Authorize(..)))) =
            action
        else {
            panic!("Expected the preflight research permit");
        };
        drop(workspace.update_firing_web(event));
    };
    authorize(workspace, block_on(stream.next()));
    let mut socket = test_support::accept(model);
    let _ = test_support::request(&mut socket);
    write_model(
        &mut socket,
        json!({"content":json!({"query":TOPIC}).to_string()}),
    );
    authorize(workspace, block_on(stream.next()));
    let Some(Action::Output(Message::ScheduledFiringWeb(event @ WebEvent::UnattendedQuery(..)))) =
        block_on(stream.next())
    else {
        panic!("Expected the reviewed preflight query");
    };
    let lookup = workspace.update_firing_web(event);
    let mut lookup = iced_runtime::task::into_stream(lookup).unwrap();
    authorize(workspace, block_on(lookup.next()));
    authorize(workspace, block_on(lookup.next()));
    let Some(Action::Output(Message::ScheduledFiringWeb(event @ WebEvent::UnattendedResults(..)))) =
        block_on(lookup.next())
    else {
        panic!("Expected preflight excerpts");
    };
    workspace.update_firing_web(event)
}

fn permit_model(workspace: &mut Workspace, action: Option<Action<Message>>) -> Ticket {
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::ModelDispatch(
        ticket,
        permit,
    ))))) = action
    else {
        panic!("Expected the exact model request permit");
    };
    drop(workspace.update_firing(Event(Progress::ModelDispatch(ticket.clone(), permit))));
    ticket
}

fn after_model_response(
    workspace: &mut Workspace,
    stream: &mut (impl iced::futures::Stream<Item = Action<Message>> + Unpin),
) -> Option<Action<Message>> {
    loop {
        match block_on(stream.next()) {
            Some(Action::Output(Message::ScheduledAgent(
                event @ Event(Progress::ModelDispatch(..)),
            ))) => drop(workspace.update_firing(event)),
            other => return other,
        }
    }
}

fn write_model(socket: &mut std::io::BufReader<std::net::TcpStream>, message: serde_json::Value) {
    write_research_json(socket, &json!({"choices":[{"message":message}]}));
    socket
        .get_mut()
        .shutdown(std::net::Shutdown::Write)
        .unwrap();
}
