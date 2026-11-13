use std::io::BufReader;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::process::Stdio;
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use iced::advanced::subscription::into_recipes;
use iced::futures::{FutureExt as _, Stream};
use jiff::{Timestamp, tz::TimeZone};
use kortty_jobs::{JobSchedule, RunTrigger};
use serde_json::Value;

use super::*;
use crate::session_journal::{self, CaptureKind, automation, tree::RunStatus};
use crate::ssh_worker::OpenSshFixture;

const SSH_HOME_MARKER: &str = "KORTTY_SCHEDULED_AGENT_OPENSSH_FIXTURE";
const SSH_TEST_NAME: &str = "workspace::scheduler_access::firing::tests::openssh::openssh_scheduled_agent_firing_runs_reviewed_commands_in_order";
const SSH_CALENDAR_TEST_NAME: &str = "workspace::scheduler_access::firing::tests::openssh::openssh_scheduled_agent_calendar_firing_requires_exact_review";
const SSH_TARGETS_TEST_NAME: &str = "workspace::scheduler_access::firing::tests::openssh::openssh_scheduled_agent_reviews_each_target_independently";
const SSH_SWARM_TEST_NAME: &str = "workspace::scheduler_access::firing::tests::openssh::openssh_scheduled_swarm_iterates_with_isolated_evidence_and_round_tickets";
const SSH_SWARM_INPUT_TEST_NAME: &str = "workspace::scheduler_access::firing::tests::openssh::openssh_scheduled_swarm_records_only_approved_journal_inputs";
const SSH_SWARM_CANCEL_TEST_NAME: &str = "workspace::scheduler_access::firing::tests::openssh::openssh_scheduled_swarm_drains_target_journal_after_cancellation";
const SSH_SWARM_PROBE_TEST_NAME: &str = "workspace::scheduler_access::firing::tests::openssh::openssh_scheduled_swarm_calendar_runs_reviewed_environment_probes";
const SSH_SWARM_PARALLEL_TEST_NAME: &str = "workspace::scheduler_access::firing::tests::openssh::openssh_scheduled_swarm_overlaps_reviewed_targets";
const SWARM_FIXTURE_PASSPHRASE: &str = "swarm-beta-test-credential";
const OPENSSH_GOAL: &str = "Inspect the fixture";
const MARKER: &str = "firing-command-marker";
const PRIVATE_STDOUT: &str = "scheduled-agent-fixture-stdout";
const PRIVATE_STDERR: &str = "scheduled-agent-fixture-stderr";

#[test]
#[ignore = "requires OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_agent_firing_runs_reviewed_commands_in_order() {
    run_reviewed_firing(SSH_TEST_NAME, RunTrigger::Manual);
}

#[test]
#[ignore = "requires OpenSSH sshd and ssh-keygen and waits for a real calendar minute"]
fn openssh_scheduled_agent_calendar_firing_requires_exact_review() {
    run_reviewed_firing(SSH_CALENDAR_TEST_NAME, RunTrigger::Scheduled);
}

#[test]
#[ignore = "requires OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_agent_reviews_each_target_independently() {
    let Some(home) = std::env::var_os(SSH_HOME_MARKER).map(PathBuf::from) else {
        run_private_child(SSH_TARGETS_TEST_NAME, 55);
        return;
    };
    let root = profile::root().unwrap();
    assert!(home.is_dir() && root.starts_with(&home) && !root.exists());
    std::fs::create_dir_all(&root).unwrap();
    let first_ssh = OpenSshFixture::new();
    let second_ssh = OpenSshFixture::new();
    let directory = first_ssh.directory().canonicalize().unwrap();
    let first_probe = observe_ssh();
    let second_probe = observe_ssh();
    let first = save_pinned_target(
        &first_ssh,
        Some(first_probe.local_addr().unwrap().port()),
        None,
    );
    let second = save_pinned_target(
        &second_ssh,
        Some(second_probe.local_addr().unwrap().port()),
        None,
    );
    let (model, _) = test_support::listener();
    let jobs = profile::jobs_path().unwrap();
    let (mut workspace, view) = approved_workspace(
        &root,
        &jobs,
        &model,
        &[first.clone(), second.clone()],
        &directory,
        FixtureJob::agent(None),
    );

    const FIRST_MARKER: &str = "first-target-firing-marker";
    const SECOND_MARKER: &str = "second-target-firing-marker";
    let first_marker = directory.join(FIRST_MARKER);
    let second_marker = directory.join(SECOND_MARKER);
    let first_commands = [
        format!(r"printf 'first\n' >> {FIRST_MARKER}"),
        format!(r"printf 'second\n' >> {FIRST_MARKER}; exit 17"),
        format!(r"printf 'skipped tail\n' >> {FIRST_MARKER}"),
    ];
    let second_commands = [format!(r"printf 'approved\n' >> {SECOND_MARKER}")];
    let deadline = Instant::now() + Duration::from_secs(40);
    let start = workspace.update_scheduler(view, SchedulerMessage::Run(false));
    let first_ticket = generate_commands(
        &mut workspace,
        start,
        &model,
        first.id,
        &directory,
        &first_commands,
        deadline,
    );
    assert_proposal(&workspace, &first_ticket, &first_commands);
    assert_no_connection(&model);
    assert_no_connection(&first_probe);
    assert_no_connection(&second_probe);
    assert!(!first_marker.exists() && !second_marker.exists());
    select(&mut workspace, view, &first_ticket);

    let (next, first_result) = execute_commands(
        &mut workspace,
        view,
        &first_ticket,
        &first_ssh,
        Some(&first_probe),
        &first_marker,
        deadline,
    );
    let failed = RunOutcome::Failed {
        exit_code: Some(17),
    };
    assert_eq!(first_result.completion.outcome, failed);
    assert!(first_result.evidence.is_none());
    assert_eq!(
        std::fs::read_to_string(&first_marker).unwrap(),
        "first\nsecond\n"
    );
    assert!(
        persisted_journal(&jobs)["journal"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
    assert_no_connection(&second_probe);
    assert!(!second_marker.exists());

    let second_ticket = generate_commands(
        &mut workspace,
        next,
        &model,
        second.id,
        &directory,
        &second_commands,
        deadline,
    );
    assert_ne!(first_ticket, second_ticket);
    assert_eq!(first_ticket.run(), second_ticket.run());
    select(&mut workspace, view, &second_ticket);
    let stale =
        workspace.update_scheduler(view, SchedulerMessage::ApproveRunProposal(first_ticket));
    assert_eq!(stale.units(), 0);
    assert_proposal(&workspace, &second_ticket, &second_commands);
    assert_no_connection(&second_probe);
    assert!(!second_marker.exists());

    let (done, second_result) = execute_commands(
        &mut workspace,
        view,
        &second_ticket,
        &second_ssh,
        Some(&second_probe),
        &second_marker,
        deadline,
    );
    assert_eq!(done.units(), 0);
    assert_eq!(second_result.completion.outcome, RunOutcome::Success);
    assert!(second_result.evidence.is_none());
    assert_eq!(
        std::fs::read_to_string(&second_marker).unwrap(),
        "approved\n"
    );
    assert_no_connection(&model);
    assert_no_connection(&first_probe);
    assert_no_connection(&second_probe);
    assert_finished(&jobs, &second_ticket, failed, 1);
    let journal = persisted_journal(&jobs);
    let detail = journal["journal_details"][second_ticket.run().value().to_string()]
        .as_str()
        .unwrap();
    let targets: Vec<_> = detail.split(" | ").collect();
    assert_eq!(targets.len(), 2, "{detail}");
    assert!(
        targets[0].starts_with(&format!("Profile {}: Failed (exit 17)", first.id)),
        "{detail}"
    );
    assert!(
        targets[1].starts_with(&format!("Profile {}: Success", second.id)),
        "{detail}"
    );
    let persisted = std::fs::read_to_string(&jobs).unwrap();
    assert!(!persisted.contains(FIRST_MARKER) && !persisted.contains(SECOND_MARKER));
}

#[test]
#[ignore = "requires OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_swarm_iterates_with_isolated_evidence_and_round_tickets() {
    run_iterative_swarm(SSH_SWARM_TEST_NAME, SwarmJournalScenario::OutputOnly);
}

#[test]
#[ignore = "requires OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_swarm_records_only_approved_journal_inputs() {
    run_iterative_swarm(SSH_SWARM_INPUT_TEST_NAME, SwarmJournalScenario::WithInput);
}

#[test]
#[ignore = "requires OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_swarm_drains_target_journal_after_cancellation() {
    run_iterative_swarm(
        SSH_SWARM_CANCEL_TEST_NAME,
        SwarmJournalScenario::CancelAfterFirstBatch,
    );
}

#[test]
#[ignore = "requires OpenSSH sshd and ssh-keygen and waits for a real calendar minute"]
fn openssh_scheduled_swarm_calendar_runs_reviewed_environment_probes() {
    use agent::proposal::{MAX_SWARM_COMMAND_ROUNDS, SWARM_PROBE_FIRING_SYSTEM};

    let Some(home) = std::env::var_os(SSH_HOME_MARKER).map(PathBuf::from) else {
        run_private_child(SSH_SWARM_PROBE_TEST_NAME, 125);
        return;
    };
    let root = profile::root().unwrap();
    assert!(home.is_dir() && root.starts_with(&home) && !root.exists());
    std::fs::create_dir_all(&root).unwrap();
    let servers = [
        OpenSshFixture::new(),
        OpenSshFixture::new(),
        OpenSshFixture::new(),
    ];
    let listeners = [observe_ssh(), observe_ssh(), observe_ssh()];
    let targets = servers
        .iter()
        .zip(&listeners)
        .map(|(server, listener)| {
            save_pinned_target(server, Some(listener.local_addr().unwrap().port()), None)
        })
        .collect::<Vec<_>>();
    let directory = servers[0].directory().canonicalize().unwrap();
    let wire = agent::initial_swarm_probe(directory.to_str().unwrap()).unwrap();
    let (model, _) = test_support::listener();
    let jobs = profile::jobs_path().unwrap();
    let (schedule, due) = next_calendar_start();
    let (mut workspace, view) = approved_workspace(
        &root,
        &jobs,
        &model,
        &targets,
        &directory,
        FixtureJob {
            probe: agent::Probe::SwarmIterative,
            initial_probe: true,
            parallelism: None,
            schedule: Some(schedule),
            automation_journal: Some(automation::Options {
                enabled: true,
                capture_input: true,
                ..automation::Options::default()
            }),
            vault: None,
        },
    );
    let journal_root = workspace.journal_directory().unwrap();
    assert!(!journal_root.exists());
    let idle = || {
        assert_no_connection(&model);
        for listener in &listeners {
            assert_no_connection(listener);
        }
    };
    let start = await_calendar_dispatch(
        &mut workspace,
        due,
        &model,
        &[&listeners[0], &listeners[1], &listeners[2]],
    );
    idle();
    let deadline = Instant::now() + Duration::from_secs(45);
    let run = *workspace.scheduler_agent_sequences.keys().next().unwrap();
    let probe = |workspace: &mut Workspace, task, index: usize, output: &str, exit: i32| {
        let review = workspace
            .scheduler
            .as_ref()
            .unwrap()
            .firing_reviews
            .by_key(Key::new(run, index))
            .expect("scheduled probe ticket");
        assert_eq!(review.phase(), Phase::Executing);
        assert_eq!(review.ticket().target_index(), index);
        assert_eq!(review.ticket().round(), 1);
        assert!(review.context().initial_probe);
        assert_eq!(
            review.context().prompt.swarm_probe_command(),
            Some(wire.as_str())
        );
        let request = review.request();
        assert_eq!(request.trigger, RunTrigger::Scheduled);
        assert!(request.started_at >= due);
        let approval = request
            .action
            .agent
            .as_ref()
            .unwrap()
            .approval
            .as_ref()
            .unwrap();
        assert!(approval.has_initial_swarm_probe());
        assert!(approval.generation_matches_for(index, &review.context().prompt));
        let ticket = review.ticket().clone();
        let (receipt, extra) = logged_environment_probe(&servers[index], output, exit);
        let (next, result) = drive_execution(
            workspace,
            task,
            &ticket,
            (&servers[index], &extra),
            Some(&listeners[index]),
            &receipt,
            deadline,
        );
        assert_eq!(
            std::fs::read_to_string(receipt).unwrap(),
            format!("{wire}\n")
        );
        assert_eq!(
            result.completion.outcome,
            if exit == 0 {
                RunOutcome::Success
            } else {
                RunOutcome::Failed {
                    exit_code: Some(exit),
                }
            }
        );
        let [evidence] = result.evidence.as_deref().expect("masked probe result") else {
            panic!("expected one probe result");
        };
        assert_eq!(evidence.command, wire);
        assert_eq!(evidence.exit, u32::try_from(exit).unwrap());
        assert!(evidence.stdout.starts_with(&format!("{output}\n")));
        assert!(evidence.stdout.contains(directory.to_str().unwrap()));
        assert!(!evidence.truncated);
        idle();
        (
            ticket,
            next,
            json!({
                "exit": evidence.exit, "stdout": evidence.stdout,
                "stderr": evidence.stderr, "truncated": evidence.truncated,
            }),
        )
    };
    let input = |index: usize, round, environment, prior| {
        json!({
            "question": OPENSSH_GOAL, "targets": [targets[index].id], "working_directory": directory,
            "round": round, "command_round_limit": MAX_SWARM_COMMAND_ROUNDS,
            "prior_rounds": prior, "omitted_rounds": 0, "environment_probe": environment,
        })
    };

    const FIRST_OUTPUT: &str = "calendar-probe-first-only";
    const SECOND_OUTPUT: &str = "calendar-probe-second-only";
    const FAILED_OUTPUT: &str = "calendar-probe-failure-only";
    let (first_probe, next, first_evidence) = probe(&mut workspace, start, 0, FIRST_OUTPUT, 0);
    let user_marker = directory.join("calendar-reviewed-user-command");
    let commands = [r"printf 'reviewed\n' > calendar-reviewed-user-command".into()];
    let (proposal, awaiting) = generate_reply(
        &mut workspace,
        next,
        &model,
        input(0, 1, first_evidence.clone(), json!([])),
        commands_reply(targets[0].id, &commands),
        Some(SWARM_PROBE_FIRING_SYSTEM),
        deadline,
    );
    assert_eq!(awaiting.units(), 0);
    assert_ne!(proposal, first_probe);
    assert_swarm_proposal(&workspace, &proposal, 0, 1, &commands);
    select(&mut workspace, view, &proposal);
    reject_stale_swarm_ticket(&mut workspace, view, &first_probe);
    idle();
    assert!(!user_marker.exists());
    let (next, executed) = execute_commands(
        &mut workspace,
        view,
        &proposal,
        &servers[0],
        Some(&listeners[0]),
        &user_marker,
        deadline,
    );
    assert_eq!(executed.completion.outcome, RunOutcome::Success);
    assert_eq!(std::fs::read_to_string(&user_marker).unwrap(), "reviewed\n");
    let (first_done, close) = generate_reply(
        &mut workspace,
        next,
        &model,
        input(
            0,
            2,
            first_evidence,
            json!([{"round": 1, "commands": executed.evidence.unwrap()}]),
        ),
        json!({"state": "done", "summary": "The first target completed."}),
        Some(SWARM_PROBE_FIRING_SYSTEM),
        deadline,
    );
    let next = close_target_journal(
        &mut workspace,
        close,
        &first_done,
        0,
        RunOutcome::Success,
        deadline,
    );

    let (second_probe, next, second_evidence) = probe(&mut workspace, next, 1, SECOND_OUTPUT, 0);
    assert_ne!(second_probe, first_probe);
    assert!(!second_evidence.to_string().contains(FIRST_OUTPUT));
    let (second_done, close) = generate_reply(
        &mut workspace,
        next,
        &model,
        input(1, 1, second_evidence, json!([])),
        json!({"state": "done", "summary": "The second target completed without commands."}),
        Some(SWARM_PROBE_FIRING_SYSTEM),
        deadline,
    );
    let next = close_target_journal(
        &mut workspace,
        close,
        &second_done,
        1,
        RunOutcome::Success,
        deadline,
    );
    let (failed_probe, close, _) = probe(&mut workspace, next, 2, FAILED_OUTPUT, 19);
    let failure = RunOutcome::Failed {
        exit_code: Some(19),
    };
    let done = close_target_journal(&mut workspace, close, &failed_probe, 2, failure, deadline);
    assert_eq!(done.units(), 0);
    idle();
    assert_finished(&jobs, &failed_probe, failure, 1);

    let journals = session_journal::catalog(&journal_root).unwrap().journals;
    assert_eq!(journals.len(), 3);
    for (index, (output, outcome)) in [
        (FIRST_OUTPUT, RunStatus::Success),
        (SECOND_OUTPUT, RunStatus::Success),
        (FAILED_OUTPUT, RunStatus::Failed),
    ]
    .into_iter()
    .enumerate()
    {
        let journal = journals
            .iter()
            .find(|row| row.port == Some(targets[index].target.port))
            .unwrap();
        let expected = if index == 0 {
            vec![wire.as_str(), commands[0].as_str()]
        } else {
            vec![wire.as_str()]
        };
        assert!(assert_target_capture(journal, outcome, &expected, true).contains(output));
    }
    drop(workspace);
    let reopened = SchedulerStore::open(&jobs).unwrap();
    let report = reopened.journal_swarm_report(run).unwrap();
    assert_eq!(
        report
            .targets
            .iter()
            .map(|target| target.outcome)
            .collect::<Vec<_>>(),
        [RunOutcome::Success, RunOutcome::Success, failure]
    );
    let persisted = std::fs::read_to_string(&jobs).unwrap();
    for private in [
        FIRST_OUTPUT,
        SECOND_OUTPUT,
        FAILED_OUTPUT,
        "calendar-reviewed-user-command",
    ] {
        assert!(!persisted.contains(private));
    }
}

#[test]
#[ignore = "requires OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_swarm_overlaps_reviewed_targets() {
    use agent::proposal::{MAX_SWARM_COMMAND_ROUNDS, SWARM_PROBE_FIRING_SYSTEM};

    let Some(home) = std::env::var_os(SSH_HOME_MARKER).map(PathBuf::from) else {
        run_private_child(SSH_SWARM_PARALLEL_TEST_NAME, 75);
        return;
    };
    let root = profile::root().unwrap();
    assert!(home.is_dir() && root.starts_with(&home) && !root.exists());
    std::fs::create_dir_all(&root).unwrap();
    let servers = [OpenSshFixture::new(), OpenSshFixture::new()];
    let listeners = [observe_ssh(), observe_ssh()];
    let targets: [_; 2] = std::array::from_fn(|index| {
        save_pinned_target(
            &servers[index],
            Some(listeners[index].local_addr().unwrap().port()),
            None,
        )
    });
    let directory = servers[0].directory().canonicalize().unwrap();
    let wire = agent::initial_swarm_probe(directory.to_str().unwrap()).unwrap();
    let (model, _) = test_support::listener();
    let jobs = profile::jobs_path().unwrap();
    let (mut workspace, view) = approved_workspace(
        &root,
        &jobs,
        &model,
        &targets,
        &directory,
        FixtureJob {
            probe: agent::Probe::SwarmIterative,
            initial_probe: true,
            parallelism: Some(2),
            schedule: None,
            automation_journal: Some(automation::Options {
                enabled: true,
                capture_input: true,
                ..automation::Options::default()
            }),
            vault: None,
        },
    );
    let journal_root = workspace.journal_directory().unwrap();
    let idle = || {
        assert_no_connection(&model);
        for listener in &listeners {
            assert_no_connection(listener);
        }
    };
    const PROBE_OUTPUT: [&str; 2] = ["parallel-probe-alpha", "parallel-probe-beta"];
    let probes: [_; 2] = std::array::from_fn(|index| {
        logged_environment_probe(&servers[index], PROBE_OUTPUT[index], 0)
    });
    let daemons: [_; 2] =
        std::array::from_fn(|index| servers[index].serve_with_extra("host", &probes[index].1));
    let deadline = Instant::now() + Duration::from_secs(45);
    idle();
    let start = workspace.update_scheduler(view, SchedulerMessage::Run(false));
    assert_eq!(workspace.scheduler_workers, 2);
    let run = *workspace.scheduler_agent_sequences.keys().next().unwrap();
    let probe_tickets: [_; 2] = std::array::from_fn(|index| {
        let review = workspace
            .scheduler
            .as_ref()
            .unwrap()
            .firing_reviews
            .by_key(Key::new(run, index))
            .unwrap();
        assert_eq!(review.phase(), Phase::Executing);
        assert!(review.context().initial_probe);
        assert_eq!(
            review.context().prompt.swarm_probe_command(),
            Some(wire.as_str())
        );
        review.ticket().clone()
    });
    let mut executions = iced_runtime::task::into_stream(start).expect("parallel probe tasks");
    let mut first_permits = [None, None];
    for _ in 0..2 {
        let Some(Action::Output(Message::ScheduledAgent(Event(Progress::ExecutionPermit(
            ticket,
            FiringPermit::Execute,
            permit,
        ))))) = next_item(&mut executions, deadline)
        else {
            panic!("both probes must ask for permission before either is allowed");
        };
        let index = ticket.target_index();
        assert_eq!(ticket, probe_tickets[index]);
        assert!(first_permits[index].replace(permit).is_none());
    }
    idle();
    assert!(probes.iter().all(|(receipt, _)| !receipt.exists()));
    assert!(!journal_root.exists());
    let relays: [_; 2] = std::array::from_fn(|index| {
        let ssh = &servers[index];
        let port = ssh
            .request(ssh.directory().join("client"))
            .target
            .endpoint
            .port;
        relay_ssh(
            listeners[index].try_clone().unwrap(),
            ([127, 0, 0, 1], port).into(),
        )
    });
    for (ticket, permit) in probe_tickets.iter().zip(first_permits) {
        let _ = workspace.update_firing(Event(Progress::ExecutionPermit(
            ticket.clone(),
            FiringPermit::Execute,
            permit.unwrap(),
        )));
    }
    let mut permits = [1, 1];
    let mut completed = [None, None];
    while let Some(event) = next_item(&mut executions, deadline) {
        match event {
            Action::Output(Message::ScheduledAgent(Event(Progress::ExecutionPermit(
                ticket,
                purpose,
                permit,
            )))) => {
                let index = ticket.target_index();
                assert_eq!(ticket, probe_tickets[index]);
                permits[index] += 1;
                let _ = workspace
                    .update_firing(Event(Progress::ExecutionPermit(ticket, purpose, permit)));
            }
            Action::Output(Message::ScheduledAgent(Event(Progress::Executed(ticket, result)))) => {
                let index = ticket.target_index();
                assert_eq!(ticket, probe_tickets[index]);
                assert_eq!(result.completion.outcome, RunOutcome::Success);
                assert_eq!(
                    std::fs::read_to_string(&probes[index].0).unwrap(),
                    format!("{wire}\n")
                );
                let [evidence] = result.evidence.as_deref().unwrap() else {
                    panic!("expected one successful probe result");
                };
                assert_eq!(evidence.command, wire);
                assert!(evidence.stdout.starts_with(PROBE_OUTPUT[index]));
                assert!(!evidence.stdout.contains(PROBE_OUTPUT[1 - index]));
                let evidence = json!({"exit": evidence.exit, "stdout": evidence.stdout,
                    "stderr": evidence.stderr, "truncated": evidence.truncated});
                let next = workspace.update_firing(Event(Progress::Executed(ticket, result)));
                assert!(completed[index].replace((next, evidence)).is_none());
            }
            _ => panic!("unexpected event while running the parallel probes"),
        }
    }
    assert!(permits.into_iter().all(|count| count >= 3));
    assert_eq!(workspace.scheduler_workers, 0);
    for (daemon, relay) in daemons.into_iter().zip(relays) {
        wait_until(deadline, || daemon.is_finished() && relay.is_finished());
        let log = daemon.join().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&log.stderr)
                .matches("Accepted publickey for ")
                .count(),
            1
        );
        relay.join().unwrap();
    }

    let [(alpha, alpha_environment), (beta, beta_environment)] = completed.map(Option::unwrap);
    let environments = [alpha_environment, beta_environment];
    let input = |index: usize, round, prior| {
        json!({
            "question": OPENSSH_GOAL, "targets": [targets[index].id], "working_directory": directory,
            "round": round, "command_round_limit": MAX_SWARM_COMMAND_ROUNDS,
            "prior_rounds": prior, "omitted_rounds": 0, "environment_probe": environments[index],
        })
    };
    let mut models =
        iced_runtime::task::into_stream(Task::batch([alpha, beta])).expect("parallel model tasks");
    let mut requests = [None, None];
    for _ in 0..2 {
        let (ticket, permit) = model_dispatch(&mut models, deadline);
        let index = ticket.target_index();
        assert_eq!(ticket.run(), run);
        assert_ne!(ticket, probe_tickets[index]);
        assert_eq!(ticket.round(), 1);
        let mut client = connect_model(&mut workspace, &model, &ticket, permit);
        assert_model_request(
            &mut client,
            input(index, 1, json!([])),
            Some(SWARM_PROBE_FIRING_SYSTEM),
        );
        assert!(requests[index].replace((ticket, client)).is_none());
    }
    let [(alpha, alpha_client), (beta, beta_client)] = requests.map(Option::unwrap);
    assert_ne!(alpha, beta);
    const COMMAND_MARKERS: [&str; 2] = ["parallel-unapproved-alpha", "parallel-approved-beta"];
    let commands = COMMAND_MARKERS.map(|marker| [format!("printf 'reviewed\\n' > {marker}")]);
    let markers = COMMAND_MARKERS.map(|marker| directory.join(marker));
    for (ticket, client) in [(beta.clone(), beta_client), (alpha.clone(), alpha_client)] {
        let index = ticket.target_index();
        send_model_reply(client, commands_reply(targets[index].id, &commands[index]));
        assert_eq!(
            finish_model(&mut workspace, &mut models, &ticket, deadline).units(),
            0
        );
    }
    assert!(next_item(&mut models, deadline).is_none());
    for ticket in [&alpha, &beta] {
        let index = ticket.target_index();
        assert_swarm_proposal(&workspace, ticket, index, 1, &commands[index]);
        select(&mut workspace, view, ticket);
        reject_stale_swarm_ticket(&mut workspace, view, &probe_tickets[index]);
    }
    idle();
    assert!(markers.iter().all(|marker| !marker.exists()));

    let (next, executed) = execute_commands(
        &mut workspace,
        view,
        &beta,
        &servers[1],
        Some(&listeners[1]),
        &markers[1],
        deadline,
    );
    assert_eq!(executed.completion.outcome, RunOutcome::Success);
    assert_eq!(std::fs::read_to_string(&markers[1]).unwrap(), "reviewed\n");
    assert!(!markers[0].exists());
    assert_no_connection(&listeners[0]);
    let (beta_done, close) = generate_reply(
        &mut workspace,
        next,
        &model,
        input(
            1,
            2,
            json!([{"round": 1, "commands": executed.evidence.unwrap()}]),
        ),
        json!({"state": "done", "summary": "The second target completed first."}),
        Some(SWARM_PROBE_FIRING_SYSTEM),
        deadline,
    );
    assert_eq!(beta_done.target_index(), 1);
    let waiting = close_target_journal(
        &mut workspace,
        close,
        &beta_done,
        1,
        RunOutcome::Success,
        deadline,
    );
    assert_eq!(waiting.units(), 0);
    assert_swarm_proposal(&workspace, &alpha, 0, 1, &commands[0]);
    assert!(
        persisted_journal(&jobs)["journal"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    select(&mut workspace, view, &alpha);
    let close =
        workspace.update_scheduler(view, SchedulerMessage::RejectRunProposal(alpha.clone()));
    let done = close_target_journal(
        &mut workspace,
        close,
        &alpha,
        0,
        RunOutcome::Blocked,
        deadline,
    );
    assert_eq!(done.units(), 0);
    idle();
    assert!(!markers[0].exists());
    assert_finished(&jobs, &alpha, RunOutcome::Blocked, 1);

    let journals = session_journal::catalog(&journal_root).unwrap().journals;
    assert_eq!(journals.len(), 2);
    for (index, outcome) in [RunStatus::Blocked, RunStatus::Success]
        .into_iter()
        .enumerate()
    {
        let journal = journals
            .iter()
            .find(|row| row.port == Some(targets[index].target.port))
            .unwrap();
        let expected = if index == 0 {
            vec![wire.as_str()]
        } else {
            vec![wire.as_str(), commands[1][0].as_str()]
        };
        let output = assert_target_capture(journal, outcome, &expected, true);
        assert!(output.contains(PROBE_OUTPUT[index]));
        assert!(!output.contains(PROBE_OUTPUT[1 - index]));
    }
    drop(workspace);
    let saved = SchedulerStore::open(&jobs).unwrap();
    let report = saved.journal_swarm_report(run).unwrap();
    assert_eq!(
        report
            .targets
            .iter()
            .map(|row| (row.target, row.outcome))
            .collect::<Vec<_>>(),
        [
            (targets[0].id, RunOutcome::Blocked),
            (targets[1].id, RunOutcome::Success),
        ]
    );
}

fn logged_environment_probe(server: &OpenSshFixture, output: &str, exit: i32) -> (PathBuf, String) {
    let receipt = server.directory().join("probe-wire-receipt");
    let script = server.directory().join("observe-probe.sh");
    let quote = |path: &Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    std::fs::write(&script, format!(
        "printf '%s\\n' \"$SSH_ORIGINAL_COMMAND\" > {}\nprintf '%s\\n' '{output}'\n/bin/sh -c \"$SSH_ORIGINAL_COMMAND\" || exit $?\nexit {exit}\n",
        quote(&receipt),
    )).unwrap();
    (
        receipt,
        format!("ForceCommand /bin/sh {}\n", quote(&script)),
    )
}

#[derive(PartialEq, Eq)]
enum SwarmJournalScenario {
    OutputOnly,
    WithInput,
    CancelAfterFirstBatch,
}

fn run_iterative_swarm(test_name: &str, scenario: SwarmJournalScenario) {
    use agent::proposal::{MAX_SWARM_COMMAND_ROUNDS, SWARM_FIRING_SYSTEM};

    let Some(home) = std::env::var_os(SSH_HOME_MARKER).map(PathBuf::from) else {
        run_private_child(test_name, 65);
        return;
    };
    let root = profile::root().unwrap();
    assert!(home.is_dir() && root.starts_with(&home) && !root.exists());
    std::fs::create_dir_all(&root).unwrap();
    let alpha_ssh = OpenSshFixture::new();
    let beta_ssh = OpenSshFixture::new();
    let directory = alpha_ssh.directory().canonicalize().unwrap();
    let alpha_listener = observe_ssh();
    let beta_listener = observe_ssh();
    let (passphrase, vault) = protect_swarm_fixture_key(&beta_ssh);
    let alpha = save_pinned_target(
        &alpha_ssh,
        Some(alpha_listener.local_addr().unwrap().port()),
        None,
    );
    let beta = save_pinned_target(
        &beta_ssh,
        Some(beta_listener.local_addr().unwrap().port()),
        Some(passphrase),
    );
    let (model, _) = test_support::listener();
    let jobs = profile::jobs_path().unwrap();
    let capture_input = scenario == SwarmJournalScenario::WithInput;
    let (mut workspace, view) = approved_workspace(
        &root,
        &jobs,
        &model,
        &[alpha.clone(), beta.clone()],
        &directory,
        FixtureJob {
            probe: agent::Probe::SwarmIterative,
            initial_probe: false,
            parallelism: None,
            schedule: None,
            automation_journal: Some(automation::Options {
                enabled: true,
                capture_input,
                ..automation::Options::default()
            }),
            vault: Some(vault),
        },
    );
    let journal_root = workspace.journal_directory().unwrap();
    assert!(journal_root.starts_with(&root));

    const ALPHA_FIRST_MARKER: &str = "swarm-alpha-first-marker";
    const ALPHA_SECOND_MARKER: &str = "swarm-alpha-second-marker";
    const BETA_MARKER: &str = "swarm-beta-marker";
    const ALPHA_FIRST_STDOUT: &str = "swarm-alpha-first-output";
    const ALPHA_FIRST_STDERR: &str = "swarm-alpha-first-error";
    const ALPHA_SECOND_STDOUT: &str = "swarm-alpha-second-output";
    const BETA_STDOUT: &str = "swarm-beta-output";
    const MASK_FIXTURE_FILE: &str = "swarm-fixture-output-data";
    const ALPHA_SUMMARY: &str = "The first target recovered after its initial error.";
    const BETA_SUMMARY: &str = "The second target could not complete its check.";
    std::fs::write(
        directory.join(MASK_FIXTURE_FILE),
        format!("{SWARM_FIXTURE_PASSPHRASE}\n"),
    )
    .unwrap();
    let alpha_first = [format!(
        r"printf 'started\n' > {ALPHA_FIRST_MARKER}; printf '{ALPHA_FIRST_STDOUT}\n'; cat {MASK_FIXTURE_FILE}; printf '{ALPHA_FIRST_STDERR}\n' >&2; exit 17"
    )];
    let alpha_second = [format!(
        r"printf 'started\n' > {ALPHA_SECOND_MARKER}; printf '{ALPHA_SECOND_STDOUT}\n'"
    )];
    let beta_first = [format!(
        r"printf 'started\n' > {BETA_MARKER}; printf '{BETA_STDOUT}\n'"
    )];
    let alpha_first_marker = directory.join(ALPHA_FIRST_MARKER);
    let alpha_second_marker = directory.join(ALPHA_SECOND_MARKER);
    let beta_marker = directory.join(BETA_MARKER);
    let check_idle = || {
        assert_no_connection(&model);
        assert_no_connection(&alpha_listener);
        assert_no_connection(&beta_listener);
    };
    let evidence = |round, command: &str, exit, stdout: &str, stderr: Option<&str>| {
        json!({"round": round, "commands": [{
            "command": command, "exit": exit, "stdout": format!("{stdout}\n"),
            "stderr": stderr.map_or_else(String::new, |stderr| format!("{stderr}\n")),
            "truncated": false,
        }]})
    };
    let alpha_first_evidence = evidence(
        1,
        &alpha_first[0],
        17,
        &format!("{ALPHA_FIRST_STDOUT}\n***"),
        Some(ALPHA_FIRST_STDERR),
    );
    let alpha_second_evidence = evidence(2, &alpha_second[0], 0, ALPHA_SECOND_STDOUT, None);
    let beta_evidence = evidence(1, &beta_first[0], 0, BETA_STDOUT, None);
    let input = |target, round, prior: Vec<Value>| {
        json!({
            "question": OPENSSH_GOAL, "targets": [target], "working_directory": directory,
            "round": round, "command_round_limit": MAX_SWARM_COMMAND_ROUNDS,
            "prior_rounds": prior, "omitted_rounds": 0,
        })
    };
    let deadline = Instant::now() + Duration::from_secs(50);

    let start = workspace.update_scheduler(view, SchedulerMessage::Run(false));
    let (alpha_one, awaiting) = generate_reply(
        &mut workspace,
        start,
        &model,
        input(alpha.id, 1, vec![]),
        commands_reply(alpha.id, &alpha_first),
        Some(SWARM_FIRING_SYSTEM),
        deadline,
    );
    assert_eq!(awaiting.units(), 0);
    assert_swarm_proposal(&workspace, &alpha_one, 0, 1, &alpha_first);
    check_idle();
    assert!(!alpha_first_marker.exists());
    assert!(!journal_root.exists());
    select(&mut workspace, view, &alpha_one);
    let (next, alpha_first_result) = execute_commands(
        &mut workspace,
        view,
        &alpha_one,
        &alpha_ssh,
        Some(&alpha_listener),
        &alpha_first_marker,
        deadline,
    );
    assert_eq!(
        alpha_first_result.completion.outcome,
        RunOutcome::Failed {
            exit_code: Some(17)
        }
    );
    assert_swarm_evidence(&alpha_first_result, &alpha_first_evidence);
    assert_eq!(
        std::fs::read_to_string(&alpha_first_marker).unwrap(),
        "started\n"
    );
    assert!(!beta_marker.exists());

    let (alpha_two, awaiting) = generate_reply(
        &mut workspace,
        next,
        &model,
        input(alpha.id, 2, vec![alpha_first_evidence.clone()]),
        commands_reply(alpha.id, &alpha_second),
        Some(SWARM_FIRING_SYSTEM),
        deadline,
    );
    assert_eq!(awaiting.units(), 0);
    assert_ne!(alpha_two, alpha_one);
    assert_eq!(alpha_two.run(), alpha_one.run());
    assert_swarm_proposal(&workspace, &alpha_two, 0, 2, &alpha_second);
    select(&mut workspace, view, &alpha_two);
    reject_stale_swarm_ticket(&mut workspace, view, &alpha_one);
    let duplicate = workspace.update_firing(Event(Progress::Executed(
        alpha_one.clone(),
        alpha_first_result,
    )));
    assert_eq!(duplicate.units(), 0);
    assert_swarm_proposal(&workspace, &alpha_two, 0, 2, &alpha_second);
    check_idle();
    assert!(!alpha_second_marker.exists());
    if scenario == SwarmJournalScenario::CancelAfterFirstBatch {
        let close = workspace.update_scheduler(view, SchedulerMessage::CancelRun(alpha_two.run()));
        let done = close_target_journal(
            &mut workspace,
            close,
            &alpha_two,
            0,
            RunOutcome::Cancelled,
            deadline,
        );
        assert_eq!(done.units(), 0);
        assert_eq!(workspace.scheduler_workers, 0);
        check_idle();
        assert!(!alpha_second_marker.exists() && !beta_marker.exists());
        assert_finished(&jobs, &alpha_two, RunOutcome::Cancelled, 1);
        drop(workspace);
        let store = SchedulerStore::open(&jobs).unwrap();
        assert!(store.journal_swarm_report(alpha_two.run()).is_none());
        let captures = session_journal::catalog(&journal_root).unwrap().journals;
        let [capture] = captures.as_slice() else {
            panic!("cancellation must retain only the executed target's journal");
        };
        assert_eq!(capture.port, Some(alpha.target.port));
        assert!(capture.run_id.is_some());
        let output = assert_target_capture(capture, RunStatus::Cancelled, &[], false);
        assert!(output.contains(ALPHA_FIRST_STDOUT) && output.contains("***"));
        assert!(!output.contains(ALPHA_SECOND_STDOUT) && !output.contains(BETA_STDOUT));
        return;
    }
    let (next, alpha_second_result) = execute_commands(
        &mut workspace,
        view,
        &alpha_two,
        &alpha_ssh,
        Some(&alpha_listener),
        &alpha_second_marker,
        deadline,
    );
    assert_eq!(alpha_second_result.completion.outcome, RunOutcome::Success);
    assert_swarm_evidence(&alpha_second_result, &alpha_second_evidence);
    assert_eq!(
        std::fs::read_to_string(&alpha_second_marker).unwrap(),
        "started\n"
    );

    let (alpha_done, next) = generate_reply(
        &mut workspace,
        next,
        &model,
        input(
            alpha.id,
            3,
            vec![alpha_first_evidence, alpha_second_evidence],
        ),
        json!({"state": "done", "summary": ALPHA_SUMMARY}),
        Some(SWARM_FIRING_SYSTEM),
        deadline,
    );
    assert_ne!(alpha_done, alpha_two);
    assert_eq!(alpha_done.run(), alpha_one.run());
    assert!(
        persisted_journal(&jobs)["journal"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
    let next = close_target_journal(
        &mut workspace,
        next,
        &alpha_done,
        0,
        RunOutcome::Success,
        deadline,
    );
    let opened = session_journal::catalog(&journal_root).unwrap().journals;
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].port, Some(alpha.target.port));
    assert_eq!(opened[0].run_status, Some(RunStatus::Success));
    assert!(opened[0].ended_at.is_some());

    let (beta_one, awaiting) = generate_reply(
        &mut workspace,
        next,
        &model,
        input(beta.id, 1, vec![]),
        commands_reply(beta.id, &beta_first),
        Some(SWARM_FIRING_SYSTEM),
        deadline,
    );
    assert_eq!(awaiting.units(), 0);
    assert_ne!(beta_one, alpha_done);
    assert_eq!(beta_one.run(), alpha_one.run());
    assert_swarm_proposal(&workspace, &beta_one, 1, 1, &beta_first);
    select(&mut workspace, view, &beta_one);
    reject_stale_swarm_ticket(&mut workspace, view, &alpha_two);
    reject_stale_swarm_ticket(&mut workspace, view, &alpha_done);
    assert_swarm_proposal(&workspace, &beta_one, 1, 1, &beta_first);
    check_idle();
    assert!(!beta_marker.exists());
    let (next, beta_result) = execute_commands(
        &mut workspace,
        view,
        &beta_one,
        &beta_ssh,
        Some(&beta_listener),
        &beta_marker,
        deadline,
    );
    assert_eq!(beta_result.completion.outcome, RunOutcome::Success);
    assert_swarm_evidence(&beta_result, &beta_evidence);
    assert_eq!(std::fs::read_to_string(&beta_marker).unwrap(), "started\n");
    let (beta_done, done) = generate_reply(
        &mut workspace,
        next,
        &model,
        input(beta.id, 2, vec![beta_evidence]),
        json!({"state": "blocked", "summary": BETA_SUMMARY}),
        Some(SWARM_FIRING_SYSTEM),
        deadline,
    );
    assert_ne!(beta_done, beta_one);
    assert_eq!(beta_done.run(), alpha_one.run());
    let done = close_target_journal(
        &mut workspace,
        done,
        &beta_done,
        1,
        RunOutcome::Blocked,
        deadline,
    );
    assert_eq!(done.units(), 0);
    check_idle();

    assert_finished(&jobs, &beta_done, RunOutcome::Blocked, 1);
    let captures = session_journal::catalog(&journal_root).unwrap().journals;
    assert_eq!(captures.len(), 2);
    let alpha_capture = captures
        .iter()
        .find(|row| row.port == Some(alpha.target.port))
        .unwrap();
    let beta_capture = captures
        .iter()
        .find(|row| row.port == Some(beta.target.port))
        .unwrap();
    assert_eq!(alpha_capture.id, opened[0].id);
    assert_ne!(alpha_capture.id, beta_capture.id);
    assert!(alpha_capture.run_id.is_some());
    assert_eq!(alpha_capture.run_id, beta_capture.run_id);
    let alpha_output = assert_target_capture(
        alpha_capture,
        RunStatus::Success,
        &[alpha_first[0].as_str(), alpha_second[0].as_str()],
        capture_input,
    );
    let beta_output = assert_target_capture(
        beta_capture,
        RunStatus::Blocked,
        &[beta_first[0].as_str()],
        capture_input,
    );
    for marker in [ALPHA_FIRST_STDOUT, ALPHA_FIRST_STDERR, ALPHA_SECOND_STDOUT] {
        assert_eq!(alpha_output.matches(marker).count(), 1, "{alpha_output}");
        assert!(!beta_output.contains(marker));
    }
    assert_eq!(alpha_output.matches("***").count(), 1, "{alpha_output}");
    assert!(!alpha_output.contains(BETA_STDOUT));
    assert_eq!(beta_output.matches(BETA_STDOUT).count(), 1, "{beta_output}");
    drop(workspace);
    let reopened = SchedulerStore::open(&jobs).unwrap();
    let report = reopened
        .journal_swarm_report(beta_done.run())
        .expect("saved structured Swarm report");
    assert_eq!(report.question, OPENSSH_GOAL);
    assert_eq!(report.targets.len(), 2);
    for (target, (id, status, summary)) in report.targets.iter().zip([
        (alpha.id, RunOutcome::Success, ALPHA_SUMMARY),
        (beta.id, RunOutcome::Blocked, BETA_SUMMARY),
    ]) {
        assert_eq!(target.target, id);
        assert_eq!(target.outcome, status);
        assert_eq!(target.detail, summary);
    }
    let persisted = std::fs::read_to_string(&jobs).unwrap();
    for private in [
        ALPHA_FIRST_MARKER,
        ALPHA_SECOND_MARKER,
        BETA_MARKER,
        ALPHA_FIRST_STDOUT,
        ALPHA_FIRST_STDERR,
        ALPHA_SECOND_STDOUT,
        BETA_STDOUT,
        SWARM_FIXTURE_PASSPHRASE,
    ] {
        assert!(
            !persisted.contains(private),
            "Swarm command or evidence was persisted"
        );
    }
}

fn assert_swarm_proposal(
    workspace: &Workspace,
    ticket: &Ticket,
    index: usize,
    round: u8,
    commands: &[String],
) {
    assert_proposal(workspace, ticket, commands);
    let review = workspace
        .scheduler
        .as_ref()
        .unwrap()
        .firing_reviews
        .get(ticket, Instant::now())
        .unwrap();
    assert_eq!(review.ticket().round(), round);
    assert_eq!(review.ticket().target_index(), index);
    assert!(review.context().prompt.is_swarm_iterative());
    let approval = review
        .request()
        .action
        .agent
        .as_ref()
        .unwrap()
        .approval
        .as_ref()
        .unwrap();
    assert!(approval.generation_matches_for(index, &review.context().prompt));
}

fn assert_swarm_evidence(result: &job_executor::agent::FiringResult, round: &Value) {
    let evidence = result.evidence.as_ref().expect("captured Swarm evidence");
    assert_eq!(serde_json::to_value(evidence).unwrap(), round["commands"]);
}

fn close_target_journal(
    workspace: &mut Workspace,
    task: Task<Message>,
    ticket: &Ticket,
    index: usize,
    outcome: RunOutcome,
    deadline: Instant,
) -> Task<Message> {
    let mut events = iced_runtime::task::into_stream(task).expect("target journal close task");
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::JournalClosed(
        run,
        target,
        status,
        detail,
        complete,
    ))))) = next_item(&mut events, deadline)
    else {
        panic!("the target did not wait for its journal to close");
    };
    assert_eq!(run, ticket.run());
    assert_eq!(target, index);
    assert_eq!(status, outcome);
    assert!(complete, "the target journal ended incomplete");
    assert!(next_item(&mut events, deadline).is_none());
    workspace.update(Message::ScheduledAgent(Event(Progress::JournalClosed(
        run, target, status, detail, complete,
    ))))
}

fn assert_target_capture(
    summary: &session_journal::Summary,
    outcome: RunStatus,
    commands: &[&str],
    capture_input: bool,
) -> String {
    assert_eq!(summary.source_kind, session_journal::tree::SourceKind::Job);
    assert_eq!(summary.source_id.as_deref(), Some("openssh-firing"));
    assert_eq!(summary.automation_action.as_deref(), Some("AI_SWARM"));
    assert_eq!(summary.run_status, Some(outcome));
    assert!(summary.ended_at.is_some());
    let page = session_journal::capture_page(&summary.directory, None, None, None).unwrap();
    assert!(page.next.is_none());
    assert!(
        page.records.iter().all(|record| {
            !record.text.contains(SWARM_FIXTURE_PASSPHRASE) && record.repeat == 1
        })
    );
    let inputs = page
        .records
        .iter()
        .filter(|record| record.kind == CaptureKind::Input)
        .map(|record| record.text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        inputs.as_slice(),
        if capture_input { commands } else { &[] }
    );
    page.records
        .iter()
        .filter(|record| record.kind == CaptureKind::Output)
        .map(|record| record.text.as_str())
        .collect::<String>()
}

fn reject_stale_swarm_ticket(workspace: &mut Workspace, view: window::Id, ticket: &Ticket) {
    let ignored =
        workspace.update_scheduler(view, SchedulerMessage::ApproveRunProposal(ticket.clone()));
    assert_eq!(ignored.units(), 0);
    for purpose in [FiringPermit::Execute, FiringPermit::Journal] {
        let (permit, response) = async_channel::bounded(1);
        let ignored = workspace.update_firing(Event(Progress::ExecutionPermit(
            ticket.clone(),
            purpose,
            permit,
        )));
        assert_eq!(ignored.units(), 0);
        assert!(response.try_recv().unwrap().is_err());
    }
}

fn run_reviewed_firing(test_name: &str, trigger: RunTrigger) {
    let scheduled = trigger == RunTrigger::Scheduled;
    let Some(home) = std::env::var_os(SSH_HOME_MARKER).map(PathBuf::from) else {
        run_private_child(test_name, if scheduled { 110 } else { 45 });
        return;
    };
    let root = profile::root().unwrap();
    assert!(home.is_dir() && root.starts_with(&home) && !root.exists());
    std::fs::create_dir_all(&root).unwrap();
    let ssh = OpenSshFixture::new();
    let ssh_directory = ssh.directory().canonicalize().unwrap();
    let marker = ssh_directory.join(MARKER);
    let observed_ssh = scheduled.then(observe_ssh);
    let relay_port = observed_ssh
        .as_ref()
        .map(|listener| listener.local_addr().unwrap().port());
    let target = save_pinned_target(&ssh, relay_port, None);
    let (model, _) = test_support::listener();
    let jobs = profile::jobs_path().unwrap();
    let (schedule, due) = if scheduled {
        let (schedule, due) = next_calendar_start();
        (Some(schedule), Some(due))
    } else {
        (None, None)
    };
    let (mut workspace, view) = approved_workspace(
        &root,
        &jobs,
        &model,
        std::slice::from_ref(&target),
        &ssh_directory,
        FixtureJob::agent(schedule),
    );
    let commands = [
        format!(r"printf 'first\n' >> {MARKER}; printf '{PRIVATE_STDOUT}\n'"),
        format!(r"printf 'second\n' >> {MARKER}; printf '{PRIVATE_STDERR}\n' >&2; exit 17"),
        format!(r"printf 'third\n' >> {MARKER}"),
    ];
    let start = if let Some(due) = due {
        await_calendar_dispatch(
            &mut workspace,
            due,
            &model,
            &[observed_ssh.as_ref().unwrap()],
        )
    } else {
        workspace.update_scheduler(view, SchedulerMessage::Run(false))
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let ticket = generate_commands(
        &mut workspace,
        start,
        &model,
        target.id,
        &ssh_directory,
        &commands,
        deadline,
    );
    assert_proposal(&workspace, &ticket, &commands);
    let review = workspace
        .scheduler
        .as_ref()
        .unwrap()
        .firing_reviews
        .get(&ticket, Instant::now())
        .unwrap();
    assert_eq!(review.request().trigger, trigger);
    if let Some(due) = due {
        assert!(review.request().started_at >= due);
    }
    assert!(!marker.exists());
    if let Some(listener) = &observed_ssh {
        assert_no_connection(listener);
    }
    assert_eq!(workspace.scheduler_workers, 0);
    select(&mut workspace, view, &ticket);

    let (done, result) = execute_commands(
        &mut workspace,
        view,
        &ticket,
        &ssh,
        observed_ssh.as_ref(),
        &marker,
        deadline,
    );
    assert_eq!(done.units(), 0);
    let failed = RunOutcome::Failed {
        exit_code: Some(17),
    };
    assert_eq!(result.completion.outcome, failed);
    assert!(result.evidence.is_none());
    assert_no_connection(&model);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "first\nsecond\n");

    let duplicate =
        workspace.update_scheduler(view, SchedulerMessage::ApproveRunProposal(ticket.clone()));
    assert_eq!(duplicate.units(), 0);
    assert_eq!(workspace.scheduler_workers, 0);
    if let Some(listener) = &observed_ssh {
        assert_no_connection(listener);
    }

    assert_finished(&jobs, &ticket, failed, 1);
    assert_eq!(
        persisted_journal(&jobs)["journal_details"][ticket.run().value().to_string()],
        "Command 2 of 3: Reviewed SSH command failed."
    );
    assert_eq!(
        persisted_journal(&jobs)["journal"][0]["trigger"],
        serde_json::to_value(trigger).unwrap()
    );
    let persisted = std::fs::read_to_string(&jobs).unwrap();
    for private in [MARKER, PRIVATE_STDOUT, PRIVATE_STDERR] {
        assert!(!persisted.contains(private), "firing content was persisted");
    }
}

fn run_private_child(test_name: &str, timeout: u64) {
    let scratch = Profile::new();
    let home = scratch.0.canonicalize().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", test_name, "--nocapture"])
        .env(SSH_HOME_MARKER, &home)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("TMPDIR", &home)
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(timeout);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the isolated OpenSSH firing test timed out");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let result = child.wait_with_output().unwrap();
    let output = String::from_utf8_lossy(&result.stdout);
    assert!(
        result.status.success() && output.contains("1 passed"),
        "{output}\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn next_calendar_start() -> (JobSchedule, SystemTime) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut next_minute = (now / 60 + 1) * 60;
    if next_minute - now < 8 {
        // Leave enough time to save the reviewed job and observe a real early tick.
        next_minute += 60;
    }
    let due = UNIX_EPOCH + Duration::from_secs(next_minute);
    let utc = Timestamp::try_from(due).unwrap().to_zoned(TimeZone::UTC);
    let schedule = JobSchedule::default()
        .with_active_dates(Some(utc.date()), Some(utc.date()))
        .unwrap()
        .with_fixed_times([utc.time()]);
    (schedule, due)
}

fn await_calendar_dispatch(
    workspace: &mut Workspace,
    due: SystemTime,
    model: &TcpListener,
    ssh: &[&TcpListener],
) -> Task<Message> {
    assert_eq!(workspace.scheduler.as_ref().unwrap().next_due(), Some(due));
    assert!(SystemTime::now() < due);
    let mut recipes = into_recipes(workspace.scheduler_subscription());
    assert_eq!(recipes.len(), 1);
    let mut ticks = recipes
        .pop()
        .unwrap()
        .stream(iced::futures::stream::empty().boxed());
    let deadline = Instant::now() + Duration::from_secs(75);
    let mut observed_early = false;
    loop {
        assert!(matches!(
            next_item(&mut ticks, deadline),
            Some(Message::RuntimeTick)
        ));
        assert_no_connection(model);
        for listener in ssh {
            assert_no_connection(listener);
        }
        let task = workspace.update(Message::RuntimeTick);
        if workspace.scheduler.as_ref().unwrap().has_active_runs() {
            assert!(observed_early && SystemTime::now() >= due);
            return task;
        }
        assert_eq!(task.units(), 0);
        observed_early = true;
    }
}

fn observe_ssh() -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}

fn relay_ssh(listener: TcpListener, upstream: SocketAddr) -> JoinHandle<()> {
    thread::spawn(move || {
        let client = test_support::accept(&listener).into_inner();
        let server = TcpStream::connect_timeout(&upstream, Duration::from_secs(4)).unwrap();
        for socket in [&client, &server] {
            socket
                .set_read_timeout(Some(Duration::from_secs(12)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(12)))
                .unwrap();
        }
        let client_reader = client.try_clone().unwrap();
        let server_reader = server.try_clone().unwrap();
        let transfer = |mut reader: TcpStream, mut writer: TcpStream| {
            let result = std::io::copy(&mut reader, &mut writer);
            let _ = writer.shutdown(Shutdown::Write);
            if let Err(error) = result {
                assert!(
                    matches!(
                        error.kind(),
                        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset
                    ),
                    "SSH relay failed: {error}"
                );
            }
        };
        thread::scope(|scope| {
            let outgoing = scope.spawn(move || transfer(client_reader, server));
            transfer(server_reader, client);
            outgoing.join().unwrap();
        });
    })
}

fn protect_swarm_fixture_key(ssh: &OpenSshFixture) -> (String, VaultWindow) {
    let changed = Command::new("ssh-keygen")
        .args(["-q", "-p", "-P", "", "-N", SWARM_FIXTURE_PASSPHRASE, "-f"])
        .arg(ssh.directory().join("client"))
        .output()
        .unwrap();
    assert!(
        changed.status.success(),
        "{}",
        String::from_utf8_lossy(&changed.stderr)
    );
    let path = profile::vault_path().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut vault = Vault::create(&path, b"swarm-journal-fixture-master").unwrap();
    let passphrase = vault
        .save(
            CredentialInput {
                label: "Swarm journal fixture credential".into(),
                server_pattern: "127.0.0.1".into(),
                kind: kortty_vault::CredentialKind::KeyPassphrase,
                ..CredentialInput::default()
            },
            Secret::new(SWARM_FIXTURE_PASSPHRASE.into()),
        )
        .unwrap();
    (passphrase, VaultWindow::with_unlocked_fixture(path, vault))
}

fn save_pinned_target(
    ssh: &OpenSshFixture,
    relay_port: Option<u16>,
    passphrase: Option<String>,
) -> ConnectionRef {
    let key = ssh.directory().join("client");
    let mut endpoint = ssh.request(key.clone()).target.endpoint;
    let host = Endpoint::new(&endpoint.host, endpoint.port).unwrap();
    let mut pins = ssh.pinned_trust().selected_pins(&[host]).unwrap();
    let (_, pin) = pins.pop().unwrap();
    if let Some(port) = relay_port {
        endpoint.port = port;
    }
    let host = Endpoint::new(&endpoint.host, endpoint.port).unwrap();
    crate::ssh_connection::trust_store()
        .unwrap()
        .remember(&host, &pin)
        .unwrap();

    let mut connections = ConnectionStore::open(profile::connections_path().unwrap()).unwrap();
    let id = connections
        .save(Connection {
            id: 0,
            name: "OpenSSH firing fixture".into(),
            tag: String::new(),
            login: SavedLogin {
                endpoint,
                auth: Auth::KeyFile {
                    path: key,
                    passphrase,
                },
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
    ConnectionRef::from(connections.get(id).unwrap())
}

struct FixtureJob {
    probe: agent::Probe,
    initial_probe: bool,
    parallelism: Option<u8>,
    schedule: Option<JobSchedule>,
    automation_journal: Option<automation::Options>,
    vault: Option<VaultWindow>,
}

impl FixtureJob {
    fn agent(schedule: Option<JobSchedule>) -> Self {
        Self {
            probe: agent::Probe::PerFiring,
            initial_probe: false,
            parallelism: None,
            schedule,
            automation_journal: None,
            vault: None,
        }
    }
}

fn approved_workspace(
    root: &Path,
    jobs: &Path,
    model: &TcpListener,
    targets: &[ConnectionRef],
    directory: &Path,
    fixture: FixtureJob,
) -> (Workspace, window::Id) {
    let FixtureJob {
        probe,
        initial_probe,
        parallelism,
        schedule,
        automation_journal,
        vault,
    } = fixture;
    let config: Configuration = serde_json::from_value(json!({"profiles": {
        "profiles": [{
            "id": 1, "name": "OpenSSH fixture model",
            "endpoint": model.local_addr().unwrap().to_string(), "model": "fixture:local"
        }],
        "default": 1, "last_id": 1
    }}))
    .unwrap();
    config
        .save_fixture_at(root, &Configuration::default())
        .unwrap();
    let (primary, additional) = targets.split_first().unwrap();
    let mut action: SshCommand = serde_json::from_value(json!({
        "connection": primary,
        "additional_connections": additional,
        "working_directory": directory,
        "automation_journal": automation_journal,
        "agent": {
            "kind": if probe == agent::Probe::SwarmIterative { "swarm" } else { "agent" },
            "probe": probe, "profile": 1, "goal": OPENSSH_GOAL,
        }
    }))
    .unwrap();
    action.agent.as_mut().unwrap().swarm_parallelism = parallelism;
    let mut job = Job::new(
        JobId::new("openssh-firing").unwrap(),
        "OpenSSH firing fixture",
        action,
    )
    .unwrap();
    if let Some(schedule) = schedule {
        job = job.with_schedule(schedule);
    }
    SchedulerStore::open(jobs)
        .unwrap()
        .commit_with_actions(&JobGraph::new(vec![job]).unwrap(), &[], &[])
        .unwrap();
    let mut scheduler = SchedulerWindow::open(jobs.to_owned()).unwrap();
    scheduler.set_execution_allowed(true);
    let view = window::Id::unique();
    let mut workspace = Workspace {
        vault,
        ..Workspace::default()
    };
    workspace.scheduler_views.insert(view, scheduler.new_view());
    workspace.scheduler = Some(scheduler);
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(root).unwrap());
    let _ = workspace.update_scheduler(view, SchedulerMessage::Page(Page::Action));
    if probe == agent::Probe::SwarmIterative && !initial_probe {
        review_legacy_swarm(&mut workspace, view);
    } else {
        let _ = workspace.update_scheduler(view, SchedulerMessage::ReviewAgent);
    }
    let request = workspace.scheduler_views[&view]
        .firing_review_ticket()
        .unwrap()
        .to_owned();
    let _ = workspace.update_scheduler(view, SchedulerMessage::ApproveFiringRequest(request));
    let _ = workspace.update_scheduler(view, SchedulerMessage::Save);
    drop(workspace.scheduler.take());
    let saved = SchedulerStore::open(jobs)
        .unwrap()
        .restore_with_actions::<SshCommand>()
        .unwrap();
    let agent = saved.graph().jobs()[0].action().agent.as_ref().unwrap();
    let approval = agent
        .approval
        .as_ref()
        .expect("the saved fixture must retain its firing approval");
    assert_eq!(approval.has_initial_swarm_probe(), initial_probe);
    assert_eq!(agent.swarm_parallelism, parallelism);
    let mut scheduler = SchedulerWindow::open(jobs.to_owned()).unwrap();
    scheduler.set_execution_allowed(true);
    workspace.scheduler_views.insert(view, scheduler.new_view());
    workspace.scheduler = Some(scheduler);
    let _ = workspace.update_scheduler(view, SchedulerMessage::Page(Page::Journal));
    assert_no_connection(model);
    (workspace, view)
}

fn review_legacy_swarm(workspace: &mut Workspace, view: window::Id) {
    let scheduler = workspace.scheduler.as_ref().unwrap();
    let request = scheduler
        .begin_agent_review(&workspace.scheduler_views[&view])
        .unwrap();
    let action = request.job.action();
    let config = workspace.ai_configuration().unwrap();
    let profile = agent::ProfileChoice::available(&config)
        .into_iter()
        .find(|profile| profile.id == crate::ai_manager::ProfileRef::Numeric(1))
        .unwrap();
    let store = ConnectionStore::open(profile::connections_path().unwrap()).unwrap();
    let trust = crate::ssh_connection::trust_store().unwrap();
    let targets = action
        .targets()
        .map(|reference| {
            agent::Target::capture(reference, store.get(reference.id).unwrap(), &trust).unwrap()
        })
        .collect();
    let redactor = workspace
        .scheduler_review_redactor(action, &store, &trust)
        .unwrap();
    let revision = workspace.firing_profile_revision(&profile.id).unwrap();
    let (transport, context_limit) = config.agent_transport_for_reference(&profile.id).unwrap();
    let prompts = action
        .targets()
        .map(|reference| {
            Prompt::new(
                transport.clone(),
                context_limit,
                OPENSSH_GOAL,
                action.working_directory.as_deref().unwrap(),
                vec![reference.id],
                redactor.clone(),
                crate::workspace::terminal_ai::mask::prompt,
            )
            .unwrap()
            .for_swarm()
            .bind_profile(revision)
        })
        .collect();
    scheduler
        .finish_agent_firing_review(
            workspace.scheduler_views.get_mut(&view).unwrap(),
            request,
            profile,
            targets,
            prompts,
        )
        .unwrap();
}

fn generate_commands(
    workspace: &mut Workspace,
    task: Task<Message>,
    model: &TcpListener,
    target: u64,
    directory: &Path,
    commands: &[String],
    deadline: Instant,
) -> Ticket {
    let (ticket, _) = generate_reply(
        workspace,
        task,
        model,
        json!({"question": OPENSSH_GOAL, "targets": [target], "working_directory": directory}),
        commands_reply(target, commands),
        None,
        deadline,
    );
    ticket
}

fn commands_reply(target: u64, commands: &[String]) -> Value {
    json!({"state":"commands","commands": commands.iter()
        .map(|command| json!({"target":target,"command":command,"purpose":"Inspect the fixture"}))
        .collect::<Vec<_>>()})
}

fn generate_reply(
    workspace: &mut Workspace,
    task: Task<Message>,
    model: &TcpListener,
    expected_input: Value,
    response: Value,
    expected_system: Option<&str>,
    deadline: Instant,
) -> (Ticket, Task<Message>) {
    let mut events = iced_runtime::task::into_stream(task).expect("scheduled model task");
    let (ticket, permit) = model_dispatch(&mut events, deadline);
    let mut client = connect_model(workspace, model, &ticket, permit);
    assert_model_request(&mut client, expected_input, expected_system);
    send_model_reply(client, response);
    let followup = finish_model(workspace, &mut events, &ticket, deadline);
    assert!(next_item(&mut events, deadline).is_none());
    (ticket, followup)
}

fn model_dispatch(
    events: &mut (impl Stream<Item = Action<Message>> + Unpin),
    deadline: Instant,
) -> (Ticket, async_channel::Sender<()>) {
    match next_item(events, deadline) {
        Some(Action::Output(Message::ScheduledAgent(Event(Progress::ModelDispatch(
            ticket,
            permit,
        ))))) => (ticket, permit),
        Some(Action::Output(Message::ScheduledAgent(Event(Progress::StartFailed(
            _,
            index,
            round,
            outcome,
            detail,
        ))))) => panic!("target {index}, round {round} could not start ({outcome:?}): {detail}"),
        _ => panic!("the firing did not ask for model dispatch authority"),
    }
}

fn connect_model(
    workspace: &mut Workspace,
    model: &TcpListener,
    ticket: &Ticket,
    permit: async_channel::Sender<()>,
) -> BufReader<TcpStream> {
    assert_no_connection(model);
    let _ = workspace.update(Message::ScheduledAgent(Event(Progress::ModelDispatch(
        ticket.clone(),
        permit,
    ))));
    test_support::accept(model)
}

fn assert_model_request(
    client: &mut BufReader<TcpStream>,
    expected_input: Value,
    expected_system: Option<&str>,
) {
    let request = test_support::request(client);
    if let Some(expected_system) = expected_system {
        assert_eq!(request["messages"][0]["content"], expected_system);
    }
    let input: serde_json::Value =
        serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input, expected_input);
}

fn send_model_reply(mut client: BufReader<TcpStream>, response: Value) {
    client
        .get_mut()
        .write_all(test_support::fixture_response("200 OK", "", &response.to_string()).as_bytes())
        .unwrap();
}

fn finish_model(
    workspace: &mut Workspace,
    events: &mut (impl Stream<Item = Action<Message>> + Unpin),
    ticket: &Ticket,
    deadline: Instant,
) -> Task<Message> {
    let Some(Action::Output(Message::ScheduledAgent(Event(Progress::Generated(current, result))))) =
        next_item(events, deadline)
    else {
        panic!("the scheduled model did not generate a proposal");
    };
    assert_eq!(&current, ticket);
    assert!(
        result.is_ok(),
        "model decision was rejected: {:?}",
        result.as_ref().err()
    );
    workspace.update(Message::ScheduledAgent(Event(Progress::Generated(
        current, result,
    ))))
}

fn assert_proposal(workspace: &Workspace, ticket: &Ticket, commands: &[String]) {
    let review = workspace
        .scheduler
        .as_ref()
        .unwrap()
        .firing_reviews
        .get(ticket, Instant::now())
        .unwrap();
    assert_eq!(review.phase(), Phase::AwaitingReview);
    assert!(
        review
            .proposal()
            .unwrap()
            .iter()
            .map(|item| item.command.as_str())
            .eq(commands.iter().map(String::as_str))
    );
    assert_eq!(workspace.scheduler_workers, 0);
}

fn execute_commands(
    workspace: &mut Workspace,
    view: window::Id,
    ticket: &Ticket,
    ssh: &OpenSshFixture,
    observed_ssh: Option<&TcpListener>,
    marker: &Path,
    deadline: Instant,
) -> (Task<Message>, job_executor::agent::FiringResult) {
    let task =
        workspace.update_scheduler(view, SchedulerMessage::ApproveRunProposal(ticket.clone()));
    drive_execution(
        workspace,
        task,
        ticket,
        (ssh, ""),
        observed_ssh,
        marker,
        deadline,
    )
}

fn drive_execution(
    workspace: &mut Workspace,
    task: Task<Message>,
    ticket: &Ticket,
    (ssh, extra): (&OpenSshFixture, &str),
    observed_ssh: Option<&TcpListener>,
    marker: &Path,
    deadline: Instant,
) -> (Task<Message>, job_executor::agent::FiringResult) {
    let sshd = ssh.serve_with_extra("host", extra);
    let mut events = iced_runtime::task::into_stream(task).expect("approved execution task");
    let mut permits = 0;
    let mut completed = None;
    let mut relay = None;
    while let Some(event) = next_item(&mut events, deadline) {
        match event {
            Action::Output(Message::ScheduledAgent(Event(Progress::ExecutionPermit(
                current,
                purpose,
                permit,
            )))) => {
                assert_eq!(&current, ticket);
                if permits == 0 {
                    assert!(matches!(purpose, FiringPermit::Execute));
                    assert!(!marker.exists(), "SSH ran before the first worker permit");
                    if let Some(listener) = observed_ssh {
                        assert_no_connection(listener);
                    }
                }
                permits += 1;
                let _ = workspace.update(Message::ScheduledAgent(Event(
                    Progress::ExecutionPermit(current, purpose, permit),
                )));
                if permits == 1
                    && let Some(listener) = observed_ssh
                {
                    let port = ssh
                        .request(ssh.directory().join("client"))
                        .target
                        .endpoint
                        .port;
                    relay = Some(relay_ssh(
                        listener.try_clone().unwrap(),
                        ([127, 0, 0, 1], port).into(),
                    ));
                }
            }
            Action::Output(Message::ScheduledAgent(Event(Progress::Executed(current, result)))) => {
                assert_eq!(&current, ticket);
                assert_eq!(result.completion.id, ticket.run());
                assert!(result.completion.recording.is_none());
                let task = workspace.update(Message::ScheduledAgent(Event(Progress::Executed(
                    current,
                    result.clone(),
                ))));
                assert!(completed.replace((task, result)).is_none());
            }
            _ => panic!("unexpected action during the reviewed SSH firing"),
        }
    }
    assert!(permits >= 3);
    assert_eq!(workspace.scheduler_workers, 0);
    wait_until(deadline, || sshd.is_finished());
    let log = String::from_utf8_lossy(&sshd.join().unwrap().stderr).into_owned();
    assert_eq!(log.matches("Accepted publickey for ").count(), 1, "{log}");
    if let Some(relay) = relay {
        wait_until(deadline, || relay.is_finished());
        relay.join().unwrap();
    }
    completed.expect("the reviewed target did not finish")
}

fn next_item<T>(stream: &mut (impl Stream<Item = T> + Unpin), deadline: Instant) -> Option<T> {
    loop {
        if let Some(action) = stream.next().now_or_never() {
            return action;
        }
        assert!(Instant::now() < deadline, "scheduled firing task timed out");
        thread::sleep(Duration::from_millis(2));
    }
}

fn wait_until(deadline: Instant, mut finished: impl FnMut() -> bool) {
    while !finished() {
        assert!(Instant::now() < deadline, "OpenSSH fixture did not stop");
        thread::sleep(Duration::from_millis(5));
    }
}
