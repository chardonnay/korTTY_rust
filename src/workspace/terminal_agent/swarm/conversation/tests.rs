use std::io::Write as _;
use std::process::Command;

use iced::futures::{StreamExt as _, executor::block_on};
use kortty_vault::{CredentialInput, Secret, Vault};
use serde_json::{Value, json};

use super::super::{MemberAction, Phase, comparison, request_review};
use super::*;
use crate::connection_store::{Connection, Store};
use crate::local_ai::test_support;
use crate::snippets::tests::Profile;
use crate::vault_window::VaultWindow;

#[cfg(unix)]
mod openssh;

const FIXTURE: &str = "KORTTY_TEST_SAVED_SWARM_HOME";
const TEST_NAME: &str = "workspace::terminal_agent::swarm::conversation::tests::saved_swarm_reviews_fresh_targets_and_appends_only_the_new_masked_turn";
const SSH_SECRET: &str = "swarm-fixture-word";
const PINNED: &str = "Review the local marker *** in this fixture.";

#[test]
fn saved_swarm_reviews_fresh_targets_and_appends_only_the_new_masked_turn() {
    let Some(home) = std::env::var_os(FIXTURE).map(PathBuf::from) else {
        let scratch = Profile::new();
        let home = scratch.0.canonicalize().unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(FIXTURE, &home)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("LOCALAPPDATA", &home)
            .env("XDG_CONFIG_HOME", &home)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
        return;
    };
    let root = home.join("private-profile");
    std::fs::create_dir(&root).unwrap();
    crate::profile::set_runtime_root(root.clone());
    let (model, _) = test_support::listener();
    let (ssh, _) = test_support::listener();
    let ssh_port = ssh.local_addr().unwrap().port();
    let settings = json!({
        "profiles": {"profiles":[{"id":1,"name":"Swarm fixture","endpoint":model.local_addr().unwrap().to_string(),"model":"fixture:local"}],"default":1,"last_id":1},
        "skills":{"last_id":51,"skills":{"51":{"name":"Target pin","target":"CONNECTION","enabled":true,
            "content":format!("Review the local marker {SSH_SECRET} in this fixture.")}}}
    });
    let config: Configuration = serde_json::from_value(settings.clone()).unwrap();
    config
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let vault_path = crate::profile::vault_path().unwrap();
    std::fs::create_dir_all(vault_path.parent().unwrap()).unwrap();
    let mut vault = Vault::create(&vault_path, b"saved-swarm-fixture-master").unwrap();
    let credential = vault
        .save(
            CredentialInput {
                label: "Saved Swarm SSH fixture".into(),
                username: "fixture".into(),
                server_pattern: "127.0.0.1".into(),
                ..Default::default()
            },
            Secret::new(SSH_SECRET.into()),
        )
        .unwrap();
    let mut connection: Connection = serde_json::from_value(json!({
        "id":0,"name":"Current fixture target","tag":"","jump":null,
        "emulation":"xterm-256color","tunnels":[],
        "login":{"endpoint":{"host":"127.0.0.1","port":ssh_port,"username":"fixture"},
            "auth":{"vault-password":credential},"keyboard_interactive":false},
    }))
    .unwrap();
    connection.terminal.ai_skills = vec![51];
    let mut connections = Store::open(crate::profile::connections_path().unwrap()).unwrap();
    let folder = connections.create_group(None, "Swarm fixtures").unwrap();
    let target = connections
        .save_in_group(connection, Some(&folder))
        .unwrap();
    let endpoint = kortty_ssh::Endpoint::new("127.0.0.1", ssh_port).unwrap();
    let pin = kortty_ssh::HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
    )
    .unwrap();
    crate::ssh_connection::trust_store()
        .unwrap()
        .remember(&endpoint, &pin)
        .unwrap();
    let missing = target + 1;
    let origin = store::Origin::from_id(&crate::private_files::nonce().unwrap()).unwrap();
    let record = store::seed(
        &root,
        &origin,
        ProfileRef::Numeric(1),
        "Swarm fixture",
        SwarmReport {
            question: "prior-private-question-marker".into(),
            targets: [target, missing]
                .into_iter()
                .map(|id| SwarmTargetReport {
                    target: id,
                    label: format!("Historical target {id}"),
                    outcome: RunOutcome::Success,
                    detail: "prior-private-answer-marker".into(),
                })
                .collect(),
            comparison: None,
        },
        || Ok(()),
    )
    .unwrap()
    .unwrap();
    let mut workspace = Workspace::default();
    workspace.window_settings.terminal_notifications.ai_runs = false;
    workspace.vault = Some(VaultWindow::with_unlocked_fixture(vault_path, vault));
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let (owner, _) = workspace.open_window(false);
    let assert_idle = || {
        for listener in [&model, &ssh] {
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
    };

    let named = ProfileRef::Policy("policy-SavedSwarm".into());
    let named_origin = store::Origin::from_id(&crate::private_files::nonce().unwrap()).unwrap();
    let named_record = store::seed(
        &root,
        &named_origin,
        named.clone(),
        "Organization AI profile",
        SwarmReport {
            question: record.turns[0].question.clone(),
            targets: record.turns[0].targets.clone(),
            comparison: None,
        },
        || Ok(()),
    )
    .unwrap()
    .unwrap();
    let project = |config: Configuration, id: &str| {
        let policy = crate::clipboard_policy::parse(&format!(
            "[meta]\nschema-version = 1\n[[ai-profile]]\nid = '{id}'\n\
             name = 'Swarm fixture'\nprovider = 'lm-studio'\nendpoint = 'http://{}'\nmodel = 'fixture:local'\n",
            model.local_addr().unwrap(),
        ))
        .unwrap();
        config.with_policy_profiles(&policy).unwrap()
    };
    let projected = |id| project(Configuration::load_fixture_from(&root).unwrap(), id);
    workspace.set_test_ai_configuration(projected("policy-savedswarm"));
    let _ = workspace.open_saved_swarm(owner, root.clone(), named_record);
    let named_group = *workspace.terminal_agents.swarms.keys().next().unwrap();
    settle(
        &mut workspace,
        headless::catalog(named_group, Some(vec![target, missing])),
    );
    let group = &workspace.terminal_agents.swarms[&named_group];
    assert!(group.profile.is_none());
    assert!(group.modes.contains(&SwarmMode::Advisory));
    assert_eq!(group.notice, super::super::SAVED_PROFILE_UNAVAILABLE);

    workspace.set_test_ai_configuration(projected("policy-SavedSwarm"));
    let refreshed = workspace.swarm_action(named_group, Message::RefreshSavedTargets);
    settle(&mut workspace, refreshed);
    let group = &workspace.terminal_agents.swarms[&named_group];
    assert_eq!(group.profile.as_ref().unwrap().reference, named);
    assert!(group.notice.is_empty());
    assert_idle();
    let _ = workspace.swarm_action(named_group, Message::Close);
    assert!(workspace.terminal_agents.swarms.is_empty());
    store::delete(&root, &store::load(&root, named_origin.id()).unwrap()).unwrap();

    let low = projected("policy-SavedSwarm");
    let runtime = low.resolve_profile_reference(&named).unwrap();
    let alias = low.legacy_policy_alias(runtime).unwrap();
    let legacy = ProfileRef::Numeric(alias);
    let legacy_origin = store::Origin::from_id(&crate::private_files::nonce().unwrap()).unwrap();
    let legacy_record = store::seed(
        &root,
        &legacy_origin,
        legacy.clone(),
        "Organization AI profile",
        SwarmReport {
            question: record.turns[0].question.clone(),
            targets: record.turns[0].targets.clone(),
            comparison: None,
        },
        || Ok(()),
    )
    .unwrap()
    .unwrap();
    let mut high = serde_json::to_value(&low).unwrap();
    high["profiles"]["profiles"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": alias, "name": "User profile", "model": "user-model"
        }));
    high["profiles"]["last_id"] = json!(u64::MAX);
    let high = project(serde_json::from_value(high).unwrap(), "policy-SavedSwarm");
    assert_eq!(high.resolve_profile_reference(&legacy), Some(alias.into()));

    workspace.set_test_ai_configuration(low);
    let _ = workspace.open_saved_swarm(owner, root.clone(), legacy_record);
    let group = *workspace.terminal_agents.swarms.keys().next().unwrap();
    settle(
        &mut workspace,
        headless::catalog(group, Some(vec![target, missing])),
    );
    workspace.set_test_ai_configuration(high);
    let refreshed = workspace.swarm_action(group, Message::RefreshSavedTargets);
    settle(&mut workspace, refreshed);
    let selected = workspace.terminal_agents.swarms[&group]
        .profile
        .as_ref()
        .unwrap();
    assert_eq!(selected.reference, named);
    assert_eq!(
        store::load(&root, legacy_origin.id()).unwrap().profile_id,
        legacy
    );
    let _ = workspace.swarm_action(group, Message::Close);
    store::delete(&root, &store::load(&root, legacy_origin.id()).unwrap()).unwrap();

    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());

    verify_named_draft(&mut workspace, owner, &root, target, &model, &assert_idle);

    let _ = workspace.open_agent_swarm(owner);
    let fresh = *workspace.terminal_agents.swarms.keys().next().unwrap();
    settle(&mut workspace, headless::catalog(fresh, None));
    let _ = workspace.swarm_action(fresh, Message::Mode(SwarmMode::Advisory));
    let _ = workspace.swarm_action(fresh, Message::ConnectionGroup(folder.clone()));
    let _ = workspace.swarm_action(fresh, Message::AddConnectionGroup);
    assert_eq!(
        workspace.terminal_agents.swarms[&fresh].selected,
        HashSet::from([Key::Saved(target)])
    );
    let _ = workspace.swarm_action(fresh, Message::Goal(format!("Fresh advisory {SSH_SECRET}")));
    let staged = workspace.swarm_action(fresh, Message::Start);
    settle(&mut workspace, staged);
    assert_idle();
    let member = sole_member(&workspace, fresh);
    let (task, messages) = send_model_review(&mut workspace, fresh, member);
    let wire = reply_reviewed(&model, &messages, "Fresh advisory answer.");
    let input: Value =
        serde_json::from_str(wire["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["goal"], "Fresh advisory ***");
    assert!(input["target"].is_null());
    assert!(
        input["context"]
            .as_str()
            .unwrap()
            .contains("Current fixture target")
    );
    assert!(messages[0].contains(PINNED));
    assert!(!wire.to_string().contains(SSH_SECRET));
    settle(&mut workspace, task);
    assert_idle();
    let save = workspace.swarm_action(fresh, Message::SaveConversation);
    settle(&mut workspace, save);
    assert_eq!(store::list(&root).unwrap().chats.len(), 1);
    let _ = workspace.swarm_action(fresh, Message::ConversationTitle("Advisory review".into()));
    let save = workspace.swarm_action(fresh, Message::ConfirmConversationSave);
    settle(&mut workspace, save);
    assert_eq!(
        workspace.terminal_agents.swarms[&fresh].notice,
        "Conversation saved. Open it in Saved Chats to continue."
    );
    let initial = store::list(&root)
        .unwrap()
        .chats
        .into_iter()
        .find(|entry| entry.id != record.id)
        .expect("new conversation");
    let initial = store::load(&root, &initial.id).unwrap();
    assert_eq!(initial.title(), "Advisory review");
    assert_eq!(initial.turns[0].question, "Fresh advisory ***");
    assert_eq!(initial.turns[0].targets[0].detail, "Fresh advisory answer.");
    assert_eq!(
        initial.target_route(target).unwrap().group.as_deref(),
        Some(folder.as_str())
    );
    assert_eq!(initial.target_ids().collect::<Vec<_>>(), [target]);
    let duplicate = workspace.swarm_action(fresh, Message::SaveConversation);
    settle(&mut workspace, duplicate);
    assert_eq!(store::list(&root).unwrap().chats.len(), 2);
    let _ = workspace.swarm_action(fresh, Message::Close);
    assert!(workspace.terminal_agents.swarms.is_empty());

    let _ = workspace.open_saved_swarm(owner, root.clone(), initial.clone());
    let restored = *workspace.terminal_agents.swarms.keys().next().unwrap();
    settle(
        &mut workspace,
        headless::catalog(restored, Some(vec![target])),
    );
    let _ = workspace.swarm_action(restored, Message::SearchHistory("ADVISORY ANSWER".into()));
    let group = &workspace.terminal_agents.swarms[&restored];
    assert_eq!(group.selected, HashSet::from([Key::Saved(target)]));
    let chat = group.conversation.as_ref().unwrap();
    assert_eq!(chat.search_matches(), [0]);
    assert!(chat.export(Format::Text).contains("Fresh advisory answer."));
    let rename = workspace.swarm_action(restored, Message::RenameConversation);
    settle(&mut workspace, rename);
    let _ = workspace.swarm_action(
        restored,
        Message::ConversationTitle(format!("Vault value {SSH_SECRET}")),
    );
    let rejected = workspace.swarm_action(restored, Message::ConfirmConversationSave);
    settle(&mut workspace, rejected);
    assert!(
        workspace.terminal_agents.swarms[&restored]
            .notice
            .contains("private or redacted")
    );
    assert!(
        store::load(&root, &initial.id)
            .unwrap()
            .same_revision(&initial)
    );

    let _ = workspace.swarm_action(
        restored,
        Message::ConversationTitle("Renamed advisory review".into()),
    );
    let rename = workspace.swarm_action(restored, Message::ConfirmConversationSave);
    settle(&mut workspace, rename);
    let renamed = store::load(&root, &initial.id).unwrap();
    assert_eq!(renamed.title(), "Renamed advisory review");
    assert!(renamed.turns == initial.turns);
    assert_eq!(
        workspace.terminal_agents.swarms[&restored]
            .conversation
            .as_ref()
            .unwrap()
            .record
            .title(),
        "Renamed advisory review"
    );
    let _ = workspace.swarm_action(restored, Message::Close);
    store::delete(&root, &renamed).unwrap();
    assert_idle();

    let _ = workspace.open_saved_swarm(owner, root.clone(), record.clone());
    let id = *workspace.terminal_agents.swarms.keys().next().unwrap();
    let _ = workspace.open_saved_swarm(owner, root.clone(), record.clone());
    assert_eq!(workspace.terminal_agents.swarms.len(), 1);
    settle(
        &mut workspace,
        headless::catalog(id, Some(vec![target, missing])),
    );
    let group = &workspace.terminal_agents.swarms[&id];
    assert!(group.goal.is_empty());
    assert!(
        group
            .choices
            .iter()
            .any(|choice| choice.target.key == Key::Saved(missing) && choice.saved.is_none())
    );
    assert!(
        group
            .choices
            .iter()
            .all(|choice| matches!(choice.target.key, Key::Saved(_)))
    );
    assert_idle();

    let act = |workspace: &mut Workspace, message| workspace.swarm_action(id, message);
    let _ = act(
        &mut workspace,
        Message::Goal(format!("Fresh question {SSH_SECRET}")),
    );
    let _ = act(
        &mut workspace,
        Message::Directory(Key::Saved(target), "/srv".into()),
    );
    let _ = act(&mut workspace, Message::ReviewFollowUp);
    assert!(
        workspace.terminal_agents.swarms[&id]
            .notice
            .contains("Remove unavailable targets")
    );
    let _ = act(&mut workspace, Message::Toggle(Key::Saved(missing), false));
    let _ = act(&mut workspace, Message::ReviewFollowUp);
    let review = |workspace: &Workspace| {
        workspace.terminal_agents.swarms[&id]
            .conversation
            .as_ref()
            .unwrap()
            .review
            .as_ref()
            .unwrap()
            .epoch
    };
    let old = review(&workspace);
    assert_idle();
    let _ = act(
        &mut workspace,
        Message::Directory(Key::Saved(target), "/var".into()),
    );
    let ignored = act(&mut workspace, Message::StartFollowUp(old));
    assert_eq!(ignored.units(), 0);
    assert_idle();
    let _ = act(&mut workspace, Message::ReviewFollowUp);
    let current = review(&workspace);
    assert_ne!(old, current);
    let start = act(&mut workspace, Message::StartFollowUp(current));
    settle(&mut workspace, start);
    assert_idle();
    let member = sole_member(&workspace, id);
    let epoch = workspace.terminal_agents.runs[&member].epoch;
    assert_eq!(
        act(
            &mut workspace,
            Message::Member(
                member,
                MemberAction::ReviewModel(request_review::Message::Send(epoch - 1))
            )
        )
        .units(),
        0
    );
    assert_idle();
    let (pending, messages) = send_model_review(&mut workspace, id, member);
    let request = reply_reviewed(&model, &messages, "Final reviewed answer.");
    let input: Value =
        serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["goal"], "Fresh question ***");
    assert_eq!(input["target"]["directory"], "/var");
    assert_eq!(input["steps"], json!([]));
    assert!(messages[0].contains(PINNED));
    assert!(!request.to_string().contains(SSH_SECRET));
    assert!(!request.to_string().contains("prior-private-"));
    settle(&mut workspace, pending);
    let saved = store::load(&root, &record.id).unwrap();
    assert_eq!(saved.turns.len(), 2);
    assert!(saved.turns[0] == record.turns[0]);
    assert_eq!(saved.turns[1].question, "Fresh question ***");
    assert_eq!(saved.turns[1].targets.len(), 1);
    assert_eq!(saved.turns[1].targets[0].target, target);
    assert_eq!(saved.turns[1].targets[0].detail, "Final reviewed answer.");
    assert_idle();

    let preview = act(
        &mut workspace,
        Message::Comparison(comparison::Message::Review),
    );
    settle(&mut workspace, preview);
    assert_idle();
    let swarm = &workspace.terminal_agents.swarms[&id];
    let (request, epoch, _) = swarm.comparison.review().unwrap();
    let messages = request.messages().map(str::to_owned);
    assert!(messages[1].contains("Fresh question ***"));
    assert!(messages[1].contains("Final reviewed answer."));
    assert!(!messages[1].contains(SSH_SECRET));
    assert!(!messages[1].contains("prior-private-"));
    let label = &swarm.queue.as_ref().unwrap().rows()[0].target.label;
    let markdown = format!(
        "| Target | Result | Errors |\n| --- | --- | --- |\n| {label} | Compared saved follow-up | - |"
    );
    let send = act(
        &mut workspace,
        Message::Comparison(comparison::Message::Send(epoch)),
    );
    let wire = reply_content(&model, &messages, &markdown);
    assert!(!wire.to_string().contains("prior-private-"));
    assert!(!wire.to_string().contains(SSH_SECRET));
    settle(&mut workspace, send);
    assert_idle();
    let compared = store::load(&root, &record.id).unwrap();
    assert_eq!(compared.revision, saved.revision);
    assert!(!compared.same_revision(&saved));
    assert_eq!(compared.turns.len(), 2);
    assert!(compared.turns[0] == saved.turns[0]);
    assert!(compared.turns[1].targets == saved.turns[1].targets);
    assert_eq!(compared.turns[1].question, saved.turns[1].question);
    assert_eq!(
        compared.turns[1].comparison.as_deref(),
        Some(markdown.as_str())
    );
    let swarm = &workspace.terminal_agents.swarms[&id];
    assert_eq!(swarm.comparison.result(), Some(markdown.as_str()));
    assert!(
        swarm
            .conversation
            .as_ref()
            .unwrap()
            .record
            .same_revision(&compared)
    );
    let saved = compared;

    let original = connections.get(target).unwrap().clone();
    let mut changed_target = original.clone();
    changed_target.terminal.ai_skills.clear();
    let original_config = Configuration::load_fixture_from(&root).unwrap();
    let mut changed_settings = settings;
    changed_settings["profiles"]["profiles"][0]["model"] = json!("changed-fixture-model");
    let changed_config: Configuration = serde_json::from_value(changed_settings).unwrap();
    for change_target in [true, false] {
        let _ = act(
            &mut workspace,
            Message::Goal("Check pending authority".into()),
        );
        let _ = act(&mut workspace, Message::ReviewFollowUp);
        let epoch = review(&workspace);
        let start = act(&mut workspace, Message::StartFollowUp(epoch));
        settle(&mut workspace, start);
        let member = sole_member(&workspace, id);
        assert_idle();
        if change_target {
            connections.save(changed_target.clone()).unwrap();
        } else {
            changed_config
                .save_fixture_at(&root, &original_config)
                .unwrap();
        }
        let (rejected, _) = send_model_review(&mut workspace, id, member);
        settle(&mut workspace, rejected);
        let queue = workspace.terminal_agents.swarms[&id]
            .queue
            .as_ref()
            .unwrap();
        assert_eq!(queue.rows()[0].status, Status::Blocked);
        assert!(queue.rows()[0].detail.contains("changed"));
        assert_idle();
        assert_eq!(store::load(&root, &record.id).unwrap().turns.len(), 2);
        if change_target {
            connections.save(original.clone()).unwrap();
        }
    }

    store::delete(&root, &saved).unwrap();
    let _ = workspace.revoke_saved_swarm(&root, &saved.id);
    assert!(workspace.terminal_agents.swarms.is_empty());
    let stale = workspace.open_saved_swarm(owner, root, saved);
    assert_eq!(stale.units(), 0);
    assert!(workspace.terminal_agents.swarms.is_empty());
}

fn verify_named_draft(
    workspace: &mut Workspace,
    owner: window::Id,
    root: &Path,
    target: u64,
    model: &std::net::TcpListener,
    assert_idle: &impl Fn(),
) {
    let _ = workspace.open_agent_swarm(owner);
    let id = *workspace.terminal_agents.swarms.keys().next().unwrap();
    settle(workspace, headless::catalog(id, None));
    let baseline = store::list(root).unwrap().chats.len();
    let key = Key::Saved(target);
    let _ = workspace.swarm_action(id, Message::Toggle(key, true));
    let _ = workspace.swarm_action(id, Message::Directory(key, "/srv/draft".into()));
    let _ = workspace.swarm_action(id, Message::Goal("Unsubmitted goal".into()));
    let _ = workspace.swarm_action(id, Message::SaveDraft);
    let _ = workspace.swarm_action(id, Message::ConversationTitle(SSH_SECRET.into()));
    let rejected = workspace.swarm_action(id, Message::ConfirmConversationSave);
    assert_eq!(rejected.units(), 0);
    assert!(
        workspace.terminal_agents.swarms[&id]
            .notice
            .contains("redacted")
    );
    assert_eq!(store::list(root).unwrap().chats.len(), baseline);
    let _ = workspace.swarm_action(id, Message::CancelConversationSave);
    assert_eq!(
        workspace.terminal_agents.swarms[&id].goal,
        "Unsubmitted goal"
    );
    let _ = workspace.swarm_action(id, Message::SaveDraft);
    let _ = workspace.swarm_action(id, Message::ConversationTitle("Saved target setup".into()));
    let task = workspace.swarm_action(id, Message::ConfirmConversationSave);
    settle(workspace, task);
    assert_idle();
    let group = &workspace.terminal_agents.swarms[&id];
    let saved = &group.conversation.as_ref().unwrap().record;
    let draft = store::load(root, &saved.id).unwrap();
    assert!(draft.is_draft() && draft.turns.is_empty());
    assert_eq!(draft.title(), "Saved target setup");
    assert_eq!(draft.target_ids().collect::<Vec<_>>(), [target]);
    assert_eq!(
        draft.target_route(target).unwrap().directory.as_deref(),
        Some("/srv/draft")
    );
    assert!(
        store::list(root)
            .unwrap()
            .chats
            .iter()
            .any(|entry| entry.id == draft.id && entry.is_draft)
    );
    let _ = workspace.swarm_action(id, Message::Close);

    let _ = workspace.open_saved_swarm(owner, root.to_owned(), draft.clone());
    let id = *workspace.terminal_agents.swarms.keys().next().unwrap();
    settle(workspace, headless::catalog(id, Some(vec![target])));
    let group = &workspace.terminal_agents.swarms[&id];
    assert!(group.goal.is_empty());
    assert_eq!(group.selected, HashSet::from([key]));
    assert_eq!(group.choices[0].directory.as_deref(), Some("/srv/draft"));
    let _ = workspace.swarm_action(id, Message::Directory(key, "/var/draft".into()));
    let _ = workspace.swarm_action(id, Message::SaveDraft);
    let _ = workspace.swarm_action(
        id,
        Message::ConversationTitle("Updated target setup".into()),
    );
    let task = workspace.swarm_action(id, Message::ConfirmConversationSave);
    settle(workspace, task);
    let edited = store::load(root, &draft.id).unwrap();
    assert!(edited.is_draft());
    assert_eq!(edited.revision, draft.revision + 1);
    assert_eq!(
        edited.target_route(target).unwrap().directory.as_deref(),
        Some("/var/draft")
    );
    assert_idle();

    let _ = workspace.swarm_action(id, Message::Goal("First reviewed task".into()));
    let _ = workspace.swarm_action(id, Message::ReviewFollowUp);
    let review = workspace.terminal_agents.swarms[&id]
        .conversation
        .as_ref()
        .unwrap()
        .review
        .as_ref()
        .unwrap();
    let task = workspace.swarm_action(id, Message::StartFollowUp(review.epoch));
    settle(workspace, task);
    assert_idle();
    let member = sole_member(workspace, id);
    let (pending, messages) = send_model_review(workspace, id, member);
    let wire = reply_reviewed(model, &messages, "First reviewed answer.");
    assert!(wire.to_string().contains("/var/draft"));
    assert!(!wire.to_string().contains("Unsubmitted goal"));
    settle(workspace, pending);
    let completed = store::load(root, &draft.id).unwrap();
    assert!(!completed.is_draft());
    assert_eq!(completed.title(), "Updated target setup");
    assert_eq!(completed.turns[0].question, "First reviewed task");
    assert_eq!(
        completed.turns[0].targets[0].detail,
        "First reviewed answer."
    );
    let _ = workspace.swarm_action(id, Message::Close);
    store::delete(root, &completed).unwrap();
    assert_idle();
}

fn sole_member(workspace: &Workspace, group: window::Id) -> window::Id {
    let members = &workspace.terminal_agents.swarms[&group].members;
    assert_eq!(members.len(), 1);
    *members.keys().next().unwrap()
}

fn send_model_review(
    workspace: &mut Workspace,
    group: window::Id,
    member: window::Id,
) -> (Task<Desktop>, [String; 2]) {
    let run = &workspace.terminal_agents.runs[&member];
    assert!(run.phase == Phase::ModelReview);
    let review = run.model_review.as_ref().unwrap();
    let messages = [review.system.clone(), review.input.clone()];
    let epoch = run.epoch;
    let pending = workspace.swarm_action(
        group,
        Message::Member(
            member,
            MemberAction::ReviewModel(request_review::Message::Send(epoch)),
        ),
    );
    (pending, messages)
}

fn reply_reviewed(model: &std::net::TcpListener, messages: &[String; 2], answer: &str) -> Value {
    let response = json!({"status":"done","summary":"Done","userMessage":answer});
    reply_content(model, messages, &response.to_string())
}

fn reply_content(model: &std::net::TcpListener, messages: &[String; 2], content: &str) -> Value {
    let mut client = test_support::accept(model);
    let request = test_support::request(&mut client);
    assert_eq!(request["max_tokens"], 2048);
    assert_eq!(request["messages"][0]["content"], messages[0]);
    assert_eq!(request["messages"][1]["content"], messages[1]);
    client
        .get_mut()
        .write_all(test_support::fixture_response("200 OK", "", content).as_bytes())
        .unwrap();
    request
}

fn settle(workspace: &mut Workspace, task: Task<Desktop>) {
    let mut pending = vec![task];
    while let Some(task) = pending.pop() {
        if let Some(mut stream) = iced_runtime::task::into_stream(task) {
            while let Some(action) = block_on(stream.next()) {
                if let iced_runtime::Action::Output(Desktop::TerminalAgent(id, message)) = action {
                    pending.push(workspace.update_terminal_agent(id, message));
                }
            }
        }
    }
}
