use crate::session_journal::{
    self,
    automation::Closing,
    tree::{RunStatus, SourceKind},
};

use super::*;

fn journal_fixture(root: &Path) -> (Workspace, window::Id, Vec<Key>) {
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
    local(&mut workspace, owner, true);
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

fn deny_automation_journals(workspace: &mut Workspace) {
    workspace.clipboard.policy = crate::clipboard_policy::parse(
        "[meta]\nschema-version = 1\n[[rule]]\n[rule.session-journal]\nautomation-allowed = false\n",
    )
    .unwrap();
    assert!(workspace.journal_allowed());
}

#[test]
fn swarm_journal_setting_records_each_completed_target_with_one_run_id() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (mut workspace, id, keys) = journal_fixture(&root);
    assert_eq!(keys.len(), 2);
    let _ = workspace.swarm_action(id, Message::Journal(SwarmJournalEdit::Enabled(true)));
    let task = workspace.swarm_action(id, Message::Start);
    drain(&mut workspace, task);
    assert!(workspace.swarm_journals_are_writing());
    for key in keys {
        let task = workspace.swarm_action(id, Message::Cancel(key));
        drain(&mut workspace, task);
    }
    assert!(!workspace.swarm_journals_are_writing());
    assert_eq!(workspace.terminal_agents.swarms[&id].journal.links.len(), 2);
    let catalog = session_journal::catalog(&root.join("journals")).unwrap();
    assert_eq!(catalog.journals.len(), 2);
    let run_id = catalog.journals[0].run_id.as_ref().unwrap();
    assert_eq!(run_id.len(), 36);
    for row in &catalog.journals {
        assert_eq!(row.run_id.as_ref(), Some(run_id));
        assert_eq!(row.source_kind, SourceKind::Swarm);
        assert_eq!(row.run_status, Some(RunStatus::Cancelled));
    }
}

#[test]
fn swarm_automation_journal_policy_rejects_stale_settings_and_runs_the_queue() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (mut workspace, id, keys) = journal_fixture(&root);
    let _ = workspace.swarm_action(id, Message::Journal(SwarmJournalEdit::Enabled(true)));
    let _ = workspace.swarm_action(
        id,
        Message::Journal(SwarmJournalEdit::Closing(Closing::Always)),
    );
    assert_eq!(
        workspace.terminal_agents.swarms[&id]
            .journal
            .draft
            .build()
            .unwrap()
            .unwrap()
            .closing,
        Closing::Always
    );

    deny_automation_journals(&mut workspace);
    let task = workspace.swarm_action(id, Message::Start);
    drain(&mut workspace, task);
    let group = &workspace.terminal_agents.swarms[&id];
    assert!(group.queue.as_ref().is_some_and(|queue| !queue.complete()));
    assert!(!group.journal_allowed);
    assert!(group.journal_run.is_none());
    for key in keys {
        let task = workspace.swarm_action(id, Message::Cancel(key));
        drain(&mut workspace, task);
    }
    let _ = workspace.swarm_action(id, Message::Journal(SwarmJournalEdit::Enabled(false)));
    let _ = workspace.swarm_action(
        id,
        Message::Journal(SwarmJournalEdit::Closing(Closing::Off)),
    );
    let group = &workspace.terminal_agents.swarms[&id];
    assert!(!group.journal_allowed && group.journal_run.is_none());
    assert_eq!(
        group.journal.draft.build().unwrap().unwrap().closing,
        Closing::Always
    );
    let catalog = session_journal::catalog(&root.join("journals")).unwrap();
    assert!(catalog.journals.is_empty());
}

#[test]
fn swarm_automation_journal_revocation_stops_pending_capture() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let (mut workspace, id, keys) = journal_fixture(&root);
    let _ = workspace.swarm_action(id, Message::Journal(SwarmJournalEdit::Enabled(true)));
    let task = workspace.swarm_action(id, Message::Start);
    drain(&mut workspace, task);
    let pending = workspace.swarm_action(id, Message::Cancel(keys[0]));
    assert_eq!(workspace.terminal_agents.swarms[&id].journals_pending, 1);

    deny_automation_journals(&mut workspace);
    workspace.revalidate_automation_journals();
    workspace.revalidate_swarm_automation_journals();
    let group = &workspace.terminal_agents.swarms[&id];
    assert!(!group.journal_allowed);
    assert!(group.journal_run.is_none());
    assert!(group.queue.as_ref().is_some_and(|queue| !queue.complete()));
    assert!(workspace.swarm_journals_are_writing());

    drain(&mut workspace, pending);
    assert!(!workspace.swarm_journals_are_writing());
    let task = workspace.swarm_action(id, Message::Cancel(keys[1]));
    drain(&mut workspace, task);
    let catalog = session_journal::catalog(&root.join("journals")).unwrap();
    assert!(catalog.journals.is_empty());
}
