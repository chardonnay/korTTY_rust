use serde_json::json;

use super::super::batch::{Message as Batch, Review};
use super::*;
use crate::workspace::terminal_agent::{Elevation, ReviewedCommand};

struct Fixture {
    workspace: Workspace,
    group: window::Id,
    members: Vec<window::Id>,
    directory: std::path::PathBuf,
    _profile: Profile,
}

impl Fixture {
    fn new() -> Self {
        let profile = Profile::new();
        let directory = profile.0.canonicalize().unwrap();
        let config: Configuration = serde_json::from_value(json!({"profiles": {
            "profiles":[{"id":1,"name":"Swarm fixture","endpoint":"127.0.0.1:12345","model":"test"}],
            "default":1,"last_id":1
        }})).unwrap();
        config
            .save_fixture_at(&directory, &Configuration::default())
            .unwrap();
        let mut workspace = Workspace::default();
        workspace.window_settings.terminal_notifications.ai_runs = false;
        workspace.set_test_ai_configuration(Configuration::load_fixture_from(&directory).unwrap());
        let (owner, _) = workspace.open_window(false);
        for _ in 0..3 {
            local(&mut workspace, owner, false);
        }
        local(&mut workspace, owner, true);
        let _ = workspace.open_agent_swarm(owner);
        let group = *workspace.terminal_agents.swarms.keys().next().unwrap();
        let keys = review_source(&mut workspace, group, &directory);
        assert_eq!(keys.len(), 3);
        let _ = workspace.swarm_action(group, Message::Mode(SwarmMode::Agent));
        let _ = workspace.swarm_action(group, Message::Goal("Review fixture commands".into()));
        for key in &keys {
            let _ = workspace.swarm_action(group, Message::Toggle(*key, true));
        }
        let start = workspace.swarm_action(group, Message::Start);
        drain(&mut workspace, start);
        let group_state = &workspace.terminal_agents.swarms[&group];
        let members = keys
            .into_iter()
            .map(|key| {
                *group_state
                    .members
                    .iter()
                    .find(|(_, ticket)| ticket.key() == key)
                    .unwrap()
                    .0
            })
            .collect::<Vec<_>>();
        assert!(
            members
                .iter()
                .all(|member| workspace.terminal_agents.runs[member].phase == Phase::ProbeApproval)
        );
        let mut fixture = Self {
            workspace,
            group,
            members,
            directory,
            _profile: profile,
        };
        for (index, (command, elevation)) in [
            ("echo first", Elevation::None),
            ("echo second", Elevation::None),
            ("echo elevated", Elevation::PasswordlessSudo),
        ]
        .into_iter()
        .enumerate()
        {
            fixture.command(index, command, elevation);
        }
        fixture
    }

    fn command(&mut self, index: usize, command: &str, elevation: Elevation) {
        self.run(index).commands.clear();
        self.append(index, command, elevation);
        self.run(index).next(Phase::Approval, "Review command");
    }

    fn append(&mut self, index: usize, command: &str, elevation: Elevation) {
        self.run(index).commands.push_back(ReviewedCommand {
            proposal: crate::terminal_agent::Command {
                command: command.into(),
                purpose: "Fixture command".into(),
                risk: crate::terminal_agent::Risk::ReadOnly,
                elevation,
            },
            sudo: None,
        });
    }

    fn run(&mut self, index: usize) -> &mut Run {
        self.workspace
            .terminal_agents
            .runs
            .get_mut(&self.members[index])
            .unwrap()
    }

    fn send(&mut self, message: Batch) -> Task<Desktop> {
        self.workspace
            .swarm_action(self.group, Message::Batch(message))
    }

    fn review(&mut self) -> Arc<Review> {
        assert_eq!(self.send(Batch::Review).units(), 0);
        self.workspace.terminal_agents.swarms[&self.group]
            .batch_review
            .clone()
            .unwrap_or_else(|| {
                panic!(
                    "{}",
                    self.workspace.terminal_agents.swarms[&self.group].notice
                )
            })
    }

    fn current_local_directory(&mut self, index: usize) -> crate::terminal_agent::LocalDirectory {
        let Some(crate::workspace::terminal_agent::RunTarget::Attached(
            crate::terminal_agent::Target::Local { directory, .. },
        )) = &self.run(index).target
        else {
            panic!("expected a reviewed local directory")
        };
        directory.clone()
    }

    fn finish_directory_check(&mut self, index: usize) {
        let epoch = self.run(index).epoch;
        let directory = self.current_local_directory(index);
        let member = self.members[index];
        let task = self
            .workspace
            .update_terminal_agent(member, RunMessage::Directory(epoch, Ok(directory)));
        assert!(task.units() >= 1);
        // The process is never started. A synthetic result below drives the same state transitions.
        drop(task);
    }

    fn finish_command(&mut self, index: usize, code: u32) -> Task<Desktop> {
        let epoch = self.run(index).epoch;
        let member = self.members[index];
        self.workspace.update_terminal_agent(
            member,
            RunMessage::Command(
                epoch,
                Arc::new(Ok(crate::terminal_agent::Outcome {
                    status: crate::terminal_agent::Status::Exited(code),
                    stdout: Default::default(),
                    stderr: Default::default(),
                })),
            ),
        )
    }
}

#[test]
fn swarm_batch_approves_only_the_snapshot_and_never_the_later_target() {
    let mut fixture = Fixture::new();
    let review = fixture.review();
    fixture.command(2, "echo later", Elevation::None);
    let task = fixture.send(Batch::Confirm(review.clone()));
    // Approval reaches the normal per-pane directory recheck. The test drops
    // its tasks before polling, so no shell command is started.
    assert!(task.units() >= 2);
    drop(task);
    assert!(fixture.run(0).phase == (Phase::ReviewedDirectory { probe: false }));
    assert!(fixture.run(1).phase == (Phase::ReviewedDirectory { probe: false }));
    assert!(fixture.run(2).phase == Phase::Approval);
    assert_eq!(fixture.send(Batch::Confirm(review)).units(), 0);
}

#[test]
fn swarm_batch_fails_closed_if_a_command_or_local_destination_changes() {
    let mut fixture = Fixture::new();
    let review = fixture.review();
    fixture
        .run(1)
        .commands
        .front_mut()
        .unwrap()
        .proposal
        .command = "echo changed".into();
    assert_eq!(fixture.send(Batch::Confirm(review)).units(), 0);
    assert!(fixture.run(0).phase == Phase::Approval);
    assert!(fixture.run(1).phase == Phase::Approval);
    assert!(
        fixture.workspace.terminal_agents.swarms[&fixture.group]
            .notice
            .contains("changed")
    );

    let review = fixture.review();
    let directory = fixture.directory.join("different-destination");
    std::fs::create_dir(&directory).unwrap();
    let Some(crate::workspace::terminal_agent::RunTarget::Attached(
        crate::terminal_agent::Target::Local { directory: old, .. },
    )) = &mut fixture.run(1).target
    else {
        panic!("expected local target")
    };
    *old = crate::terminal_agent::LocalDirectory::capture(directory).unwrap();
    assert_eq!(fixture.send(Batch::Confirm(review)).units(), 0);
    assert!(fixture.run(0).phase == Phase::Approval);
    assert!(fixture.run(1).phase == Phase::Approval);
}

#[test]
fn swarm_batch_cancel_pause_and_policy_revocation_leave_commands_waiting() {
    let mut fixture = Fixture::new();
    let cancelled = fixture.review();
    assert_eq!(fixture.send(Batch::Cancel(cancelled.clone())).units(), 0);
    let pending = fixture.review();
    assert_eq!(fixture.send(Batch::Confirm(cancelled)).units(), 0);
    assert!(
        fixture.workspace.terminal_agents.swarms[&fixture.group]
            .batch_review
            .is_some()
    );
    let _ = fixture
        .workspace
        .swarm_action(fixture.group, Message::Pause(true));
    let _ = fixture
        .workspace
        .swarm_action(fixture.group, Message::Pause(false));
    assert_eq!(fixture.send(Batch::Confirm(pending)).units(), 0);

    let revoked = fixture.review();
    fixture.workspace.clipboard.policy.ai_agent_execution = AiAgentExecution::ReadOnly;
    assert_eq!(fixture.send(Batch::Confirm(revoked)).units(), 0);
    assert!(fixture.run(0).phase == Phase::Approval);
    assert!(fixture.run(1).phase == Phase::Approval);
    assert!(
        fixture.workspace.terminal_agents.swarms[&fixture.group]
            .notice
            .contains("blocks")
    );
}

#[test]
fn swarm_batch_keeps_literal_elevation_in_single_target_review() {
    let mut fixture = Fixture::new();
    fixture.command(1, "/usr/bin/sudo -n echo privileged", Elevation::None);
    assert_eq!(fixture.send(Batch::Review).units(), 0);
    let group = &fixture.workspace.terminal_agents.swarms[&fixture.group];
    assert!(group.batch_review.is_none());
    assert!(group.notice.contains("At least two"));
}

#[test]
fn swarm_batch_runs_the_reviewed_prefix_in_order_and_leaves_elevation_and_new_work_for_review() {
    let mut fixture = Fixture::new();
    fixture.append(0, "echo third", Elevation::None);
    fixture.append(0, "sudo echo fourth", Elevation::None);
    let review = fixture.review();
    drop(fixture.send(Batch::Confirm(review)));
    fixture.finish_directory_check(0);
    assert!(fixture.run(0).phase == Phase::Running);
    assert_eq!(
        fixture.run(0).activity.back().map(String::as_str),
        Some("$ echo first")
    );
    let second = fixture.finish_command(0, 0);
    assert!(second.units() >= 1);
    drop(second);
    assert!(fixture.run(0).phase == (Phase::ReviewedDirectory { probe: false }));
    fixture.finish_directory_check(0);
    assert_eq!(
        fixture.run(0).activity.back().map(String::as_str),
        Some("$ echo third")
    );
    drop(fixture.finish_command(0, 0));
    assert!(fixture.run(0).phase == Phase::Approval);
    assert!(fixture.run(0).batch_commands.is_none());
    assert_eq!(
        fixture.run(0).reviewed_command().unwrap().1,
        "sudo echo fourth"
    );

    fixture.command(0, "echo newly proposed", Elevation::None);
    assert!(fixture.run(0).phase == Phase::Approval);
    assert!(fixture.run(0).batch_commands.is_none());
}

#[test]
fn swarm_batch_rejects_mutated_tails_and_revokes_on_failure_or_per_command_policy() {
    let mut fixture = Fixture::new();
    fixture.append(0, "echo second reviewed", Elevation::None);
    let review = fixture.review();
    fixture.run(0).commands.back_mut().unwrap().proposal.command = "echo changed tail".into();
    assert_eq!(fixture.send(Batch::Confirm(review)).units(), 0);
    assert!(fixture.run(0).phase == Phase::Approval);

    let review = fixture.review();
    fixture.workspace.clipboard.policy.ai_agent_execution = AiAgentExecution::Confirm;
    assert_eq!(fixture.send(Batch::Confirm(review)).units(), 0);
    assert!(fixture.run(0).batch_commands.is_none());
    assert!(
        fixture.workspace.terminal_agents.swarms[&fixture.group]
            .notice
            .contains("separate approval")
    );
    fixture.workspace.clipboard.policy.ai_agent_execution = AiAgentExecution::Allow;

    let review = fixture.review();
    drop(fixture.send(Batch::Confirm(review)));
    fixture.finish_directory_check(0);
    drop(fixture.finish_command(0, 1));
    assert!(fixture.run(0).phase == Phase::Approval);
    assert!(fixture.run(0).batch_commands.is_none());

    // A policy update after the first command has already started also revokes its tail.
    fixture.command(0, "echo first again", Elevation::None);
    fixture.append(0, "echo withheld", Elevation::None);
    let review = fixture.review();
    drop(fixture.send(Batch::Confirm(review)));
    fixture.finish_directory_check(0);
    fixture.workspace.clipboard.policy.ai_agent_execution = AiAgentExecution::Confirm;
    drop(fixture.finish_command(0, 0));
    assert!(fixture.run(0).phase == Phase::Approval);
    assert!(fixture.run(0).batch_commands.is_none());
}

#[test]
fn swarm_batch_pause_revokes_in_flight_directory_checks() {
    let mut fixture = Fixture::new();
    fixture.append(0, "echo withheld", Elevation::None);
    let review = fixture.review();
    drop(fixture.send(Batch::Confirm(review)));
    let epoch = fixture.run(0).epoch;
    let member = fixture.members[0];
    let directory = fixture.current_local_directory(0);
    let ticket = fixture.workspace.terminal_agents.swarms[&fixture.group].members[&member];
    drop(
        fixture
            .workspace
            .swarm_action(fixture.group, Message::PauseTarget(ticket, true)),
    );
    drop(
        fixture
            .workspace
            .swarm_action(fixture.group, Message::PauseTarget(ticket, false)),
    );
    assert!(fixture.run(0).phase == Phase::Approval);
    assert!(fixture.run(0).batch_commands.is_none());
    assert_eq!(
        fixture
            .workspace
            .update_terminal_agent(member, RunMessage::Directory(epoch, Ok(directory)))
            .units(),
        0
    );
    assert!(fixture.run(0).phase == Phase::Approval);
}
