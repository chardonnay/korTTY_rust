use std::io::{ErrorKind, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use iced::futures::{StreamExt as _, executor::block_on};
use iced_runtime::Action;
use kortty_jobs::{Job, JobGraph, JobId, SchedulerStore};
use kortty_ssh::{Endpoint, HostKey};
use kortty_vault::{CredentialInput, Secret, Vault};
use serde_json::json;

use super::*;
use crate::ai_manager::Configuration;
use crate::connection_store::{Auth, Connection, SavedLogin};
use crate::local_ai::{self, test_support};
use crate::scheduler_window::{ConnectionRef, Message as SchedulerMessage, Page, SchedulerWindow};
use crate::snippets::tests::Profile;
use crate::ssh_dialog::LoginTarget;
use crate::vault_window::VaultWindow;

#[cfg(unix)]
mod openssh;
mod proposal_skills;

const HOME_MARKER: &str = "KORTTY_SCHEDULED_AGENT_REVIEW_FIXTURE";
const TEST_NAME: &str = "workspace::scheduler_access::firing::tests::scheduled_agent_firings_wait_for_exact_review_and_finish_once";
const PASSWORD: &str = "scheduled-agent-fixture-secret";
const GOAL: &str = "Inspect the fixture containing scheduled-agent-fixture-secret";
const SKILL_ID: u64 = 51;
const MASKED_SKILL: &str = "Use the *** fixture inventory.";

#[test]
fn scheduled_agent_firings_wait_for_exact_review_and_finish_once() {
    let Some(home) = std::env::var_os(HOME_MARKER).map(PathBuf::from) else {
        let scratch = Profile::new();
        let home = scratch.0.canonicalize().unwrap();
        let result = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(HOME_MARKER, &home)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("LOCALAPPDATA", &home)
            .env("XDG_CONFIG_HOME", &home)
            .output()
            .unwrap();
        let output = String::from_utf8_lossy(&result.stdout);
        assert!(
            result.status.success() && output.contains("1 passed"),
            "{output}\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    };
    let root = profile::root().unwrap();
    assert!(home.is_dir() && root.starts_with(&home) && !root.exists());
    std::fs::create_dir_all(&root).unwrap();

    let (model, _) = test_support::listener();
    let ssh = TcpListener::bind("127.0.0.1:0").unwrap();
    ssh.set_nonblocking(true).unwrap();
    let config: Configuration = serde_json::from_value(json!({
        "profiles": {
            "profiles": [{
                "id": 1, "name": "Scheduled fixture model",
                "endpoint": model.local_addr().unwrap().to_string(), "model": "fixture:local"
            }],
            "default": 1, "last_id": 1
        },
        "skills": {"last_id": SKILL_ID, "skills": {
            (SKILL_ID.to_string()): {
                "name": "Reviewed inventory", "target": "CONNECTION", "enabled": true,
                "content": format!("Use the {PASSWORD} fixture inventory.")
            }
        }}
    }))
    .unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();

    let (reference, vault) = saved_target(ssh.local_addr().unwrap().port());
    let mut connections = ConnectionStore::open(profile::connections_path().unwrap()).unwrap();
    let mut saved = connections.get(reference.id).unwrap().clone();
    saved.terminal.ai_skills = vec![SKILL_ID];
    connections.save(saved.clone()).unwrap();
    let mut without_skill = saved.clone();
    without_skill.terminal.ai_skills.clear();
    let jobs = profile::jobs_path().unwrap();
    let action: SshCommand = serde_json::from_value(json!({
        "connection": reference,
        "working_directory": "/srv",
        "agent": {"kind":"agent","probe":"per_firing","profile":1,"goal":GOAL}
    }))
    .unwrap();
    let job = Job::new(
        JobId::new("per-firing").unwrap(),
        "Review every start",
        action,
    )
    .unwrap();
    let mut store = SchedulerStore::open(&jobs).unwrap();
    store
        .commit_with_actions(&JobGraph::new(vec![job]).unwrap(), &[], &[])
        .unwrap();
    drop(store);
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
    let request_ticket = workspace.scheduler_views[&id]
        .firing_review_ticket()
        .unwrap()
        .to_owned();
    let _ = workspace.update_scheduler(id, SchedulerMessage::ApproveAgent);
    assert_eq!(
        workspace.scheduler_views[&id].firing_review_ticket(),
        Some(request_ticket.as_str())
    );
    connections.save(without_skill.clone()).unwrap();
    let _ = workspace.update_scheduler(
        id,
        SchedulerMessage::ApproveFiringRequest(request_ticket.clone()),
    );
    assert!(
        workspace.scheduler_views[&id]
            .firing_review_ticket()
            .is_none()
    );
    assert!(!workspace.scheduler_views[&id].dirty());
    connections.save(saved.clone()).unwrap();
    let _ = workspace.update_scheduler(id, SchedulerMessage::ReviewAgent);
    let current_ticket = workspace.scheduler_views[&id]
        .firing_review_ticket()
        .unwrap()
        .to_owned();
    assert_ne!(request_ticket, current_ticket);
    let _ = workspace.update_scheduler(id, SchedulerMessage::ApproveFiringRequest(request_ticket));
    assert_eq!(
        workspace.scheduler_views[&id].firing_review_ticket(),
        Some(current_ticket.as_str())
    );
    let _ = workspace.update_scheduler(id, SchedulerMessage::ApproveFiringRequest(current_ticket));
    let _ = workspace.update_scheduler(id, SchedulerMessage::Save);
    drop(workspace.scheduler.take());
    let mut scheduler = SchedulerWindow::open(jobs.clone()).unwrap();
    scheduler.set_execution_allowed(true);
    workspace.scheduler_views.insert(id, scheduler.new_view());
    workspace.scheduler = Some(scheduler);
    let _ = workspace.update_scheduler(id, SchedulerMessage::Page(Page::Journal));
    assert_no_connection(&model);
    assert_no_connection(&ssh);

    let first = generate(&mut workspace, id, &model, &ssh, reference.id, "pwd -P");
    let first_review = workspace
        .scheduler
        .as_ref()
        .unwrap()
        .firing_reviews
        .get(&first, Instant::now())
        .unwrap();
    let prompt = Arc::clone(&first_review.context().prompt);
    assert!(
        prompt
            .firing_skill_guidance()
            .unwrap()
            .contains(MASKED_SKILL)
    );
    assert_eq!(first_review.proposal().unwrap()[0].command, "pwd -P");
    select(&mut workspace, id, &first);
    let _ = workspace.update_scheduler(id, SchedulerMessage::RejectRunProposal(first.clone()));
    assert_finished(&jobs, &first, RunOutcome::Cancelled, 1);
    let _ = workspace.update_scheduler(id, SchedulerMessage::RejectRunProposal(first.clone()));
    assert_finished(&jobs, &first, RunOutcome::Cancelled, 1);

    let second = generate(&mut workspace, id, &model, &ssh, reference.id, "id -un");
    assert_ne!(first, second);
    select(&mut workspace, id, &second);
    let _ = workspace.update_scheduler(id, SchedulerMessage::ApproveRunProposal(first));
    let scheduler = workspace.scheduler.as_ref().unwrap();
    assert_eq!(
        scheduler
            .firing_reviews
            .get(&second, Instant::now())
            .unwrap()
            .phase(),
        Phase::AwaitingReview
    );
    assert_eq!(workspace.scheduler_workers, 0);
    assert_no_connection(&ssh);
    let _ = workspace.update_scheduler(id, SchedulerMessage::CancelRun(second.run()));
    assert_finished(&jobs, &second, RunOutcome::Cancelled, 2);

    // Backdate a live reservation's review window to exercise expiry without
    // waiting for the real UI deadline or opening another model connection.
    let scheduler = workspace.scheduler.as_mut().unwrap();
    let view = workspace.scheduler_views.get_mut(&id).unwrap();
    let request = scheduler
        .update_view(view, SchedulerMessage::Run(false))
        .pop()
        .unwrap();
    let run = request.id;
    let mut sequence = Sequence::new(request, 1);
    let (target_index, request) = sequence.claim(0).unwrap();
    assert_eq!(target_index, 0);
    assert!(
        workspace
            .scheduler_agent_sequences
            .insert(run, sequence)
            .is_none()
    );
    let (session, cancel) = local_ai::Session::begin(prompt.transport.clone()).unwrap();
    let past = Instant::now() - Duration::from_secs(2);
    let deadline = past + Duration::from_secs(1);
    let expired = scheduler
        .firing_reviews
        .begin(
            request,
            target_index,
            1,
            firing::Context {
                prompt,
                connection_store: profile::connections_path().unwrap(),
                vault_revision: None,
                shared_secrets: None,
                initial_probe: false,
                _cancel: Some(cancel),
                skill_usage: None,
            },
            past,
            deadline,
        )
        .unwrap();
    assert!(scheduler.firing_reviews.publish(
        &expired,
        vec![Item {
            target: reference.id,
            command: "pwd -P".into(),
            purpose: "Fixture".into()
        }],
        past,
        deadline,
    ));
    let _ = workspace.expire_firing();
    assert_finished(&jobs, &expired, RunOutcome::Blocked, 3);
    assert_eq!(
        persisted_journal(&jobs)["journal_details"][expired.run().value().to_string()],
        "The command review expired before SSH execution."
    );
    let _ = workspace.update_firing(Event(Progress::Generated(
        expired.clone(),
        Ok(Decision::Done {}),
    )));
    assert_finished(&jobs, &expired, RunOutcome::Blocked, 3);
    drop(session);

    let task = workspace.update_scheduler(id, SchedulerMessage::Run(false));
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::ModelDispatch(
        revoked,
        permit,
    ))))) = block_on(stream.next())
    else {
        panic!("the firing must request permission before dispatch");
    };
    connections.save(without_skill).unwrap();
    let denial = "The scheduled AI skill guidance changed. Review this job again.";
    assert_eq!(
        workspace.current_firing(&revoked, Phase::Generating).err(),
        Some(denial)
    );
    // Deliver a stale permit to exercise the worker's independent saved-state check.
    permit.try_send(()).unwrap();
    permit.close();
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::Generated(
        completed,
        result,
    ))))) = block_on(stream.next())
    else {
        panic!("the revoked firing must return a generation failure");
    };
    assert_eq!(revoked, completed);
    assert_eq!(result.as_ref().err().map(String::as_str), Some(denial));
    let _ = workspace.update_firing(Event(Progress::Generated(completed, result)));
    assert_finished(&jobs, &revoked, RunOutcome::Blocked, 4);
    assert_eq!(
        persisted_journal(&jobs)["journal_details"][revoked.run().value().to_string()],
        denial
    );
    assert_no_connection(&model);
    assert_no_connection(&ssh);
    connections.save(saved).unwrap();

    let locked = generate(&mut workspace, id, &model, &ssh, reference.id, "uname -s");
    select(&mut workspace, id, &locked);
    workspace.vault = None;
    let _ = workspace.update_scheduler(id, SchedulerMessage::ApproveRunProposal(locked.clone()));
    assert_finished(&jobs, &locked, RunOutcome::Blocked, 5);
    assert_eq!(
        persisted_journal(&jobs)["journal_details"][locked.run().value().to_string()],
        "The SSH credential vault changed or was locked."
    );
    assert_no_connection(&model);
    assert_no_connection(&ssh);
}

fn saved_target(port: u16) -> (ConnectionRef, VaultWindow) {
    let vault_path = profile::vault_path().unwrap();
    std::fs::create_dir_all(vault_path.parent().unwrap()).unwrap();
    let mut vault = Vault::create(&vault_path, b"scheduled-agent-fixture-master").unwrap();
    let credential = vault
        .save(
            CredentialInput {
                label: "Scheduled fixture credential".into(),
                username: "fixture".into(),
                server_pattern: "127.0.0.1".into(),
                ..CredentialInput::default()
            },
            Secret::new(PASSWORD.into()),
        )
        .unwrap();
    let mut store = ConnectionStore::open(profile::connections_path().unwrap()).unwrap();
    let id = store
        .save(Connection {
            id: 0,
            name: "Scheduled fixture server".into(),
            tag: String::new(),
            login: SavedLogin {
                endpoint: LoginTarget {
                    host: "127.0.0.1".into(),
                    port,
                    username: "fixture".into(),
                },
                auth: Auth::VaultPassword(credential),
                keyboard_interactive: false,
            },
            jump: None,
            emulation: kortty_terminal::TerminalProfile::Xterm.term_name().into(),
            tunnels: Vec::new(),
            incognito: false,
            isolation: Default::default(),
            startup: Default::default(),
            logging: Default::default(),
            terminal_effect: None,
            command_timestamps: None,
            tab_color: None,
            paste_warning: None,
            window_geometry: None,
            terminal: Default::default(),
        })
        .unwrap();
    let key = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
    )
    .unwrap();
    crate::ssh_connection::trust_store()
        .unwrap()
        .remember(&Endpoint::new("127.0.0.1", port).unwrap(), &key)
        .unwrap();
    (
        ConnectionRef::from(store.get(id).unwrap()),
        VaultWindow::with_unlocked_fixture(vault_path, vault),
    )
}

fn generate(
    workspace: &mut Workspace,
    id: window::Id,
    model: &TcpListener,
    ssh: &TcpListener,
    target: u64,
    command: &str,
) -> Ticket {
    let task = workspace.update_scheduler(id, SchedulerMessage::Run(false));
    let mut stream = iced_runtime::task::into_stream(task).unwrap();
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::ModelDispatch(
        ticket,
        permit,
    ))))) = block_on(stream.next())
    else {
        panic!("the firing must request permission for the model dispatch");
    };
    assert_no_connection(model);
    assert_no_connection(ssh);
    let _ = workspace.update_firing(Event(Progress::ModelDispatch(ticket.clone(), permit)));
    let mut client = test_support::accept(model);
    let request = test_support::request(&mut client);
    let messages = request["messages"].as_array().unwrap();
    assert!(
        messages[0]["content"]
            .as_str()
            .unwrap()
            .contains(MASKED_SKILL)
    );
    let input: serde_json::Value =
        serde_json::from_str(messages[1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        input,
        json!({"question":"Inspect the fixture containing ***","targets":[target],"working_directory":"/srv"})
    );
    assert!(!request.to_string().contains(PASSWORD));
    let response = json!({"state":"commands","commands":[
        {"target":target,"command":command,"purpose":"Inspect the reviewed account"}
    ]});
    client
        .get_mut()
        .write_all(test_support::fixture_response("200 OK", "", &response.to_string()).as_bytes())
        .unwrap();
    drop(client);
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::Generated(
        completed,
        result,
    ))))) = block_on(stream.next())
    else {
        panic!("the model must return a proposal for this firing");
    };
    assert_eq!(ticket, completed);
    assert!(
        matches!(&result, Ok(Decision::Commands { commands }) if commands.len() == 1 && commands[0].command == command)
    );
    let _ = workspace.update_firing(Event(Progress::Generated(completed, result)));
    assert!(block_on(stream.next()).is_none());
    assert_no_connection(model);
    assert_no_connection(ssh);
    ticket
}

fn select(workspace: &mut Workspace, id: window::Id, ticket: &Ticket) {
    let _ = workspace.update_scheduler(id, SchedulerMessage::SelectActiveRun(ticket.run()));
    assert!(
        workspace
            .scheduler
            .as_ref()
            .unwrap()
            .can_decide_proposal(&workspace.scheduler_views[&id], ticket)
    );
}

fn assert_finished(jobs: &Path, ticket: &Ticket, outcome: RunOutcome, count: usize) {
    let journal = persisted_journal(jobs);
    let records = journal["journal"].as_array().unwrap();
    assert_eq!(records.len(), count);
    let record = records
        .iter()
        .find(|record| record["id"] == ticket.run().value())
        .unwrap();
    assert_eq!(record["outcome"], serde_json::to_value(outcome).unwrap());
}

fn persisted_journal(jobs: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(jobs).unwrap()).unwrap()
}

fn assert_no_connection(listener: &TcpListener) {
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
}
