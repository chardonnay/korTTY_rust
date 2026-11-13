use super::*;

fn fixture(root: &Path) -> (Workspace, window::Id, Vec<Key>) {
    let config: Configuration = serde_json::from_value(serde_json::json!({"profiles": {
        "profiles":[{"id":1,"name":"Swarm fixture","endpoint":"127.0.0.1:12345","model":"test"}],
        "default":1,"last_id":1
    }}))
    .unwrap();
    config
        .save_fixture_at(root, &Configuration::default())
        .unwrap();
    let mut workspace = Workspace::default();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(root).unwrap());
    workspace.window_settings.session_journal.directory =
        root.join("journals").to_str().unwrap().into();
    let (owner, _) = workspace.open_window(false);
    local(&mut workspace, owner, false);
    local(&mut workspace, owner, false);
    let _ = workspace.open_agent_swarm(owner);
    let id = *workspace.terminal_agents.swarms.keys().next().unwrap();
    let keys = review_source(&mut workspace, id, root);
    let _ = workspace.swarm_action(id, Message::Mode(SwarmMode::Agent));
    let _ = workspace.swarm_action(id, Message::Goal("Inspect these two fixtures".into()));
    for &key in &keys {
        let _ = workspace.swarm_action(id, Message::Toggle(key, true));
    }
    (workspace, id, keys)
}

fn current(workspace: &Workspace, id: window::Id, key: Key) -> (Ticket, window::Id, Status) {
    let group = &workspace.terminal_agents.swarms[&id];
    let queue = group.queue.as_ref().unwrap();
    let ticket = queue.ticket(key).unwrap();
    (
        ticket,
        group.member_for(ticket).unwrap(),
        queue.current(ticket).unwrap().status,
    )
}

#[test]
fn unsuccessful_new_swarm_start_cannot_replay_the_older_round() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (mut workspace, id, keys) = fixture(&root);
    let start = workspace.swarm_action(id, Message::Start);
    drain(&mut workspace, start);
    let (ticket, _, _) = current(&workspace, id, keys[0]);
    let stop = workspace.swarm_action(id, Message::CancelAll);
    drain(&mut workspace, stop);
    let previous = workspace.terminal_agents.swarms[&id].round;
    assert!(workspace.terminal_agents.swarms[&id].can_restart_round());

    workspace.clipboard.policy.ai_swarm_feature_allowed = false;
    let denied = workspace.swarm_action(id, Message::Start);
    drain(&mut workspace, denied);
    workspace.clipboard.policy.ai_swarm_feature_allowed = true;
    let denied_round = workspace.terminal_agents.swarms[&id].round;
    assert_ne!(denied_round, previous);
    assert!(!workspace.terminal_agents.swarms[&id].can_restart_round());
    assert!(!workspace.terminal_agents.swarms[&id].can_restart_target(ticket));
    for message in [
        Message::RestartRound(denied_round),
        Message::RestartTarget(ticket),
    ] {
        let stale = workspace.swarm_action(id, message);
        drain(&mut workspace, stale);
        let group = &workspace.terminal_agents.swarms[&id];
        assert_eq!(group.round, denied_round);
        assert!(group.members.is_empty());
    }
}

#[test]
fn swarm_member_restarts_only_after_the_previous_attempt_and_drops_old_approvals() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (mut workspace, id, keys) = fixture(&root);
    let start = workspace.swarm_action(id, Message::Start);
    drain(&mut workspace, start);
    let (ticket, member, status) = current(&workspace, id, keys[0]);
    let (neighbor_ticket, neighbor, _) = current(&workspace, id, keys[1]);
    assert_eq!(status, Status::Review);
    let epoch = workspace.terminal_agents.runs[&member].epoch;
    let pause = workspace.swarm_action(id, Message::PauseTarget(ticket, true));
    drain(&mut workspace, pause);
    assert_eq!(current(&workspace, id, keys[0]).2, Status::Paused);
    assert_eq!(current(&workspace, id, keys[1]).2, Status::Review);
    let stale = workspace.swarm_action(id, Message::Member(member, MemberAction::Approve(epoch)));
    drain(&mut workspace, stale);
    assert_eq!(current(&workspace, id, keys[0]).2, Status::Paused);
    let resume = workspace.swarm_action(id, Message::PauseTarget(ticket, false));
    drain(&mut workspace, resume);
    assert_eq!(current(&workspace, id, keys[0]).2, Status::Review);

    // Simulate an outstanding directory worker. Restart must leave its slot owned until the
    // matching callback, even when a neighbouring target is already waiting for approval.
    let worker_epoch = workspace
        .terminal_agents
        .runs
        .get_mut(&member)
        .unwrap()
        .next(Phase::Directory, "Resolving the fixture directory…");
    let restart = workspace.swarm_action(id, Message::RestartTarget(ticket));
    drain(&mut workspace, restart);
    assert_eq!(
        current(&workspace, id, keys[0]),
        (ticket, member, Status::Cancelling)
    );
    let done = workspace.update_terminal_agent(
        member,
        RunMessage::Directory(worker_epoch, Err("fixture stopped".into())),
    );
    drain(&mut workspace, done);
    let (fresh, new_member, status) = current(&workspace, id, keys[0]);
    assert_ne!(fresh, ticket);
    assert_ne!(new_member, member);
    assert_eq!(status, Status::Review);
    assert_eq!(
        current(&workspace, id, keys[1]),
        (neighbor_ticket, neighbor, Status::Review)
    );
    assert!(
        workspace.terminal_agents.runs[&new_member]
            .activity
            .is_empty()
    );
    for event in [
        Message::StopTarget(ticket),
        Message::RestartTarget(ticket),
        Message::Member(member, MemberAction::Approve(epoch)),
    ] {
        let stale = workspace.swarm_action(id, event);
        drain(&mut workspace, stale);
        assert_eq!(
            current(&workspace, id, keys[0]),
            (fresh, new_member, Status::Review)
        );
    }
}

#[test]
fn swarm_round_restart_drains_attempt_journals_and_rechecks_the_original_panes() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (mut workspace, id, keys) = fixture(&root);
    let _ = workspace.swarm_action(id, Message::Journal(SwarmJournalEdit::Enabled(true)));
    let start = workspace.swarm_action(id, Message::Start);
    drain(&mut workspace, start);
    let round = workspace.terminal_agents.swarms[&id].round;
    let (ticket, member, _) = current(&workspace, id, keys[0]);
    let first = workspace.swarm_action(id, Message::RestartTarget(ticket));
    drain(&mut workspace, first);
    let (_, replacement, _) = current(&workspace, id, keys[0]);
    assert_ne!(member, replacement);
    let restart = workspace.swarm_action(id, Message::RestartRound(round));
    assert_eq!(workspace.terminal_agents.swarms[&id].round, round);
    assert!(
        workspace.terminal_agents.swarms[&id]
            .restarts
            .round_pending()
    );
    assert_eq!(workspace.terminal_agents.swarms[&id].journals_pending, 2);
    workspace
        .sessions
        .get_mut(&keys[0].pane().unwrap().0)
        .unwrap()
        .stream_generation += 1;
    drain(&mut workspace, restart);

    let group = &workspace.terminal_agents.swarms[&id];
    assert_eq!(group.round, round + 1);
    assert!(!group.restarts.round_pending());
    assert_eq!(
        group.queue.as_ref().unwrap().rows()[0].status,
        Status::Blocked
    );
    let (_, fresh_neighbor, status) = current(&workspace, id, keys[1]);
    assert_eq!(status, Status::Review);
    assert_ne!(fresh_neighbor, replacement);
    let catalog = crate::session_journal::catalog(&root.join("journals")).unwrap();
    assert_eq!(catalog.journals.len(), 3);
    let prior = catalog.journals[0].run_id.as_ref().unwrap();
    assert!(
        catalog
            .journals
            .iter()
            .all(|journal| journal.run_id.as_ref() == Some(prior))
    );
    assert!(
        catalog.journals.iter().all(|journal| journal.run_status
            == Some(crate::session_journal::tree::RunStatus::Cancelled))
    );
    let stale = workspace.swarm_action(id, Message::RestartRound(round));
    drain(&mut workspace, stale);
    assert_eq!(workspace.terminal_agents.swarms[&id].round, round + 1);
}
