use kortty_jobs::{Job, JobGraph, JobId, SchedulerStore};

use super::*;
use crate::ai_manager::Configuration;
use crate::job_executor::agent::{self as worker, Context, Resend};
use crate::job_executor::automation::Plan;
use crate::local_ai::{self, test_support};
use crate::scheduler_window::{Message, Page, SchedulerWindow, agent, agent::proposal::Prompt};
use crate::session_journal::automation::{Caps, CaptureSettings, Kind, Run};
use crate::session_journal::{CaptureKind, tree::RunStatus};
use crate::terminal_agent::swarm::headless::{Execution, Review};
use crate::terminal_logging::{Redactor, Secrets};

const SECRET: &str = "swarm-read-only-fixture-secret";
const GOAL: &str = "Compare the fixed probe evidence for swarm-read-only-fixture-secret";
const MASKED_GOAL: &str = "Compare the fixed probe evidence for ***";
const INJECTION_MARKER: &str = "rejected-swarm-model-command";
const GUIDANCE: [&str; 2] = [
    "Summarize the first fixture evidence.",
    "Summarize the second fixture evidence.",
];

struct SwarmFixture {
    servers: [Sshd; 2],
    connections: Store,
    runtime: SchedulerWindow,
    request: RunRequest<SshCommand>,
    trust: TrustStore,
    jobs: PathBuf,
    prompts: Vec<Prompt>,
}

impl SwarmFixture {
    fn new() -> Self {
        Self::with_guidance(None)
    }

    fn with_guidance(model: Option<&TcpListener>) -> Self {
        let servers = [Sshd::new(), Sshd::new()];
        let root = &servers[0].profile.0;
        let (_, seed, mut connections) =
            scheduled_batch(root, &[&servers[0], &servers[1]], "true", Vec::new());
        let references = seed.action.targets().cloned().collect::<Vec<_>>();
        let trust = TrustStore::from_pins(servers.iter().map(Sshd::pin).collect()).unwrap();
        let action: SshCommand = serde_json::from_value(json!({
            "connection": references[0],
            "additional_connections": [references[1]],
            "working_directory": root,
            "agent": {"kind":"swarm","probe":"swarm_read_only","profile":9,"goal":GOAL},
            "automation_journal": {"enabled":true},
        }))
        .unwrap();
        let jobs = root.join("read-only-swarm-jobs.json");
        let job = Job::new(
            JobId::new("read-only-swarm").unwrap(),
            "Read-only Swarm",
            action,
        )
        .unwrap();
        SchedulerStore::open(&jobs)
            .unwrap()
            .commit_with_actions(&JobGraph::new(vec![job]).unwrap(), &[], &[])
            .unwrap();
        let mut runtime = SchedulerWindow::open(jobs.clone()).unwrap();
        runtime.set_execution_allowed(true);
        let mut view = runtime.new_view();
        runtime.update_view(&mut view, Message::Page(Page::Action));
        let review = runtime.begin_agent_review(&view).unwrap();
        let profile = agent::ProfileChoice {
            id: crate::ai_manager::ProfileRef::Numeric(9),
            name: "Read-only Swarm fixture".into(),
            model: "fixture:local".into(),
            destination: "Loopback test endpoint".into(),
            policy_model_fingerprint: None,
            reviewed_https: false,
            reviewed_cli: None,
        };
        let targets = references
            .iter()
            .map(|reference| {
                agent::Target::capture(reference, connections.get(reference.id).unwrap(), &trust)
                    .unwrap()
            })
            .collect();
        let prompts = if let Some(model) = model {
            let config: Configuration = serde_json::from_value(json!({
                "profiles": {"profiles": [{
                    "id": 9, "name": "Read-only Swarm fixture", "model": "fixture:local",
                    "endpoint": model.local_addr().unwrap().to_string(), "selection_limit": 8192
                }], "default": 9, "last_id": 9},
                "skills": {"last_id": 52, "skills": {
                    "51": {"name": "First fixture", "target": "CONNECTION", "enabled": true, "content": GUIDANCE[0]},
                    "52": {"name": "Second fixture", "target": "CONNECTION", "enabled": true, "content": GUIDANCE[1]}
                }}
            })).unwrap();
            config
                .save_fixture_at(root, &Configuration::default())
                .unwrap();
            let config = Configuration::load_fixture_from(root).unwrap();
            let (transport, limit) = config.agent_transport(9).unwrap();
            let revision = config.agent_profile_revision(9).unwrap();
            let profile = agent::ProfileChoice::available(&config)
                .into_iter()
                .next()
                .unwrap();
            let redactor =
                Redactor::for_journal(Secrets::default().seal().unwrap(), fixture_log_policy());
            let mut pins = Vec::new();
            let mut prompts = Vec::new();
            for (reference, id) in references.iter().zip([51, 52]) {
                let mut connection = connections.get(reference.id).unwrap().clone();
                connection.terminal.ai_skills = vec![id];
                connections.save(connection).unwrap();
                pins.push((reference.id, vec![id]));
                let guidance = config.reviewed_agent_pinned_skill_guidance(&[id]).text;
                prompts.push(
                    Prompt::new_advisory(
                        transport.clone(),
                        limit,
                        GOAL,
                        root.to_str().unwrap(),
                        reference.id,
                        redactor.clone(),
                        fixture_mask,
                    )
                    .unwrap()
                    .bind_profile(revision)
                    .with_firing_skill_guidance(&guidance)
                    .unwrap(),
                );
            }
            runtime
                .finish_guided_advisory_review(
                    &mut view,
                    review,
                    profile,
                    targets,
                    prompts.clone(),
                    MASKED_GOAL.into(),
                    revision,
                    pins,
                )
                .unwrap();
            prompts
        } else {
            runtime
                .finish_swarm_read_only_review(
                    &mut view,
                    review,
                    profile,
                    targets,
                    MASKED_GOAL.into(),
                    [7; 32],
                )
                .unwrap();
            Vec::new()
        };
        let ticket = view.firing_review_ticket().unwrap().to_owned();
        runtime.update_view(&mut view, Message::ApproveFiringRequest(ticket));
        runtime.update_view(&mut view, Message::Save);
        drop(runtime);

        let mut runtime = SchedulerWindow::open(jobs.clone()).unwrap();
        runtime.set_execution_allowed(true);
        let mut view = runtime.new_view();
        let request = runtime
            .update_view(&mut view, Message::Run(false))
            .pop()
            .unwrap();
        assert!(runtime.active_agent_run_approved(&request));
        if model.is_none() {
            let grant = request
                .action
                .agent
                .as_ref()
                .unwrap()
                .approval
                .as_ref()
                .unwrap()
                .advisory
                .as_ref()
                .unwrap();
            let receipt = serde_json::to_value(grant).unwrap();
            assert_eq!(receipt.as_object().unwrap().len(), 2);
            assert_eq!(receipt["profile_revision"], json!(vec![7_u8; 32]));
            assert!(receipt["question"].is_array() && grant.matches_question(MASKED_GOAL));
            assert!(!grant.is_guided());
        }
        Self {
            servers,
            connections,
            runtime,
            request,
            trust,
            jobs,
            prompts,
        }
    }

    fn context(&self, transport: local_ai::Profile, trust: TrustStore) -> Context {
        let directory = self.request.action.working_directory.as_deref().unwrap();
        let targets = self
            .request
            .action
            .targets()
            .map(|reference| {
                let connection = self.connections.get(reference.id).unwrap();
                Ok(Execution {
                    review: Arc::new(
                        Review::capture(reference.clone(), directory.into(), &self.trust).unwrap(),
                    ),
                    current: reference.clone(),
                    credentials: Credentials::resolve_directory_browser(
                        reference,
                        connection,
                        &ServerAccess::default(),
                        |_, _, _| Err("unexpected fixture credential"),
                    )
                    .unwrap(),
                    startup: connection.startup,
                })
            })
            .collect();
        Context {
            transport,
            context_limit: 8192,
            targets,
            target_vault_revisions: Vec::new(),
            connection_store: self.connections.path().to_owned(),
            trust,
            log_policy: fixture_log_policy(),
            mask: fixture_mask,
            proposal: None,
            journal: None,
        }
    }

    fn journal_plan(&self, capture_input: bool) -> Plan {
        let mut options = self.request.action.automation_journal.clone().unwrap();
        assert!(options.enabled);
        options.capture_input = capture_input;
        let run = Run::new(
            Kind::Job,
            self.request.job.as_str().to_owned(),
            "Read-only Swarm".into(),
            "AI_SWARM",
            jiff::Timestamp::try_from(self.request.started_at).unwrap(),
            options,
            Caps::default(),
        )
        .unwrap();
        Plan::new(
            CaptureSettings {
                root: self.servers[0].profile.0.join("swarm-journals"),
                format: Default::default(),
                max_parts: None,
                markers: Default::default(),
                stopped: Default::default(),
            },
            run,
            fixture_log_policy(),
        )
    }
}

fn fixture_mask(redactor: &Redactor, input: &str) -> Result<String, String> {
    redactor
        .journal_text(input, Instant::now() + Duration::from_secs(1))
        .map(|masked| masked.replace(SECRET, "***"))
        .ok_or_else(|| "fixture mask failed".into())
}

fn fixture_log_policy() -> crate::terminal_logging::Policy {
    let mut policy = crate::terminal_logging::Policy::default();
    let document = format!("[[session-journal.replace]]\npattern = '{SECRET}'")
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    policy.read_rule(document.as_table()).unwrap();
    policy
}

fn fixture_policy() -> crate::clipboard_policy::ClipboardPolicy {
    crate::clipboard_policy::ClipboardPolicy {
        terminal_logging: fixture_log_policy(),
        ..Default::default()
    }
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_read_only_swarm_isolates_target_evidence_and_rejects_model_commands() {
    let mut fixture = SwarmFixture::new();
    let targets = fixture
        .request
        .action
        .targets()
        .map(|reference| reference.id)
        .collect::<Vec<_>>();
    let markers = [
        logged_probe(&fixture.servers[0], "first-probe-evidence"),
        logged_probe(&fixture.servers[1], "second-probe-evidence"),
    ];
    let daemons = fixture.servers.each_ref().map(Sshd::spawn);
    let (model, transport) = test_support::listener();
    let journal = fixture.journal_plan(false);
    let mut context = fixture.context(transport, fixture.trust.clone());
    context.journal = Some(journal.clone());
    let (send, permits) = async_channel::bounded(1);
    let done = worker::spawn_with_policy(fixture.request, Ok(context), send, fixture_policy);
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut observed = Vec::new();
    let completion = loop {
        match next_event(&done, &permits, deadline) {
            Event::Done(completion) => break completion,
            Event::Permit(permit) => {
                let [target] = permit.scope.evidence_targets.as_slice() else {
                    panic!("each Swarm model send must carry one exact target");
                };
                assert!(
                    fixture
                        .runtime
                        .active_agent_run_approved(&permit.scope.request)
                );
                let target = *target;
                let repair = observed.contains(&target);
                assert_no_connection(&model);
                permit.settle(Ok(()));
                let mut connection = test_support::accept(&model);
                let wire = test_support::request(&mut connection);
                assert_request(&wire, target, targets[0], repair);
                let response = if target == targets[0] {
                    assert!(!repair);
                    json!({
                        "status":"done","summary":"First target advice",
                        "userMessage":format!("First advisory result for {SECRET}.")
                    })
                } else if repair {
                    assert_eq!(target, targets[1]);
                    json!({
                        "status":"blocked","summary":"Second target refused",
                        "userMessage":"Second advisory result is blocked."
                    })
                } else {
                    assert_eq!(target, targets[1]);
                    json!({
                        "status":"run_commands","summary":"Unsafe response",
                        "userMessage":"Run the generated command.",
                        "commands":[{
                            "command":format!("touch {INJECTION_MARKER}"),
                            "purpose":"Fixture must never reach SSH",
                            "risk":"read_only"
                        }]
                    })
                };
                connection
                    .get_mut()
                    .write_all(
                        test_support::fixture_response("200 OK", "", &response.to_string())
                            .as_bytes(),
                    )
                    .unwrap();
                observed.push(target);
            }
        }
    };
    assert_eq!(observed, [targets[0], targets[1], targets[1]]);
    assert_no_connection(&model);
    for (daemon, marker) in daemons.into_iter().zip(markers) {
        assert_login(daemon, deadline);
        let actual = fs::read_to_string(marker).unwrap();
        assert_eq!(actual.lines().count(), 1, "{actual}");
        assert!(actual.contains("/usr/bin/uname") && !actual.contains(INJECTION_MARKER));
    }
    assert!(!fixture.servers[0].profile.0.join(INJECTION_MARKER).exists());
    assert_eq!(
        completion.outcome,
        RunOutcome::Blocked,
        "{:?}",
        completion.detail
    );
    let run = completion.id;
    fixture
        .runtime
        .update_runtime(Message::Finished(completion));
    drop(fixture.runtime);
    let store = SchedulerStore::open(&fixture.jobs).unwrap();
    let report = store.journal_swarm_report(run).unwrap();
    assert_eq!(report.question, MASKED_GOAL);
    assert_eq!(
        report
            .targets
            .iter()
            .map(|row| row.target)
            .collect::<Vec<_>>(),
        targets
    );
    assert_eq!(report.targets[0].outcome, RunOutcome::Success);
    assert!(report.targets[0].detail.contains("First target advice"));
    assert_eq!(report.targets[1].outcome, RunOutcome::Blocked);
    let detail = store.journal_detail(run).unwrap();
    let first = detail.find(&format!("Profile {}:", targets[0])).unwrap();
    let second = detail.find(&format!("Profile {}:", targets[1])).unwrap();
    assert!(first < second, "{detail}");
    assert!(
        detail[first..second].contains("First target advice"),
        "{detail}"
    );
    assert!(
        detail[second..].to_ascii_lowercase().contains("blocked"),
        "{detail}"
    );
    for marker in [
        SECRET,
        INJECTION_MARKER,
        "first-probe-evidence",
        "second-probe-evidence",
    ] {
        assert!(!detail.contains(marker), "{detail}");
        assert!(
            report
                .targets
                .iter()
                .all(|row| !row.detail.contains(marker))
        );
    }
    let mut artifacts = journal.take_completed();
    assert_eq!(artifacts.len(), 2);
    artifacts.sort_by_key(|artifact| artifact.outcome == RunStatus::Blocked);
    let mut summaries = Vec::new();
    for (index, artifact) in artifacts.into_iter().enumerate() {
        assert!(artifact.complete);
        assert_eq!(
            artifact.outcome,
            [RunStatus::Success, RunStatus::Blocked][index]
        );
        let summary = artifact.source.read(None).unwrap().unwrap().summary;
        assert_eq!(summary.automation_action.as_deref(), Some("AI_SWARM"));
        assert_eq!(summary.source_id.as_deref(), Some("read-only-swarm"));
        assert_eq!(summary.run_status, Some(artifact.outcome));
        summaries.push(summary);
        let page = artifact.source.capture_page(None, None, None).unwrap();
        assert!(
            page.records
                .iter()
                .all(|record| record.kind != CaptureKind::Input)
        );
        let output = page
            .records
            .iter()
            .filter(|record| record.kind == CaptureKind::Output)
            .map(|record| record.text.as_str())
            .collect::<String>();
        let evidence = ["first-probe-evidence", "second-probe-evidence"];
        assert!(output.contains(evidence[index]) && output.contains("***"));
        assert!(!output.contains(SECRET) && !output.contains(evidence[1 - index]));
    }
    assert!(summaries[0].run_id.is_some());
    assert_eq!(summaries[0].run_id, summaries[1].run_id);
    assert_ne!(summaries[0].id, summaries[1].id);
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_guided_swarm_preserves_each_review_and_revokes_stale_repair() {
    let (model, _) = test_support::listener();
    let mut fixture = SwarmFixture::with_guidance(Some(&model));
    let targets = fixture
        .request
        .action
        .targets()
        .map(|reference| reference.id)
        .collect::<Vec<_>>();
    let markers = [
        logged_probe(&fixture.servers[0], "first-probe-evidence"),
        logged_probe(&fixture.servers[1], "second-probe-evidence"),
    ];
    let daemons = fixture.servers.each_ref().map(Sshd::spawn);
    let context = fixture.context(fixture.prompts[0].transport.clone(), fixture.trust.clone());
    let (send, permits) = async_channel::bounded(1);
    let done = worker::spawn_with_policy(fixture.request, Ok(context), send, fixture_policy);
    let deadline = Instant::now() + Duration::from_secs(25);

    for (index, target) in targets.iter().copied().enumerate() {
        let Event::Permit(permit) = next_event(&done, &permits, deadline) else {
            panic!("the reviewed target {target} must reach the model authority boundary");
        };
        assert_eq!(permit.scope.evidence_targets, [target]);
        assert!(
            fixture
                .runtime
                .active_agent_run_approved(&permit.scope.request)
        );
        assert_no_connection(&model);
        permit.settle(Ok(()));
        let mut socket = test_support::accept(&model);
        let wire = test_support::request(&mut socket);
        assert_request(&wire, target, targets[0], false);
        let prompt = &fixture.prompts[index];
        assert_eq!(wire["max_tokens"], 2048);
        let system = wire["messages"][0]["content"].as_str().unwrap();
        assert_eq!(system, prompt.firing_system());
        assert!(system.contains(GUIDANCE[index]) && !system.contains(GUIDANCE[1 - index]));
        let input = wire["messages"][1]["content"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(input).unwrap();
        let evidence = parsed["diagnostics"][0]["output"].as_str().unwrap();
        assert_eq!(input, prompt.advisory_input(Some(evidence)).unwrap());
        let response = if index == 0 {
            let mut stale = fixture.connections.get(target).unwrap().clone();
            stale.terminal.ai_skills.clear();
            fixture.connections.save(stale).unwrap();
            json!({"status":"run_commands","summary":"Rejected proposal", "userMessage":"Run the command.",
                "commands":[{"command":format!("touch {INJECTION_MARKER}"),"purpose":"Fixture injection","risk":"read_only"}]})
        } else {
            json!({"status":"done","summary":"Other target advice", "userMessage":"Other target evidence remains available."})
        };
        socket
            .get_mut()
            .write_all(
                test_support::fixture_response("200 OK", "", &response.to_string()).as_bytes(),
            )
            .unwrap();
    }

    let Event::Done(completion) = next_event(&done, &permits, deadline) else {
        panic!("a stale review must stop before a repair request");
    };
    assert_no_connection(&model);
    for (daemon, marker) in daemons.into_iter().zip(markers) {
        assert_login(daemon, deadline);
        let actual = fs::read_to_string(marker).unwrap();
        assert_eq!(actual.lines().count(), 1, "{actual}");
        assert!(actual.contains("/usr/bin/uname") && !actual.contains(INJECTION_MARKER));
    }
    assert_eq!(completion.outcome, RunOutcome::Blocked);
    let report = completion.swarm_report.unwrap();
    assert_eq!(report.question, MASKED_GOAL);
    assert_eq!(report.targets[0].outcome, RunOutcome::Blocked);
    assert!(report.targets[0].detail.contains("AI skill pins changed"));
    assert_eq!(report.targets[1].outcome, RunOutcome::Success);
    assert!(report.targets[1].detail.contains("Other target advice"));
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_read_only_swarm_blocks_a_stale_pin_and_cancels_before_model_send() {
    let mut fixture = SwarmFixture::new();
    let (model, transport) = test_support::listener();
    let (stale_endpoint, _) = fixture.servers[0].pin();
    let live_pin = fixture.servers[1].pin();
    let trust =
        TrustStore::from_pins(vec![(stale_endpoint, live_pin.1.clone()), live_pin]).unwrap();
    let journal = fixture.journal_plan(true);
    let mut context = fixture.context(transport, trust);
    context.journal = Some(journal.clone());
    let (stale, live) = {
        let mut targets = fixture.request.action.targets();
        (targets.next().unwrap().id, targets.next().unwrap().id)
    };
    let run = fixture.request.id;
    let daemon = fixture.servers[1].spawn();
    let (send, permits) = async_channel::bounded(1);
    let done = worker::spawn_with_policy(fixture.request, Ok(context), send, fixture_policy);
    let deadline = Instant::now() + Duration::from_secs(20);
    let Event::Permit(permit) = next_event(&done, &permits, deadline) else {
        panic!("the other pinned target must reach the model authority boundary");
    };
    assert_eq!(permit.scope.evidence_targets, [live]);
    assert!(fixture.runtime.cancel_menu_run(run.value()));
    permit.settle(Ok(()));
    let Event::Done(completion) = next_event(&done, &permits, deadline) else {
        panic!("a cancelled Swarm cannot request more model authority");
    };
    assert_eq!(completion.outcome, RunOutcome::Cancelled);
    assert!(completion.swarm_report.is_none());
    assert_no_connection(&fixture.servers[0].listener);
    assert_no_connection(&model);
    assert_login(daemon, deadline);
    fixture
        .runtime
        .update_runtime(Message::Finished(completion));
    drop(fixture.runtime);
    let store = SchedulerStore::open(&fixture.jobs).unwrap();
    assert_eq!(store.journal().len(), 1);
    assert!(store.journal_swarm_report(run).is_none());
    assert_eq!(
        store.journal_record(run).unwrap().outcome,
        RunOutcome::Cancelled
    );
    let detail = store.journal_detail(run).unwrap();
    assert!(
        detail.contains(&format!("Profile {stale}: Blocked")),
        "{detail}"
    );
    assert!(
        detail.contains(&format!("Profile {live}: Cancelled")),
        "{detail}"
    );
    let artifacts = journal.take_completed();
    let [artifact] = artifacts.as_slice() else {
        panic!("only the current reviewed target can open a session journal");
    };
    assert_eq!(artifact.outcome, RunStatus::Cancelled);
    let page = artifact.source.capture_page(None, None, None).unwrap();
    let input = page
        .records
        .iter()
        .find(|record| record.kind == CaptureKind::Input);
    assert!(input.unwrap().text.contains("/usr/bin/uname"));
}

fn assert_request(wire: &serde_json::Value, target: u64, first: u64, repair: bool) {
    let messages = wire["messages"].as_array().unwrap();
    assert!(
        messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("no execution capability")
    );
    assert!(!wire.to_string().contains(SECRET));
    let content = messages[1]["content"].as_str().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(content).unwrap();
    let input = if repair {
        serde_json::from_str(parsed["reviewedContext"].as_str().unwrap()).unwrap()
    } else {
        parsed
    };
    assert_eq!(input["question"], MASKED_GOAL);
    let evidence = input["diagnostics"].as_array().unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0]["target"], target);
    let output = evidence[0]["output"].as_str().unwrap();
    let (expected, other) = if target == first {
        ("first-probe-evidence", "second-probe-evidence")
    } else {
        ("second-probe-evidence", "first-probe-evidence")
    };
    assert!(output.contains(expected) && output.contains("***") && !output.contains(other));
}

fn logged_probe(server: &Sshd, evidence: &str) -> PathBuf {
    let root = &server.profile.0;
    let marker = root.join("actual-probes.txt");
    let script = root.join("log-probe.sh");
    let quote = |path: &Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    fs::write(
        &script,
        format!(
            "printf '%s\\n' \"$SSH_ORIGINAL_COMMAND\" >> {}\nprintf '%s\\n' '{evidence} {SECRET}'\nexec /bin/sh -c \"$SSH_ORIGINAL_COMMAND\"\n",
            quote(&marker)
        ),
    )
    .unwrap();
    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .open(&server.config)
            .unwrap(),
        "ForceCommand /bin/sh {}",
        quote(&script)
    )
    .unwrap();
    marker
}

enum Event {
    Permit(Resend),
    Done(Completion),
}

fn next_event(
    done: &async_channel::Receiver<Completion>,
    permits: &async_channel::Receiver<Resend>,
    deadline: Instant,
) -> Event {
    loop {
        if let Ok(completion) = done.try_recv() {
            return Event::Done(completion);
        }
        if let Ok(permit) = permits.try_recv() {
            return Event::Permit(permit);
        }
        assert!(
            Instant::now() < deadline,
            "read-only Swarm worker timed out"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn assert_login(daemon: JoinHandle<Output>, deadline: Instant) {
    while !daemon.is_finished() {
        assert!(Instant::now() < deadline, "fixture sshd did not exit");
        thread::sleep(Duration::from_millis(5));
    }
    let output = daemon.join().unwrap();
    let log = String::from_utf8_lossy(&output.stderr);
    assert_eq!(log.matches("Accepted publickey for ").count(), 1, "{log}");
}

fn assert_no_connection(listener: &TcpListener) {
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
}
