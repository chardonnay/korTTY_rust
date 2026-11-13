use std::process::Command;
use std::time::{Duration, SystemTime};

use iced::futures::{StreamExt as _, executor::block_on};
use iced_runtime::Action;
use kortty_jobs::{Job, JobGraph, JobId, RunOutcome, Scheduler, SchedulerStore, SwarmTargetReport};
use serde_json::json;

use super::*;
use crate::scheduler_window::{SchedulerWindow, SshCommand};
use crate::snippets::tests::Profile;

const HOME_MARKER: &str = "KORTTY_SCHEDULER_SWARM_CHAT_FIXTURE";
const TEST_NAME: &str = "workspace::scheduler_access::swarm_chat::tests::scheduler_swarm_chat_publication_is_bound_to_profile_and_survives_restart";
const CONFLICT: &str = "11111111111111111111111111111111";
const SAVED: &str = "22222222222222222222222222222222";
const DELETED: &str = "33333333333333333333333333333333";
const UNRELATED: &str = "44444444444444444444444444444444";
const QUESTION: &str = "Inspect the fixture containing ***";
const PROFILE_ID: u64 = 91;

#[test]
fn scheduler_swarm_chat_publication_is_bound_to_profile_and_survives_restart() {
    let Some(home) = std::env::var_os(HOME_MARKER).map(PathBuf::from) else {
        let scratch = Profile::new();
        let home = scratch.0.canonicalize().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(HOME_MARKER, &home)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("LOCALAPPDATA", &home)
            .env("XDG_CONFIG_HOME", &home)
            .env("TEMP", &home)
            .env("TMP", &home)
            .env("TMPDIR", &home)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };
    let root = profile::root().unwrap();
    assert!(home.is_dir() && root.starts_with(&home) && !root.exists());
    std::fs::create_dir_all(&root).unwrap();

    let unrelated_root = home.join("different-profile");
    let unrelated_jobs = unrelated_root.join("jobs.json");
    prepare_outbox(
        &unrelated_jobs,
        &[UNRELATED],
        ProfileRef::Numeric(PROFILE_ID),
    );
    let mut unrelated = workspace_for(&unrelated_jobs);
    assert!(unrelated.scheduler.as_ref().unwrap().has_swarm_chat_seed());
    assert_eq!(unrelated.tick_swarm_chat_seed().units(), 0);
    assert!(swarm::list(&unrelated_root).unwrap().chats.is_empty());
    assert!(swarm::list(&root).unwrap().chats.is_empty());
    drop(unrelated);
    let unrelated_store = SchedulerStore::open(unrelated_jobs).unwrap();
    let unrelated_seed = unrelated_store.next_swarm_chat_seed(|_| true).unwrap();
    assert_eq!(unrelated_seed.origin.nonce, UNRELATED);
    assert_eq!(
        unrelated_seed.origin.profile,
        ProfileRef::Numeric(PROFILE_ID)
    );

    let jobs = profile::jobs_path().unwrap();
    let named = ProfileRef::Policy("policy-Swarm\u{202e}Fixture".into());
    prepare_outbox(&jobs, &[CONFLICT, SAVED, DELETED], named.clone());
    let conflicting = swarm::Origin::from_id(CONFLICT).unwrap();
    swarm::seed(
        &root,
        &conflicting,
        named.clone(),
        "Fixture profile",
        report("Different seed"),
        || Ok(()),
    )
    .unwrap()
    .expect("conflicting private conversation");

    let mut workspace = workspace_for(&jobs);
    defer_conflict(&mut workspace);
    let saved = publish(&mut workspace);
    assert_eq!(saved.nonce, SAVED);
    let saved_record = saved.result.as_ref().unwrap().as_ref().unwrap();
    assert_eq!(saved_record.id, SAVED);
    assert_eq!(saved_record.profile_id, named);
    assert_eq!(saved_record.profile_name, "Organization AI profile");
    assert_eq!(saved_record.turns[0].question, QUESTION);
    assert_eq!(saved_record.target_ids().collect::<Vec<_>>(), [31, 14]);
    assert!(
        swarm::load(&root, SAVED)
            .unwrap()
            .same_revision(saved_record)
    );
    assert!(pending(&workspace, SAVED));
    assert_eq!(
        workspace.update(Message::SchedulerSwarmChat(saved)).units(),
        0
    );
    assert!(!pending(&workspace, SAVED));

    let interrupted = publish(&mut workspace);
    assert_eq!(interrupted.nonce, DELETED);
    assert!(interrupted.result.as_ref().unwrap().is_some());
    assert!(pending(&workspace, DELETED));
    drop(workspace);

    let removed = swarm::load(&root, DELETED).unwrap();
    assert!(!swarm::delete(&root, &removed).unwrap());
    let mut workspace = workspace_for(&jobs);
    assert!(swarm::load(&root, SAVED).is_ok());
    assert!(!pending(&workspace, SAVED));
    assert!(pending(&workspace, DELETED));
    defer_conflict(&mut workspace);
    let tombstone = publish(&mut workspace);
    assert_eq!(tombstone.nonce, DELETED);
    assert!(matches!(&tombstone.result, Ok(None)));
    assert_eq!(
        workspace
            .update(Message::SchedulerSwarmChat(tombstone))
            .units(),
        0
    );
    assert!(!pending(&workspace, DELETED));
    assert_eq!(
        swarm::load(&root, DELETED).err().unwrap().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(workspace.tick_swarm_chat_seed().units(), 0);
    drop(workspace);

    let store = SchedulerStore::open(jobs).unwrap();
    assert_eq!(
        store.next_swarm_chat_seed(|_| true).unwrap().origin.nonce,
        CONFLICT
    );
    assert!(
        store
            .next_swarm_chat_seed(|seed| seed.origin.nonce != CONFLICT)
            .is_none()
    );
    let library = swarm::list(&root).unwrap();
    assert_eq!(library.chats.len(), 2);
    assert!(library.chats.iter().all(|row| row.id != DELETED));
}

fn workspace_for(jobs: &Path) -> Workspace {
    let mut workspace = Workspace {
        scheduler: Some(SchedulerWindow::open(jobs.to_owned()).unwrap()),
        ..Workspace::default()
    };
    workspace.set_test_ai_configuration(crate::ai_manager::Configuration::default());
    assert!(workspace.swarm_chat_access_allowed());
    workspace
}

fn prepare_outbox(jobs: &Path, nonces: &[&str], profile: ProfileRef) {
    let action: SshCommand = serde_json::from_value(json!({
        "connection": {"id":31,"target":{"host":"alpha.fixture.invalid","port":22,"username":"fixture"}},
        "additional_connections": [{"id":14,"target":{"host":"beta.fixture.invalid","port":22,"username":"fixture"}}],
        "working_directory": "/srv",
        "agent": {"kind":"swarm","probe":"swarm_iterative","profile":profile,"goal":"Inspect the fixture"}
    })).unwrap();
    let job_id = JobId::new("swarm-chat-fixture").unwrap();
    let graph = JobGraph::new(vec![
        Job::new(job_id.clone(), "Swarm chat fixture", action).unwrap(),
    ])
    .unwrap();
    let mut scheduler = Scheduler::new(graph);
    let mut store = SchedulerStore::open(jobs).unwrap();
    store
        .commit_with_actions(scheduler.graph(), &[], &[])
        .unwrap();
    let mut records = Vec::new();
    let mut reports = Vec::new();
    let mut origins = Vec::new();
    for (index, nonce) in nonces.iter().enumerate() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2 + index as u64);
        let run = scheduler.start_manual(&job_id, now).unwrap().started[0].id;
        let mut batch = scheduler.complete(run, RunOutcome::Success, now).unwrap();
        records.push(batch.finished.pop().unwrap());
        reports.push((run, report(QUESTION)));
        origins.push((
            run,
            SwarmChatOrigin {
                nonce: (*nonce).to_owned(),
                profile: profile.clone(),
            },
        ));
    }
    store
        .commit_records_with_swarm_chat_origins(
            scheduler.graph(),
            &records,
            &[],
            &[],
            &reports,
            &origins,
        )
        .unwrap();
}

fn report(question: &str) -> SwarmReport {
    SwarmReport {
        question: question.into(),
        targets: [31, 14]
            .map(|target| SwarmTargetReport {
                target,
                label: format!("Profile {target}"),
                outcome: RunOutcome::Success,
                detail: "Final masked result.".into(),
            })
            .into(),
        comparison: None,
    }
}

fn pending(workspace: &Workspace, nonce: &str) -> bool {
    workspace
        .scheduler
        .as_ref()
        .unwrap()
        .next_swarm_chat_seed(|seed| seed.origin.nonce == nonce)
        .is_some()
}

fn publish(workspace: &mut Workspace) -> Event {
    let task = workspace.tick_swarm_chat_seed();
    let mut stream = iced_runtime::task::into_stream(task).expect("a ready Swarm seed task");
    let Some(Action::Output(Message::SchedulerSwarmChat(event))) = block_on(stream.next()) else {
        panic!("the Swarm publication did not report its result");
    };
    assert!(block_on(stream.next()).is_none());
    event
}

fn defer_conflict(workspace: &mut Workspace) {
    let conflict = publish(workspace);
    assert_eq!(conflict.nonce, CONFLICT);
    assert!(conflict.result.is_err());
    assert_eq!(
        workspace
            .update(Message::SchedulerSwarmChat(conflict))
            .units(),
        0
    );
    assert!(pending(workspace, CONFLICT));
    assert!(
        workspace
            .scheduler_swarm_chat
            .retry_at
            .contains_key(CONFLICT)
    );
}
