use std::fs;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

use kortty_jobs::{DependencyMode, Job};

use super::*;

pub(super) fn grouped_connection(host: &str) -> crate::connection_store::Connection {
    use crate::connection_store::{Auth, Connection, SavedLogin};
    use crate::ssh_dialog::LoginTarget;
    Connection {
        id: 0,
        name: host.into(),
        tag: String::new(),
        login: SavedLogin {
            preferences: Default::default(),
            endpoint: LoginTarget {
                host: host.into(),
                port: 22,
                username: "operator".into(),
            },
            auth: Auth::StoredKey(1),
            keyboard_interactive: false,
        },
        jump: None,
        emulation: "xterm-256color".into(),
        tunnels: Vec::new(),
        incognito: false,
        isolation: Default::default(),
        inherit_isolation: false,
        startup: Default::default(),
        logging: Default::default(),
        terminal_effect: None,
        command_timestamps: None,
        tab_color: None,
        paste_warning: None,
        window_geometry: None,
        terminal: Default::default(),
    }
}

#[test]
fn imported_privileged_adoption_requires_a_sudo_mode_current_pins_and_native_file_review() {
    use crate::clipboard_policy::ClipboardPolicy;
    use crate::job_executor::virtual_terminal::Options;
    use crate::sftp_window::privileged_file::ProtectedOperation;
    use kortty_ssh::{HostKey, TrustStore};
    use serde_json::json;

    let profile = Profile::temporary();
    let path = profile.file();
    let mut connections = ConnectionStore::open(profile.0.join("connections.json")).unwrap();
    let saved_id = connections
        .save(grouped_connection("protected.example"))
        .unwrap();
    let saved = connections.get(saved_id).unwrap().clone();
    let route = ConnectionRef::from(&saved);
    let terminal = Options {
        columns: 132,
        ..Options::default()
    };
    let id = JobId::new("java-00000000000040009000000000000001").unwrap();
    let from_settings = |settings| {
        serde_json::from_value::<SshCommand>(json!({
            "connection": route,
            "imported_privileged": {"routes": [route], "settings": settings},
        }))
        .unwrap()
    };
    let command = from_settings(json!({
        "kind": "command", "text": "id -u",
        "options": {
            "working_directory": "/srv/project files",
            "virtual_terminal": terminal,
        }
    }));
    let imported_job = |action| {
        Job::new(id.clone(), "Imported sudo job", action)
            .unwrap()
            .with_enabled(false)
    };
    let job = imported_job(command.clone());
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(
            &JobGraph::new(vec![imported_job(command.clone())]).unwrap(),
            &[],
            &[],
        )
        .unwrap();
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    app.update(Message::Page(Page::Action));
    app.update(Message::Enabled(true));
    app.update(Message::Sudo(SudoMode::Passwordless));
    app.update(Message::CommandSource(Source::Command));
    app.update(Message::ConnectionRemoved(saved_id));
    assert!(app.update(Message::Run(false)).is_empty());
    app.update(Message::Save);
    let saved_draft = app.selected_job().unwrap();
    assert!(!saved_draft.enabled());
    assert_eq!(saved_draft.action(), &command);
    assert_eq!(
        saved_draft.action().command_for_exec(None),
        Err(imported_privileged::REVIEW_REQUIRED)
    );

    let key = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
    )
    .unwrap();
    let trust = TrustStore::from_pins(vec![(Endpoint::new("protected.example", 22).unwrap(), key)])
        .unwrap();
    let untrusted = TrustStore::from_pins(Vec::new()).unwrap();
    let policy = ClipboardPolicy {
        file_transfer_allowed: false,
        ..ClipboardPolicy::default()
    };
    let denied = ClipboardPolicy {
        scheduler_sudo_allowed: false,
        ..ClipboardPolicy::default()
    };
    let snippet_path = app.runtime.snippet_path.clone();
    let mut draft = Draft::from_job(&job);
    assert!(
        draft
            .adopt_imported_privileged(
                &connections,
                &SharedTargets::default(),
                &trust,
                &policy,
                &snippet_path
            )
            .unwrap_err()
            .contains("Choose passwordless")
    );
    draft.select_imported_sudo(SudoMode::Passwordless);
    assert!(
        draft
            .adopt_imported_privileged(
                &connections,
                &SharedTargets::default(),
                &untrusted,
                &policy,
                &snippet_path
            )
            .unwrap_err()
            .contains("host key")
    );
    assert!(
        draft
            .adopt_imported_privileged(
                &connections,
                &SharedTargets::default(),
                &trust,
                &denied,
                &snippet_path
            )
            .unwrap_err()
            .contains("policy")
    );
    let mut changed = saved.clone();
    changed.login.endpoint.port = 2222;
    connections.save(changed).unwrap();
    assert!(
        draft
            .adopt_imported_privileged(
                &connections,
                &SharedTargets::default(),
                &trust,
                &policy,
                &snippet_path
            )
            .unwrap_err()
            .contains("target changed")
    );
    connections.save(saved).unwrap();

    for (mode, expected_terminal) in [
        (SudoMode::Passwordless, Some(terminal)),
        (SudoMode::Vault, None),
    ] {
        let mut draft = Draft::from_job(&job);
        draft.select_imported_sudo(mode);
        assert!(
            !draft
                .adopt_imported_privileged(
                    &connections,
                    &SharedTargets::default(),
                    &trust,
                    &policy,
                    &snippet_path
                )
                .unwrap()
        );
        let adopted = draft.build(id.clone()).unwrap();
        assert!(!adopted.enabled());
        assert!(adopted.action().imported_privileged.is_none());
        assert_eq!(adopted.action().sudo, mode);
        assert_eq!(adopted.action().virtual_terminal, expected_terminal);
        assert_eq!(
            adopted.action().command_for_exec(None).unwrap(),
            "CDPATH= cd '/srv/project files' || exit\nid -u"
        );
    }

    let deletion = imported_job(from_settings(json!({
        "kind": "file", "action": {"operation": "delete", "path": "/srv/retired"},
    })));
    let mut draft = Draft::from_job(&deletion);
    draft.select_imported_sudo(SudoMode::Passwordless);
    assert!(
        draft
            .adopt_imported_privileged(
                &connections,
                &SharedTargets::default(),
                &trust,
                &policy,
                &snippet_path
            )
            .unwrap()
    );
    assert!(draft.build(id.clone()).is_err());
    draft.set(Field::RemoteDeleteConfirmation, "/srv/retired".into());
    let adopted = draft.build(id).unwrap();
    assert!(!adopted.enabled());
    let Ok(RemoteAction::PrivilegedFileAction(file)) = adopted.action().remote_action() else {
        panic!("expected native file action");
    };
    assert!(file.approval.is_none());
    assert_eq!(file.path.as_str(), "/srv/retired");
    assert!(matches!(
        file.operation,
        ProtectedOperation::Delete { recursive: true }
    ));
    app.view.draft = draft;
    app.view.dirty = true;
    app.update(Message::Save);
    assert!(app.status().unwrap().contains("Review the exact"));
    assert_eq!(app.selected_job().unwrap().action(), &command);
}

#[test]
fn imported_archive_adoption_requires_its_route_pins_local_path_and_vault_choice() {
    use kortty_server_archive::protocol::ArchiveMemberLayout;
    use kortty_ssh::{HostKey, TrustStore};
    use serde_json::json;

    let profile = Profile::temporary();
    let path = profile.file();
    let mut connections = ConnectionStore::open(profile.0.join("connections.json")).unwrap();
    let saved_id = connections
        .save(grouped_connection("archive.example"))
        .unwrap();
    let saved = connections.get(saved_id).unwrap().clone();
    let route = ConnectionRef::from(&saved);
    let action = serde_json::from_value::<SshCommand>(json!({
        "connection": route,
        "remote": {"kind": "imported_archive", "route": route, "settings": {
            "sources": ["/input/東京", "/input/hosts", "/etc/hosts"], "target": "/output/archive.zip",
            "format": "zip_password", "level": 8, "java_excludes": ["original*"],
            "local_hint": r"C:\Java path\archive.zip",
        }}
    }))
    .unwrap();
    let job_id = JobId::new("java-00000000000040009000000000000006").unwrap();
    let job = Job::new(job_id.clone(), "Imported archive", action.clone())
        .unwrap()
        .with_enabled(false);
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&JobGraph::new(vec![job]).unwrap(), &[], &[])
        .unwrap();
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(job_id.clone()));
    app.update(Message::Page(Page::Action));
    app.update(Message::Enabled(true));
    app.update(Message::ArchiveServerOnly(false));
    app.update(Message::ArchiveFormat(ArchiveFormat::Zip));
    app.update(Message::ArchiveServerPath(false));
    app.update(Message::ArchiveReuseOutput(true));
    app.update(Message::CommandSource(Source::Command));
    assert!(app.update(Message::Run(false)).is_empty());
    app.update(Message::Save);
    assert_eq!(app.selected_job().unwrap().action(), &action);

    let policy = crate::clipboard_policy::ClipboardPolicy::default();
    let untrusted = TrustStore::from_pins(Vec::new()).unwrap();
    let key = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
    )
    .unwrap();
    let trust =
        TrustStore::from_pins(vec![(Endpoint::new("archive.example", 22).unwrap(), key)]).unwrap();
    assert!(
        app.view
            .draft
            .adopt_imported_archive(
                &connections,
                &SharedTargets::default(),
                &untrusted,
                &policy,
                &[]
            )
            .unwrap_err()
            .contains("host keys")
    );
    let mut changed = saved.clone();
    changed.login.endpoint.port = 2222;
    connections.save(changed).unwrap();
    assert!(
        app.view
            .draft
            .adopt_imported_archive(
                &connections,
                &SharedTargets::default(),
                &trust,
                &policy,
                &[]
            )
            .unwrap_err()
            .contains("no longer matches")
    );
    connections.save(saved).unwrap();
    assert!(
        app.view
            .draft
            .adopt_imported_archive(
                &connections,
                &SharedTargets::default(),
                &trust,
                &policy,
                &[]
            )
            .unwrap_err()
            .contains("local archive destination")
    );
    let local = profile.0.join("reviewed copy.zip");
    app.update(Message::Edit(
        Field::ArchiveDownload,
        local.to_str().unwrap().into(),
    ));
    assert!(
        app.view
            .draft
            .adopt_imported_archive(
                &connections,
                &SharedTargets::default(),
                &trust,
                &policy,
                &[]
            )
            .unwrap_err()
            .contains("from the vault")
    );

    let denied = crate::clipboard_policy::ClipboardPolicy {
        file_transfer_allowed: false,
        ..crate::clipboard_policy::ClipboardPolicy::default()
    };
    let choice = CredentialChoice {
        id: "ba".repeat(16),
        label: "Archive password".into(),
        username: String::new(),
    };
    app.runtime.set_archive_passwords(vec![choice.clone()]);
    app.update(Message::ArchivePassword(choice.clone()));
    assert!(
        app.view
            .draft
            .adopt_imported_archive(
                &connections,
                &SharedTargets::default(),
                &trust,
                &denied,
                std::slice::from_ref(&choice)
            )
            .unwrap_err()
            .contains("policy")
    );
    app.update(Message::EditArchiveExcludes(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new(".tmp/".into())),
    )));
    app.view
        .draft
        .adopt_imported_archive(
            &connections,
            &SharedTargets::default(),
            &trust,
            &policy,
            std::slice::from_ref(&choice),
        )
        .unwrap();
    app.update(Message::Save);
    let job = app.selected_job().unwrap();
    assert!(!job.enabled());
    assert!(job.action().require_executable().is_ok());
    assert_eq!(job.action().connection.as_ref(), Some(&route));
    let RemoteAction::ServerArchive(archive) = job.action().remote_action().unwrap() else {
        panic!("expected adopted archive")
    };
    assert_eq!(
        archive
            .sources
            .iter()
            .map(RemotePath::as_str)
            .collect::<Vec<_>>(),
        ["/input/東京", "/input/hosts", "/etc/hosts"]
    );
    assert_eq!(archive.target.as_str(), "/output/archive.zip");
    assert_eq!(archive.format, ArchiveFormat::ZipPassword);
    assert_eq!(archive.member_layout, ArchiveMemberLayout::ServerPath);
    assert_eq!(
        archive.server_spec().unwrap().member_layout,
        ArchiveMemberLayout::ServerPath
    );
    assert!(!archive.reuse_output);
    assert_eq!(archive.level, 8);
    assert_eq!(archive.download.as_ref(), Some(&local));
    assert_eq!(
        archive.password_credential.as_deref(),
        Some(choice.id.as_str())
    );
    assert!(archive.excludes.join("\n").contains(".tmp/"));

    app.update(Message::ArchiveReuseOutput(true));
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    drop(app);
    let mut reopened = TestWindow::open(path).unwrap();
    reopened.update(Message::Select(job_id));
    let job = reopened.selected_job().unwrap();
    assert!(!job.enabled());
    let remote = serde_json::to_value(job.action()).unwrap()["remote"].clone();
    assert_eq!(remote["member_layout"], "server_path");
    assert_eq!(remote["reuse_output"], true);
    assert_eq!(
        reopened
            .view
            .draft
            .build(job.id().clone())
            .unwrap()
            .action(),
        job.action()
    );
}

#[test]
fn imported_privileged_archive_needs_native_approval_for_its_exact_local_copy() {
    use kortty_server_archive::protocol::ArchiveMemberLayout;
    use kortty_ssh::{HostKey, TrustStore};
    use serde_json::json;

    let profile = Profile::temporary();
    let path = profile.file();
    let mut connections = ConnectionStore::open(profile.0.join("connections.json")).unwrap();
    let saved_id = connections
        .save(grouped_connection("protected.example"))
        .unwrap();
    let route = ConnectionRef::from(connections.get(saved_id).unwrap());
    let action: SshCommand = serde_json::from_value(json!({
        "connection": route,
        "remote": {"kind": "imported_archive", "route": route, "settings": {
            "sources": ["/protected/records", "/retained/records"], "target": "/backups/archive.tar",
            "format": "tar", "level": 6, "java_excludes": [], "java_sudo": true,
            "local_hint": "/java-only/archive.tar"
        }}
    }))
    .unwrap();
    let id = JobId::new("java-00000000000040009000000000000007").unwrap();
    let job = Job::new(id.clone(), "Imported protected archive", action)
        .unwrap()
        .with_enabled(false);
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&JobGraph::new(vec![job]).unwrap(), &[], &[])
        .unwrap();

    let key = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
    )
    .unwrap();
    let pins = vec![(Endpoint::new("protected.example", 22).unwrap(), key)];
    let trust = TrustStore::from_pins(pins.clone()).unwrap();
    let policy = crate::clipboard_policy::ClipboardPolicy::default();
    let denied = crate::clipboard_policy::ClipboardPolicy {
        scheduler_sudo_allowed: false,
        ..Default::default()
    };
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    app.update(Message::Page(Page::Action));
    app.update(Message::Sudo(SudoMode::Vault));
    assert_eq!(app.view.draft.sudo, SudoMode::Disabled);
    assert!(app.update(Message::Run(false)).is_empty());
    assert!(
        app.view
            .draft
            .adopt_imported_archive(
                &connections,
                &SharedTargets::default(),
                &trust,
                &denied,
                &[]
            )
            .unwrap_err()
            .contains("forbids Scheduler sudo")
    );
    assert!(
        app.view
            .draft
            .adopt_imported_archive(
                &connections,
                &SharedTargets::default(),
                &trust,
                &policy,
                &[]
            )
            .unwrap_err()
            .contains("local archive destination")
    );
    let local = profile.0.join("reviewed archive.tar");
    app.update(Message::Edit(
        Field::ArchiveDownload,
        local.to_str().unwrap().into(),
    ));
    app.view
        .draft
        .adopt_imported_archive(
            &connections,
            &SharedTargets::default(),
            &trust,
            &policy,
            &[],
        )
        .unwrap();
    assert_eq!(app.view.draft.sudo, SudoMode::Passwordless);
    app.update(Message::Save);
    assert!(app.status().is_some_and(|s| s.contains("Review the exact")));
    assert!(matches!(
        app.selected_job().unwrap().action().remote,
        Some(RemoteAction::ImportedArchive(_))
    ));
    app.runtime
        .review_privileged(&mut app.view, |reference| {
            assert_eq!(reference, &route);
            Ok(sync::fingerprints(&pins))
        })
        .unwrap();
    app.update(Message::ApprovePrivileged);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    assert!(!app.selected_job().unwrap().enabled());
    drop(app);

    let mut reopened = TestWindow::open(path).unwrap();
    reopened.runtime.set_execution_allowed(true);
    reopened.update(Message::Select(id));
    let request = reopened.update(Message::Run(false)).pop().unwrap();
    let context = reopened.runtime.privileged_run_context(&request).unwrap();
    let RemoteAction::PrivilegedArchive(archive) = request.action.remote_action().unwrap() else {
        panic!("expected an approved protected archive");
    };
    assert_eq!(archive.archive.download.as_ref(), Some(&local));
    assert_eq!(
        archive.archive.member_layout,
        ArchiveMemberLayout::ServerPath
    );
    assert!(!archive.archive.reuse_output);
    assert_eq!(request.action.sudo, SudoMode::Passwordless);
    assert!(
        context
            .authorize_archive(archive, &route, SudoMode::Passwordless, &pins)
            .is_ok()
    );
    let mut changed = archive.clone();
    changed.archive.download = Some(profile.0.join("changed archive.tar"));
    assert!(
        context
            .authorize_archive(&changed, &route, SudoMode::Passwordless, &pins)
            .is_err()
    );
}

#[test]
fn scheduler_target_picker_filters_and_applies_a_reviewed_batch_atomically() {
    use target_picker::{Choice, Event};

    let scratch = Profile::temporary();
    let path = scratch.file();
    let mut store = ConnectionStore::open(scratch.0.join("connections.json")).unwrap();
    let parent = store.create_group(None, "Fleet").unwrap();
    let child = store.create_group(Some(&parent), "Testing").unwrap();
    let primary = store
        .save_in_group(grouped_connection("primary.example.test"), Some(&parent))
        .unwrap();
    let mut node = grouped_connection("runtime.example.test");
    node.name = "Night build node".into();
    let selected = store.save_in_group(node, Some(&child)).unwrap();
    let descendant = store
        .save_in_group(grouped_connection("descendant.example.test"), Some(&child))
        .unwrap();
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_connections(Some(&store));
    let find = |app: &TestWindow, id: u64| {
        app.runtime
            .connection_choices
            .iter()
            .find(|choice| {
                choice
                    .reference()
                    .is_some_and(|reference| reference.id == id)
            })
            .unwrap()
            .clone()
    };
    app.update(Message::Edit(Field::Name, "Reviewed target batch".into()));
    app.update(Message::EditCommand(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("true".into())),
    )));
    let primary_choice = find(&app, primary);
    app.update(Message::ConnectionSelected(primary_choice));
    app.update(Message::Save);
    let original = app.selected_job().unwrap().action().clone();
    assert!(!app.view.dirty);

    app.update(Message::TargetPicker(Event::Open));
    let matching = |app: &TestWindow| {
        app.view
            .draft
            .target_picker
            .as_ref()
            .unwrap()
            .matching(
                &app.runtime.connection_choices,
                &app.runtime.connection_group_choices,
                &app.runtime.connection_group_paths,
            )
            .map(|(choice, _)| choice)
            .collect::<Vec<_>>()
    };
    let node = Choice::Connection(find(&app, selected));
    app.update(Message::TargetPicker(Event::Search(
        "  NIGHT BUILD  ".into(),
    )));
    assert!(matching(&app) == [node.clone()]);
    app.update(Message::TargetPicker(Event::Search(
        "RUNTIME.EXAMPLE".into(),
    )));
    assert!(matching(&app) == [node.clone()]);
    app.update(Message::TargetPicker(Event::Select(node, true)));
    app.update(Message::TargetPicker(Event::Search("Testing".into())));
    let child_choice = Choice::Group(child.clone().into());
    let child_matches = matching(&app);
    assert_eq!(child_matches.len(), 3);
    assert!(child_matches.contains(&child_choice));
    assert!(child_matches.contains(&Choice::Connection(find(&app, descendant))));
    let parent_choice = Choice::Group(parent.clone().into());
    for group in [&parent_choice, &child_choice] {
        app.update(Message::TargetPicker(Event::Select(group.clone(), true)));
    }

    let mut updated = store.get(selected).unwrap().clone();
    updated.login.endpoint.host = "replacement.example.test".into();
    store.save(updated).unwrap();
    app.update(Message::TargetPicker(Event::Apply));
    assert!(
        app.status()
            .unwrap()
            .contains("selected SSH connection changed")
    );
    assert!(app.view.draft.target_picker.is_some());
    assert!(!app.view.dirty);
    assert!(app.selected_job().unwrap().action() == &original);

    app.update(Message::TargetPicker(Event::Clear));
    let replacement = Choice::Connection(find(&app, selected));
    for choice in [replacement, parent_choice, child_choice] {
        app.update(Message::TargetPicker(Event::Select(choice, true)));
    }
    app.update(Message::TargetPicker(Event::Apply));
    assert!(app.view.draft.target_picker.is_none());
    assert!(app.view.dirty);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    drop(app);

    let reopened = TestWindow::open(path).unwrap();
    let action = reopened.selected_job().unwrap().action();
    assert_eq!(
        action.targets().map(|target| target.id).collect::<Vec<_>>(),
        [selected, primary, descendant]
    );
    let groups = action.target_groups.as_ref().unwrap();
    assert_eq!(
        groups.groups,
        [ConnectionGroup::from(parent), ConnectionGroup::from(child)]
    );
    // Clearing the picker removes the old explicit target. It is still
    // reached through the selected parent group, after the new explicit one.
    assert_eq!(groups.explicit.len(), 1);
    assert_eq!(groups.explicit[0].target.host, "replacement.example.test");
}

#[test]
fn scheduler_target_groups_persist_source_and_block_changed_membership_until_refresh() {
    let scratch = Profile::temporary();
    let path = scratch.file();
    let mut store = ConnectionStore::open(scratch.0.join("connections.json")).unwrap();
    let parent = store.create_group(None, "Fleet").unwrap();
    let child = store.create_group(Some(&parent), "Testing").unwrap();
    let prefix_sibling = store.create_group(None, "Fleet-old").unwrap();
    let direct = store
        .save_in_group(grouped_connection("a.example.test"), Some(&parent))
        .unwrap();
    let nested = store
        .save_in_group(grouped_connection("b.example.test"), Some(&child))
        .unwrap();
    let _unrelated = store
        .save_in_group(grouped_connection("c.example.test"), Some(&prefix_sibling))
        .unwrap();
    let choice = ConnectionChoice::Saved {
        reference: store.get(nested).unwrap().into(),
        name: "Explicit first".into(),
    };
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_connections(Some(&store));
    app.update(Message::Edit(Field::Name, "Group job".into()));
    app.update(Message::EditCommand(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("true".into())),
    )));
    app.update(Message::ConnectionSelected(choice));
    app.update(Message::ConnectionGroupAdded(parent.clone().into()));
    app.update(Message::ConnectionGroupAdded(child.clone().into()));
    app.update(Message::Save);
    let job = app.selected_job().expect("saved group job");
    let id = job.id().clone();
    let action = job.action().clone();
    assert_eq!(
        action.targets().map(|target| target.id).collect::<Vec<_>>(),
        [nested, direct]
    );
    let source = action.target_groups.as_ref().unwrap();
    assert_eq!(
        source.groups,
        [
            ConnectionGroup::from(parent.clone()),
            ConnectionGroup::from(child.clone())
        ]
    );
    assert_eq!(source.explicit[0].id, nested);
    assert!(action.require_current_target_groups(&store).is_ok());
    drop(app);

    let added = store
        .save_in_group(grouped_connection("d.example.test"), Some(&child))
        .unwrap();
    assert!(action.require_current_target_groups(&store).is_err());
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_connections(Some(&store));
    assert_eq!(app.selected_job().unwrap().action(), &action);
    app.update(Message::Edit(Field::Name, "Changed job".into()));
    app.update(Message::Save);
    assert!(app.status().unwrap().contains("group targets changed"));
    app.update(Message::RefreshConnectionGroups);
    app.update(Message::Save);
    let job = app.selected_job().unwrap();
    assert_eq!(job.id(), &id);
    assert_eq!(
        job.action()
            .targets()
            .map(|target| target.id)
            .collect::<Vec<_>>(),
        [nested, direct, added]
    );
    assert_eq!(
        job.action().target_groups.as_ref().unwrap().groups,
        [ConnectionGroup::from(parent), ConnectionGroup::from(child)]
    );
}

#[test]
fn scheduler_teamwork_jump_requires_unique_local_login_and_both_host_keys() {
    use crate::clipboard_policy::{ClipboardPolicy, ServerAccess};
    use crate::connection_store::Auth;
    use crate::connection_store::teamwork::{
        Catalog, DefaultAuth, ManagedSource, Source, SourceKind,
    };
    use crate::job_executor::Credentials;
    use crate::vault_window::Kind;
    use kortty_ssh::{Endpoint, HostKey, TrustStore};

    let scratch = Profile::temporary();
    let mut store = ConnectionStore::open(scratch.0.join("connections.json")).unwrap();
    let source = scratch.0.join("fleet.xml");
    fs::write(
        &source,
        r#"<connections schemaVersion="2"><connection id="jump-route">
            <host>destination.example</host><username>operator</username>
            <jumpServer><enabled>true</enabled><host>bastion.example</host>
                <port>2222</port><username>relay</username><authMethod>PASSWORD</authMethod>
                <credentialId>source-credential</credentialId></jumpServer>
        </connection></connections>"#,
    )
    .unwrap();
    let policy = ClipboardPolicy {
        custom_teamwork_sources_allowed: false,
        teamwork_sources: vec![ManagedSource {
            name: "Fleet".into(),
            source: Source {
                location: source.to_str().unwrap().into(),
                enabled: true,
                kind: SourceKind::File,
                check_interval_minutes: 15,
            },
        }],
        ..Default::default()
    };
    let catalog = Catalog::load_for_scheduler(&scratch.0, &policy).unwrap();
    let target = Endpoint::new("destination.example", 22).unwrap();
    let bastion = Endpoint::new("bastion.example", 2222).unwrap();
    let target_key = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKuSNsxvt1EJ0ELnEMXiOgkaExGvVly+uWvDAKyIGKcX",
    )
    .unwrap();
    let jump_key = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDFjq5gouQTbXMW4LzV1KkPtjZgGxFcenR2uthQaggSo",
    )
    .unwrap();
    let trust = TrustStore::from_pins(vec![
        (target.clone(), target_key.clone()),
        (bastion.clone(), jump_key),
    ])
    .unwrap();
    let choice = |store: &ConnectionStore| {
        SharedTargets::new(&catalog, store, &trust, |entry| {
            Some((
                entry.connection.login.endpoint.clone(),
                SharedAuth::Password {
                    credential: "target-local".into(),
                },
            ))
        })
        .choices()
        .next()
        .unwrap()
        .reference()
        .unwrap()
        .clone()
    };
    assert!(choice(&store).validate().is_err());

    let mut profile = grouped_connection("bastion.example");
    profile.login.endpoint.port = 2222;
    profile.login.endpoint.username = "relay".into();
    profile.login.auth = Auth::VaultPassword("jump-local".into());
    let id = store.save(profile).unwrap();
    let reviewed = choice(&store);
    reviewed.validate().unwrap();
    let saved: ConnectionRef =
        serde_json::from_slice(&serde_json::to_vec(&reviewed).unwrap()).unwrap();
    assert_eq!(saved, reviewed);
    let entry = catalog.get(saved.shared_identity().unwrap()).unwrap();
    let default_auth = DefaultAuth::Password {
        credential: "target-local".into(),
    };
    let check = |store: &ConnectionStore, trust: &TrustStore| {
        SharedTargets::recheck(&catalog, store, &default_auth, trust, [saved.clone()])
            .require(&saved)
    };
    assert!(check(&store, &trust).is_ok());
    assert!(
        saved
            .strict_trust(trust.clone())
            .unwrap()
            .selected_pins(&[target.clone(), bastion.clone()])
            .is_ok()
    );
    let access = ServerAccess::default();
    let mut requests = Vec::new();
    let credentials =
        Credentials::resolve_shared(&saved, entry, &store, false, &access, |id, kind, login| {
            assert_eq!(kind, Kind::Password);
            requests.push((id.to_owned(), login.host.clone(), login.username.clone()));
            Ok(kortty_vault::Secret::new("fixture-secret".into()))
        })
        .unwrap();
    assert_eq!(
        requests,
        [
            (
                "target-local".into(),
                "destination.example".into(),
                "operator".into()
            ),
            (
                "jump-local".into(),
                "bastion.example".into(),
                "relay".into()
            ),
        ]
    );
    let (destination, hops) = credentials.into_swarm_route().unwrap();
    assert_eq!(destination.endpoint, target);
    assert_eq!(hops.len(), 1);
    assert_eq!(hops[0].endpoint, bastion);
    let missing = TrustStore::from_pins(vec![(target.clone(), target_key.clone())]).unwrap();
    let rotated =
        TrustStore::from_pins(vec![(target, target_key.clone()), (bastion, target_key)]).unwrap();
    for changed in [missing, rotated] {
        assert!(check(&store, &changed).is_err());
        assert!(saved.strict_trust(changed).is_err());
    }

    let mut updated = store.get(id).unwrap().clone();
    updated.login.auth = Auth::VaultPassword("changed-jump-credential".into());
    store.save(updated).unwrap();
    assert!(check(&store, &trust).is_err());
    assert!(
        Credentials::resolve_shared(&saved, entry, &store, false, &access, |_, _, _| {
            panic!("a changed route must not retrieve credentials")
        })
        .is_err()
    );
    let mut competing = store.get(id).unwrap().clone();
    competing.name = "Alternate bastion login".into();
    competing.id = 0;
    store.save(competing).unwrap();
    assert!(choice(&store).validate().is_err());
}

#[test]
fn scheduler_teamwork_targets_round_trip_and_require_current_membership_and_pins() {
    use crate::clipboard_policy::ClipboardPolicy;
    use crate::connection_store::teamwork::{
        Catalog, DefaultAuth, HiddenEntry, ManagedSource, Source, SourceKind,
    };
    use kortty_ssh::{Endpoint, HostKey, TrustStore};

    let scratch = Profile::temporary();
    let mut store = ConnectionStore::open(scratch.0.join("connections.json")).unwrap();
    let local_group = store.create_group(None, "Fleet").unwrap();
    let local = store
        .save_in_group(grouped_connection("local.example.test"), Some(&local_group))
        .unwrap();
    let first = scratch.0.join("first.xml");
    let second = scratch.0.join("second.xml");
    let row = |id: &str, host: &str, group: &str| {
        format!(
            r#"<connection id="{id}"><host>{host}</host><port>22</port><username>shared</username><group>{group}</group></connection>"#
        )
    };
    let initial = row("1", "one.example.test", "Fleet/Nested");
    let write = |path: &Path, rows: &str| {
        fs::write(
            path,
            format!(r#"<connections schemaVersion="2">{rows}</connections>"#),
        )
        .unwrap();
    };
    write(&first, &initial);
    write(&second, &row("1", "two.example.test", "Fleet"));
    let policy = ClipboardPolicy {
        custom_teamwork_sources_allowed: false,
        teamwork_sources: [&first, &second]
            .into_iter()
            .map(|path| ManagedSource {
                name: path.file_stem().unwrap().to_string_lossy().into_owned(),
                source: Source {
                    location: path.to_str().unwrap().into(),
                    enabled: true,
                    kind: SourceKind::File,
                    check_interval_minutes: 15,
                },
            })
            .collect(),
        ..ClipboardPolicy::default()
    };
    let read = || Catalog::load_for_scheduler(&scratch.0, &policy).unwrap();
    let auth = DefaultAuth::Key {
        id: 7,
        username: "operator".into(),
    };
    let key = |seed: u8| {
        let public = if seed == 9 {
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDFjq5gouQTbXMW4LzV1KkPtjZgGxFcenR2uthQaggSo"
        } else {
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKuSNsxvt1EJ0ELnEMXiOgkaExGvVly+uWvDAKyIGKcX"
        };
        HostKey::from_openssh(public).unwrap()
    };
    let trust = |one_seed| {
        TrustStore::from_pins(
            [
                ("one.example.test", one_seed),
                ("two.example.test", 2),
                ("new.example.test", 3),
            ]
            .into_iter()
            .map(|(host, seed)| (Endpoint::new(host, 22).unwrap(), key(seed)))
            .collect(),
        )
        .unwrap()
    };
    let trusted = trust(1);
    let chooser = |entry: &crate::connection_store::teamwork::Entry| {
        let mut target = entry.connection.login.endpoint.clone();
        target.username = "operator".into();
        Some((
            target,
            SharedAuth::Key {
                id: 7,
                password_preference: None,
            },
        ))
    };
    let catalogue = read();
    let shared = SharedTargets::new(&catalogue, &store, &trusted, chooser);
    let pick = |host: &str| {
        shared
            .choices()
            .find(|choice| choice.reference().unwrap().target.host == host)
            .unwrap()
    };
    let one = pick("one.example.test");
    let two = pick("two.example.test");
    let one_ref = one.reference().unwrap().clone();
    let two_ref = two.reference().unwrap().clone();
    assert_ne!(one_ref.id, two_ref.id);
    assert_ne!(one_ref.id, local);
    assert_eq!(one_ref.target.username, "operator");
    let parent = shared
        .groups()
        .find(|group| group.to_string() == format!("Teamwork / Fleet [{}]", first.display()))
        .unwrap();
    let second_parent = shared
        .groups()
        .find(|group| group.to_string() == format!("Teamwork / Fleet [{}]", second.display()))
        .unwrap();
    let scoped = TargetGroups {
        group_order: Vec::new(),
        groups: vec![parent.clone()],
        explicit: vec![],
    };
    assert_eq!(
        scoped.resolve(&store, &shared, false).unwrap().as_slice(),
        std::slice::from_ref(&one_ref)
    );
    let mut forged = one_ref.clone();
    forged.id = local;
    assert!(forged.validate().is_err());

    let mut app = TestWindow::open(scratch.file()).unwrap();
    app.runtime.set_shared_targets(shared);
    app.runtime.set_connections(Some(&store));
    assert!(app.runtime.connection_choices.contains(&one));
    assert!(app.runtime.connection_choices.contains(&two));
    app.update(Message::Edit(Field::Name, "Shared group job".into()));
    app.update(Message::EditCommand(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("true".into())),
    )));
    use target_picker::{Choice, Event};
    app.update(Message::TargetPicker(Event::Open));
    app.update(Message::TargetPicker(Event::Search("Fleet".into())));
    let picks = [
        Choice::Connection(one),
        Choice::Group(parent),
        Choice::Group(second_parent),
        Choice::Group(local_group.into()),
    ];
    let visible: Vec<_> = app
        .view
        .draft
        .target_picker
        .as_ref()
        .unwrap()
        .matching(
            &app.runtime.connection_choices,
            &app.runtime.connection_group_choices,
            &app.runtime.connection_group_paths,
        )
        .map(|(choice, _)| choice)
        .collect();
    for pick in picks {
        assert!(visible.contains(&pick));
        app.update(Message::TargetPicker(Event::Select(pick, true)));
    }
    app.update(Message::TargetPicker(Event::Apply));
    app.update(Message::Save);
    let saved = app
        .selected_job()
        .expect("saved Teamwork group job")
        .action()
        .clone();
    assert!(saved.targets().eq([
        &one_ref,
        &two_ref,
        &ConnectionRef::from(store.get(local).unwrap())
    ]));
    drop(app);
    let app = TestWindow::open(scratch.file()).unwrap();
    assert_eq!(app.selected_job().unwrap().action(), &saved);
    let serialized = serde_json::to_value(&saved).unwrap();
    assert_eq!(serialized["target_groups"]["groups"][2], "Fleet");
    assert!(
        serialized["additional_connections"][1]
            .get("teamwork")
            .is_none()
    );

    let recheck = |store: &ConnectionStore, auth: &DefaultAuth, trust: &TrustStore| {
        saved.require_current_targets(
            store,
            &SharedTargets::recheck(&read(), store, auth, trust, saved.targets().cloned()),
        )
    };
    assert!(recheck(&store, &auth, &trusted).is_ok());
    assert!(one_ref.strict_trust(trusted.clone()).is_ok());
    let rsync_action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": one_ref,
        "remote": {
            "kind": "rsync_download",
            "program": scratch.0.join("rsync"),
            "local": scratch.0.join("download"),
            "remote": "/artifact"
        }
    }))
    .unwrap();
    let rsync_action = rsync_action.validated().unwrap();
    let transfer = rsync_action
        .remote_action()
        .unwrap()
        .rsync_transfer()
        .unwrap()
        .0;
    let request = rsync::ReviewRequests::for_transfer(transfer, &one_ref, &[], false).unwrap();
    assert!(request.references().eq([&one_ref]));
    let approval: rsync::Approval = serde_json::from_value(serde_json::json!({
        "program": scratch.0.join("rsync"),
        "digest": vec![0; 32],
        "reference": one_ref,
        "pins": sync::fingerprints(&sync::pins(&one_ref, &trusted).unwrap())
    }))
    .unwrap();
    let mut reviewed_transfer = transfer.clone();
    reviewed_transfer.approval = Some(approval);
    let reviewed = rsync::Reviewed::restore(&reviewed_transfer).unwrap();
    assert!(request.matches(&reviewed));
    let changed_targets = SharedTargets::new(&catalogue, &store, &trusted, |entry| {
        let (target, _) = chooser(entry)?;
        Some((
            target,
            SharedAuth::Key {
                id: 8,
                password_preference: None,
            },
        ))
    });
    let changed = changed_targets
        .choices()
        .find_map(|choice| {
            let reference = choice.reference()?;
            (reference.shared_identity() == one_ref.shared_identity()).then(|| reference.clone())
        })
        .unwrap();
    assert_eq!(changed.id, one_ref.id);
    let changed_request =
        rsync::ReviewRequests::for_transfer(transfer, &changed, &[], false).unwrap();
    assert!(!changed_request.matches(&reviewed));
    assert!(changed_targets.require_saved(&store, &one_ref).is_err());

    verify_teamwork_privileged_review(
        &store,
        SharedTargets::new(&catalogue, &store, &trusted, chooser),
        &one_ref,
        &changed,
        scoped,
        &trusted,
    );

    // The worker resolves the locally bound credential using the reviewed effective login.
    let password_targets = SharedTargets::new(&catalogue, &store, &trusted, |entry| {
        let mut target = entry.connection.login.endpoint.clone();
        target.username = "vault-login".into();
        Some((
            target,
            SharedAuth::Password {
                credential: "fixture-password".into(),
            },
        ))
    });
    let password = password_targets
        .choices()
        .find_map(|choice| {
            let reference = choice.reference()?;
            (reference.shared_identity() == one_ref.shared_identity()).then(|| reference.clone())
        })
        .unwrap();
    let entry = catalogue.get(password.shared_identity().unwrap()).unwrap();
    let secret = |id: &str, kind, login: &crate::ssh_dialog::LoginTarget| {
        assert_eq!(id, "fixture-password");
        assert_eq!(kind, crate::vault_window::Kind::Password);
        assert_eq!(login.username, "vault-login");
        assert_eq!(login.host, "one.example.test");
        Ok(kortty_vault::Secret::new("test-secret".into()))
    };
    let access = crate::clipboard_policy::ServerAccess::default();
    let mut credentials = crate::job_executor::Credentials::resolve_shared(
        &password, entry, &store, false, &access, secret,
    )
    .unwrap();
    let revision = Arc::new(());
    credentials.bind_shared_vault_revision(Arc::downgrade(&revision));
    drop(revision);
    assert!(
        matches!(credentials.into_swarm_route(), Err(error) if error.contains("vault changed"))
    );
    assert!(matches!(
        crate::job_executor::Credentials::resolve_shared(&password, entry, &store, true, &access, |_, _, _| panic!("a recording must not retrieve a Teamwork password")),
        Err(error) if error.contains("cannot record")
    ));
    assert!(one_ref.strict_trust(trust(9)).is_err());
    assert!(recheck(&store, &auth, &trust(9)).is_err());
    assert!(
        recheck(
            &store,
            &DefaultAuth::TemporaryKey {
                username: "operator".into()
            },
            &trusted
        )
        .is_err()
    );
    assert!(
        recheck(
            &store,
            &DefaultAuth::Key {
                id: 7,
                username: "other".into()
            },
            &trusted
        )
        .is_err()
    );

    write(
        &first,
        &format!("{initial}{}", row("2", "new.example.test", "Fleet/Nested")),
    );
    assert_eq!(
        recheck(&store, &auth, &trusted),
        Err(connection::GROUP_TARGETS_CHANGED)
    );
    write(&first, &row("1", "one.example.test", "Fleet/Moved"));
    assert!(recheck(&store, &auth, &trusted).is_err());
    write(&first, &initial);
    store
        .hide_teamwork(&[HiddenEntry::from(
            catalogue.get(one_ref.shared_identity().unwrap()).unwrap(),
        )])
        .unwrap();
    assert!(recheck(&store, &auth, &trusted).is_err());
    store
        .restore_teamwork(one_ref.shared_identity().unwrap())
        .unwrap();
    assert!(recheck(&store, &auth, &trusted).is_ok());
    fs::remove_file(&first).unwrap();
    assert!(recheck(&store, &auth, &trusted).is_err());
}

fn verify_teamwork_privileged_review(
    store: &ConnectionStore,
    shared: SharedTargets,
    route: &ConnectionRef,
    changed: &ConnectionRef,
    groups: TargetGroups,
    trust: &kortty_ssh::TrustStore,
) {
    let profile = Profile::temporary();
    let path = profile.file();
    let id = JobId::new("teamwork-privileged-review").unwrap();
    let policy = crate::clipboard_policy::ClipboardPolicy::default();
    let imported: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": route,
        "target_groups": groups,
        "imported_privileged": {"routes": [route], "settings": {
            "kind": "command", "text": "id -u", "options": {}
        }}
    }))
    .unwrap();
    let job = Job::new(
        id.clone(),
        "Imported sudo command",
        imported.validated().unwrap(),
    )
    .unwrap()
    .with_enabled(false);
    let mut draft = Draft::from_job(&job);
    draft.select_imported_sudo(SudoMode::Passwordless);
    assert!(
        draft
            .adopt_imported_privileged(store, &SharedTargets::default(), trust, &policy, &profile.0)
            .is_err()
    );
    assert!(
        !draft
            .adopt_imported_privileged(store, &shared, trust, &policy, &profile.0)
            .unwrap()
    );
    let adopted = draft.build(id.clone()).unwrap();
    assert!(!adopted.enabled());
    assert_eq!(adopted.action().sudo, SudoMode::Passwordless);
    assert_eq!(adopted.action().connection.as_ref(), Some(route));

    let archive: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": route,
        "remote": {"kind": "imported_archive", "route": route, "settings": {
            "sources": ["/input/hosts"], "target": "/output/archive.zip",
            "format": "zip", "level": 8, "java_excludes": [],
        }}
    }))
    .unwrap();
    let job = Job::new(id.clone(), "Imported archive", archive.validated().unwrap())
        .unwrap()
        .with_enabled(false);
    let mut draft = Draft::from_job(&job);
    let untrusted = kortty_ssh::TrustStore::from_pins(Vec::new()).unwrap();
    assert!(
        draft
            .adopt_imported_archive(store, &shared, &untrusted, &policy, &[])
            .is_err()
    );
    draft
        .adopt_imported_archive(store, &shared, trust, &policy, &[])
        .unwrap();
    let adopted = draft.build(id.clone()).unwrap();
    assert!(!adopted.enabled());
    assert!(matches!(
        &adopted.action().remote,
        Some(RemoteAction::ServerArchive(_))
    ));

    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": route,
        "target_groups": groups,
        "remote": {"kind": "chmod", "path": "/approved/teamwork", "mode": 416}
    }))
    .unwrap();
    let graph = JobGraph::new(vec![
        Job::new(id.clone(), "Teamwork permissions", action).unwrap(),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.runtime.set_shared_targets(shared);
    app.runtime.set_connections(Some(store));
    app.update(Message::Select(id));
    app.update(Message::Page(Page::Action));
    app.update(Message::Sudo(SudoMode::Passwordless));
    let pins = sync::pins(route, trust).unwrap();
    app.runtime
        .review_privileged(&mut app.view, |reviewed| {
            assert_eq!(reviewed, route);
            Ok(sync::fingerprints(&pins))
        })
        .unwrap();
    app.update(Message::ApprovePrivileged);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));

    let request = app.update(Message::Run(false)).pop().unwrap();
    let context = app.runtime.privileged_run_context(&request).unwrap();
    let RemoteAction::PrivilegedFileAction(metadata) = request.action.remote_action().unwrap()
    else {
        panic!("expected a reviewed privileged action");
    };
    assert!(
        context
            .authorize_file_action(metadata, route, SudoMode::Passwordless, &pins)
            .is_ok()
    );
    assert!(
        context
            .authorize_file_action(metadata, changed, SudoMode::Passwordless, &pins)
            .is_err()
    );
    assert!(
        context
            .authorize_file_action(metadata, route, SudoMode::Vault, &pins)
            .is_err()
    );
    assert!(
        context
            .authorize_file_action(metadata, route, SudoMode::Passwordless, &[])
            .is_err()
    );
}

#[test]
fn scheduler_target_groups_allow_group_only_and_refuse_empty_or_oversized_groups() {
    let scratch = Profile::temporary();
    let mut store = ConnectionStore::open(scratch.0.join("connections.json")).unwrap();
    let group = store.create_group(None, "Fleet").unwrap();
    let empty = store.create_group(None, "Empty").unwrap();
    let mut draft = Draft::default();
    assert!(
        draft
            .add_connection_group(empty.into(), &store, &SharedTargets::default())
            .is_err()
    );
    assert!(!draft.has_connection_groups());
    let id = store
        .save_in_group(grouped_connection("only.example.test"), Some(&group))
        .unwrap();
    draft.set(Field::Name, "Group-only job".into());
    draft.edit_command(text_editor::Action::Edit(text_editor::Edit::Paste(
        Arc::new("true".into()),
    )));
    assert!(
        draft
            .add_connection_group(group.clone().into(), &store, &SharedTargets::default())
            .unwrap()
    );
    let job_id = JobId::new("group-only").unwrap();
    let action = draft.build(job_id.clone()).unwrap().action().clone();
    assert!(action.target_groups.as_ref().unwrap().explicit.is_empty());
    assert_eq!(
        action.targets().map(|target| target.id).collect::<Vec<_>>(),
        [id]
    );
    for number in 1..=MAX_TARGETS {
        store
            .save_in_group(
                grouped_connection(&format!("host-{number}.example.test")),
                Some(&group),
            )
            .unwrap();
    }
    assert!(
        draft
            .refresh_connection_groups(&store, &SharedTargets::default())
            .is_err()
    );
    assert_eq!(draft.build(job_id).unwrap().action(), &action);
}

#[test]
fn scheduler_target_groups_keep_imported_swarm_selection_when_adopting_profile() {
    let scratch = Profile::temporary();
    let mut store = ConnectionStore::open(scratch.0.join("connections.json")).unwrap();
    let group = store.create_group(None, "Fleet").unwrap();
    let targets = (0..=MAX_TARGETS)
        .map(|number| {
            let id = store
                .save_in_group(
                    grouped_connection(&format!("host-{number}.example.test")),
                    Some(&group),
                )
                .unwrap();
            store.get(id).unwrap().into()
        })
        .collect::<Vec<ConnectionRef>>();
    let selection = TargetGroups {
        group_order: Vec::new(),
        groups: vec![group.into()],
        explicit: Vec::new(),
    };
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": targets[0],
        "additional_connections": targets[1..],
        "target_groups": selection,
        "imported_ai": {
            "kind": "swarm", "goal": "Inspect the fleet", "working_directory": "/srv",
            "parallelism": 4, "swarm_read_only": true
        }
    }))
    .unwrap();
    let id = JobId::new("java-imported-group-swarm").unwrap();
    let job = Job::new(id.clone(), "Imported Swarm", action)
        .unwrap()
        .with_enabled(false);
    assert!(SshCommand::validate_definition(&job).is_ok());
    assert!(job.action().require_current_target_groups(&store).is_ok());
    assert_eq!(
        job.action().require_executable(),
        Err(IMPORTED_AI_REVIEW_REQUIRED)
    );

    let mut draft = Draft::from_job(&job);
    draft.select_agent_profile(ProfileRef::Numeric(1));
    let converted = draft.build(id).unwrap();
    assert!(!converted.enabled());
    let action = converted.action();
    assert!(action.imported_ai.is_none());
    let agent = action.agent.as_ref().unwrap();
    assert_eq!(agent.probe, agent::Probe::SwarmReadOnly);
    assert!(agent.approval.is_none());
    assert_eq!(action.target_groups.as_ref(), Some(&selection));
    assert!(action.targets().eq(&targets));
    assert!(action.require_current_target_groups(&store).is_ok());
}

#[test]
fn scheduler_target_groups_refresh_bootstraps_and_removes_stale_selections() {
    let scratch = Profile::temporary();
    let mut store = ConnectionStore::open(scratch.0.join("connections.json")).unwrap();
    let mut app = TestWindow::open(scratch.file()).unwrap();
    app.runtime.set_connections(Some(&store));
    assert!(app.runtime.connection_group_choices.is_empty());
    let first = store.create_group(None, "First").unwrap();
    let second = store.create_group(None, "Second").unwrap();
    let a = store
        .save_in_group(grouped_connection("one.example.test"), Some(&first))
        .unwrap();
    let b = store
        .save_in_group(grouped_connection("two.example.test"), Some(&second))
        .unwrap();
    app.update(Message::RefreshConnectionGroups);
    assert_eq!(
        app.runtime.connection_group_choices,
        [
            ConnectionGroup::from(first.clone()),
            ConnectionGroup::from(second.clone())
        ]
    );
    assert!(!app.view.dirty());
    app.update(Message::Edit(Field::Name, "Group recovery".into()));
    app.update(Message::EditCommand(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("true".into())),
    )));
    app.update(Message::ConnectionGroupAdded(first.clone().into()));
    app.update(Message::ConnectionGroupAdded(second.clone().into()));
    let choice = ConnectionChoice::Saved {
        reference: store.get(b).unwrap().into(),
        name: "Explicit".into(),
    };
    app.update(Message::ConnectionAdded(choice));
    let job_id = JobId::new("group-recovery").unwrap();
    let job = app.view.draft.build(job_id.clone()).unwrap();
    assert_eq!(
        job.action()
            .targets()
            .map(|target| target.id)
            .collect::<Vec<_>>(),
        [b, a]
    );
    app.update(Message::ConnectionRemoved(b));
    let job = app.view.draft.build(job_id.clone()).unwrap();
    assert!(
        job.action()
            .target_groups
            .as_ref()
            .unwrap()
            .explicit
            .is_empty()
    );
    assert_eq!(
        job.action()
            .targets()
            .map(|target| target.id)
            .collect::<Vec<_>>(),
        [a, b]
    );

    let one = store.get(a).unwrap().clone();
    let two = store.get(b).unwrap().clone();
    store.save_in_group(one, None).unwrap();
    store.save_in_group(two, None).unwrap();
    app.update(Message::ConnectionGroupRemoved(first.into()));
    assert!(app.status().unwrap().contains("no saved SSH targets"));
    assert!(app.view.draft.build(job_id.clone()).is_err());
    app.update(Message::ConnectionGroupRemoved(second.into()));
    assert!(!app.view.draft.has_connection_groups());
    let choice = ConnectionChoice::Saved {
        reference: store.get(a).unwrap().into(),
        name: "Restored selection".into(),
    };
    app.update(Message::ConnectionSelected(choice));
    let job = app.view.draft.build(job_id).unwrap();
    assert_eq!(
        job.action()
            .targets()
            .map(|target| target.id)
            .collect::<Vec<_>>(),
        [a]
    );
}

#[test]
fn swarm_scheduler_handoff_builds_disabled_unapproved_draft() {
    use crate::ai_manager::ProfileRef;
    use crate::ssh_dialog::LoginTarget;

    let targets = [1, 2].map(|id| ConnectionRef {
        teamwork: None,
        temporary_keys: None,
        id,
        target: LoginTarget {
            host: format!("host-{id}.example.test"),
            port: 22,
            username: "operator".into(),
        },
        jump: None,
    });
    let make = |read_only| SwarmHandoff {
        name: "Swarm fixture".into(),
        goal: "Check disk space".into(),
        profile: ProfileRef::Numeric(1),
        targets: targets.to_vec(),
        read_only,
        journal: Some(crate::session_journal::automation::Options {
            enabled: true,
            ..crate::session_journal::automation::Options::default()
        }),
    };
    for (read_only, probe) in [
        (true, agent::Probe::SwarmReadOnly),
        (false, agent::Probe::SwarmIterative),
    ] {
        let draft = Draft::from_swarm_handoff(make(read_only)).unwrap();
        assert!(draft.id.is_none());
        let job = draft.build(JobId::new("swarm-fixture").unwrap()).unwrap();
        assert!(!job.enabled());
        assert!(!job.schedule().unwrap().enabled());
        assert!(job.action().automation_journal.as_ref().unwrap().enabled);
        assert_eq!(
            job.action().targets().collect::<Vec<_>>(),
            targets.iter().collect::<Vec<_>>()
        );
        let action = job.action().agent.as_ref().unwrap();
        assert_eq!(action.probe, probe);
        assert_eq!(action.profile, ProfileRef::Numeric(1));
        assert_eq!(action.goal, "Check disk space");
        assert!(action.approval.is_none());
    }
    let mut duplicate = make(true);
    duplicate.targets[1] = duplicate.targets[0].clone();
    assert!(Draft::from_swarm_handoff(duplicate).is_err());
}

struct TestWindow {
    runtime: SchedulerWindow,
    view: SchedulerView,
}

impl TestWindow {
    fn open(path: PathBuf) -> Result<Self, String> {
        let runtime = SchedulerWindow::open(path)?;
        let view = runtime.new_view();
        Ok(Self { runtime, view })
    }

    fn update(&mut self, message: Message) -> Vec<RunRequest<SshCommand>> {
        if matches!(
            message,
            Message::Tick | Message::Finished(_) | Message::FinishedWithSessionJournals(..)
        ) {
            let requests = self.runtime.update_runtime(message);
            self.runtime.sync_view(&mut self.view);
            requests
        } else {
            self.runtime.update_view(&mut self.view, message)
        }
    }

    fn selected_job(&self) -> Option<&Job<SshCommand>> {
        self.runtime.selected_job(&self.view)
    }

    fn request_close(&mut self) -> bool {
        self.view.request_close()
    }

    fn close_pending(&self) -> bool {
        self.view.close_pending()
    }

    fn confirm_close(&mut self) -> bool {
        self.runtime.confirm_close(&mut self.view)
    }

    fn status(&self) -> Option<&str> {
        self.view
            .status
            .as_deref()
            .or(self.runtime.status.as_deref())
    }
}

#[test]
fn scheduler_journal_output_uses_saved_mode_and_respects_capture_policy() {
    use crate::clipboard_policy::ClipboardPolicy;
    use kortty_jobs::RunOutput;

    let legacy: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "example.test", "port": 22, "username": "operator", "command": "true"
    }))
    .unwrap();
    assert_eq!(legacy.journal_detail, JournalDetail::Off);
    assert_eq!(
        Draft::default().journal_detail,
        JournalDetail::LimitedRedacted
    );

    let scratch = Profile::temporary();
    let path = scratch.file();
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.runtime.set_journal_output_allowed(true);
    app.update(Message::Edit(Field::Name, "Output fixture".into()));
    app.update(Message::Edit(Field::Host, "example.test".into()));
    app.update(Message::Edit(Field::Username, "operator".into()));
    app.update(Message::Edit(
        Field::KeyFile,
        scratch.0.join("fixture-key").to_string_lossy().into_owned(),
    ));
    app.update(Message::EditCommand(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("printf ready".into())),
    )));
    app.update(Message::JournalDetail(JournalDetail::Full));
    app.update(Message::Save);
    assert_eq!(
        app.selected_job().unwrap().action().journal_detail,
        JournalDetail::Full
    );

    let output = RunOutput {
        stdout: "Service ready".into(),
        stderr: "Retained diagnostic".into(),
    };
    let policy = ClipboardPolicy::load();
    assert!(policy.automation_journal_allowed());
    let mut ids = Vec::new();
    for (allowed, outcome) in [
        (true, RunOutcome::Failed { exit_code: None }),
        (false, RunOutcome::Success),
        (true, RunOutcome::Cancelled),
    ] {
        app.runtime.set_journal_output_allowed(allowed);
        let request = app.update(Message::Run(false)).pop().unwrap();
        app.update(Message::Finished(Completion {
            id: request.id,
            outcome,
            detail: None,
            recording: None,
            output: Some(CapturedOutput::fixture(output.clone(), &policy)),
            swarm_report: None,
        }));
        ids.push(request.id);
    }
    drop(app);
    let restored = TestWindow::open(path).unwrap();
    assert_eq!(
        restored.selected_job().unwrap().action().journal_detail,
        JournalDetail::Full
    );
    assert_eq!(restored.runtime.store.journal_output(ids[0]), Some(&output));
    assert!(restored.runtime.store.journal_output(ids[1]).is_none());
    assert!(restored.runtime.store.journal_output(ids[2]).is_none());
    assert_eq!(restored.runtime.store.journal().len(), 3);
}

#[test]
fn new_scheduler_ai_draft_suggests_the_default_without_review_or_replacement() {
    let profile = Profile::temporary();
    let mut store = ConnectionStore::open(profile.0.join("connections.json")).unwrap();
    let connection = store
        .save(grouped_connection("fleet.example.test"))
        .unwrap();
    let config: Configuration = serde_json::from_value(serde_json::json!({
        "profiles": {
            "profiles": [
                {"id": 1, "name": "Alternative", "model": "alternate"},
                {"id": 2, "name": "Default", "model": "default"}
            ],
            "default": 2,
            "last_id": 2
        }
    }))
    .unwrap();
    let mut app = TestWindow::open(profile.file()).unwrap();
    app.runtime.set_connections(Some(&store));
    app.runtime.set_agent_profiles(Some(&config), true);
    app.update(Message::Edit(Field::Name, "Fleet diagnostics".into()));
    app.update(Message::Edit(Field::WorkingDirectory, "/srv".into()));
    app.update(Message::EditAgentGoal(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("Inspect system state".into())),
    )));
    app.update(Message::ConnectionSelected(ConnectionChoice::Saved {
        reference: store.get(connection).unwrap().into(),
        name: "Fleet".into(),
    }));
    app.update(Message::CommandSource(Source::AgentDiagnostics));
    let id = JobId::new("default-ai").unwrap();
    let job = app.view.draft.build(id.clone()).unwrap();
    let action = job.action().agent.as_ref().unwrap();
    assert_eq!(action.profile, ProfileRef::Numeric(2));
    assert!(action.approval.is_none());

    let alternate = app
        .runtime
        .agent_profiles
        .selected(&ProfileRef::Numeric(1))
        .unwrap()
        .clone();
    app.update(Message::AgentProfile(alternate));
    app.update(Message::CommandSource(Source::SwarmDiagnostics));
    let job = app.view.draft.build(id).unwrap();
    let action = job.action().agent.as_ref().unwrap();
    assert_eq!(action.profile, ProfileRef::Numeric(1));
    assert!(action.approval.is_none());
}

pub(super) struct Profile(PathBuf);

impl Profile {
    pub(super) fn temporary() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let index = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "kortty-scheduler-window-{}-{index}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("create temporary scheduler profile: {error}"),
            }
        }
    }

    pub(super) fn file(&self) -> PathBuf {
        self.0.join("scheduler.json")
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn save_reviewed_folder_copy(app: &mut TestWindow, standing_roots: Option<[u8; 32]>) {
    app.update(Message::Page(Page::Action));
    let pending = app.runtime.begin_sync_review(&mut app.view).unwrap();
    app.update(Message::SyncPreviewed(
        pending.ticket,
        Ok(Arc::new(sync::Review {
            preview: crate::sftp_window::scheduled::Preview {
                roots: ["Local reviewed".into(), "Server reviewed".into()],
                replacements: Vec::new(),
                paths: vec![(false, "new.txt".into())],
                proof: [3; 32],
                bytes: 4,
            },
            pins: vec!["server.example SHA256:fixture".into()],
            standing_roots,
        })),
    ));
    app.update(Message::ApproveSync);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
}

#[test]
fn reviewed_sftp_approval_round_trips_through_editor_and_claims_once() {
    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = root.join("scheduler.json");
    let id = JobId::new("reviewed-sftp").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
        "remote": {"kind": "sync", "direction": "upload", "local": root.join("local"), "remote": "/reviewed"}
    })).unwrap();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph =
            JobGraph::new(vec![Job::new(id.clone(), "Reviewed copy", action).unwrap()]).unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    {
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        save_reviewed_folder_copy(&mut app, None);
    }
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    let request = app.update(Message::Run(false)).pop().unwrap();
    let context = app.runtime.sync_run_context(&request).unwrap();
    assert!(context.permits(&request));
    let replacement = RunRequest {
        id: request.id,
        job: request.job.clone(),
        action: Arc::new(request.action.as_ref().clone()),
        trigger: request.trigger,
        started_at: request.started_at,
        cancellation: request.cancellation.clone(),
    };
    assert!(!context.permits(&replacement));
    let replacement = RunRequest {
        action: Arc::clone(&request.action),
        cancellation: kortty_jobs::Cancellation::default(),
        ..replacement
    };
    assert!(app.runtime.sync_run_context(&replacement).is_none());
    assert!(!context.permits(&replacement));
    let RemoteAction::Sync(copy) = request.action.remote_action().unwrap() else {
        panic!("expected reviewed copy")
    };
    let approval = copy.approval.as_deref();
    assert!(approval.is_some());
    let receipt = context.store.load(&id, approval, context.scope).unwrap();
    assert_eq!(receipt.proof, [3; 32]);
    let wrong_scope = [255; 32];
    assert!(context.store.load(&id, approval, wrong_scope).is_err());
    context.store.claim(&id, &receipt).unwrap();
    assert!(context.store.claim(&id, &receipt).is_err());
    assert!(context.store.load(&id, approval, context.scope).is_err());
}

#[test]
fn scheduler_changed_download_edit_and_reopen_use_the_new_exact_approval() {
    scheduler_changed_copy_edit_and_reopen("download");
}

#[test]
fn scheduler_changed_upload_edit_and_reopen_use_the_new_exact_approval() {
    scheduler_changed_copy_edit_and_reopen("upload");
}

fn scheduler_changed_copy_edit_and_reopen(direction: &str) {
    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = root.join("scheduler.json");
    let id = JobId::new("reviewed-replacement").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
        "remote": {"kind": "sync", "direction": direction, "local": root.join("local"), "remote": "/reviewed"}
    })).unwrap();
    let graph = JobGraph::new(vec![
        Job::new(id.clone(), "Reviewed replacement", action).unwrap(),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();

    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    save_reviewed_folder_copy(&mut app, None);
    let old = app.selected_job().unwrap().action().clone();
    let RemoteAction::Sync(old_copy) = old.remote_action().unwrap() else {
        panic!("expected sync")
    };
    let old_nonce = old_copy.approval.clone().unwrap();

    app.update(Message::Page(Page::Action));
    app.update(Message::SyncReplaceChanged(true));
    let pending = app.runtime.begin_sync_review(&mut app.view).unwrap();
    assert!(pending.copy.replace_changed && pending.copy.approval.is_none());
    let [source, target] = if direction == "download" {
        ["/reviewed/changed.txt", "local/changed.txt"]
    } else {
        ["local/changed.txt", "/reviewed/changed.txt"]
    };
    app.update(Message::SyncPreviewed(
        pending.ticket,
        Ok(Arc::new(sync::Review {
            preview: crate::sftp_window::scheduled::Preview {
                proof: [5; 32],
                roots: ["Remote reviewed".into(), "Local reviewed".into()],
                paths: vec![],
                replacements: vec![crate::sftp_window::scheduled::ReplacementPreview {
                    source: source.into(),
                    target: target.into(),
                    source_sha256: "a".repeat(64),
                    target_sha256: "b".repeat(64),
                }],
                bytes: 4,
            },
            pins: vec!["server.example SHA256:fixture".into()],
            standing_roots: None,
        })),
    ));
    app.update(Message::ApproveSync);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    drop(app);

    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    let request = app.update(Message::Run(false)).pop().unwrap();
    let context = app.runtime.sync_run_context(&request).unwrap();
    let RemoteAction::Sync(copy) = request.action.remote_action().unwrap() else {
        panic!("expected sync")
    };
    assert!(copy.replace_changed);
    assert!(context.load_receipt(Some(&old_nonce)).is_err());
    let receipt = context.load_receipt(copy.approval.as_deref()).unwrap();
    assert_eq!(receipt.proof, [5; 32]);
    context.store.claim(&id, &receipt).unwrap();
    context.store.require_claimed(&id, &receipt).unwrap();
    assert!(context.store.claim(&id, &receipt).is_err());
}

#[test]
fn legacy_download_review_reopens_but_execution_requires_a_fresh_editor_review() {
    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = root.join("scheduler.json");
    let id = JobId::new("legacy-download").unwrap();
    let mut action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
        "remote": {"kind": "sync", "direction": "download", "local": root.join("local"), "remote": "/reviewed"}
    })).unwrap();
    let Some(RemoteAction::Sync(copy)) = action.remote.as_mut() else {
        panic!("expected ordinary download");
    };
    let old_scope = copy
        .scope(action.connection.as_ref().unwrap(), None)
        .unwrap();
    let old =
        sync::Receipt::for_operation(old_scope, [1; 32], vec!["pinned route".into()]).unwrap();
    copy.approval = Some(old.approval.clone());
    sync::Store::new(&root)
        .swap(&id, old.clone(), |_| true)
        .unwrap();
    let graph = JobGraph::new(vec![Job::new(id.clone(), "Download", action).unwrap()]).unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();

    {
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        let request = app.update(Message::Run(false)).pop().unwrap();
        let context = app.runtime.sync_run_context(&request).unwrap();
        assert_ne!(context.scope, old_scope);
        assert_eq!(
            context
                .load_receipt(Some(&old.approval))
                .err()
                .unwrap()
                .to_string(),
            "This scheduled SFTP copy needs a fresh folder review."
        );
    }

    {
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        save_reviewed_folder_copy(&mut app, None);
    }
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    let request = app.update(Message::Run(false)).pop().unwrap();
    let context = app.runtime.sync_run_context(&request).unwrap();
    let Ok(RemoteAction::Sync(copy)) = request.action.remote_action() else {
        panic!("expected reviewed download");
    };
    assert_ne!(copy.approval.as_deref(), Some(old.approval.as_str()));
    let fresh = context.load_receipt(copy.approval.as_deref()).unwrap();
    assert_eq!(fresh.proof, [3; 32]);
    context.store.claim(&id, &fresh).unwrap();
    assert!(context.load_receipt(copy.approval.as_deref()).is_err());
}

#[test]
fn recurring_sftp_editor_reopens_and_authorizes_only_its_calendar() {
    let profile = Profile::temporary();
    let id = JobId::new("recurring-copy").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
        "remote": {"kind": "sync", "direction": "upload", "local": profile.0.join("local"), "remote": "/reviewed",
            "standing": {"days": 7, "limits": {"max_firings": 3, "max_actions": 8, "max_bytes": 1048576}}}
    })).unwrap();
    let path = profile.file();
    let schedule = kortty_jobs::JobSchedule::default().with_fixed_times(["12:00".parse().unwrap()]);
    {
        let graph = JobGraph::new(vec![
            Job::new(id.clone(), "Recurring copy", action)
                .unwrap()
                .with_schedule(schedule),
        ])
        .unwrap();
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&graph, &[], &[])
            .unwrap();
    }
    {
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.zone = TimeZone::UTC;
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        save_reviewed_folder_copy(&mut app, Some([4; 32]));
    }
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.zone = TimeZone::UTC;
    app.runtime.set_execution_allowed(true);
    app.runtime
        .scheduler
        .initialize_schedules(TimeZone::UTC, SystemTime::now())
        .unwrap();
    app.update(Message::Select(id.clone()));
    for cascade in [false, true] {
        assert!(app.update(Message::Run(cascade)).is_empty());
        assert_eq!(
            app.status(),
            Some("Recurring SFTP copies start through an approved calendar or Chain.")
        );
    }
    let due = app.runtime.scheduler.scheduled_at(&id).unwrap();
    let request = app
        .runtime
        .update_runtime_at(Message::Tick, due)
        .pop()
        .unwrap();
    assert_eq!(request.trigger, RunTrigger::Scheduled);
    let context = app.runtime.sync_run_context(&request).unwrap();
    let (approval, _) = context.standing_approval().unwrap();
    assert_eq!(approval.anchors, [4; 32]);
    assert_eq!(approval.limits.max_firings, 3);
}

#[test]
fn recurring_protected_folder_editor_persists_modes_and_admits_chain_and_calendar() {
    use serde_json::json;
    for direction in ["upload", "download"] {
        for sudo in ["passwordless", "vault"] {
            for (calendar, replace_changed) in
                [(false, false), (false, true), (true, false), (true, true)]
            {
                let profile = Profile::temporary();
                let root = profile.0.canonicalize().unwrap();
                let path = root.join("scheduler.json");
                let id = JobId::new("protected-recurring-folder").unwrap();
                let predecessor = JobId::new("prepare-protected-copy").unwrap();
                let connection = json!({"id":1,"target":{"host":"server.example","port":22,"username":"operator"}});
                let prepare: SshCommand =
                    serde_json::from_value(json!({"connection":connection,"command":"true"}))
                        .unwrap();
                let action: SshCommand = serde_json::from_value(json!({
                    "connection":connection,"sudo":sudo,
                    "remote":{"kind":format!("privileged_tree_{direction}"),"direction":direction,
                    "local":root.join("receiver"),"remote":"/protected/receiver","replace_changed":replace_changed,"standing":sync::standing::Settings::default()}
                })).unwrap();
                let mut job = Job::new(id.clone(), "Recurring protected folder", action)
                    .unwrap()
                    .with_dependencies(DependencyMode::Chain, vec![predecessor.clone()], None);
                if calendar {
                    job = job.with_schedule(
                        kortty_jobs::JobSchedule::default()
                            .with_fixed_times(["12:00".parse().unwrap()]),
                    );
                }
                let graph = JobGraph::new(vec![
                    Job::new(predecessor.clone(), "Prepare", prepare).unwrap(),
                    job,
                ])
                .unwrap();
                SchedulerStore::open(&path)
                    .unwrap()
                    .commit_with_actions(&graph, &[], &[])
                    .unwrap();
                let mut app = TestWindow::open(path.clone()).unwrap();
                app.runtime.zone = TimeZone::UTC;
                app.runtime.set_execution_allowed(true);
                app.update(Message::Select(id.clone()));
                save_reviewed_folder_copy(&mut app, Some([6; 32]));
                drop(app);
                let mut app = TestWindow::open(path).unwrap();
                app.runtime.zone = TimeZone::UTC;
                app.runtime.set_execution_allowed(true);
                app.runtime
                    .scheduler
                    .initialize_schedules(TimeZone::UTC, SystemTime::now())
                    .unwrap();
                app.update(Message::Select(id.clone()));
                assert!(app.update(Message::Run(false)).is_empty());
                app.update(Message::Select(predecessor));
                let prepare = app.update(Message::Run(false)).pop().unwrap();
                let completed = |id| {
                    Message::Finished(Completion {
                        id,
                        outcome: RunOutcome::Success,
                        detail: None,
                        output: None,
                        recording: None,
                        swarm_report: None,
                    })
                };
                let request = app.update(completed(prepare.id)).pop().unwrap();
                assert_eq!(request.job, id);
                assert_eq!(request.trigger, RunTrigger::Dependency);
                let context = app.runtime.sync_run_context(&request).unwrap();
                let (approval, admission) = context.standing_approval().unwrap();
                let receipt = context
                    .store
                    .load_tree(
                        &id,
                        Some(&approval.approval),
                        context.scope,
                        SystemTime::now(),
                        Some(admission),
                    )
                    .unwrap();
                assert_eq!(receipt.receiver, [6; 32]);
                assert_eq!(receipt.create_root, !replace_changed);
                let original_job = app.runtime.scheduler.graph().job(&id).unwrap();
                let mut changed_action = original_job.action().clone();
                changed_action.sudo = if changed_action.sudo == SudoMode::Vault {
                    SudoMode::Passwordless
                } else {
                    SudoMode::Vault
                };
                let mut changed_job = Job::new(id.clone(), "Different elevation", changed_action)
                    .unwrap()
                    .with_dependencies(
                        original_job.mode(),
                        original_job.dependencies().to_vec(),
                        original_job.max_age_minutes(),
                    );
                if let Some(schedule) = original_job.schedule() {
                    changed_job = changed_job.with_schedule(schedule.clone());
                }
                let changed_copy = match changed_job.action().remote.as_ref().unwrap() {
                    RemoteAction::PrivilegedTreeUpload(copy)
                    | RemoteAction::PrivilegedTreeDownload(copy) => copy,
                    _ => unreachable!(),
                };
                let changed_scope = changed_copy
                    .standing_scope(
                        &changed_job,
                        changed_job.action().connection.as_ref().unwrap(),
                        &TimeZone::UTC,
                    )
                    .unwrap();
                assert_ne!(changed_scope, context.scope);
                assert!(
                    context
                        .store
                        .load_tree(
                            &id,
                            Some(&approval.approval),
                            changed_scope,
                            SystemTime::now(),
                            Some(admission)
                        )
                        .is_err()
                );
                let forged = RunRequest {
                    id: request.id,
                    job: request.job.clone(),
                    action: Arc::clone(&request.action),
                    trigger: RunTrigger::Scheduled,
                    started_at: request.started_at,
                    cancellation: request.cancellation.clone(),
                };
                assert!(app.runtime.sync_run_context(&forged).is_none());

                assert!(
                    context
                        .store
                        .load_standing(
                            &id,
                            Some(&approval.approval),
                            context.scope,
                            SystemTime::now()
                        )
                        .is_err()
                );
                context
                    .store
                    .arm_tree(&id, &receipt, admission, (1, 4), &|| false)
                    .unwrap();
                context
                    .store
                    .finish_tree(
                        &id,
                        &receipt,
                        admission.firing(),
                        if replace_changed { [6; 32] } else { [7; 32] },
                    )
                    .unwrap();
                app.update(completed(request.id));
                let advanced = context
                    .store
                    .load_tree(
                        &id,
                        Some(&approval.approval),
                        context.scope,
                        SystemTime::now(),
                        None,
                    )
                    .unwrap();
                assert!(!advanced.create_root);
                assert_eq!(
                    advanced.receiver,
                    if replace_changed { [6; 32] } else { [7; 32] }
                );
                if calendar {
                    let due = app.runtime.scheduler.scheduled_at(&id).unwrap();
                    let next = app
                        .runtime
                        .update_runtime_at(Message::Tick, due)
                        .pop()
                        .unwrap();
                    assert_eq!(next.trigger, RunTrigger::Scheduled);
                    assert!(
                        app.runtime
                            .sync_run_context(&next)
                            .unwrap()
                            .standing_approval()
                            .is_some()
                    );
                }
            }
        }
    }
}

#[test]
fn recurring_sftp_editor_admits_exact_chain_rounds_and_optional_calendar() {
    use serde_json::json;

    for calendar in [false, true] {
        let profile = Profile::temporary();
        let root = profile.0.canonicalize().unwrap();
        let path = root.join("scheduler.json");
        let id = JobId::new("chain-sftp-copy").unwrap();
        let predecessor = JobId::new("prepare-copy").unwrap();
        let connection = json!({"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}});
        let prepare: SshCommand = serde_json::from_value(json!({
            "connection": connection, "command": "true",
        }))
        .unwrap();
        let action: SshCommand = serde_json::from_value(json!({
            "connection": connection,
            "remote": {"kind":"sync","direction":"download","local":root.join("local"),
                "remote":"/source","replace_changed":true,"standing":sync::standing::Settings::default()},
        })).unwrap();
        let mut job = Job::new(id.clone(), "Recurring SFTP", action)
            .unwrap()
            .with_dependencies(DependencyMode::Chain, vec![predecessor.clone()], None);
        if calendar {
            job = job.with_schedule(
                kortty_jobs::JobSchedule::default().with_fixed_times(["12:00".parse().unwrap()]),
            );
        }
        let graph = JobGraph::new(vec![
            Job::new(predecessor.clone(), "Prepare source", prepare).unwrap(),
            job,
        ])
        .unwrap();
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&graph, &[], &[])
            .unwrap();
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.zone = TimeZone::UTC;
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        app.update(Message::Page(Page::Action));
        let pending = app.runtime.begin_sync_review(&mut app.view).unwrap();
        let mut other = app.runtime.new_view();
        app.runtime
            .update_view(&mut other, Message::Select(predecessor.clone()));
        app.runtime.update_view(
            &mut other,
            Message::Edit(Field::Name, "Prepare source again".into()),
        );
        app.runtime.update_view(&mut other, Message::Save);
        app.runtime.sync_view(&mut app.view);
        assert!(pending.cancelled.load(Ordering::Acquire));
        save_reviewed_folder_copy(&mut app, Some([6; 32]));
        drop(app);

        let mut app = TestWindow::open(path).unwrap();
        app.runtime.zone = TimeZone::UTC;
        app.runtime.set_execution_allowed(true);
        app.runtime
            .scheduler
            .initialize_schedules(TimeZone::UTC, SystemTime::now())
            .unwrap();
        app.update(Message::Select(id.clone()));
        for cascade in [false, true] {
            assert!(app.update(Message::Run(cascade)).is_empty());
            assert_eq!(
                app.status(),
                Some("Recurring SFTP copies start through an approved calendar or Chain.")
            );
        }
        app.update(Message::Select(predecessor.clone()));
        let prepare = app.update(Message::Run(false)).pop().unwrap();
        let completed = |id| {
            Message::Finished(Completion {
                id,
                outcome: RunOutcome::Success,
                detail: None,
                output: None,
                recording: None,
                swarm_report: None,
            })
        };
        let request = app.update(completed(prepare.id)).pop().unwrap();
        assert_eq!(request.job, id);
        assert_eq!(request.trigger, RunTrigger::Dependency);
        let context = app.runtime.sync_run_context(&request).unwrap();
        let (approval, _) = context.standing_approval().unwrap();
        assert_eq!(approval.anchors, [6; 32]);
        assert!(context.permits(&request));
        let fake_calendar = RunRequest {
            id: request.id,
            job: request.job.clone(),
            action: Arc::clone(&request.action),
            trigger: RunTrigger::Scheduled,
            started_at: request.started_at,
            cancellation: request.cancellation.clone(),
        };
        assert!(app.runtime.sync_run_context(&fake_calendar).is_none());
        app.update(completed(request.id));

        if calendar {
            let due = app.runtime.scheduler.scheduled_at(&id).unwrap();
            let request = app
                .runtime
                .update_runtime_at(Message::Tick, due)
                .pop()
                .unwrap();
            assert_eq!(request.trigger, RunTrigger::Scheduled);
            assert!(
                app.runtime
                    .sync_run_context(&request)
                    .unwrap()
                    .standing_approval()
                    .is_some()
            );
        } else {
            assert!(app.runtime.scheduler.scheduled_at(&id).is_none());
        }
    }
}

#[test]
fn reviewed_sftp_cascade_refreshes_a_spent_receipt_and_keeps_the_other_branch() {
    let profile = Profile::temporary();
    let root = JobId::new("cascade-root").unwrap();
    let copy = JobId::new("cascade-copy").unwrap();
    let sibling = JobId::new("cascade-sibling").unwrap();
    let prerequisite = JobId::new("cascade-prerequisite").unwrap();
    let route = serde_json::json!({"id": 1, "target": {
        "host": "server.example", "port": 22, "username": "operator"
    }});
    let command: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": route, "command": "true"
    }))
    .unwrap();
    let transfer: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": route,
        "remote": {"kind": "sync", "direction": "upload", "local": profile.0.join("local"), "remote": "/reviewed"}
    })).unwrap();
    let path = profile.file();
    {
        let graph = JobGraph::new(vec![
            Job::new(root.clone(), "Root", command.clone())
                .unwrap()
                .with_dependencies(DependencyMode::Gate, vec![prerequisite.clone()], None),
            Job::new(copy.clone(), "Copy", transfer)
                .unwrap()
                .with_dependencies(DependencyMode::Gate, vec![root.clone()], None),
            Job::new(sibling.clone(), "Sibling", command.clone())
                .unwrap()
                .with_dependencies(DependencyMode::Gate, vec![root.clone()], None),
            Job::new(prerequisite.clone(), "Prerequisite", command).unwrap(),
        ])
        .unwrap();
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&graph, &[], &[])
            .unwrap();
    }
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(copy.clone()));
    save_reviewed_folder_copy(&mut app, None);
    app.update(Message::Select(root.clone()));
    assert!(app.update(Message::Run(true)).is_empty());
    assert!(
        matches!(&app.view.confirm, Some(Confirmation::SyncCascade(review, _))
        if review.copies().eq([(&copy, true)]))
    );

    let mut other_view = app.runtime.new_view();
    app.runtime
        .update_view(&mut other_view, Message::Select(copy.clone()));
    assert!(
        app.runtime
            .update_view(&mut other_view, Message::Run(false))
            .is_empty()
    );
    let manual = app
        .runtime
        .update_view(&mut other_view, Message::Confirm)
        .pop()
        .unwrap();
    let context = app.runtime.sync_run_context(&manual).unwrap();
    let RemoteAction::Sync(action) = manual.action.remote_action().unwrap() else {
        unreachable!()
    };
    let receipt = context.load_receipt(action.approval.as_deref()).unwrap();
    context.store.claim(&copy, &receipt).unwrap();
    let finish = |id| {
        Message::Finished(Completion {
            id,
            outcome: RunOutcome::Success,
            detail: None,
            recording: None,
            output: None,
            swarm_report: None,
        })
    };
    assert!(app.update(finish(manual.id)).is_empty());

    assert!(app.update(Message::Confirm).is_empty());
    assert!(
        matches!(&app.view.confirm, Some(Confirmation::SyncCascade(review, _))
        if review.copies().eq([(&copy, false)]))
    );
    let start = app.update(Message::Confirm).pop().unwrap();
    assert_eq!(start.job, root);
    assert!(!app.runtime.reviewed_cascade_root_blocked(&start));

    app.runtime
        .update_view(&mut other_view, Message::Select(prerequisite));
    let failed = app
        .runtime
        .update_view(&mut other_view, Message::Run(false))
        .pop()
        .unwrap();
    assert!(
        app.update(Message::Finished(Completion {
            id: failed.id,
            outcome: RunOutcome::Failed { exit_code: Some(1) },
            detail: None,
            recording: None,
            output: None,
            swarm_report: None,
        }))
        .is_empty()
    );
    assert!(app.runtime.reviewed_cascade_root_blocked(&start));
    let recovered = app
        .runtime
        .update_view(&mut other_view, Message::Run(false))
        .pop()
        .unwrap();
    assert!(app.update(finish(recovered.id)).is_empty());
    assert!(!app.runtime.reviewed_cascade_root_blocked(&start));

    let starts = app.update(finish(start.id));
    assert_eq!(starts.len(), 2);
    assert!(starts.iter().any(|request| request.job == sibling));
    let denied = starts.iter().find(|request| request.job == copy).unwrap();
    assert!(app.runtime.sync_run_context(denied).is_none());
}

#[test]
fn privileged_tree_upload_uses_editor_review_and_a_distinct_one_use_receipt() {
    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = profile.file();
    let id = JobId::new("reviewed-root-tree").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
        "sudo": "passwordless",
        "remote": {"kind": "privileged_tree_upload", "direction": "upload",
                   "local": root.join("local"), "remote": "/protected/new", "excludes": "*.tmp"}
    })).unwrap();
    let mut forged = action.clone();
    forged.sudo = SudoMode::Disabled;
    assert!(forged.validated().is_err());
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![
            Job::new(id.clone(), "Reviewed root tree", action).unwrap(),
        ])
        .unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    {
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        app.update(Message::Page(Page::Action));
        assert!(app.view.editing_privileged_tree());
        let pending = app.runtime.begin_sync_review(&mut app.view).unwrap();
        assert_eq!(pending.sudo, SudoMode::Passwordless);
        app.update(Message::SyncPreviewed(
            pending.ticket,
            Ok(Arc::new(sync::Review {
                preview: crate::sftp_window::scheduled::Preview {
                    roots: ["Local reviewed".into(), "Server reviewed".into()],
                    replacements: Vec::new(),
                    paths: vec![(true, "empty".into()), (false, "new.txt".into())],
                    proof: [8; 32],
                    bytes: 4,
                },
                pins: vec!["server.example SHA256:fixture".into()],
                standing_roots: None,
            })),
        ));
        app.update(Message::ApproveSync);
        app.update(Message::Save);
        assert_eq!(app.status(), Some("Job saved"));
    }
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    let request = app.update(Message::Run(false)).pop().unwrap();
    let context = app.runtime.sync_run_context(&request).unwrap();
    let RemoteAction::PrivilegedTreeUpload(copy) = request.action.remote_action().unwrap() else {
        panic!("expected privileged tree upload");
    };
    let reference = request.action.connection.as_ref().unwrap();
    let schedule = app.runtime.scheduler.graph().job(&id).unwrap().schedule();
    let normal = copy.scope(reference, schedule).unwrap();
    assert_ne!(context.scope, normal);
    assert_ne!(
        context.scope,
        copy.privileged_scope(reference, schedule, SudoMode::Vault)
            .unwrap()
    );
    let approval = copy.approval.as_deref();
    let receipt = context.store.load(&id, approval, context.scope).unwrap();
    assert_eq!(receipt.proof, [8; 32]);
    assert!(context.store.require_claimed(&id, &receipt).is_err());
    context.store.claim(&id, &receipt).unwrap();
    context.store.require_claimed(&id, &receipt).unwrap();
    assert!(context.store.claim(&id, &receipt).is_err());
}

#[test]
fn privileged_tree_download_requires_complete_source_and_saved_elevation() {
    let profile = Profile::temporary();
    let path = profile.0.canonicalize().unwrap().join("new-folder");
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
        "sudo": "passwordless",
        "remote": {"kind": "privileged_tree_download", "direction": "download",
                   "local": path, "remote": "/protected/source"}
    })).unwrap();
    assert!(action.clone().validated().is_ok());
    assert!(action.remote_action().unwrap().writes_local_file());
    let mut missing_sudo = action.clone();
    missing_sudo.sudo = SudoMode::Disabled;
    assert!(missing_sudo.validated().is_err());
    let mut excluded = action.clone();
    let Some(RemoteAction::PrivilegedTreeDownload(copy)) = &mut excluded.remote else {
        unreachable!();
    };
    copy.excludes = "*.tmp".into();
    assert!(excluded.validated().is_err());
    let mut wrong_direction = action;
    let Some(RemoteAction::PrivilegedTreeDownload(copy)) = &mut wrong_direction.remote else {
        unreachable!();
    };
    copy.direction = sync::Direction::Upload;
    assert!(wrong_direction.validated().is_err());
}

#[test]
fn privileged_staging_is_explicit_and_saved_review_binds_recurring_transfers() {
    use kortty_ssh::HostKey;

    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = root.join("scheduler.json");
    let id = JobId::new("privileged-sftp").unwrap();
    let ordinary: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {
            "id": 7,
            "target": {"host": "server.example", "port": 22, "username": "operator"},
            "jump": {"host": "jump.example", "port": 2222, "username": "relay"}
        },
        "remote": {"kind":"upload", "local":root.join("config.txt"), "remote":"/approved/config.txt"}
    })).unwrap();
    assert!(ordinary.clone().validated().is_ok());
    let mut forged = ordinary.clone();
    forged.sudo = SudoMode::Passwordless;
    assert!(forged.validated().is_err());
    let key = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
    )
    .unwrap();
    let pins = [
        (Endpoint::new("jump.example", 2222).unwrap(), key.clone()),
        (Endpoint::new("server.example", 22).unwrap(), key),
    ];
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![
            Job::new(id.clone(), "Privileged fixture", ordinary).unwrap(),
        ])
        .unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    {
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        app.update(Message::Page(Page::Action));
        app.update(Message::Sudo(SudoMode::Vault));
        app.update(Message::Save);
        assert!(
            app.status()
                .is_some_and(|status| status.contains("Review the exact"))
        );
        app.runtime
            .review_privileged(&mut app.view, |_| Ok(sync::fingerprints(&pins)))
            .unwrap();
        app.update(Message::Edit(Field::RemotePath, "/other/config.txt".into()));
        app.update(Message::ApprovePrivileged);
        assert!(
            app.status()
                .is_some_and(|status| status.contains("changed"))
        );
        app.update(Message::Edit(
            Field::RemotePath,
            "/approved/config.txt".into(),
        ));
        app.runtime
            .review_privileged(&mut app.view, |_| Ok(sync::fingerprints(&pins)))
            .unwrap();
        app.update(Message::ApprovePrivileged);
        app.update(Message::Save);
        assert_eq!(app.status(), Some("Job saved"));

        app.update(Message::Edit(Field::RemotePath, "/new/config.txt".into()));
        app.runtime
            .review_privileged(&mut app.view, |_| Ok(sync::fingerprints(&pins)))
            .unwrap();
        app.update(Message::ApprovePrivileged);
        let backup = root.join("previous.json");
        fs::rename(&path, &backup).unwrap();
        fs::create_dir(&path).unwrap();
        app.update(Message::Save);
        assert_ne!(app.status(), Some("Job saved"));
        assert!(app.view.dirty());
        fs::remove_dir(&path).unwrap();
        fs::rename(backup, &path).unwrap();
    }
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id));
    let request = app.update(Message::Run(false)).pop().unwrap();
    let context = app.runtime.privileged_run_context(&request).unwrap();
    let RemoteAction::Privileged(transfer) = request.action.remote_action().unwrap() else {
        panic!("expected explicitly approved staging");
    };
    let route = request.action.connection.as_ref().unwrap();
    assert!(transfer.approval.is_some());
    for _ in 0..2 {
        assert!(
            context
                .authorize(transfer, route, SudoMode::Vault, &pins)
                .is_ok()
        );
    }
    assert!(
        context
            .authorize(transfer, route, SudoMode::Vault, &pins[1..])
            .is_err()
    );
    assert!(
        context
            .authorize(transfer, route, SudoMode::Passwordless, &pins)
            .is_err()
    );
    let mut wrong_route = route.clone();
    wrong_route.target.username = "other".into();
    assert!(
        context
            .authorize(transfer, &wrong_route, SudoMode::Vault, &pins)
            .is_err()
    );
    let mut changed = transfer.clone();
    changed.local = root.join("other.txt");
    assert!(
        context
            .authorize(&changed, route, SudoMode::Vault, &pins)
            .is_err()
    );
    let mut edited = Draft::from_job(app.selected_job().unwrap());
    edited.select_source(Source::Command);
    assert_eq!(edited.sudo, SudoMode::Disabled);
}

#[test]
fn privileged_download_replacement_review_survives_reopen_and_can_only_be_claimed_once() {
    use crate::sftp_window::{ReviewedFileReceiver, local_tree};
    use std::ffi::OsStr;
    use std::io::Write as _;

    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = root.join("jobs.json");
    let mut connections =
        ConnectionStore::open(crate::profile::connections_path_at(&root)).unwrap();
    let connection = connections
        .save(grouped_connection("protected.example"))
        .unwrap();
    let reference = ConnectionRef::from(connections.get(connection).unwrap());
    let key = kortty_ssh::HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
    )
    .unwrap();
    let pins = [(Endpoint::new("protected.example", 22).unwrap(), key)];

    let parent = local_tree::Directory::open_path(&root).unwrap();
    let workspace =
        local_tree::create_reviewed_tree_stage(&parent, OsStr::new("workspace")).unwrap();
    let name = OsStr::new("receiver.txt");
    let mut file = local_tree::create_reviewed_tree_file(&workspace, name).unwrap();
    file.write_all(b"review this original").unwrap();
    drop(file);
    drop(workspace);
    let local = root.join("workspace").join(name);
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": reference,
        "remote": {"kind":"download", "local":local, "remote":"/protected/receiver.txt"}
    }))
    .unwrap();
    let id = JobId::new("protected-replacement").unwrap();
    let graph = JobGraph::new(vec![
        Job::new(id.clone(), "Protected download", action).unwrap(),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    {
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        app.update(Message::Page(Page::Action));
        app.update(Message::Sudo(SudoMode::Passwordless));
        app.update(Message::PrivilegedReplaceExisting(true));
        let preview = app
            .runtime
            .review_privileged(&mut app.view, |_| Ok(sync::fingerprints(&pins)))
            .unwrap()
            .unwrap();
        app.update(Message::ApprovePrivileged);
        app.update(Message::Save);
        assert_ne!(app.status(), Some("Job saved"));

        let reviewed = iced::futures::executor::block_on(preview.inspect(None));
        app.update(Message::PrivilegedReplacementPreviewed(reviewed));
        app.update(Message::ApprovePrivileged);
        app.update(Message::Save);
        assert_eq!(app.status(), Some("Job saved"));
    }
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id));
    let request = app.update(Message::Run(false)).pop().unwrap();
    let context = app.runtime.privileged_run_context(&request).unwrap();
    let RemoteAction::Privileged(transfer) = request.action.remote_action().unwrap() else {
        panic!("Expected a reviewed privileged download");
    };
    assert!(transfer.replace_existing);
    assert!(context.saved_route_current(&reference));
    let receipt = context
        .authorize(transfer, &reference, SudoMode::Passwordless, &pins)
        .unwrap();
    let destination = ReviewedFileReceiver::inspect(&local, &|| false).unwrap();
    assert_eq!(receipt.proof(), destination.fingerprint);
    let mut legacy = transfer.clone();
    legacy.replace_existing = false;
    assert!(
        context
            .authorize(&legacy, &reference, SudoMode::Passwordless, &pins)
            .is_err()
    );
    context.claim_replacement(&receipt, 0, &|| false).unwrap();
    context.require_claimed(&receipt).unwrap();
    assert!(context.claim_replacement(&receipt, 0, &|| false).is_err());
    assert!(
        context
            .authorize(transfer, &reference, SudoMode::Passwordless, &pins)
            .is_err()
    );
}

#[test]
fn recurring_protected_replacement_editor_admits_each_fresh_chain_round() {
    use std::ffi::OsStr;
    use std::io::Write as _;

    use kortty_server_archive::rsync_tree::Manifest;
    use kortty_ssh::HostKey;
    use serde_json::json;
    use sha2::{Digest as _, Sha256};

    use crate::sftp_window::{ReviewedFilePublication, ReviewedFileReceiver, local_tree};

    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = root.join("scheduler.json");
    let parent = local_tree::Directory::open_path(&root).unwrap();
    let workspace =
        local_tree::create_reviewed_tree_stage(&parent, OsStr::new("workspace")).unwrap();
    let name = OsStr::new("receiver");
    let mut original = local_tree::create_reviewed_tree_file(&workspace, name).unwrap();
    original.write_all(b"original file").unwrap();
    drop(original);
    drop(workspace);
    let local = root.join("workspace").join(name);
    let id = JobId::new("recurring-protected-download").unwrap();
    let predecessor = JobId::new("prepare-protected-download").unwrap();
    let connection =
        json!({"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}});
    let prepare: SshCommand =
        serde_json::from_value(json!({"connection": connection, "command": "true"})).unwrap();
    let transfer: SshCommand = serde_json::from_value(json!({
        "connection": connection,
        "remote": {"kind":"download","local":local,"remote":"/approved/file"},
    }))
    .unwrap();
    let graph = JobGraph::new(vec![
        Job::new(predecessor.clone(), "Prepare", prepare).unwrap(),
        Job::new(id.clone(), "Replace file", transfer)
            .unwrap()
            .with_dependencies(DependencyMode::Chain, vec![predecessor.clone()], None),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    let pins = [(
        Endpoint::new("server.example", 22).unwrap(),
        HostKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
        )
        .unwrap(),
    )];
    let open = || {
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.zone = TimeZone::UTC;
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        app.update(Message::Page(Page::Action));
        app
    };
    let review = |app: &mut TestWindow| {
        let preview = app
            .runtime
            .review_privileged(&mut app.view, |_| Ok(sync::fingerprints(&pins)))
            .unwrap()
            .unwrap();
        let inspected = iced::futures::executor::block_on(preview.inspect(None));
        app.update(Message::PrivilegedReplacementPreviewed(inspected));
    };
    let mut app = open();
    app.update(Message::Sudo(SudoMode::Vault));
    app.update(Message::PrivilegedReplaceExisting(true));
    app.update(Message::PrivilegedStanding(true));
    review(&mut app);
    app.update(Message::Edit(Field::SyncStandingFirings, "2".into()));
    app.update(Message::ApprovePrivileged);
    let status = app.status().unwrap();
    assert!(status.contains("action changed"), "{status}");
    review(&mut app);
    app.update(Message::ApprovePrivileged);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    drop(app);

    let mut app = open();
    assert!(app.update(Message::Run(false)).is_empty());
    assert!(app.status().unwrap().contains("approved calendar or Chain"));
    let complete = |id| {
        Message::Finished(Completion {
            id,
            outcome: RunOutcome::Success,
            detail: None,
            output: None,
            recording: None,
            swarm_report: None,
        })
    };
    let mut last = b"original file".as_slice();
    for payload in [b"first replacement".as_slice(), b"second replacement"] {
        app.update(Message::Select(predecessor.clone()));
        let prepare = app.update(Message::Run(false)).pop().unwrap();
        let request = app.update(complete(prepare.id)).pop().unwrap();
        assert_eq!(request.job, id);
        assert_eq!(request.trigger, RunTrigger::Dependency);
        let context = app.runtime.privileged_run_context(&request).unwrap();
        let fake = RunRequest {
            id: request.id,
            job: request.job.clone(),
            action: Arc::clone(&request.action),
            trigger: RunTrigger::Manual,
            started_at: request.started_at,
            cancellation: request.cancellation.clone(),
        };
        assert!(app.runtime.privileged_run_context(&fake).is_none());
        let action = request.action.as_ref();
        let reference = action.connection.as_ref().unwrap();
        let Some(RemoteAction::Privileged(transfer)) = &action.remote else {
            panic!("expected protected download")
        };
        let receipt = context
            .authorize(transfer, reference, action.sudo, &pins)
            .unwrap();
        let receiver = ReviewedFileReceiver::open_transfer(&local, &|| false).unwrap();
        assert_eq!(receipt.proof(), receiver.fingerprint());
        let stage = receiver.stage().unwrap();
        stage.writer().unwrap().write_all(payload).unwrap();
        let bytes = payload.len() as u64;
        let publication = receiver
            .publish(
                stage,
                Manifest::regular_file(bytes, Sha256::digest(payload).into()),
                &|| false,
                &|| Ok(()),
                |_| context.claim_replacement(&receipt, bytes, &|| false),
            )
            .unwrap();
        let ReviewedFilePublication::Published {
            successor,
            recovery,
        } = publication
        else {
            panic!("expected the held successor")
        };
        assert_eq!(fs::read(recovery).unwrap(), last);
        context.require_claimed(&receipt).unwrap();
        context
            .finish_replacement(&receipt, Some(successor.fingerprint))
            .unwrap();
        assert_eq!(fs::read(&local).unwrap(), payload);
        last = payload;
        assert!(app.update(complete(request.id)).is_empty());
    }
    app.update(Message::Select(predecessor));
    let prepare = app.update(Message::Run(false)).pop().unwrap();
    let next = app.update(complete(prepare.id)).pop().unwrap();
    let context = app.runtime.privileged_run_context(&next).unwrap();
    let action = next.action.as_ref();
    let Some(RemoteAction::Privileged(transfer)) = &action.remote else {
        panic!("expected protected download")
    };
    assert!(
        context
            .authorize(
                transfer,
                action.connection.as_ref().unwrap(),
                action.sudo,
                &pins
            )
            .is_err()
    );
}

#[test]
fn privileged_file_action_review_persists_and_binds_the_typed_operation() {
    use crate::sftp_window::privileged_file::ProtectedOperation;
    use kortty_ssh::HostKey;
    use serde_json::json;

    let profile = Profile::temporary();
    let path = profile.0.join("scheduler.json");
    let id = JobId::new("privileged-metadata").unwrap();
    let connection = json!({
        "id": 7,
        "target": {"host": "server.example", "port": 22, "username": "operator"}
    });
    let key = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
    )
    .unwrap();
    let pins = [(Endpoint::new("server.example", 22).unwrap(), key)];
    for operation in [
        json!({"kind": "mkdir"}),
        json!({"kind": "chmod", "mode": 416}),
        json!({"kind": "chmod_symbolic", "mode": "g=u,o-r"}),
        json!({"kind": "chown", "uid": null, "gid": 42}),
        json!({"kind": "copy", "to": "/approved/copied"}),
        json!({"kind": "copy_tree", "to": "/approved/copied-tree"}),
        json!({"kind": "rename", "to": "/approved/renamed"}),
        json!({"kind": "delete", "recursive": false}),
        json!({"kind": "delete", "recursive": true}),
    ] {
        let action: SshCommand = serde_json::from_value(json!({
            "connection": connection,
            "sudo": "passwordless",
            "remote": {
                "kind": "privileged_file_action",
                "path": "/approved/target",
                "operation": operation
            }
        }))
        .unwrap();
        let action = action.validated().unwrap();
        assert!(!action.remote_action().unwrap().transfers_files());
        let job = Job::new(id.clone(), "Privileged metadata fixture", action.clone()).unwrap();
        let mut draft = Draft::from_job(&job);
        assert_eq!(draft.build(id.clone()).unwrap().action(), &action);
        if operation["kind"] == "copy_tree" {
            draft.set_privileged_copy_tree(false);
            let changed = draft.build(id.clone()).unwrap();
            assert!(changed.action().remote_action() != action.remote_action());
            draft.set_privileged_copy_tree(true);
            draft.set(Field::RemoteDestination, "/approved/target/child".into());
            assert!(draft.build(id.clone()).is_err());
        }
        if operation["kind"] == "delete" {
            draft.set(Field::RemotePath, "/approved/changed".into());
            assert!(draft.build(id.clone()).is_err());
            draft.set(Field::RemoteDeleteConfirmation, "/approved/changed".into());
            assert!(draft.build(id.clone()).is_ok());
        }
    }
    let ordinary: SshCommand = serde_json::from_value(json!({
        "connection": connection,
        "remote": {"kind": "chmod", "path": "/approved/target", "mode": 416}
    }))
    .unwrap();
    let graph = JobGraph::new(vec![
        Job::new(id.clone(), "Metadata fixture", ordinary).unwrap(),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    {
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        app.update(Message::Page(Page::Action));
        app.update(Message::Sudo(SudoMode::Passwordless));
        app.update(Message::Save);
        assert!(
            app.status()
                .is_some_and(|status| status.contains("Review the exact"))
        );
        app.runtime
            .review_privileged(&mut app.view, |_| Ok(sync::fingerprints(&pins)))
            .unwrap();
        app.update(Message::Edit(Field::RemoteMode, "g=u,o-r".into()));
        app.update(Message::ApprovePrivileged);
        assert!(
            app.status()
                .is_some_and(|status| status.contains("changed"))
        );
        app.runtime
            .review_privileged(&mut app.view, |_| Ok(sync::fingerprints(&pins)))
            .unwrap();
        app.update(Message::ApprovePrivileged);
        app.update(Message::Save);
        assert_eq!(app.status(), Some("Job saved"));
    }
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id));
    let request = app.update(Message::Run(false)).pop().unwrap();
    let context = app.runtime.privileged_run_context(&request).unwrap();
    let RemoteAction::PrivilegedFileAction(metadata) = request.action.remote_action().unwrap()
    else {
        panic!("expected the reviewed privileged file action");
    };
    assert_eq!(
        metadata.operation,
        ProtectedOperation::ChmodSymbolic {
            mode: SymbolicMode::try_from("g=u,o-r".to_owned()).unwrap()
        }
    );
    assert!(metadata.approval.is_some());
    let route = request.action.connection.as_ref().unwrap();
    assert!(
        context
            .authorize_file_action(metadata, route, SudoMode::Passwordless, &pins)
            .is_ok()
    );
    assert!(
        context
            .authorize_file_action(metadata, route, SudoMode::Vault, &pins)
            .is_err()
    );
    assert!(
        context
            .authorize_file_action(metadata, route, SudoMode::Passwordless, &[])
            .is_err()
    );
    let mut changed = metadata.clone();
    changed.operation = ProtectedOperation::ChmodSymbolic {
        mode: SymbolicMode::try_from("g=u,o+x".to_owned()).unwrap(),
    };
    assert!(
        context
            .authorize_file_action(&changed, route, SudoMode::Passwordless, &pins)
            .is_err()
    );
    changed.operation = ProtectedOperation::Chmod { mode: 0o4600 };
    assert!(changed.validate().is_err());
    let mut edited = Draft::from_job(app.selected_job().unwrap());
    edited.select_source(Source::Command);
    assert_eq!(edited.sudo, SudoMode::Disabled);
}

#[test]
fn privileged_archive_review_binds_saved_spec_route_and_pins() {
    use kortty_server_archive::protocol::ArchiveMemberLayout;
    use kortty_ssh::HostKey;
    use serde_json::json;

    let profile = Profile::temporary();
    let path = profile.file();
    let id = JobId::new("privileged-archive").unwrap();
    let command: SshCommand = serde_json::from_value(json!({
        "connection": {
            "id": 7,
            "target": {"host": "server.example", "port": 22, "username": "operator"}
        },
        "remote": {
            "kind": "server_archive",
            "sources": ["/protected/logs", "/protected/config"],
            "target": "/backups/snapshot.tar.bz2",
            "format": "tar_bz2",
            "level": 6,
            "excludes": ["*.tmp"]
        }
    }))
    .unwrap();
    let job = Job::new(id.clone(), "Protected archive", command).unwrap();
    let graph = JobGraph::new(vec![job]).unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    let key = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
    )
    .unwrap();
    let pins = [(Endpoint::new("server.example", 22).unwrap(), key)];
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    app.update(Message::Page(Page::Action));
    assert!(app.view.editing_privileged_file_action());
    app.update(Message::Sudo(SudoMode::Vault));
    assert_eq!(app.view.draft.sudo, SudoMode::Vault);
    app.update(Message::Sudo(SudoMode::Passwordless));
    app.update(Message::Save);
    assert!(
        app.status()
            .is_some_and(|status| status.contains("Review the exact"))
    );
    app.runtime
        .review_privileged(&mut app.view, |_| Ok(sync::fingerprints(&pins)))
        .unwrap();
    app.update(Message::Edit(Field::ArchiveLevel, "7".into()));
    app.update(Message::ApprovePrivileged);
    assert!(
        app.status()
            .is_some_and(|status| status.contains("changed"))
    );
    app.runtime
        .review_privileged(&mut app.view, |_| Ok(sync::fingerprints(&pins)))
        .unwrap();
    app.update(Message::ApprovePrivileged);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    drop(app);

    let mut reopened = TestWindow::open(path.clone()).unwrap();
    reopened.runtime.set_execution_allowed(true);
    reopened.update(Message::Select(id.clone()));
    let request = reopened.update(Message::Run(false)).pop().unwrap();
    let context = reopened.runtime.privileged_run_context(&request).unwrap();
    let route = request.action.connection.as_ref().unwrap();
    let RemoteAction::PrivilegedArchive(archive) = request.action.remote_action().unwrap() else {
        panic!("expected a protected archive");
    };
    assert_eq!(archive.archive.level, 7);
    assert!(!archive.archive.reuse_output);
    assert_eq!(
        archive.archive.member_layout,
        ArchiveMemberLayout::SourceBasename
    );
    assert!(!request.action.remote_action().unwrap().transfers_files());
    assert!(
        context
            .authorize_archive(archive, route, SudoMode::Passwordless, &pins)
            .is_ok()
    );
    assert!(
        context
            .authorize_archive(archive, route, SudoMode::Vault, &pins)
            .is_err()
    );
    assert!(
        context
            .authorize_archive(archive, route, SudoMode::Passwordless, &[])
            .is_err()
    );
    let mut other_route = route.clone();
    other_route.id += 1;
    assert!(
        context
            .authorize_archive(archive, &other_route, SudoMode::Passwordless, &pins)
            .is_err()
    );
    let mut reuse_with_legacy_approval = archive.clone();
    reuse_with_legacy_approval.archive.reuse_output = true;
    assert!(reuse_with_legacy_approval.validate().is_ok());
    assert!(
        context
            .authorize_archive(
                &reuse_with_legacy_approval,
                route,
                SudoMode::Passwordless,
                &pins
            )
            .is_err()
    );
    let mut changed = archive.clone();
    changed.archive.sources.swap(0, 1);
    assert!(
        context
            .authorize_archive(&changed, route, SudoMode::Passwordless, &pins)
            .is_err()
    );
    changed.archive.target = "/protected/logs/snapshot.tar.bz2"
        .to_owned()
        .try_into()
        .unwrap();
    assert!(changed.validate().is_err());

    let mut draft = Draft::from_job(reopened.selected_job().unwrap());
    assert_eq!(
        draft.build(id.clone()).unwrap().action(),
        request.action.as_ref()
    );
    assert!(draft.select_archive_format(ArchiveFormat::ZipPassword));
    assert!(draft.select_archive_format(ArchiveFormat::TarBz2));
    assert!(draft.set_archive_server_only(false));
    assert_eq!(draft.sudo, SudoMode::Disabled);
    assert!(matches!(
        draft.build(id.clone()).unwrap().action().remote_action(),
        Ok(RemoteAction::Archive(_))
    ));

    drop(reopened);
    let mut reopened = TestWindow::open(path.clone()).unwrap();
    reopened.runtime.set_execution_allowed(true);
    reopened.update(Message::Select(id.clone()));
    reopened.update(Message::Page(Page::Action));
    reopened.update(Message::Sudo(SudoMode::Vault));
    reopened.update(Message::Save);
    assert!(
        reopened
            .status()
            .is_some_and(|status| status.contains("Review the exact"))
    );
    reopened.update(Message::ArchiveFormat(ArchiveFormat::ZipPassword));
    reopened.update(Message::Edit(
        Field::RemotePath,
        "/backups/snapshot.zip".into(),
    ));
    let password = |digit: &str| CredentialChoice {
        id: digit.repeat(32),
        label: "Archive password".into(),
        username: String::new(),
    };
    let original = password("a");
    let replacement = password("b");
    let local_copy = profile.0.join("protected copy.zip");
    reopened.update(Message::Edit(
        Field::ArchiveDownload,
        local_copy.to_str().unwrap().into(),
    ));
    reopened
        .runtime
        .set_archive_passwords(vec![original.clone(), replacement.clone()]);
    reopened.update(Message::ArchivePassword(original.clone()));
    reopened
        .runtime
        .review_privileged(&mut reopened.view, |_| Ok(sync::fingerprints(&pins)))
        .unwrap();
    reopened.update(Message::ApprovePrivileged);
    reopened.update(Message::ArchiveReuseOutput(true));
    reopened.update(Message::Save);
    assert!(
        reopened
            .status()
            .is_some_and(|status| status.contains("Review the exact"))
    );
    reopened.update(Message::ArchiveServerPath(true));
    reopened
        .runtime
        .review_privileged(&mut reopened.view, |_| Ok(sync::fingerprints(&pins)))
        .unwrap();
    reopened.update(Message::ArchivePassword(replacement.clone()));
    reopened.update(Message::ApprovePrivileged);
    assert!(
        reopened
            .status()
            .is_some_and(|status| status.contains("changed"))
    );
    reopened
        .runtime
        .review_privileged(&mut reopened.view, |_| Ok(sync::fingerprints(&pins)))
        .unwrap();
    reopened.update(Message::ApprovePrivileged);
    reopened.update(Message::Save);
    assert_eq!(reopened.status(), Some("Job saved"));
    drop(reopened);
    let mut reopened = TestWindow::open(path).unwrap();
    reopened.runtime.set_execution_allowed(true);
    reopened.update(Message::Select(id));
    let encrypted = reopened.update(Message::Run(false)).pop().unwrap();
    let RemoteAction::PrivilegedArchive(archive) = encrypted.action.remote_action().unwrap() else {
        panic!("expected a protected password archive");
    };
    let pinned = encrypted.action.connection.as_ref().unwrap();
    let review = reopened.runtime.privileged_run_context(&encrypted).unwrap();
    let remote = encrypted.action.remote_action().unwrap();
    assert_eq!(encrypted.action.sudo, SudoMode::Vault);
    assert!(archive.archive.reuse_output);
    assert_eq!(
        archive.archive.member_layout,
        ArchiveMemberLayout::ServerPath
    );
    assert!(remote.transfers_files() && remote.writes_local_file());
    assert_eq!(
        archive.archive.download.as_deref(),
        Some(local_copy.as_path())
    );
    assert!(archive.archive.server_spec().unwrap().report_payload);
    assert_eq!(
        archive.archive.password_credential.as_ref(),
        Some(&replacement.id)
    );
    assert!(
        review
            .authorize_archive(archive, pinned, SudoMode::Vault, &pins)
            .is_ok()
    );
    let mut legacy_with_reuse_approval = archive.clone();
    legacy_with_reuse_approval.archive.reuse_output = false;
    assert!(
        review
            .authorize_archive(&legacy_with_reuse_approval, pinned, SudoMode::Vault, &pins)
            .is_err()
    );
    let mut changed_layout = archive.clone();
    changed_layout.archive.member_layout = ArchiveMemberLayout::SourceBasename;
    assert!(
        review
            .authorize_archive(&changed_layout, pinned, SudoMode::Vault, &pins)
            .is_err()
    );
    assert!(
        review
            .authorize_archive(archive, pinned, SudoMode::Passwordless, &pins)
            .is_err()
    );
    let mut changed_password = archive.clone();
    changed_password.archive.password_credential = Some(original.id);
    assert!(
        review
            .authorize_archive(&changed_password, pinned, SudoMode::Vault, &pins)
            .is_err()
    );
    let mut changed_copy = archive.clone();
    changed_copy.archive.download = Some(profile.0.join("different copy.zip"));
    assert!(
        review
            .authorize_archive(&changed_copy, pinned, SudoMode::Vault, &pins)
            .is_err()
    );

    let mut draft = Draft::from_job(reopened.selected_job().unwrap());
    assert!(draft.select_archive_format(ArchiveFormat::Tar));
    assert!(!draft.set_archive_server_path(true));
    assert!(draft.set_archive_server_path(false));
    assert!(draft.set_archive_server_path(true));
    assert!(draft.select_archive_format(ArchiveFormat::TarBz2));
    assert!(!draft.set_archive_server_path(true));
    assert!(draft.select_archive_format(ArchiveFormat::SevenZip));
    assert!(!draft.set_archive_server_path(true));
    assert!(draft.select_archive_format(ArchiveFormat::ZipPassword));
    assert!(draft.set_archive_server_path(true));
    assert!(draft.set_archive_server_only(false));
    assert!(!draft.set_archive_server_path(true));
}

#[test]
fn scheduled_agent_editor_saves_exact_probe_approval_and_invalidates_edits() {
    check_scheduled_agent_editor_saves_exact_probe_approval(ProfileRef::Numeric(7));
}

#[test]
fn scheduled_agent_editor_preserves_exact_named_approval_on_reopen() {
    check_scheduled_agent_editor_saves_exact_probe_approval(ProfileRef::Policy(
        "policy-Diagnostics".into(),
    ));
}

fn check_scheduled_agent_editor_saves_exact_probe_approval(profile_ref: ProfileRef) {
    use crate::connection_store::{Auth, Connection, SavedLogin};
    use crate::ssh_dialog::LoginTarget;
    use kortty_ssh::{HostKey, TrustStore};

    let scratch = Profile::temporary();
    let path = scratch.file();
    let id = JobId::new("diagnostics").unwrap();
    let connection = Connection {
        id: 1,
        name: "Diagnostic target".into(),
        tag: String::new(),
        login: SavedLogin {
            preferences: Default::default(),
            endpoint: LoginTarget {
                host: "example.test".into(),
                port: 22,
                username: "operator".into(),
            },
            auth: Auth::VaultPassword("fixture-vault-reference".into()),
            keyboard_interactive: false,
        },
        jump: None,
        emulation: "xterm-256color".into(),
        tunnels: Vec::new(),
        incognito: false,
        isolation: Default::default(),
        inherit_isolation: false,
        startup: Default::default(),
        logging: Default::default(),
        terminal_effect: None,
        command_timestamps: None,
        tab_color: None,
        paste_warning: None,
        window_geometry: None,
        terminal: Default::default(),
    };
    let reference = ConnectionRef::from(&connection);
    let host = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
    )
    .unwrap();
    let trust =
        TrustStore::from_pins(vec![(Endpoint::new("example.test", 22).unwrap(), host)]).unwrap();
    let target = agent::Target::capture(&reference, &connection, &trust).unwrap();
    let mut changed = connection.clone();
    changed.terminal.ai_agent_disabled = true;
    assert!(agent::Target::capture(&reference, &changed, &trust).is_err());
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": reference,
        "working_directory": "/srv",
        "agent": {"kind":"agent","probe":"system","profile":profile_ref,"goal":"Summarize the system"}
    }))
    .unwrap();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph =
            JobGraph::new(vec![Job::new(id.clone(), "Diagnostics", action).unwrap()]).unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    app.update(Message::Page(Page::Action));
    app.update(Message::AutomationJournal(
        automation_journal::Event::Enabled(true),
    ));
    app.update(Message::AutomationJournal(
        automation_journal::Event::Input(true),
    ));
    let review = app.runtime.begin_agent_review(&app.view).unwrap();
    let profile = agent::ProfileChoice {
        id: profile_ref.clone(),
        name: "Local".into(),
        model: "fixture".into(),
        destination: "127.0.0.1:11434".into(),
        policy_model_fingerprint: matches!(profile_ref, ProfileRef::Policy(_)).then_some([7; 32]),
        reviewed_https: false,
        reviewed_cli: None,
    };
    app.runtime
        .finish_agent_review(&mut app.view, review, profile, vec![target])
        .unwrap();
    app.update(Message::ApproveAgent);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    drop(app);

    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    let request = app.update(Message::Run(false)).pop().unwrap();
    let action = request.action.agent.as_ref().unwrap();
    assert_eq!(action.profile, profile_ref);
    assert_eq!(action.approval.as_ref().unwrap().profile.id, profile_ref);
    let journal = request.action.automation_journal.as_ref().unwrap();
    assert!(journal.enabled && journal.capture_input);
    assert!(app.runtime.agent_run_approved(&request));
    assert!(app.runtime.active_agent_run_approved(&request));
    assert!(app.runtime.cancel_menu_run(request.id.value()));
    assert!(!app.runtime.active_agent_run_approved(&request));
    drop(app);
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id));
    app.update(Message::AgentProbe(agent::Probe::Filesystem));
    app.update(Message::Save);
    let request = app.update(Message::Run(false)).pop().unwrap();
    assert!(!app.runtime.agent_run_approved(&request));
}

#[test]
fn rsync_program_approval_is_bound_to_the_saved_editor_executable_and_route() {
    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = root.join("scheduler.json");
    let id = JobId::new("approved-rsync").unwrap();
    let program = root.join("rsync");
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
        "remote": {"kind": "rsync_download", "program": program, "local": root.join("new-file"), "remote": "/reviewed"}
    })).unwrap();
    let approval = rsync::Approval {
        program,
        digest: [8; 32],
        reference: action.connection.clone().unwrap(),
        pins: vec!["server.example SHA256:fixture".into()],
        upload: None,
        directory: None,
        directory_upload: None,
        existing: None,
        existing_download: None,
        vacant_directory_download: None,
        existing_file_download: None,
        vacant_file_download: None,
        mixed: None,
        batch: None,
        download: None,
        privileged: None,
        ordinary_receiver: None,
    };
    let Some(RemoteAction::RsyncDownload(transfer)) = &action.remote else {
        panic!("expected rsync download");
    };
    let mut reviewed = transfer.clone();
    reviewed.approval = Some(approval);
    let reviewed = rsync::Reviewed::restore(&reviewed).unwrap();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![
            Job::new(id.clone(), "Rsync download", action).unwrap(),
        ])
        .unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    app.update(Message::Page(Page::Action));
    app.view.rsync_review.begin("first".into(), None);
    app.update(Message::RsyncProgramReviewed(
        "first".into(),
        Ok(Box::new(reviewed)),
    ));
    app.update(Message::ApproveRsyncProgram);
    app.update(Message::Save);
    let action = app.selected_job().unwrap().action();
    let RemoteAction::RsyncDownload(download) = action.remote_action().unwrap() else {
        panic!("expected rsync download");
    };
    assert_eq!(download.approval.as_ref().unwrap().digest, [8; 32]);
    drop(app);

    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id));
    app.update(Message::Edit(
        Field::RsyncProgram,
        root.join("different").to_str().unwrap().into(),
    ));
    app.update(Message::Save);
    let request = app.update(Message::Run(false)).pop().unwrap();
    let RemoteAction::RsyncDownload(download) = request.action.remote_action().unwrap() else {
        panic!("expected rsync download");
    };
    assert!(download.approval.is_none());
    assert!(request.action.remote_action().unwrap().transfers_files());
}

#[test]
fn journal_open_job_selects_the_saved_job_without_discarding_another_draft() {
    let profile = Profile::temporary();
    let path = profile.file();
    let first = JobId::new("journal-first").unwrap();
    let second = JobId::new("journal-second").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "server.example",
        "port": 22,
        "username": "operator",
        "key_file": profile.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![
            Job::new(first.clone(), "First job", action.clone()).unwrap(),
            Job::new(second.clone(), "Second job", action).unwrap(),
        ])
        .unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    let mut runtime = SchedulerWindow::open(path).unwrap();
    let mut view = runtime.new_view();
    view.page = Page::Journal;
    runtime.select_journal_job(&mut view, &second);
    assert_eq!(view.draft.id.as_ref(), Some(&second));
    assert_eq!(view.page, Page::Job);
    runtime.update_view(&mut view, Message::Edit(Field::Name, "Unsaved name".into()));
    runtime.select_journal_job(&mut view, &first);
    assert_eq!(view.draft.id.as_ref(), Some(&second));
    assert!(view.dirty());
    assert_eq!(view.draft.build(second).unwrap().name(), "Unsaved name");
}

#[test]
fn scheduler_views_keep_independent_drafts_and_share_job_execution() {
    let profile = Profile::temporary();
    let path = profile.file();
    let id = JobId::new("shared-job").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "server.example",
        "port": 22,
        "username": "operator",
        "key_file": profile.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![Job::new(id.clone(), "Original", action).unwrap()]).unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    let mut runtime = SchedulerWindow::open(path.clone()).unwrap();
    runtime.set_execution_allowed(true);
    let mut first = runtime.new_view();
    let mut second = runtime.new_view();
    runtime.update_view(&mut first, Message::Edit(Field::Name, "First edit".into()));
    runtime.update_view(
        &mut second,
        Message::Edit(Field::Name, "Second edit".into()),
    );
    runtime.update_view(&mut first, Message::Save);
    assert_eq!(runtime.selected_job(&first).unwrap().name(), "First edit");
    runtime.sync_view(&mut second);
    assert_eq!(second.draft.build(id).unwrap().name(), "Second edit");
    assert!(second.dirty());

    runtime.update_view(&mut second, Message::Save);
    assert!(
        second
            .status
            .as_deref()
            .unwrap()
            .contains("changed in another view")
    );
    assert_eq!(runtime.selected_job(&second).unwrap().name(), "First edit");
    assert!(second.dirty());
    assert!(
        runtime
            .update_view(&mut second, Message::Run(false))
            .is_empty()
    );
    assert!(
        second
            .status
            .as_deref()
            .unwrap()
            .contains("changed in another view")
    );
    let runs = runtime.update_view(&mut first, Message::Run(false));
    assert_eq!(runs.len(), 1);
    let run = runs[0].id;
    drop(first);
    drop(second);
    // Test-only worker completion. Runtime scheduling and journal persistence are real.
    assert!(
        runtime
            .update_runtime(Message::Finished(Completion {
                id: run,
                outcome: RunOutcome::Success,
                detail: None,
                recording: None,
                output: None,
                swarm_report: None,
            }))
            .is_empty()
    );
    assert!(!runtime.has_active_runs());
    assert_eq!(runtime.store.journal().len(), 1);
    assert_eq!(runtime.store.journal()[0].id, run);
    drop(runtime);

    let restored = SchedulerWindow::open(path).unwrap();
    assert_eq!(restored.store.journal().len(), 1);
    assert_eq!(
        restored.selected_job(&restored.new_view()).unwrap().name(),
        "First edit"
    );
}

#[test]
fn scheduler_run_saves_a_new_draft_before_dispatch() {
    let profile = Profile::temporary();
    let path = profile.file();
    let mut window = TestWindow::open(path.clone()).unwrap();
    for (field, value) in [
        (Field::Name, "Maintenance".to_owned()),
        (Field::Host, "server.example".to_owned()),
        (Field::Username, "operator".to_owned()),
        (
            Field::KeyFile,
            profile.0.join("fixture-key").display().to_string(),
        ),
    ] {
        window.update(Message::Edit(field, value));
    }
    window.update(Message::EditCommand(text_editor::Action::Edit(
        text_editor::Edit::Paste(std::sync::Arc::new("printf maintenance".into())),
    )));

    assert!(window.update(Message::Run(false)).is_empty());
    assert!(window.runtime.store.definitions().is_empty());
    assert!(window.view.dirty());

    window.runtime.set_execution_allowed(true);
    let request = window.update(Message::Run(false)).pop().unwrap();
    assert_eq!(request.trigger, RunTrigger::Manual);
    assert_eq!(request.action.command, "printf maintenance");
    assert_eq!(window.view.draft.id.as_ref(), Some(&request.job));
    assert!(!window.view.dirty());
    drop(window);

    let restored = TestWindow::open(path).unwrap();
    let job = restored.selected_job().unwrap();
    assert_eq!(job.id(), &request.job);
    assert_eq!(job.name(), "Maintenance");
    assert_eq!(job.action(), request.action.as_ref());
}

#[test]
fn scheduler_run_saves_the_draft_before_checking_cascade_dependencies() {
    let profile = Profile::temporary();
    let path = profile.file();
    let first = JobId::new("first").unwrap();
    let predecessor = JobId::new("external-predecessor").unwrap();
    let successor = JobId::new("successor").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "server.example",
        "port": 22,
        "username": "operator",
        "key_file": profile.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![
            Job::new(first.clone(), "First job", action.clone()).unwrap(),
            Job::new(predecessor.clone(), "External predecessor", action.clone()).unwrap(),
            Job::new(successor.clone(), "Successor", action)
                .unwrap()
                .with_dependencies(DependencyMode::Gate, vec![first.clone()], None),
        ])
        .unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }

    let mut window = TestWindow::open(path).unwrap();
    window.runtime.set_execution_allowed(true);
    window.update(Message::Select(first.clone()));
    window.update(Message::Dependency(predecessor.clone(), true));
    window.update(Message::Edit(Field::WorkingDirectory, "/updated".into()));
    assert!(window.update(Message::Run(true)).is_empty());
    assert!(!window.runtime.has_active_runs());
    assert!(!window.view.dirty());
    assert!(matches!(
        &window.view.confirm,
        Some(Confirmation::Run(job, true, blockers))
            if job == &first && blockers.iter().any(|blocker| blocker.predecessor == predecessor)
    ));
    let saved = window
        .runtime
        .store
        .definitions()
        .iter()
        .find(|definition| definition.id == first)
        .unwrap();
    assert_eq!(
        saved.dependencies.as_slice(),
        std::slice::from_ref(&predecessor)
    );

    let mut predecessor_view = window.runtime.new_view();
    window
        .runtime
        .update_view(&mut predecessor_view, Message::Select(predecessor.clone()));
    let predecessor_run = window
        .runtime
        .update_view(&mut predecessor_view, Message::Run(false))
        .pop()
        .unwrap();
    let failed = RunOutcome::Failed { exit_code: Some(1) };
    assert!(
        window
            .update(Message::Finished(Completion {
                id: predecessor_run.id,
                outcome: failed,
                detail: None,
                recording: None,
                output: None,
                swarm_report: None,
            }))
            .is_empty()
    );
    assert!(window.update(Message::Confirm).is_empty());
    assert!(matches!(
        &window.view.confirm,
        Some(Confirmation::Run(job, true, blockers))
            if job == &first && blockers.iter().any(|blocker| blocker.predecessor == predecessor
                && blocker.reason == kortty_jobs::BlockReason::NotSuccessful(failed))
    ));

    let request = window.update(Message::Confirm).pop().unwrap();
    assert_eq!(request.job, first);
    assert_eq!(request.trigger, RunTrigger::Cascade);
    assert_eq!(
        request.action.working_directory.as_deref(),
        Some("/updated")
    );
    let next = window
        .update(Message::Finished(Completion {
            id: request.id,
            outcome: RunOutcome::Success,
            detail: None,
            recording: None,
            output: None,
            swarm_report: None,
        }))
        .pop()
        .unwrap();
    assert_eq!(next.job, successor);
}

#[test]
fn scheduler_run_saves_nothing_and_dispatches_nothing_after_a_failed_save() {
    let profile = Profile::temporary();
    let path = profile.file();
    let id = JobId::new("original").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "server.example",
        "port": 22,
        "username": "operator",
        "key_file": profile.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![Job::new(id, "Original", action).unwrap()]).unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }

    let mut window = TestWindow::open(path.clone()).unwrap();
    window.runtime.set_execution_allowed(true);
    window.update(Message::Edit(Field::Name, "Edited".into()));
    window.update(Message::Edit(Field::Port, "invalid".into()));
    assert!(window.update(Message::Run(false)).is_empty());
    assert_eq!(window.status(), Some("Enter a valid SSH port"));

    window.update(Message::Edit(Field::Port, "22".into()));
    let backup = profile.0.join("previous.json");
    fs::rename(&path, &backup).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(window.update(Message::Run(true)).is_empty());
    assert!(window.view.dirty());
    assert!(!window.runtime.has_active_runs());
    assert_eq!(window.selected_job().unwrap().name(), "Original");
    fs::remove_dir(&path).unwrap();
    fs::rename(backup, path).unwrap();

    let request = window.update(Message::Run(true)).pop().unwrap();
    assert_eq!(request.trigger, RunTrigger::Cascade);
    assert_eq!(window.selected_job().unwrap().name(), "Edited");
}

#[test]
fn jobs_menu_cancels_only_the_current_exact_run() {
    let profile = Profile::temporary();
    let path = profile.file();
    let first = JobId::new("menu-first").unwrap();
    let second = JobId::new("menu-second").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "server.example",
        "port": 22,
        "username": "operator",
        "key_file": profile.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![
            Job::new(first.clone(), "Duplicate display name", action.clone()).unwrap(),
            Job::new(second.clone(), "Duplicate display name", action).unwrap(),
        ])
        .unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }

    let mut window = TestWindow::open(path).unwrap();
    let runtime = &mut window.runtime;
    runtime.set_execution_allowed(true);
    let now = SystemTime::now();
    assert!(runtime.menu_status(now, 5).is_none());
    let mut first_view = runtime.new_view();
    runtime.update_view(&mut first_view, Message::Select(first));
    let first_run = runtime
        .update_view(&mut first_view, Message::Run(false))
        .pop()
        .unwrap();
    let mut second_view = runtime.new_view();
    runtime.update_view(&mut second_view, Message::Select(second));
    let second_run = runtime
        .update_view(&mut second_view, Message::Run(false))
        .pop()
        .unwrap();
    let status = runtime.menu_status(now, 5).unwrap();
    assert_eq!(status.running.len(), 2);
    assert!(status.running.iter().all(|run| !run.cancelling));

    assert!(runtime.cancel_menu_run(first_run.id.value()));
    assert!(!runtime.cancel_menu_run(first_run.id.value()));
    assert!(first_run.cancellation.is_requested());
    assert!(!second_run.cancellation.is_requested());
    let status = runtime.menu_status(now, 5).unwrap();
    assert_eq!(
        status.running.iter().filter(|run| run.cancelling).count(),
        1,
    );

    // Test-only executor completion through the real scheduler and journal.
    runtime.update_runtime(Message::Finished(Completion {
        id: first_run.id,
        outcome: RunOutcome::Cancelled,
        detail: None,
        recording: None,
        output: None,
        swarm_report: None,
    }));
    let rerun = runtime
        .update_view(&mut first_view, Message::Run(false))
        .pop()
        .unwrap();
    assert_ne!(rerun.id, first_run.id);
    assert!(!runtime.cancel_menu_run(first_run.id.value()));
    assert!(!rerun.cancellation.is_requested());
    assert!(!second_run.cancellation.is_requested());
}

#[test]
fn jobs_menu_lists_five_enabled_deadlines_in_local_time_order() {
    let profile = Profile::temporary();
    let path = profile.file();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "server.example",
        "port": 22,
        "username": "operator",
        "key_file": profile.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    let mut jobs = (1..=7)
        .rev()
        .map(|hour| {
            let schedule =
                kortty_jobs::JobSchedule::default()
                    .with_fixed_times([format!("{hour:02}:00").parse::<Time>().unwrap()]);
            Job::new(
                JobId::new(format!("hour-{hour}")).unwrap(),
                format!("At hour {hour}"),
                action.clone(),
            )
            .unwrap()
            .with_schedule(schedule)
        })
        .collect::<Vec<_>>();
    let early =
        kortty_jobs::JobSchedule::default().with_fixed_times(["00:30".parse::<Time>().unwrap()]);
    jobs.push(
        Job::new(
            JobId::new("job-disabled").unwrap(),
            "Disabled",
            action.clone(),
        )
        .unwrap()
        .with_schedule(early.clone())
        .with_enabled(false),
    );
    jobs.push(
        Job::new(
            JobId::new("schedule-disabled").unwrap(),
            "Schedule disabled",
            action,
        )
        .unwrap()
        .with_schedule(early.with_enabled(false)),
    );
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        store
            .commit_with_actions(&JobGraph::new(jobs).unwrap(), &[], &[])
            .unwrap();
    }
    let now = SystemTime::UNIX_EPOCH;
    let mut runtime = SchedulerWindow::open(path).unwrap();
    runtime.zone = TimeZone::UTC;
    runtime
        .scheduler
        .initialize_schedules(TimeZone::UTC, now)
        .unwrap();
    runtime.set_execution_allowed(true);
    let status = runtime.menu_status(now, 5).unwrap();
    assert!(status.running.is_empty());
    assert_eq!(
        status
            .upcoming
            .iter()
            .map(|job| job.name)
            .collect::<Vec<_>>(),
        [
            "At hour 1",
            "At hour 2",
            "At hour 3",
            "At hour 4",
            "At hour 5"
        ]
    );
    assert_eq!(status.upcoming[0].local_time, "1970-01-01 01:00:00");
    assert_eq!(status.upcoming[0].due, now + Duration::from_secs(3600));
}

#[test]
fn virtual_terminal_attachment_survives_reopen_and_is_removed_with_its_record() {
    let profile = Profile::temporary();
    let id = JobId::new("virtual").unwrap();
    let path = profile.file();
    let options = virtual_terminal::Options {
        max_frames: 2,
        ..virtual_terminal::Options::default()
    };
    let action = SshCommand {
        host: "example.test".into(),
        port: 22,
        username: "test".into(),
        key_file: profile.0.join("fixture-key"),
        connection: None,
        additional_connections: Vec::new(),
        target_groups: None,
        command: "true".into(),
        working_directory: None,
        script: None,
        snippet: None,
        remote: None,
        agent: None,
        imported_ai: None,
        imported_transfer: None,
        imported_privileged: None,
        sudo: SudoMode::Disabled,
        virtual_terminal: Some(options),
        journal_detail: JournalDetail::Off,
        automation_journal: None,
        desktop_notices: Default::default(),
        webhook_targets: Vec::new(),
    };
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![Job::new(id, "Virtual terminal", action).unwrap()]).unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }

    let mut window = TestWindow::open(path.clone()).unwrap();
    window.runtime.set_execution_allowed(true);
    let request = window.update(Message::Run(false)).pop().unwrap();
    assert_eq!(request.action.virtual_terminal, Some(options));
    let token = [17; 16];
    let directory = &window.runtime.recording_directory;
    fs::create_dir(directory).unwrap();
    let file = virtual_terminal::path_for(directory, request.id, token);
    // A real recording artifact, standing in for output from the SSH fixture.
    let mut recorder = kortty_recording::Recorder::create(&file, Default::default()).unwrap();
    let mut terminal = kortty_terminal::Terminal::new(40, 10);
    terminal.advance(b"stale\r\x1b[2K\x1b[31mfinal\x1b[0m");
    recorder.start(Duration::ZERO).unwrap();
    recorder
        .record_screen(Duration::ZERO, 1, &terminal.snapshot(), (0, 0))
        .unwrap();
    recorder.finish(Duration::ZERO).unwrap();
    use crate::recording_window::screenshots;
    screenshots::attach(&file, || false).unwrap();
    let attached = screenshots::attachment_dir(&file);
    let orphan = virtual_terminal::path_for(directory, request.id, [18; 16]);
    fs::copy(&file, &orphan).unwrap();
    screenshots::attach(&orphan, || false).unwrap();
    window.update(Message::Finished(Completion {
        id: request.id,
        outcome: RunOutcome::Success,
        detail: None,
        recording: Some(token),
        output: None,
        swarm_report: None,
    }));
    drop(window);

    let mut window = TestWindow::open(path).unwrap();
    assert!(!orphan.exists());
    assert!(!screenshots::attachment_dir(&orphan).exists());
    assert!(attached.join("001.png").is_file());
    assert_eq!(
        window.runtime.recording_path(request.id),
        Some(file.clone())
    );
    let mut replay = kortty_recording::Replay::open(&file).unwrap();
    assert!(replay.is_complete());
    let frame = replay.frame(0).unwrap().unwrap();
    let visible: String = frame
        .screen
        .row(0)
        .unwrap()
        .iter()
        .map(|cell| cell.text.as_str())
        .collect();
    assert_eq!(visible, "final");
    drop(replay);
    window.update(Message::Page(Page::Journal));
    window.update(Message::SelectRecord(request.id));
    assert_eq!(
        window
            .runtime
            .selected_recording_path(&window.view, request.id),
        Some(file.clone())
    );
    window.update(Message::DeleteRecord);
    assert!(matches!(
        window.view.confirm,
        Some(Confirmation::DeleteRecord(_))
    ));
    window.update(Message::JournalSearch("no matching run".into()));
    window.update(Message::SelectRecord(request.id));
    window.update(Message::Confirm);
    assert_eq!(window.view.selected_run, None);
    assert!(
        window
            .runtime
            .selected_recording_path(&window.view, request.id)
            .is_none()
    );
    assert!(file.is_file());
    window.update(Message::JournalSearch(String::new()));
    window.update(Message::SelectRecord(request.id));
    window.update(Message::DeleteRecord);
    window.update(Message::Confirm);
    assert!(window.runtime.recording_path(request.id).is_none());
    assert!(!file.exists());
    assert!(!attached.exists());
}

#[test]
fn scheduler_journal_search_uses_real_run_fields_and_view_specific_columns() {
    let profile = Profile::temporary();
    let path = profile.file();
    let job = JobId::new("internal-id").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "server.example",
        "port": 22,
        "username": "operator",
        "key_file": profile.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![Job::new(job, "Backlog upload", action).unwrap()]).unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    let mut window = TestWindow::open(path).unwrap();
    window.runtime.set_execution_allowed(true);
    let request = window.update(Message::Run(false)).pop().unwrap();
    window.update(Message::Page(Page::Journal));
    window.update(Message::JournalSearch("back*up running manual".into()));
    assert!(
        window
            .view
            .journal_active_visible(&window.runtime, request.id)
    );
    window.update(Message::JournalSearchMode(
        JournalSearchMode::SelectedColumns,
    ));
    assert!(
        !window
            .view
            .journal_active_visible(&window.runtime, request.id)
    );
    window.update(Message::JournalColumns);
    window.update(Message::JournalColumn(JournalColumn::Trigger, true));
    assert!(
        window
            .view
            .journal_active_visible(&window.runtime, request.id)
    );

    // Test double for the remote executor's completion through the real runtime and store.
    window.update(Message::Finished(Completion {
        id: request.id,
        outcome: RunOutcome::Failed {
            exit_code: Some(42),
        },
        detail: Some("Permission denied".into()),
        recording: None,
        output: None,
        swarm_report: None,
    }));
    // A clock value containing "42" must not satisfy the exit-code check.
    window.update(Message::JournalColumn(JournalColumn::Started, false));
    window.update(Message::JournalSearch("back*up per*denied 42".into()));
    assert!(
        !window
            .view
            .journal_record_visible(&window.runtime, request.id)
    );
    window.update(Message::JournalColumn(JournalColumn::ExitCode, true));
    assert!(
        window
            .view
            .journal_record_visible(&window.runtime, request.id)
    );
    window.update(Message::SelectRecord(request.id));
    assert_eq!(window.view.selected_run, Some(request.id));

    let mut other = window.runtime.new_view();
    other.page = Page::Journal;
    other
        .journal_search
        .set_text(format!("internal-id {} 42", request.id));
    assert!(other.journal_record_visible(&window.runtime, request.id));
    window.update(Message::JournalColumn(JournalColumn::ExitCode, false));
    assert_eq!(window.view.selected_run, None);
    assert!(
        !window
            .view
            .journal_record_visible(&window.runtime, request.id)
    );
    assert!(other.journal_record_visible(&window.runtime, request.id));
}

#[test]
fn scheduler_journal_sorts_runs_and_deletes_only_the_reviewed_visible_selection() {
    let profile = Profile::temporary();
    let path = profile.file();
    let zulu = JobId::new("zulu").unwrap();
    let amber = JobId::new("amber").unwrap();
    let active = JobId::new("active").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "server.example",
        "port": 22,
        "username": "operator",
        "key_file": profile.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    let graph = JobGraph::new(vec![
        Job::new(zulu.clone(), "Zulu backup", action.clone()).unwrap(),
        Job::new(amber.clone(), "amber backup", action.clone()).unwrap(),
        Job::new(active.clone(), "Bravo backup", action).unwrap(),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();

    let mut window = TestWindow::open(path.clone()).unwrap();
    window.runtime.set_execution_allowed(true);
    let now = SystemTime::now();
    let start = |window: &mut TestWindow, job: &JobId, seconds| {
        let at = now + Duration::from_secs(seconds);
        let batch = window.runtime.start(job, false, &[], at).unwrap();
        window.runtime.receive(batch, None, at).pop().unwrap().id
    };
    let finish = |window: &mut TestWindow, id, outcome, detail: &str, seconds| {
        window.runtime.update_runtime_at(
            Message::Finished(Completion {
                id,
                outcome,
                detail: Some(detail.into()),
                recording: None,
                output: None,
                swarm_report: None,
            }),
            now + Duration::from_secs(seconds),
        );
    };
    let first = start(&mut window, &zulu, 0);
    finish(&mut window, first, RunOutcome::Success, "First run", 1);
    let second = start(&mut window, &amber, 2);
    finish(&mut window, second, RunOutcome::Cancelled, "Second run", 3);
    let running = start(&mut window, &active, 4);

    let sorted = |window: &TestWindow| {
        let records = window.runtime.store.journal();
        window
            .view
            .journal_rows(&window.runtime, &records)
            .into_iter()
            .map(|row| row.id)
            .collect::<Vec<_>>()
    };
    window.update(Message::Page(Page::Journal));
    assert_eq!(sorted(&window), [running, second, first]);
    window.update(Message::JournalSort(JournalSortColumn::Job));
    assert_eq!(sorted(&window), [second, running, first]);
    window.update(Message::JournalSort(JournalSortColumn::Job));
    assert_eq!(sorted(&window), [first, running, second]);
    window.update(Message::JournalSort(JournalSortColumn::Summary));
    assert_eq!(sorted(&window), [running, first, second]);

    window.update(Message::SelectRecord(first));
    window.update(Message::MarkRecord(second, true));
    window.update(Message::MarkRecord(running, true));
    assert_eq!(window.view.selected_run, Some(second));
    assert_eq!(window.view.marked_records, BTreeSet::from([first, second]));
    window.update(Message::DeleteRecord);
    assert!(window.view.confirm.is_none());

    finish(&mut window, running, RunOutcome::Success, "Third run", 5);
    window.update(Message::DeleteRecord);
    assert!(matches!(
        &window.view.confirm,
        Some(Confirmation::DeleteRecord(ids))
            if ids.len() == 2 && ids.iter().copied().collect::<BTreeSet<_>>() == BTreeSet::from([first, second])
    ));

    window.update(Message::JournalSearch("amber".into()));
    window.update(Message::Confirm);
    assert_eq!(window.runtime.store.journal().len(), 3);
    assert_eq!(window.view.marked_records, BTreeSet::from([second]));
    window.update(Message::JournalSearch(String::new()));
    window.update(Message::MarkRecord(first, true));
    window.update(Message::DeleteRecord);
    window.update(Message::Confirm);
    assert_eq!(window.status(), Some("2 journal entries deleted"));
    assert_eq!(window.view.selected_run, None);
    assert!(window.view.marked_records.is_empty());
    drop(window);
    let store = SchedulerStore::open(path).unwrap();
    let remaining = store.journal();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, running);
}

#[test]
fn scheduler_run_journal_retention_follows_local_calendar_days() {
    let zone = TimeZone::get("Europe/Berlin").unwrap();
    for (now, expected) in [
        ("2025-03-30T12:00:00+02:00", "2025-03-29T12:00:00+01:00"),
        ("2025-03-31T02:30:00+02:00", "2025-03-30T03:30:00+02:00"),
        ("2025-10-27T02:30:00+01:00", "2025-10-26T02:30:00+01:00"),
    ] {
        let now = SystemTime::from(now.parse::<Timestamp>().unwrap());
        let expected = SystemTime::from(expected.parse::<Timestamp>().unwrap());
        assert_eq!(journal_cutoff(now, &zone, 1).unwrap(), expected);
    }
}

#[test]
fn scheduler_run_journal_retention_prunes_after_apply_commit_and_reopen() {
    let profile = Profile::temporary();
    let path = profile.file();
    let old = JobId::new("old").unwrap();
    let recent = JobId::new("recent").unwrap();
    let dependent = JobId::new("dependent").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "server.example",
        "port": 22,
        "username": "operator",
        "key_file": profile.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    let graph = JobGraph::new(vec![
        Job::new(old.clone(), "Old", action.clone()).unwrap(),
        Job::new(recent.clone(), "Recent", action.clone()).unwrap(),
        Job::new(dependent.clone(), "Dependent", action)
            .unwrap()
            .with_dependencies(DependencyMode::Gate, vec![old.clone()], None),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();

    let mut window = TestWindow::open(path.clone()).unwrap();
    let directory = window.runtime.recording_directory.clone();
    fs::create_dir(&directory).unwrap();
    window.runtime.zone = TimeZone::UTC;
    window.runtime.set_execution_allowed(true);
    let now = SystemTime::from("2025-01-15T12:00:00Z".parse::<Timestamp>().unwrap());
    let day = Duration::from_secs(86_400);
    let start = |window: &mut TestWindow, job: &JobId, at| {
        let batch = window.runtime.start(job, false, &[], at).unwrap();
        window.runtime.receive(batch, None, at).pop().unwrap().id
    };
    let finish = |window: &mut TestWindow, id, at, token| {
        window.runtime.update_runtime_at(
            Message::Finished(Completion {
                id,
                outcome: RunOutcome::Success,
                detail: None,
                recording: token,
                output: None,
                swarm_report: None,
            }),
            at,
        );
    };
    let expired = start(&mut window, &old, now - day - Duration::from_secs(1));
    let boundary = start(&mut window, &recent, now - day);
    let expired_file = virtual_terminal::path_for(&directory, expired, [1; 16]);
    let boundary_file = virtual_terminal::path_for(&directory, boundary, [2; 16]);
    let screenshots = crate::recording_window::screenshots::attachment_dir(&expired_file);
    fs::create_dir(&screenshots).unwrap();
    fs::write(&expired_file, b"old recording").unwrap();
    fs::write(&boundary_file, b"cutoff recording").unwrap();
    finish(
        &mut window,
        expired,
        now - day - Duration::from_secs(1),
        Some([1; 16]),
    );
    finish(&mut window, boundary, now - day, Some([2; 16]));

    window.update(Message::Page(Page::Journal));
    window.update(Message::MarkRecord(expired, true));
    window.update(Message::SelectRecord(expired));
    window.update(Message::EditJournalRetention("3651".into()));
    assert_eq!(window.view.journal_retention_change(&window.runtime), None);
    window.update(Message::EditJournalRetention("1".into()));
    assert_eq!(
        window.view.journal_retention_change(&window.runtime),
        Some(1)
    );
    assert_eq!(window.runtime.store.journal().len(), 2);
    assert_eq!(window.runtime.set_journal_retention(1, now), Some(1));
    window.runtime.sync_view(&mut window.view);
    assert!(window.view.journal_retention_draft.is_none());
    assert!(window.view.marked_records.is_empty());
    assert!(window.view.selected_run.is_none());
    assert!(window.runtime.store.journal_record(expired).is_none());
    assert!(!expired_file.exists() && !screenshots.exists());
    assert!(boundary_file.is_file());
    assert!(window.runtime.store.journal_record(boundary).is_some());
    assert!(window.runtime.scheduler.last_finished(&old).is_none());
    assert!(
        window
            .runtime
            .scheduler
            .blockers(&dependent, now)
            .unwrap()
            .is_empty()
    );

    let run = start(&mut window, &recent, now);
    let previous = profile.0.join("previous.json");
    fs::rename(&path, &previous).unwrap();
    fs::create_dir(&path).unwrap();
    let later = now + Duration::from_secs(1);
    finish(&mut window, run, later, None);
    assert!(window.runtime.store.journal_record(run).is_none());
    assert!(boundary_file.is_file());
    assert!(window.runtime.store.journal_record(boundary).is_some());
    fs::remove_dir(&path).unwrap();
    fs::rename(previous, &path).unwrap();
    window.runtime.update_runtime_at(Message::Tick, later);
    assert!(window.runtime.store.journal_record(run).is_some());
    assert!(!boundary_file.exists());
    assert!(window.runtime.store.journal_record(boundary).is_none());
    drop(window);

    let reopened = SchedulerWindow::open_with_journal_retention(path, 1).unwrap();
    assert!(reopened.store.journal().is_empty());
    assert!(
        reopened
            .scheduler
            .blockers(&dependent, now)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ai_cascade_root_rechecks_only_the_predecessor_blockers_accepted_at_start() {
    let profile = Profile::temporary();
    let path = profile.file();
    let upstream = JobId::new("upstream").unwrap();
    let root = JobId::new("ai-root").unwrap();
    let connection = serde_json::json!({
        "id": 1, "target": {"host": "fixture.example", "port": 22, "username": "operator"}
    });
    let command: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": connection, "command": "true"
    }))
    .unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": connection,
        "working_directory": "/srv",
        "agent": {"kind": "agent", "probe": "per_firing", "profile": 1, "goal": "Inspect fixture"}
    }))
    .unwrap();
    let graph = JobGraph::new(vec![
        Job::new(upstream.clone(), "Upstream", command).unwrap(),
        Job::new(root.clone(), "AI root", action)
            .unwrap()
            .with_dependencies(DependencyMode::Gate, vec![upstream.clone()], None),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.runtime.select(&mut app.view, &root);
    assert!(app.update(Message::Run(true)).is_empty());
    let root_run = app.update(Message::Confirm).pop().unwrap();
    assert_eq!(root_run.trigger, RunTrigger::Cascade);
    assert!(
        app.runtime
            .agent_predecessors_current(&root_run, SystemTime::now())
    );

    app.runtime.select(&mut app.view, &upstream);
    let predecessor = app.update(Message::Run(false)).pop().unwrap();
    assert!(
        app.update(Message::Finished(Completion {
            id: predecessor.id,
            outcome: RunOutcome::Failed { exit_code: Some(1) },
            detail: None,
            recording: None,
            output: None,
            swarm_report: None,
        }))
        .is_empty()
    );
    assert!(
        !app.runtime
            .agent_predecessors_current(&root_run, SystemTime::now())
    );

    let predecessor = app.update(Message::Run(false)).pop().unwrap();
    assert!(
        app.update(Message::Finished(Completion {
            id: predecessor.id,
            outcome: RunOutcome::Success,
            detail: None,
            recording: None,
            output: None,
            swarm_report: None,
        }))
        .is_empty()
    );
    assert!(
        app.runtime
            .agent_predecessors_current(&root_run, SystemTime::now())
    );
}

#[test]
fn imported_ai_drafts_cannot_start_or_be_enabled_with_a_chain() {
    let profile = Profile::temporary();
    let root = JobId::new("command").unwrap();
    let imported = JobId::new("java-imported-ai").unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": {"id": 1, "target": {"host": "fixture.example", "port": 22, "username": "operator"}},
        "imported_ai": {"kind": "agent", "goal": "Inspect fixture", "parallelism": 1, "swarm_read_only": true}
    })).unwrap();
    assert_eq!(
        action.command_for_exec(None),
        Err(IMPORTED_AI_REVIEW_REQUIRED)
    );
    let mut mixed = action.clone();
    mixed.command = "true".into();
    assert!(mixed.validated().is_err());
    let mut command = action.clone();
    command.imported_ai = None;
    command.command = "true".into();
    let ancestor = Job::new(root.clone(), "Command", command)
        .unwrap()
        .with_enabled(false);
    let draft = Job::new(imported.clone(), "Imported Agent", action)
        .unwrap()
        .with_dependencies(DependencyMode::Chain, vec![root.clone()], None);
    assert!(SshCommand::validate_definition(&draft).is_err());
    let draft = draft.with_enabled(false);
    let path = profile.file();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        store
            .commit_with_actions(&JobGraph::new(vec![ancestor, draft]).unwrap(), &[], &[])
            .unwrap();
    }
    let unchanged = fs::read(&path).unwrap();
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.runtime.select(&mut app.view, &imported);
    for cascade in [false, true] {
        assert!(app.update(Message::Run(cascade)).is_empty());
        assert_eq!(app.status(), Some(IMPORTED_AI_REVIEW_REQUIRED));
        assert!(!app.runtime.has_active_runs());
        assert!(app.runtime.store.journal().is_empty());
    }
    app.runtime.select(&mut app.view, &root);
    assert!(app.update(Message::SwitchChain).is_empty());
    assert!(app.update(Message::Confirm).is_empty());
    assert!(app.status().unwrap().contains(IMPORTED_AI_REVIEW_REQUIRED));
    assert_eq!(fs::read(path).unwrap(), unchanged);
    assert!(
        app.runtime
            .scheduler
            .graph()
            .jobs()
            .iter()
            .all(|job| !job.enabled())
    );
}

#[test]
fn a_chain_waits_for_the_predecessor_journal_commit_with_session_journals() {
    let profile = Profile::temporary();
    let root = JobId::new("first").unwrap();
    let leaf = JobId::new("second").unwrap();
    let action = SshCommand {
        host: "example.test".into(),
        port: 22,
        username: "test".into(),
        key_file: profile.0.join("fixture-key"),
        connection: None,
        additional_connections: Vec::new(),
        target_groups: None,
        command: "true".into(),
        working_directory: None,
        script: None,
        snippet: None,
        remote: None,
        agent: None,
        imported_ai: None,
        imported_transfer: None,
        imported_privileged: None,
        sudo: SudoMode::Disabled,
        virtual_terminal: None,
        journal_detail: JournalDetail::Off,
        automation_journal: None,
        desktop_notices: Default::default(),
        webhook_targets: Vec::new(),
    };
    let jobs = vec![
        Job::new(root.clone(), "First", action.clone()).unwrap(),
        Job::new(leaf.clone(), "Second", action)
            .unwrap()
            .with_dependencies(DependencyMode::Chain, vec![root.clone()], None),
    ];
    let path = profile.file();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        store
            .commit_with_actions(&JobGraph::new(jobs).unwrap(), &[], &[])
            .unwrap();
    }

    let mut window = TestWindow::open(path.clone()).unwrap();
    assert!(window.update(Message::Run(false)).is_empty());
    window.runtime.set_execution_allowed(true);
    let run = window.update(Message::Run(false)).pop().unwrap();
    assert_eq!(run.job, root);

    let backup = profile.0.join("previous.json");
    fs::rename(&path, &backup).unwrap();
    fs::create_dir(&path).unwrap();
    // Simulated executor completion. All runtime, dependency and file operations are real.
    let links = vec![
        "a0000000-0000-4000-8000-000000000001".to_owned(),
        "b0000000-0000-4000-8000-000000000002".to_owned(),
    ];
    let completion = Message::FinishedWithSessionJournals(
        Completion {
            id: run.id,
            outcome: RunOutcome::Success,
            detail: Some("Exit status 0".into()),
            recording: Some([3; 16]),
            output: None,
            swarm_report: None,
        },
        links.clone(),
    );
    assert!(window.update(completion).is_empty());
    assert!(window.runtime.has_active_runs());
    assert!(window.runtime.next_due().is_some());
    assert!(window.runtime.store.journal().is_empty());
    assert_eq!(window.runtime.store.session_journals(run.id), None);

    fs::remove_dir(&path).unwrap();
    fs::rename(backup, &path).unwrap();
    let leaf_run = window.update(Message::Tick).pop().unwrap();
    assert_eq!(leaf_run.job, leaf);
    assert_eq!(
        window.runtime.store.journal_detail(run.id),
        Some("Exit status 0")
    );
    assert_eq!(
        window.runtime.store.session_journals(run.id),
        Some(links.as_slice())
    );
    assert!(window.update(Message::Tick).is_empty());
    let failed = RunOutcome::Failed { exit_code: Some(5) };
    assert!(
        window
            .update(Message::Finished(Completion {
                id: leaf_run.id,
                outcome: failed,
                detail: None,
                recording: None,
                output: None,
                swarm_report: None,
            }))
            .is_empty()
    );
    assert!(!window.runtime.has_active_runs());
    drop(window);

    let mut window = TestWindow::open(path).unwrap();
    assert_eq!(window.runtime.store.journal().len(), 2);
    assert_eq!(window.runtime.store.journal()[0].outcome, failed);
    assert_eq!(
        window.runtime.store.journal_detail(run.id),
        Some("Exit status 0")
    );
    assert_eq!(
        window
            .runtime
            .selected_session_journals(&window.view, run.id),
        None
    );
    window.update(Message::Page(Page::Journal));
    window.update(Message::SelectRecord(run.id));
    assert_eq!(
        window
            .runtime
            .selected_session_journals(&window.view, run.id),
        Some((&root, links.as_slice()))
    );
    assert!(
        window
            .runtime
            .selected_recording_path(&window.view, run.id)
            .is_some()
    );
    assert_eq!(
        window
            .runtime
            .selected_session_journals(&window.view, leaf_run.id),
        None
    );
    window.update(Message::SelectRecord(leaf_run.id));
    assert_eq!(
        window
            .runtime
            .selected_session_journals(&window.view, run.id),
        None
    );
    window.update(Message::SelectRecord(run.id));
    window.update(Message::JournalSearch("not-in-any-run-field".into()));
    assert_eq!(
        window
            .runtime
            .selected_session_journals(&window.view, run.id),
        None
    );
}

#[test]
fn native_form_reopens_the_command_and_calendar() {
    use std::sync::Arc;

    let profile = Profile::temporary();
    let path = profile.file();
    let mut window = TestWindow::open(path.clone()).unwrap();
    for (field, value) in [
        (Field::Name, "Maintenance".to_owned()),
        (Field::Host, "TEST.Example.".to_owned()),
        (Field::Username, "operator".to_owned()),
        (
            Field::KeyFile,
            profile.0.join("reference-key").display().to_string(),
        ),
        (Field::WorkingDirectory, "/jobs/it's literal $HOME".into()),
        (Field::From, "2026-11-07".to_owned()),
        (Field::Until, "2026-11-20".to_owned()),
        (Field::WindowStart, "21:30".to_owned()),
        (Field::WindowEnd, "02:00".to_owned()),
        (Field::Interval, "30".to_owned()),
        (Field::Age, "45".to_owned()),
    ] {
        assert!(window.update(Message::Edit(field, value)).is_empty());
    }
    window.update(Message::FixedTimeInput("23:15, 21:37, 23:59, 21:37".into()));
    window.update(Message::AddFixedTimes);
    let retained: Time = "21:37".parse().unwrap();
    let removed: Time = "23:59".parse().unwrap();
    window.update(Message::SelectFixedTime(removed));
    window.update(Message::RemoveFixedTime(retained));
    window.update(Message::RemoveFixedTime(removed));
    window.update(Message::FixedTimeInput("12:00, 24:00".into()));
    window.update(Message::AddFixedTimes);
    assert_eq!(window.status(), Some("Enter schedule times as HH:MM"));
    window.update(Message::ScheduleEnabled(true));
    window.update(Message::AllWeekdays(true));
    window.update(Message::Weekday(Weekday::Saturday, false));
    window.update(Message::Mode(Mode::Chain));
    let command = "printf '%s\\n' first\nprintf '%s\\n' second".to_owned();
    window.update(Message::EditCommand(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new(command.clone())),
    )));
    window.update(Message::Save);
    assert_eq!(window.status(), Some("Job saved"));
    let original = window.selected_job().unwrap();
    let original_definition = original.definition();
    let original_action = original.action().clone();
    let schedule = original.schedule().unwrap();
    assert_eq!(
        schedule.fixed_times().collect::<Vec<_>>(),
        [retained, "23:15".parse::<Time>().unwrap()]
    );
    assert_eq!(schedule.weekdays().count(), 6);
    assert!(!schedule.weekdays().any(|day| day == Weekday::Saturday));
    assert_eq!(original_action.host, "test.example.");
    assert_eq!(original_action.command, command);
    assert_eq!(
        original_action.command_for_exec(None).unwrap(),
        format!("CDPATH= cd '/jobs/it'\\''s literal $HOME' || exit\n{command}")
    );
    drop(window);

    let mut window = TestWindow::open(path).unwrap();
    window.update(Message::Save);
    let reopened = window.selected_job().unwrap();
    assert_eq!(reopened.definition(), original_definition);
    assert_eq!(reopened.action(), &original_action);
    window.update(Message::FixedTimeInput("21:37".into()));
    window.update(Message::AddFixedTimes);
    assert!(!window.view.dirty());
    window.update(Message::FixedTimeInput("x".repeat(4097)));
    window.update(Message::AddFixedTimes);
    assert!(!window.view.dirty());
    window.update(Message::AllWeekdays(false));
    window.update(Message::Edit(Field::Interval, "0".into()));
    window.update(Message::Save);
    let saved = window.selected_job().unwrap();
    let saved_definition = saved.definition();
    let schedule = saved.schedule().unwrap();
    assert_eq!(schedule.weekdays().count(), 0);
    assert!(schedule.interval_minutes().is_none());
    assert_eq!(schedule.fixed_times().count(), 2);

    assert!(window.request_close());
    assert!(!window.confirm_close());
    window.update(Message::Edit(Field::Name, "Unsaved name".into()));
    assert!(!window.request_close());
    assert!(window.close_pending());
    window.update(Message::Save);
    assert!(window.close_pending());
    assert_eq!(window.selected_job().unwrap().name(), "Maintenance");
    window.update(Message::Dismiss);
    assert!(!window.close_pending());
    assert!(!window.confirm_close());
    assert!(!window.request_close());
    assert!(window.confirm_close());
    assert!(window.request_close());
    window.update(Message::Save);
    assert_eq!(
        window.selected_job().unwrap().definition(),
        saved_definition
    );
}

#[test]
fn working_directory_wraps_shell_actions_and_validates_stored_paths() {
    let mut action: SshCommand = serde_json::from_value(serde_json::json!({
        "command": "# first line\nfalse\ntrue",
    }))
    .unwrap();
    assert!(matches!(
        action.command_for_exec(None),
        Ok(Cow::Borrowed(_))
    ));
    assert!(
        serde_json::to_value(&action)
            .unwrap()
            .get("working_directory")
            .is_none()
    );
    action.working_directory = Some("-job".into());
    assert_eq!(
        action.command_for_exec(None).unwrap(),
        "CDPATH= cd './-job' || exit\n# first line\nfalse\ntrue"
    );
    for path in ["/invalid\npath".to_owned(), "x".repeat(4097)] {
        action.working_directory = Some(path);
        assert!(action.command_for_exec(None).is_err());
    }
    action.working_directory = Some("/run".into());
    action.command = "x".repeat(MAX_COMMAND_BYTES);
    assert!(action.command_for_exec(None).is_err());

    action.command.clear();
    action.remote = Some(RemoteAction::Mkdir {
        path: "/run/test".to_owned().try_into().unwrap(),
    });
    assert!(action.remote_action().is_err());
}

#[test]
fn imported_sftp_form_requires_local_path_and_adopts_only_the_chosen_transfer() {
    use imported_transfer::Mode;
    use serde_json::json;

    for (kind, mode, native_kind, folder) in [
        ("upload", Mode::File, "upload", false),
        ("download", Mode::Folder, "download", true),
        ("sync_download", Mode::Compare, "sync", false),
    ] {
        let profile = Profile::temporary();
        let path = profile.file();
        let id = JobId::new("java-00000000000040009000000000000000").unwrap();
        let action: SshCommand = serde_json::from_value(json!({
            "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
            "imported_transfer": {"kind": kind, "remote": "/stage/file", "local_hint": "/java-only/original"}
        })).unwrap();
        {
            let mut store = SchedulerStore::open(&path).unwrap();
            let job = Job::new(id.clone(), "Transfer draft", action.clone())
                .unwrap()
                .with_enabled(false);
            let graph = JobGraph::new(vec![job]).unwrap();
            store.commit_with_actions(&graph, &[], &[]).unwrap();
        }
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        app.update(Message::Page(Page::Action));
        assert_eq!(app.view.draft.source, Source::ImportedTransfer);
        app.update(Message::Enabled(true));
        app.update(Message::CommandSource(Source::RemoteUpload));
        app.update(Message::AdoptImportedTransfer(mode));
        assert!(app.status().unwrap().contains("absolute local path"));
        assert!(app.view.draft.has_imported_transfer());
        assert!(app.update(Message::Run(false)).is_empty());

        let local = profile.0.join("chosen path");
        app.update(Message::Edit(
            Field::LocalFile,
            local.to_str().unwrap().into(),
        ));
        app.update(Message::Save);
        let saved = app.selected_job().unwrap();
        assert!(!saved.enabled());
        assert_eq!(saved.action(), &action);
        drop(app);

        let mut app = TestWindow::open(path).unwrap();
        app.update(Message::Select(id));
        app.update(Message::Page(Page::Action));
        app.update(Message::AdoptImportedTransfer(mode));
        assert!(app.status().unwrap().contains("absolute local path"));
        app.update(Message::Edit(
            Field::LocalFile,
            local.to_str().unwrap().into(),
        ));
        app.update(Message::AdoptImportedTransfer(mode));
        app.update(Message::Save);
        assert_eq!(app.status(), Some("Job saved"));
        let selected = app.selected_job().unwrap();
        assert!(!selected.enabled());
        assert_eq!(selected.action().connection, action.connection);
        assert!(selected.action().imported_transfer.is_none());
        let remote = serde_json::to_value(selected.action()).unwrap()["remote"].clone();
        assert_eq!(remote["kind"], native_kind);
        assert_eq!(remote["local"], json!(local));
        assert_eq!(remote["remote"], "/stage/file");
        assert_eq!(remote["folder"].as_bool().unwrap_or(false), folder);
        if mode == Mode::Compare {
            assert_eq!(remote["direction"], "download");
            assert!(remote["approval"].is_null());
            assert!(remote["standing"].is_null());
            assert!(!selected.schedule().unwrap().enabled());
        }
    }
}

#[test]
fn imported_sudo_staging_requires_a_native_path_mode_and_transfer_review() {
    use kortty_jobs::JobSchedule;

    use imported_transfer::{Mode, StagedDestination};
    use serde_json::json;

    for (kind, destination, sudo, native_kind) in [
        (
            "upload",
            StagedDestination::NewFile,
            SudoMode::Passwordless,
            "privileged",
        ),
        (
            "download",
            StagedDestination::ReplaceFile,
            SudoMode::Vault,
            "privileged",
        ),
        (
            "sync_upload",
            StagedDestination::NewFolder,
            SudoMode::Vault,
            "privileged_tree_upload",
        ),
        (
            "sync_download",
            StagedDestination::NewFolder,
            SudoMode::Passwordless,
            "privileged_tree_download",
        ),
        (
            "sync_download",
            StagedDestination::ExistingFolder,
            SudoMode::Vault,
            "privileged_tree_download",
        ),
        (
            "sync_upload",
            StagedDestination::ExistingFolder,
            SudoMode::Passwordless,
            "privileged_tree_upload",
        ),
    ] {
        let profile = Profile::temporary();
        let path = profile.file();
        let id = JobId::new("java-00000000000040009000000000000000").unwrap();
        let route = json!({
            "id": 1,
            "target": {"host": "server.example", "port": 22, "username": "operator"},
            "jump": {"host": "jump.example", "port": 2222, "username": "relay"}
        });
        let action: SshCommand = serde_json::from_value(json!({
            "connection": route,
            "imported_transfer": {
                "route": route, "settings": {
                    "kind": kind, "local_hint": "/java-only/original", "remote_hint": "relative/file"
                }
            }
        }))
        .unwrap();
        let schedule = JobSchedule::default()
            .with_enabled(true)
            .with_fixed_times([jiff::civil::Time::new(8, 15, 0, 0).unwrap()]);
        let job = Job::new(id.clone(), "Staged transfer", action.clone())
            .unwrap()
            .with_enabled(false)
            .with_schedule(schedule.clone());
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&JobGraph::new(vec![job]).unwrap(), &[], &[])
            .unwrap();

        let mut app = TestWindow::open(path).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id));
        app.update(Message::Page(Page::Action));
        let adopt = || Message::AdoptImportedTransfer(Mode::Staged(destination));
        let local = profile.0.join("chosen path");
        app.update(Message::Edit(
            Field::LocalFile,
            local.to_str().unwrap().into(),
        ));
        app.update(Message::Enabled(true));
        app.update(Message::Sudo(sudo));
        app.update(Message::ConnectionRemoved(1));
        app.update(adopt());
        assert!(
            app.status()
                .unwrap()
                .contains("Choose passwordless or vault-backed sudo")
        );
        assert!(app.update(Message::Run(false)).is_empty());
        app.update(Message::ImportedStagingSudo(sudo));
        app.update(adopt());
        assert!(app.status().unwrap().contains("absolute SFTP path"));
        assert!(app.view.draft.has_imported_transfer());
        app.update(Message::Save);
        assert_eq!(app.selected_job().unwrap().action(), &action);

        app.update(Message::ImportedStagingSudo(sudo));
        app.update(Message::Edit(
            Field::LocalFile,
            local.to_str().unwrap().into(),
        ));
        app.update(Message::Edit(Field::RemotePath, "/stage/file".into()));
        app.update(adopt());
        app.update(Message::Save);
        assert_eq!(app.status(), Some("Job saved"));
        let job = app.selected_job().unwrap();
        assert!(!job.enabled());
        assert_eq!(job.schedule(), Some(&schedule));
        assert_eq!(job.action().sudo, sudo);
        assert_eq!(job.action().connection, action.connection);
        assert!(job.action().imported_transfer.is_none());
        let remote = &serde_json::to_value(job.action()).unwrap()["remote"];
        assert_eq!(remote["kind"], native_kind);
        assert_eq!(remote["local"], json!(local));
        assert_eq!(remote["remote"], "/stage/file");
        assert!(remote["approval"].is_null());
        assert_eq!(
            remote["replace_existing"].as_bool().unwrap_or_default(),
            destination == StagedDestination::ReplaceFile
        );

        app.update(Message::Enabled(true));
        app.update(Message::Save);
        assert!(!app.selected_job().unwrap().enabled());
        let error = app.status().unwrap();
        assert_eq!(
            remote["replace_changed"].as_bool().unwrap_or_default(),
            destination == StagedDestination::ExistingFolder
        );
        if destination.is_folder() {
            assert!(
                error.contains("requires a manual run or one date"),
                "{error}"
            );
        } else {
            assert!(error.contains("Review the exact"), "{error}");
        }
    }
}

#[test]
fn imported_rsync_form_keeps_unsupported_options_and_requires_exact_new_review() {
    use serde_json::json;

    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = profile.file();
    let id = JobId::new("java-00000000000040009002000000000000").unwrap();
    let unsupported = JobId::new("java-00000000000040009002000000000001").unwrap();
    let route =
        json!({"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}});
    let draft = |delete| -> SshCommand {
        serde_json::from_value(json!({
            "connection": route,
            "imported_transfer": {
                "direction": "download", "java_sources": ["/stage/file"],
                "java_target_root": "/java-only/root", "java_delete": delete, "java_sudo": false,
            },
        }))
        .unwrap()
    };
    let graph = JobGraph::new(vec![
        Job::new(id.clone(), "Java file rsync", draft(false))
            .unwrap()
            .with_enabled(false),
        Job::new(unsupported.clone(), "Java rsync delete", draft(true))
            .unwrap()
            .with_enabled(false),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(unsupported));
    app.update(Message::Page(Page::Action));
    app.update(Message::ConfigureImportedRsync { directory: false });
    assert!(app.status().unwrap().contains("Java delete option"));
    assert!(app.view.draft.has_imported_transfer());
    assert!(app.update(Message::Run(false)).is_empty());

    app.update(Message::Select(id.clone()));
    app.update(Message::Page(Page::Action));
    app.update(Message::ConfigureImportedRsync { directory: false });
    assert_eq!(app.view.draft.source, Source::RsyncDownload);
    let local = root.join("published");
    let program = root.join("rsync");
    app.update(Message::Edit(
        Field::LocalFile,
        local.to_str().unwrap().into(),
    ));
    app.update(Message::Edit(
        Field::RsyncProgram,
        program.to_str().unwrap().into(),
    ));
    app.update(Message::Save);
    assert!(
        app.status()
            .unwrap()
            .contains("Review the imported rsync action")
    );
    assert!(
        app.selected_job()
            .unwrap()
            .action()
            .imported_transfer
            .is_some()
    );

    let reviewed: RemoteAction = serde_json::from_value(json!({
        "kind": "rsync_download", "program": program, "local": local, "remote": "/stage/file",
        "approval": {
            "program": program, "digest": vec![8; 32], "reference": route,
            "pins": ["server.example SHA256:fixture"],
            "download": {"source": "/stage/file", "destination": local},
        },
    }))
    .unwrap();
    let RemoteAction::RsyncDownload(reviewed) = reviewed else {
        panic!("not a download")
    };
    app.view.rsync_review.begin("review".into(), None);
    app.update(Message::RsyncProgramReviewed(
        "review".into(),
        Ok(Box::new(rsync::Reviewed::restore(&reviewed).unwrap())),
    ));
    app.update(Message::ApproveRsyncProgram);
    app.update(Message::Edit(
        Field::LocalFile,
        root.join("different").to_str().unwrap().into(),
    ));
    app.update(Message::Save);
    assert!(
        app.status()
            .unwrap()
            .contains("Review the imported rsync action")
    );
    app.update(Message::Edit(
        Field::LocalFile,
        local.to_str().unwrap().into(),
    ));
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    drop(app);

    let mut app = TestWindow::open(path).unwrap();
    app.update(Message::Select(id));
    let job = app.selected_job().unwrap();
    assert!(!job.enabled() && job.action().imported_transfer.is_none());
    let exported = serde_json::to_value(job.action()).unwrap();
    assert_eq!(
        exported["remote"]["approval"]["download"]["destination"],
        json!(local)
    );
    assert!(!exported.to_string().contains("/java-only/root"));
}

#[test]
fn java_existing_rsync_upload_adoption_preserves_contents_mirror_and_disables_schedule() {
    use serde_json::json;

    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = profile.file();
    let id = JobId::new("java-existing-rsync").unwrap();
    let connection =
        json!({"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}});
    let action: SshCommand = serde_json::from_value(json!({
        "connection": connection,
        "imported_transfer": {
            "direction": "upload", "java_sources": ["/from/old-source/"],
            "java_target_root": "/server/receiver/", "java_delete": true, "java_sudo": false,
        }
    }))
    .unwrap();
    let graph = JobGraph::new(vec![
        Job::new(id.clone(), "Java rsync mirror", action)
            .unwrap()
            .with_enabled(false),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    app.update(Message::Page(Page::Action));
    app.update(Message::ConfigureImportedRsync { directory: true });
    assert!(app.view.draft.has_imported_transfer());
    app.update(Message::ScheduleEnabled(true));
    app.update(Message::ConfigureImportedExistingRsync);
    assert_eq!(app.view.draft.source, Source::RsyncUploadTree);
    assert!(!app.view.draft.has_imported_transfer());
    let program = root.join("rsync");
    let source = root.join("new-source");
    app.update(Message::Edit(
        Field::LocalFile,
        source.to_str().unwrap().into(),
    ));
    app.update(Message::Edit(
        Field::RsyncProgram,
        program.to_str().unwrap().into(),
    ));
    app.update(Message::RsyncMirror(false));
    app.update(Message::RsyncIncludeSourceName(true));
    app.update(Message::RsyncExisting(false));
    let targets = app.view.rsync_review_requests().unwrap().existing_targets();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].1.as_str(), "/server/receiver");
    app.update(Message::Save);
    assert!(app.status().unwrap().contains("Review the imported rsync"));
    assert!(
        app.selected_job()
            .unwrap()
            .action()
            .imported_transfer
            .is_some()
    );

    let settings = json!({"delete": true, "include_source_name": false});
    let inventory = kortty_server_archive::rsync_tree::Manifest::default().finish();
    let remote: RemoteAction = serde_json::from_value(json!({
        "kind": "rsync_upload", "program": program, "local": source, "remote": "/server/receiver",
        "directory": true, "existing": settings,
        "approval": {
            "program": program, "digest": vec![8; 32], "reference": connection, "pins": ["SHA256:fixture"],
            "directory_upload": {"source": source, "destination": "/server/receiver", "inventory": inventory},
            "existing": {"destination": "/server/receiver", "settings": settings,
                "snapshot": {"inventory": inventory, "fingerprint": vec![4; 32]}},
        },
    })).unwrap();
    let RemoteAction::RsyncUpload(transfer) = remote else {
        panic!("expected rsync upload")
    };
    let reviewed = rsync::Reviewed::restore(&transfer).unwrap();
    app.view.rsync_review.begin("existing".into(), None);
    app.update(Message::RsyncProgramReviewed(
        "existing".into(),
        Ok(Box::new(reviewed)),
    ));
    app.update(Message::ApproveRsyncProgram);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    drop(app);

    let mut reopened = TestWindow::open(path).unwrap();
    reopened.update(Message::Select(id));
    let saved = reopened.selected_job().unwrap();
    assert!(!saved.enabled());
    assert!(!saved.schedule().unwrap().enabled());
    let RemoteAction::RsyncUpload(transfer) = saved.action().remote_action().unwrap() else {
        panic!("expected approved native rsync upload")
    };
    let existing = transfer.existing.unwrap();
    assert!(existing.delete);
    assert!(!existing.include_source_name);
    assert!(saved.action().require_executable().is_ok());
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn java_passwordless_rsync_requires_exact_helper_review_and_keeps_mirror_layout() {
    use kortty_server_archive::privileged_rsync::{Operation, Review};
    use kortty_server_archive::rsync_tree::{DeleteScope, Manifest};
    use serde_json::json;

    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = profile.file();
    let id = JobId::new("java-passwordless-rsync").unwrap();
    let connection =
        json!({"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}});
    let action: SshCommand = serde_json::from_value(json!({
        "connection": connection,
        "imported_transfer": {
            "direction": "upload", "java_sources": ["/old/source"],
            "java_target_root": "/protected/target/", "java_delete": true, "java_sudo": true,
        }
    }))
    .unwrap();
    let graph = JobGraph::new(vec![
        Job::new(id.clone(), "Java passwordless mirror", action)
            .unwrap()
            .with_enabled(false),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    app.update(Message::Page(Page::Action));
    app.update(Message::ConfigureImportedExistingRsync);
    assert!(app.view.draft.has_imported_transfer());
    app.update(Message::ConfigureImportedPasswordlessRsync {
        directory: true,
        existing: false,
    });
    assert!(app.view.draft.has_imported_transfer());
    app.update(Message::ConfigureImportedPasswordlessRsync {
        directory: true,
        existing: true,
    });
    assert!(!app.view.draft.has_imported_transfer());
    let program = root.join("rsync");
    let source = root.join("source");
    app.update(Message::Edit(
        Field::LocalFile,
        source.to_str().unwrap().into(),
    ));
    app.update(Message::Edit(
        Field::RsyncProgram,
        program.to_str().unwrap().into(),
    ));
    app.update(Message::RsyncPasswordless(false));
    let requests = app.view.rsync_review_requests().unwrap();
    assert!(requests.existing_targets().is_empty());
    let mut targets = requests.privileged_targets();
    assert_eq!(targets.len(), 1);
    let (_, spec) = targets.pop().unwrap();
    assert_eq!(spec.path, "/protected/target/source");
    assert_eq!(
        spec.operation,
        Operation::Upload {
            directory: true,
            replace: true,
            delete: DeleteScope::All,
        }
    );
    app.update(Message::Save);
    assert!(app.status().unwrap().contains("Review the imported rsync"));

    let inventory = Manifest::default().finish();
    let proof = Review {
        spec,
        policy: [5; 32],
        fingerprint: [7; 32],
        inventory: Some(inventory),
        handoff: "/run/kortty/rsync-handoff".into(),
        first_file: false,
        first_tree: false,
    };
    let settings = json!({"delete": true, "include_source_name": true});
    let remote: RemoteAction = serde_json::from_value(json!({
        "kind": "rsync_upload", "program": program, "local": source, "remote": "/protected/target",
        "directory": true, "existing": settings, "elevation": "passwordless",
        "approval": {
            "program": program, "digest": vec![8; 32], "reference": connection, "pins": ["SHA256:fixture"],
            "directory_upload": {"source": source, "destination": "/protected/target", "inventory": inventory},
            "existing": {"destination": "/protected/target/source", "settings": settings,
                "snapshot": {"inventory": inventory, "fingerprint": vec![7; 32]}},
            "privileged": proof,
        }
    })).unwrap();
    let RemoteAction::RsyncUpload(mut transfer) = remote else {
        panic!("expected passwordless rsync upload");
    };
    assert!(transfer.validate(true).is_ok());
    let good = transfer
        .approval
        .as_ref()
        .unwrap()
        .privileged
        .clone()
        .unwrap();
    transfer.approval.as_mut().unwrap().privileged = None;
    assert!(transfer.validate(true).is_err());
    let mut changed = good.clone();
    changed.spec.path = "/protected/other".into();
    transfer.approval.as_mut().unwrap().privileged = Some(changed);
    assert!(transfer.validate(true).is_err());
    transfer.approval.as_mut().unwrap().privileged = Some(good);

    let reviewed = rsync::Reviewed::restore(&transfer).unwrap();
    app.view.rsync_review.begin("privileged".into(), None);
    app.update(Message::RsyncProgramReviewed(
        "privileged".into(),
        Ok(Box::new(reviewed)),
    ));
    app.update(Message::ApproveRsyncProgram);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    let saved = app.selected_job().unwrap();
    assert!(!saved.enabled());
    assert!(!saved.schedule().unwrap().enabled());
    assert!(saved.action().require_executable().is_ok());

    let mut vacant = transfer;
    let approved = vacant.approval.as_mut().unwrap();
    approved.existing = None;
    let proof = approved.privileged.as_mut().unwrap();
    proof.first_tree = true;
    proof.inventory = None;
    let reviewed = rsync::Reviewed::restore(&vacant).unwrap();
    assert!(!app.view.rsync_review_requests().unwrap().matches(&reviewed));
    app.update(Message::RsyncAllowVacantServerTree(true));
    assert!(!app.view.rsync_review_requests().unwrap().matches(&reviewed));
    app.update(Message::ScheduleEnabled(true));
    app.update(Message::RsyncStanding(true));
    app.update(Message::RsyncAllowVacantServerTree(true));
    assert!(app.view.rsync_review_requests().unwrap().matches(&reviewed));
    let intent = app.view.draft.rsync_standing_intent().unwrap();
    app.view
        .rsync_review
        .begin("vacant-server-folder".into(), intent);
    app.update(Message::RsyncProgramReviewed(
        "vacant-server-folder".into(),
        Ok(Box::new(reviewed)),
    ));
    app.update(Message::ApproveRsyncProgram);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    let saved = app.selected_job().unwrap();
    let RemoteAction::RsyncUpload(transfer) = saved.action().remote_action().unwrap() else {
        panic!("expected reviewed vacant server folder");
    };
    assert!(transfer.allow_vacant_server_tree_upload);
    assert!(transfer.requires_automatic_receiver(true));
    assert_eq!(
        transfer.existing_destination().unwrap().as_str(),
        "/protected/target/source"
    );
    assert!(saved.action().require_executable().is_ok());
    let mut tampered = transfer.clone();
    tampered
        .approval
        .as_mut()
        .unwrap()
        .privileged
        .as_mut()
        .unwrap()
        .spec
        .path = "/protected/target".into();
    assert!(tampered.validate(true).is_err());
    let mut manual = transfer.clone();
    manual.standing = None;
    assert!(manual.validate(true).is_err());
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn java_passwordless_existing_file_upload_binds_every_grouped_receiver() {
    use kortty_server_archive::privileged_rsync::Operation;
    use kortty_server_archive::rsync_tree::{DeleteScope, Manifest};
    use serde_json::json;

    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = profile.file();
    let id = JobId::new("java-passwordless-existing-file").unwrap();
    let primary = json!({
        "id": 1, "target": {"host": "first.example", "port": 22, "username": "operator"}
    });
    let additional = json!({
        "id": 2, "target": {"host": "second.example", "port": 22, "username": "operator"}
    });
    let action: SshCommand = serde_json::from_value(json!({
        "connection": primary, "additional_connections": [additional],
        "imported_transfer": {"direction": "upload", "java_sources": [r"C:\export\report.txt"],
            "java_target_root": "/protected/reports/", "java_delete": false, "java_sudo": true},
    }))
    .unwrap();
    let graph = JobGraph::new(vec![
        Job::new(id.clone(), "Java protected file", action)
            .unwrap()
            .with_enabled(false),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id));
    app.update(Message::Page(Page::Action));
    app.update(Message::ConfigureImportedRsync { directory: false });
    assert!(app.view.draft.has_imported_transfer());
    app.update(Message::ConfigureImportedPasswordlessRsync {
        directory: false,
        existing: true,
    });
    assert_eq!(app.view.draft.source, Source::RsyncUpload);
    assert!(!app.view.draft.has_imported_transfer());
    let program = root.join("rsync");
    let source = root.join("current-name.txt");
    let first_path = "/protected/reports/report.txt";
    let second_path = "/protected/other/report.txt";
    app.update(Message::Edit(
        Field::RsyncProgram,
        program.to_str().unwrap().into(),
    ));
    app.update(Message::Edit(
        Field::LocalFile,
        source.to_str().unwrap().into(),
    ));
    let defaults = app
        .view
        .rsync_review_requests()
        .unwrap()
        .privileged_targets();
    assert!(defaults.iter().all(|(_, spec)| spec.path == first_path));
    app.update(Message::RsyncDestination(2, second_path.into()));
    app.update(Message::RsyncMirror(true));
    app.update(Message::RsyncIncludeSourceName(true));
    let requests = app.view.rsync_review_requests().unwrap();
    assert!(requests.existing_targets().is_empty());
    let targets = requests.privileged_targets();
    assert_eq!(targets.len(), 2);
    for ((_, spec), destination) in targets.iter().zip([first_path, second_path]) {
        assert_eq!(spec.path, destination);
        assert_eq!(
            spec.operation,
            Operation::Upload {
                directory: false,
                replace: true,
                delete: DeleteScope::Retain,
            }
        );
    }
    app.update(Message::Save);
    assert!(app.status().unwrap().contains("Review the imported rsync"));

    let mut original = Manifest::default();
    original.add("", Some((5, [4; 32]))).unwrap();
    let original = original.finish();
    let settings = json!({"delete": false, "include_source_name": false});
    let approval = |index: usize, destination: &str, reference, fingerprint: u8| {
        json!({
            "program": program, "digest": vec![8; 32], "reference": reference, "pins": ["SHA256:fixture"],
            "upload": {"source": source, "destination": destination, "bytes": 8, "digest": vec![5; 32]},
            "existing": {"destination": destination, "settings": settings,
                "snapshot": {"inventory": original, "fingerprint": vec![fingerprint; 32]}},
            "privileged": {"spec": targets[index].1,
                "policy": vec![7; 32], "fingerprint": vec![fingerprint; 32], "inventory": original,
                "handoff": "/run/kortty/rsync-handoff"},
        })
    };
    let remote: RemoteAction = serde_json::from_value(json!({
        "kind": "rsync_upload", "program": program, "local": source, "remote": first_path,
        "existing": settings, "elevation": "passwordless",
        "approval": approval(0, first_path, &primary, 2),
        "routes": [{"profile": 2, "destination": second_path,
            "approval": approval(1, second_path, &additional, 3)}],
    }))
    .unwrap();
    let RemoteAction::RsyncUpload(transfer) = remote else {
        panic!("expected protected file upload");
    };
    let grouped = transfer.for_target(1, true).unwrap();
    assert!(grouped.validate(true).is_ok());
    let mut tampered = grouped;
    tampered
        .approval
        .as_mut()
        .unwrap()
        .privileged
        .as_mut()
        .unwrap()
        .fingerprint = [2; 32];
    assert!(tampered.validate(true).is_err());
    tampered.approval = None;
    tampered.elevation = rsync::Elevation::Ordinary;
    assert!(tampered.validate(true).is_err());

    let reviewed = rsync::Reviewed::restore(&transfer).unwrap();
    app.view.rsync_review.begin("protected-file".into(), None);
    app.update(Message::RsyncProgramReviewed(
        "protected-file".into(),
        Ok(Box::new(reviewed)),
    ));
    app.update(Message::ApproveRsyncProgram);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    let saved = app.selected_job().unwrap();
    assert!(!saved.enabled());
    assert!(!saved.schedule().unwrap().enabled());
    assert!(saved.action().require_executable().is_ok());

    app.update(Message::ScheduleEnabled(true));
    app.update(Message::RsyncStanding(true));
    let intent = app.view.draft.rsync_standing_intent().unwrap();
    assert!(intent.is_some());
    let reviewed = rsync::Reviewed::restore(&transfer).unwrap();
    app.view.rsync_review.begin("recurring-file".into(), intent);
    app.update(Message::RsyncProgramReviewed(
        "recurring-file".into(),
        Ok(Box::new(reviewed)),
    ));
    app.update(Message::ApproveRsyncProgram);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    let saved = app.selected_job().unwrap();
    assert!(!saved.enabled());
    assert!(saved.schedule().unwrap().enabled());
    assert!(saved.action().require_executable().is_ok());
    app.runtime
        .rsync_standing
        .saved(saved, &app.runtime.zone, SystemTime::now())
        .unwrap();

    let RemoteAction::RsyncUpload(mut create) = saved.action().remote_action().unwrap().clone()
    else {
        panic!("expected reviewed regular file upload");
    };
    let vacant = create.routes[0].approval.as_mut().unwrap();
    vacant.existing = None;
    let proof = vacant.privileged.as_mut().unwrap();
    proof.first_file = true;
    proof.inventory = None;
    let reviewed = rsync::Reviewed::restore(&create).unwrap();
    assert!(!app.view.rsync_review_requests().unwrap().matches(&reviewed));
    assert!(create.require_group_review(true).is_err());
    create.allow_vacant_server_file_upload = true;
    assert!(create.for_target(1, true).unwrap().validate(true).is_ok());
    assert!(create.require_group_review(true).is_ok());
    assert!(create.requires_automatic_receiver(true));
    let mut manual = create.clone();
    manual.standing = None;
    assert!(manual.validate(true).is_err());

    app.update(Message::RsyncAllowVacantServerFile(true));
    assert!(app.view.rsync_review_requests().unwrap().matches(&reviewed));
    let intent = app.view.draft.rsync_standing_intent().unwrap();
    app.view
        .rsync_review
        .begin("first-server-file".into(), intent);
    app.update(Message::RsyncProgramReviewed(
        "first-server-file".into(),
        Ok(Box::new(reviewed)),
    ));
    app.update(Message::ApproveRsyncProgram);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    let saved = app.selected_job().unwrap();
    assert!(saved.action().require_executable().is_ok());
    let serialized = serde_json::to_value(saved.action()).unwrap();
    assert_eq!(
        serialized["remote"]["allow_vacant_server_file_upload"],
        true
    );
    assert_eq!(
        serialized["remote"]["routes"][0]["approval"]["privileged"]["first_file"],
        true
    );
    assert!(
        serialized["remote"]["approval"]["privileged"]
            .get("first_file")
            .is_none()
    );
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn ordinary_recurring_file_upload_reviews_java_group_destinations() {
    use kortty_server_archive::rsync_tree::Manifest;
    use serde_json::json;

    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = profile.file();
    let id = JobId::new("java-ordinary-recurring-upload").unwrap();
    let primary =
        json!({"id": 1, "target": {"host": "first.example", "port": 22, "username": "operator"}});
    let additional =
        json!({"id": 2, "target": {"host": "second.example", "port": 22, "username": "operator"}});
    let action: SshCommand = serde_json::from_value(json!({
        "connection": primary, "additional_connections": [additional],
        "imported_transfer": {"direction": "upload", "java_sources": [r"C:\export\report.txt"],
            "java_target_root": "/reports/", "java_delete": false, "java_sudo": false},
    }))
    .unwrap();
    let graph = JobGraph::new(vec![
        Job::new(id.clone(), "Java recurring file upload", action)
            .unwrap()
            .with_enabled(false),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    let mut app = TestWindow::open(path).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id));
    app.update(Message::Page(Page::Action));
    app.update(Message::ConfigureImportedRsync { directory: false });
    let program = root.join("rsync");
    let source = root.join("replacement.txt");
    app.update(Message::Edit(
        Field::RsyncProgram,
        program.to_str().unwrap().into(),
    ));
    app.update(Message::Edit(
        Field::LocalFile,
        source.to_str().unwrap().into(),
    ));

    let destination = "/reports/report.txt";
    let settings = json!({"delete": false, "include_source_name": false});
    let mut original = Manifest::default();
    original.add("", Some((4, [5; 32]))).unwrap();
    let approval = |reference, receiver| {
        json!({
            "program": program, "digest": vec![8; 32], "reference": reference, "pins": ["SHA256:fixture"],
            "upload": {"source": source, "destination": destination, "bytes": 8, "digest": vec![7; 32]},
            "ordinary_receiver": {"destination": destination, "directory": false, "settings": settings,
                "receiver": receiver},
        })
    };
    let mut transfer: rsync::Transfer = serde_json::from_value(json!({
        "program": program, "local": source, "remote": destination, "existing": settings,
        "allow_vacant_server_file_upload": true,
        "standing": {"settings": rsync::standing::Settings::default()},
        "approval": approval(&primary, json!({"kind": "vacant", "fingerprint": vec![4; 32]})),
        "routes": [{"profile": 2, "destination": destination, "approval": approval(&additional,
            json!({"kind": "existing", "snapshot": {"inventory": original.finish(), "fingerprint": vec![6; 32]}}))}],
    })).unwrap();
    let reviewed = rsync::Reviewed::restore(&transfer).unwrap();
    assert!(!app.view.rsync_review_requests().unwrap().matches(&reviewed));
    app.update(Message::ScheduleEnabled(true));
    app.update(Message::RsyncStanding(true));
    app.update(Message::RsyncAllowVacantServerFile(true));
    let requests = app.view.rsync_review_requests().unwrap();
    assert!(requests.privileged_targets().is_empty());
    let targets = requests.existing_targets();
    assert_eq!(targets.len(), 2);
    assert!(targets.iter().all(|(_, path)| path.as_str() == destination));
    assert!(requests.matches(&reviewed));
    assert!(transfer.require_group_review(true).is_ok());

    let intent = app.view.draft.rsync_standing_intent().unwrap();
    app.view
        .rsync_review
        .begin("ordinary-server-file".into(), intent);
    app.update(Message::RsyncProgramReviewed(
        "ordinary-server-file".into(),
        Ok(Box::new(reviewed)),
    ));
    app.update(Message::ApproveRsyncProgram);
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    let saved = app.selected_job().unwrap();
    assert!(saved.action().require_executable().is_ok());
    let stored: serde_json::Value = serde_json::to_value(saved.action()).unwrap();
    assert_eq!(
        stored["remote"]["approval"]["ordinary_receiver"]["receiver"]["kind"],
        "vacant"
    );
    assert_eq!(
        stored["remote"]["routes"][0]["approval"]["ordinary_receiver"]["receiver"]["kind"],
        "existing"
    );

    transfer.standing = None;
    assert!(transfer.validate(true).is_err());
    let forged = transfer.routes[0].approval.as_mut().unwrap();
    forged.ordinary_receiver = None;
    assert!(transfer.require_group_review(true).is_err());
}

#[test]
fn java_mixed_rsync_adoption_preserves_order_slash_and_delete_until_review() {
    verify_java_mixed_rsync_adoption(false);
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn java_mixed_passwordless_rsync_adoption_requires_all_ordered_protected_sources() {
    verify_java_mixed_rsync_adoption(true);
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn verify_java_mixed_rsync_adoption(elevated: bool) {
    use kortty_server_archive::privileged_rsync::{Operation, Review, Spec};
    use kortty_server_archive::rsync_tree::Manifest;
    use kortty_server_archive::rsync_tree::projection::{self, Entry, Placement, Source as Input};
    use serde_json::json;

    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = profile.file();
    let connection =
        json!({"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}});
    let sources = ["/srv/README", "/srv/project", "/srv/shared/"];
    let action: SshCommand = serde_json::from_value(json!({
        "connection": connection,
        "imported_transfer": {"direction": "download", "java_sources": sources,
            "java_target_root": "/java-only", "java_delete": true, "java_sudo": elevated},
    }))
    .unwrap();
    let id = JobId::new("java-mixed-rsync").unwrap();
    let graph = JobGraph::new(vec![
        Job::new(id.clone(), "Java mixed rsync", action)
            .unwrap()
            .with_enabled(false),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();
    let mut app = TestWindow::open(path.clone()).unwrap();
    app.runtime.set_execution_allowed(true);
    app.update(Message::Select(id.clone()));
    app.update(Message::Page(Page::Action));
    app.update(Message::ScheduleEnabled(true));
    if elevated {
        app.update(Message::ConfigureImportedMixedRsync);
        assert!(app.view.draft.has_imported_transfer());
        app.update(Message::ConfigureImportedPasswordlessMixedRsync);
    } else {
        app.update(Message::ConfigureImportedMixedRsync);
    }
    assert_eq!(app.view.draft.source, Source::RsyncDownloadTree);
    let program = root.join("rsync");
    let local = root.join("missing-parent/receiver");
    app.update(Message::Edit(
        Field::RsyncProgram,
        program.to_str().unwrap().into(),
    ));
    app.update(Message::Edit(
        Field::LocalFile,
        local.to_str().unwrap().into(),
    ));
    app.update(Message::RsyncMixed(false));
    app.update(Message::RsyncMirror(false));
    let requests = app.view.rsync_review_requests().unwrap();
    let targets = requests.mixed_targets();
    assert_eq!(targets.len(), 1);
    let target = &targets[0];
    assert_eq!(target.sources, sources);
    assert!(target.delete);
    assert!(!target.review_vacant);
    let elevation = if elevated {
        rsync::Elevation::Passwordless
    } else {
        rsync::Elevation::Ordinary
    };
    assert_eq!(target.elevation, elevation);
    assert!(matches!(&target.destination, rsync::MixedDestination::Local(path) if path == &local));
    app.update(Message::Save);
    assert!(app.status().unwrap().contains("Review the imported rsync"));

    let inputs = [
        Input {
            placement: Placement::File("README".into()),
            entries: vec![Entry {
                relative: "".into(),
                file: Some((6, [1u8; 32])),
            }],
        },
        Input {
            placement: Placement::Named("project".into()),
            entries: Vec::new(),
        },
        Input {
            placement: Placement::Contents,
            entries: Vec::new(),
        },
    ];
    let projection = projection::project(&inputs, true).unwrap();
    let privileged = elevated.then(|| {
        let mut file = Manifest::default();
        file.add("", Some((6, [1; 32]))).unwrap();
        let empty = Manifest::default().finish();
        let reviews = sources
            .iter()
            .zip([file.finish(), empty, empty])
            .enumerate()
            .map(|(index, (path, inventory))| Review {
                spec: Spec {
                    path: path.trim_end_matches('/').into(),
                    operation: Operation::Download {
                        directory: index != 0,
                    },
                },
                policy: [3; 32],
                fingerprint: [5; 32],
                inventory: Some(inventory),
                handoff: "/run/kortty/rsync-handoff".into(),
                first_file: false,
                first_tree: false,
            })
            .collect();
        rsync::MixedPrivilegedReview::Download(reviews)
    });
    let review = rsync::MixedReview {
        plan: rsync::MixedPlan::from_projection(target, &inputs, &projection).unwrap(),
        receiver: rsync::MixedReceiver::Local {
            destination: local.clone(),
            snapshot: None,
            vacant: None,
        },
        privileged,
        ordinary_receiver: None,
    };
    assert!(target.matches(&review));
    if elevated {
        let mut changed = review.clone();
        let Some(rsync::MixedPrivilegedReview::Download(proofs)) = &mut changed.privileged else {
            panic!("expected an ordered protected review");
        };
        proofs.swap(0, 1);
        assert!(!target.matches(&changed));
        let mut ordinary = review.clone();
        ordinary.privileged = None;
        assert!(!target.matches(&ordinary));
    }
    let remote: RemoteAction = serde_json::from_value(json!({
        "kind": "rsync_download", "program": program, "local": local, "remote": "/srv/README", "directory": true,
        "elevation": elevation,
        "existing_download": {"delete": true, "include_source_name": false}, "mixed": sources,
        "approval": {"program": program, "digest": vec![8u8; 32], "reference": connection,
            "pins": ["SHA256:fixture"], "mixed": review},
    })).unwrap();
    let RemoteAction::RsyncDownload(transfer) = remote else {
        panic!("expected rsync download")
    };
    let mut recurring = transfer.clone();
    recurring.standing = Some(rsync::standing::Config {
        settings: rsync::standing::Settings::default(),
        approval: None,
    });
    assert!(recurring.validate(false).is_err());
    let reviewed = rsync::Reviewed::restore(&transfer).unwrap();
    app.view.rsync_review.begin("mixed".into(), None);
    app.update(Message::RsyncProgramReviewed(
        "mixed".into(),
        Ok(Box::new(reviewed)),
    ));
    app.update(Message::ApproveRsyncProgram);
    app.update(Message::Save);
    drop(app);

    let mut reopened = TestWindow::open(path).unwrap();
    reopened.update(Message::Select(id));
    let saved = reopened.selected_job().unwrap();
    assert!(!saved.enabled());
    assert!(!saved.schedule().unwrap().enabled());
    assert!(saved.action().require_executable().is_ok());
    let RemoteAction::RsyncDownload(transfer) = saved.action().remote_action().unwrap() else {
        panic!("expected mixed job")
    };
    assert_eq!(transfer.mixed.as_ref().unwrap(), &sources);
    assert_eq!(transfer.elevation, elevation);

    let mut recurring = transfer.clone();
    let mixed_review = recurring.approval.as_mut().unwrap().mixed.as_mut().unwrap();
    let rsync::MixedReceiver::Local { vacant, .. } = &mut mixed_review.receiver else {
        panic!("expected a vacant local tree review");
    };
    *vacant = Some(serde_json::from_value(json!({
        "fingerprint": vec![6; 32],
        "missing_parents": 1,
        "location": {"mount": [1, 2], "alias": {"parent": vec![7; 32], "name": {"literal": vec![8; 32]}}}
    })).unwrap());
    assert!(recurring.validate(false).is_err());

    reopened.runtime.set_execution_allowed(true);
    reopened.update(Message::Page(Page::Action));
    reopened.update(Message::RsyncAllowVacantDirectory(true));
    let targets = reopened
        .view
        .rsync_review_requests()
        .unwrap()
        .mixed_targets();
    assert!(targets[0].review_vacant);
    assert!(targets[0].matches(recurring.approval.as_ref().unwrap().mixed.as_ref().unwrap()));
    recurring.allow_vacant_directory_download = true;
    assert!(recurring.validate(false).is_ok());
    assert!(!recurring.requires_automatic_receiver(false));
    let approved = rsync::Reviewed::restore(&recurring).unwrap();
    for recurring in [false, true] {
        if recurring {
            reopened.update(Message::ScheduleEnabled(true));
            reopened.update(Message::RsyncStanding(true));
        }
        let intent = reopened.view.draft.rsync_standing_intent().unwrap();
        reopened
            .view
            .rsync_review
            .begin("checked-mixed".into(), intent);
        reopened.update(Message::RsyncProgramReviewed(
            "checked-mixed".into(),
            Ok(Box::new(approved.clone())),
        ));
        reopened.update(Message::ApproveRsyncProgram);
        reopened.update(Message::Save);
        assert_eq!(reopened.status(), Some("Job saved"));
        let saved = reopened.selected_job().unwrap();
        assert_eq!(saved.schedule().unwrap().enabled(), recurring);
        assert!(saved.action().require_executable().is_ok());
        if recurring {
            reopened
                .runtime
                .rsync_standing
                .saved(saved, &reopened.runtime.zone, SystemTime::now())
                .unwrap();
        }
    }
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn java_rsync_existing_file_download_prefills_the_original_name_and_requires_exact_review() {
    use kortty_server_archive::rsync_tree::Manifest;
    use serde_json::json;

    for elevated in [false, true] {
        let profile = Profile::temporary();
        let root = profile.0.canonicalize().unwrap();
        let path = profile.file();
        let id = JobId::new("java-existing-rsync-local-file").unwrap();
        let connection = json!({"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}});
        let server_path = "/site/reports/report.txt";
        let receiver = root.join("receiver");
        let destination = receiver.join("report.txt");
        let program = root.join("rsync");
        let action: SshCommand = serde_json::from_value(json!({
            "connection": connection,
            "imported_transfer": {"direction": "download", "java_sources": [server_path],
                "java_target_root": receiver, "java_delete": false, "java_sudo": elevated},
        }))
        .unwrap();
        let graph = JobGraph::new(vec![
            Job::new(id.clone(), "Java existing receiver file", action)
                .unwrap()
                .with_enabled(false),
        ])
        .unwrap();
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&graph, &[], &[])
            .unwrap();
        let mut app = TestWindow::open(path).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id));
        app.update(Message::Page(Page::Action));
        app.update(if elevated {
            Message::ConfigureImportedPasswordlessRsync {
                directory: false,
                existing: true,
            }
        } else {
            Message::ConfigureImportedExistingRsyncFile
        });
        assert_eq!(app.view.draft.source, Source::RsyncDownload);
        app.update(Message::Edit(
            Field::RsyncProgram,
            program.to_str().unwrap().into(),
        ));
        let routes = app.view.rsync_review_requests().unwrap();
        if elevated {
            assert_eq!(routes.privileged_targets()[0].1.path, server_path);
        } else {
            assert_eq!(routes.local_existing_targets()[0].1, destination);
        }
        app.update(Message::Edit(
            Field::LocalFile,
            receiver.join("renamed.txt").to_str().unwrap().into(),
        ));
        assert!(app.view.rsync_review_requests().is_none());
        app.update(Message::Edit(
            Field::LocalFile,
            destination.to_str().unwrap().into(),
        ));
        app.update(Message::Save);
        assert!(app.status().unwrap().contains("Review the imported rsync"));

        let mut inventory = Manifest::default();
        inventory.add("", Some((4, [1; 32]))).unwrap();
        let inventory = inventory.finish();
        let privileged = elevated.then(|| json!({
            "spec": {"path": server_path, "operation": {"direction": "download", "directory": false}},
            "policy": vec![5; 32], "fingerprint": vec![6; 32], "inventory": inventory,
            "handoff": "/run/kortty/rsync-handoff",
        }));
        let remote: RemoteAction = serde_json::from_value(json!({
            "kind": "rsync_download", "program": program, "local": destination, "remote": server_path,
            "elevation": if elevated { "passwordless" } else { "ordinary" },
            "existing_download": {"delete": false, "include_source_name": false},
            "approval": {"program": program, "digest": vec![8; 32], "reference": connection,
                "pins": ["SHA256:fixture"], "privileged": privileged,
                "download": {"source": server_path, "destination": destination},
                "existing_file_download": {"destination": destination, "snapshot": {
                    "inventory": inventory, "fingerprint": vec![2; 32], "root_identity": vec![3; 32]
                }},
            },
        }))
        .unwrap();
        let RemoteAction::RsyncDownload(transfer) = remote else {
            panic!("expected existing local file download");
        };
        assert!(transfer.validate(false).is_ok());
        let mut without_proof = transfer.clone();
        without_proof
            .approval
            .as_mut()
            .unwrap()
            .existing_file_download = None;
        assert!(without_proof.validate(false).is_err());
        app.update(Message::ScheduleEnabled(true));
        app.update(Message::RsyncStanding(true));
        let intent = app.view.draft.rsync_standing_intent().unwrap();
        let approval = rsync::Reviewed::restore(&transfer).unwrap();
        app.view
            .rsync_review
            .begin("recurring-local-file".into(), intent);
        app.update(Message::RsyncProgramReviewed(
            "recurring-local-file".into(),
            Ok(Box::new(approval)),
        ));
        app.update(Message::ApproveRsyncProgram);
        app.update(Message::Save);
        assert_eq!(app.status(), Some("Job saved"));
        let saved = app.selected_job().unwrap();
        assert!(!saved.enabled());
        assert!(saved.action().require_executable().is_ok());
        app.runtime
            .rsync_standing
            .saved(saved, &app.runtime.zone, SystemTime::now())
            .unwrap();
    }
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn java_existing_rsync_download_retains_placement_and_mirror_through_local_review() {
    use serde_json::json;

    let profile = Profile::temporary();
    let root = profile.0.canonicalize().unwrap();
    let path = profile.file();
    let connection =
        json!({"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}});
    let program = root.join("rsync");
    let destination = root.join("missing-parent/receiver");
    for (suffix, include_source_name) in [("", true), ("/", false)] {
        let id = JobId::new("java-existing-rsync-download").unwrap();
        let source = format!("/site/documents{suffix}");
        let action: SshCommand = serde_json::from_value(json!({
            "connection": connection,
            "imported_transfer": {"direction": "download", "java_sources": [source],
                "java_target_root": "/java-only", "java_delete": true, "java_sudo": false},
        }))
        .unwrap();
        let graph = JobGraph::new(vec![
            Job::new(id.clone(), "Java rsync download", action)
                .unwrap()
                .with_enabled(false),
        ])
        .unwrap();
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&graph, &[], &[])
            .unwrap();
        let mut app = TestWindow::open(path.clone()).unwrap();
        app.runtime.set_execution_allowed(true);
        app.update(Message::Select(id.clone()));
        app.update(Message::Page(Page::Action));
        app.update(Message::ScheduleEnabled(true));
        app.update(Message::ConfigureImportedExistingRsync);
        assert_eq!(app.view.draft.source, Source::RsyncDownloadTree);
        assert!(app.view.rsync_review_requests().is_none());
        app.update(Message::Edit(
            Field::LocalFile,
            destination.to_str().unwrap().into(),
        ));
        app.update(Message::Edit(
            Field::RsyncProgram,
            program.to_str().unwrap().into(),
        ));
        app.update(Message::RsyncExisting(false));
        app.update(Message::RsyncMirror(false));
        let effective = if include_source_name {
            destination.join("documents")
        } else {
            destination.clone()
        };
        let requests = app.view.rsync_review_requests().unwrap();
        let routes = requests.local_existing_targets();
        assert_eq!(routes[0].1, effective);
        app.update(Message::Save);
        assert!(app.status().unwrap().contains("Review the imported rsync"));
        assert!(
            app.selected_job()
                .unwrap()
                .action()
                .imported_transfer
                .is_some()
        );

        let settings = json!({"delete": true, "include_source_name": include_source_name});
        let inventory = kortty_server_archive::rsync_tree::Manifest::default().finish();
        let remote: RemoteAction = serde_json::from_value(json!({
            "kind": "rsync_download", "program": program, "local": destination, "remote": "/site/documents",
            "directory": true, "existing_download": settings,
            "approval": {"program": program, "digest": vec![8u8; 32], "reference": connection,
                "pins": ["SHA256:fixture"], "directory": {"source": "/site/documents", "destination": destination},
                "existing_download": {"destination": effective, "settings": settings,
                    "snapshot": {"inventory": inventory, "fingerprint": vec![4u8; 32], "root_identity": vec![5u8; 32]}}},
        })).unwrap();
        let RemoteAction::RsyncDownload(transfer) = remote else {
            panic!("expected rsync download")
        };
        let reviewed = rsync::Reviewed::restore(&transfer).unwrap();
        app.view.rsync_review.begin("existing-local".into(), None);
        app.update(Message::RsyncProgramReviewed(
            "existing-local".into(),
            Ok(Box::new(reviewed)),
        ));
        app.update(Message::ApproveRsyncProgram);
        app.update(Message::Save);

        assert!(
            app.selected_job()
                .unwrap()
                .action()
                .imported_transfer
                .is_none(),
            "{:?}",
            app.status()
        );
        drop(app);
        let mut reopened = TestWindow::open(path.clone()).unwrap();
        reopened.update(Message::Select(id));
        let saved = reopened.selected_job().unwrap();
        assert!(!saved.enabled());
        assert!(!saved.schedule().unwrap().enabled());
        let RemoteAction::RsyncDownload(transfer) = saved.action().remote_action().unwrap() else {
            panic!("expected approved native rsync download")
        };
        assert!(transfer.existing.is_none());
        assert!(transfer.existing_download.unwrap().delete);
        assert_eq!(
            transfer.existing_download.unwrap().include_source_name,
            include_source_name
        );
        assert_eq!(transfer.existing_local_destination().unwrap(), effective);
        assert!(saved.action().require_executable().is_ok());

        let serialized = json!(transfer);
        assert!(serialized.get("allow_vacant_directory_download").is_none());
        assert!(
            serialized["approval"]
                .get("vacant_directory_download")
                .is_none()
        );
        let mut vacant = transfer.clone();
        let approval = vacant.approval.as_mut().unwrap();
        let existing = approval.existing_download.take().unwrap();
        approval.vacant_directory_download = Some(Box::new(rsync::LocalVacantDirectoryReview {
            destination: existing.destination,
            settings: existing.settings,
            snapshot: serde_json::from_value(json!({
                "fingerprint": vec![6; 32],
                "missing_parents": 1 + u8::from(include_source_name),
                "location": {"mount": [1, 2], "alias": {"parent": vec![7; 32], "name": {"literal": vec![8; 32]}}}
            })).unwrap(),
        }));
        let reviewed = rsync::Reviewed::restore(&vacant).unwrap();
        assert!(vacant.validate(false).is_err());
        vacant.allow_vacant_directory_download = true;
        assert!(vacant.validate(false).is_ok());
        assert!(vacant.validate(true).is_err());
        let mut unsafe_program = vacant.clone();
        unsafe_program.program = root.join("missing-parent/bin/rsync");
        unsafe_program.approval.as_mut().unwrap().program = unsafe_program.program.clone();
        assert!(unsafe_program.validate(false).is_err());
        let mut invalid_depth = json!(vacant);
        invalid_depth["approval"]["vacant_directory_download"]["snapshot"]["missing_parents"] =
            json!(255);
        let invalid_depth: rsync::Transfer = serde_json::from_value(invalid_depth).unwrap();
        assert!(invalid_depth.validate(false).is_err());

        reopened.runtime.set_execution_allowed(true);
        reopened.update(Message::Page(Page::Action));
        assert!(
            !reopened
                .view
                .rsync_review_requests()
                .unwrap()
                .matches(&reviewed)
        );
        reopened.update(Message::RsyncAllowVacantDirectory(true));
        let requests = reopened.view.rsync_review_requests().unwrap();
        assert_eq!(requests.local_existing_targets()[0].1, effective);
        assert!(requests.matches(&reviewed));
        for recurring in [false, true] {
            if recurring {
                reopened.update(Message::ScheduleEnabled(true));
                reopened.update(Message::RsyncStanding(true));
            }
            let intent = reopened.view.draft.rsync_standing_intent().unwrap();
            reopened
                .view
                .rsync_review
                .begin("vacant-local".into(), intent);
            reopened.update(Message::RsyncProgramReviewed(
                "vacant-local".into(),
                Ok(Box::new(reviewed.clone())),
            ));
            reopened.update(Message::ApproveRsyncProgram);
            reopened.update(Message::Save);
            assert_eq!(reopened.status(), Some("Job saved"));
            let saved = reopened.selected_job().unwrap();
            let RemoteAction::RsyncDownload(transfer) = saved.action().remote_action().unwrap()
            else {
                panic!("expected approved native rsync download")
            };
            assert!(transfer.allow_vacant_directory_download);
            assert_eq!(transfer.standing.is_some(), recurring);
            assert_eq!(transfer.existing_local_destination().unwrap(), effective);
            assert!(saved.action().require_executable().is_ok());
            if recurring {
                reopened
                    .runtime
                    .rsync_standing
                    .saved(saved, &reopened.runtime.zone, SystemTime::now())
                    .unwrap();
            }
        }
    }
}

#[test]
fn native_form_reopens_each_sftp_action_and_honors_the_scheduler_policy() {
    use crate::session_journal::automation::{Closing, Keep};
    use automation_journal::Event;

    for (source, kind) in [
        (Source::RemoteMkdir, "mkdir"),
        (Source::RemoteRename, "rename"),
        (Source::RemoteChmod, "chmod"),
        (Source::RemoteChmod, "chmod_symbolic"),
        (Source::RemoteChown, "chown"),
        (Source::RemoteCopy, "copy_remote"),
        (Source::RemoteUpload, "upload"),
        (Source::RemoteDownload, "download"),
        (Source::RemoteUploadFolder, "upload"),
        (Source::RemoteDownloadFolder, "download"),
    ] {
        let profile = Profile::temporary();
        let path = profile.file();
        let mut window = TestWindow::open(path.clone()).unwrap();
        window.update(Message::VirtualTerminal(true));
        window.update(Message::CommandSource(source));
        for event in [
            Event::Enabled(true),
            Event::Input(true),
            Event::Keep(Keep::Failures),
            Event::Closing(Closing::Failures),
        ] {
            window.update(Message::AutomationJournal(event));
        }
        for (field, value) in [
            (Field::Name, "SFTP metadata".to_owned()),
            (Field::Host, "example.test".to_owned()),
            (Field::Username, "operator".to_owned()),
            (Field::KeyFile, profile.0.join("key").display().to_string()),
            (Field::RemotePath, "/canonical/remote path".to_owned()),
            (Field::RemoteDestination, "/canonical/renamed".to_owned()),
            (
                Field::RemoteMode,
                if kind == "chmod_symbolic" {
                    "u+rw,o-w"
                } else {
                    "0640"
                }
                .to_owned(),
            ),
            (Field::RemoteUid, "1001".to_owned()),
            (
                Field::LocalFile,
                profile.0.join("configured-file").display().to_string(),
            ),
        ] {
            window.update(Message::Edit(field, value));
        }
        window.update(Message::Save);
        assert_eq!(window.status(), Some("Job saved"));
        let action = window.selected_job().unwrap().action().clone();
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["remote"]["kind"], kind);
        assert_eq!(
            json["remote"]["folder"].as_bool().unwrap_or(false),
            matches!(
                source,
                Source::RemoteUploadFolder | Source::RemoteDownloadFolder
            )
        );
        assert!(action.virtual_terminal.is_none());
        assert!(action.command_for_exec(None).is_err());
        let options = action.automation_journal.as_ref().unwrap();
        assert!(options.enabled && options.capture_input);
        assert_eq!(options.keep, Keep::Failures);
        assert_eq!(options.closing, Closing::Failures);
        drop(window);

        let mut window = TestWindow::open(path).unwrap();
        assert_eq!(window.view.draft.source, source);
        let estimate = window.view.draft.automation_estimate_subject().unwrap();
        assert_eq!(estimate.targets, 1);
        assert_eq!(estimate.mode, (Closing::Failures, Keep::Failures));
        window.update(Message::Save);
        assert_eq!(window.selected_job().unwrap().action(), &action);
        assert!(window.update(Message::Run(false)).is_empty());
        window.runtime.set_execution_allowed(true);
        let ticket = window.update(Message::Run(false)).pop().unwrap();
        assert_eq!(ticket.action.as_ref(), &action);
    }
}

#[test]
fn automation_policy_preserves_scheduler_options_and_invalidates_estimates() {
    use crate::session_journal::automation::Closing;
    use automation_journal::Event;

    let profile = Profile::temporary();
    let mut window = TestWindow::open(profile.file()).unwrap();
    window.update(Message::Page(Page::Action));
    window.update(Message::AutomationJournal(Event::Enabled(true)));
    window.update(Message::AutomationJournal(Event::Closing(Closing::Always)));
    let options = window.view.draft.automation_journal.build().unwrap();
    let old = window
        .runtime
        .begin_automation_estimate(&mut window.view)
        .unwrap();

    window.view.set_automation_journal_allowed(false);
    window.update(Message::AutomationJournal(Event::Enabled(false)));
    assert_eq!(
        window.view.draft.automation_journal.build().unwrap(),
        options
    );
    assert!(
        window
            .runtime
            .begin_automation_estimate(&mut window.view)
            .is_none()
    );

    window.view.set_automation_journal_allowed(true);
    let current = window
        .runtime
        .begin_automation_estimate(&mut window.view)
        .unwrap();
    window
        .view
        .finish_automation_estimate(old.ticket, AutomationEstimate::unavailable());
    assert!(
        window
            .runtime
            .begin_automation_estimate(&mut window.view)
            .is_none()
    );
    window
        .view
        .finish_automation_estimate(current.ticket, AutomationEstimate::unavailable());
    assert!(
        window
            .runtime
            .begin_automation_estimate(&mut window.view)
            .is_some()
    );
}

#[test]
fn scheduled_delete_requires_exact_confirmation_after_scope_or_endpoint_changes() {
    let profile = Profile::temporary();
    let path = profile.file();
    let mut window = TestWindow::open(path.clone()).unwrap();
    window.update(Message::CommandSource(Source::RemoteDelete));
    let target = "/canonical/stale output";
    for (field, value) in [
        (Field::Name, "Remove stale output".into()),
        (Field::Host, "example.test".into()),
        (Field::Username, "operator".into()),
        (Field::KeyFile, profile.0.join("key").display().to_string()),
        (Field::RemotePath, target.into()),
    ] {
        window.update(Message::Edit(field, value));
    }
    let save_without_confirmation = |window: &mut TestWindow| {
        window.update(Message::Save);
        assert_eq!(
            window.status(),
            Some("Repeat the exact remote path to authorize deletion")
        );
    };
    let confirm_and_save = |window: &mut TestWindow| {
        window.update(Message::Edit(
            Field::RemoteDeleteConfirmation,
            target.into(),
        ));
        window.update(Message::Save);
        assert_eq!(window.status(), Some("Job saved"));
    };
    save_without_confirmation(&mut window);
    confirm_and_save(&mut window);
    assert!(matches!(
        window.selected_job().unwrap().action().remote_action(),
        Ok(RemoteAction::Delete {
            recursive: false,
            ..
        })
    ));
    drop(window);

    let mut window = TestWindow::open(path).unwrap();
    window.update(Message::Save);
    assert_eq!(window.status(), Some("Job saved"));
    window.update(Message::RecursiveDelete(true));
    save_without_confirmation(&mut window);
    confirm_and_save(&mut window);
    assert!(matches!(
        window.selected_job().unwrap().action().remote_action(),
        Ok(RemoteAction::Delete {
            recursive: true,
            ..
        })
    ));

    window.update(Message::Edit(Field::RemotePath, "/other/path".into()));
    window.update(Message::Edit(Field::RemotePath, target.into()));
    save_without_confirmation(&mut window);
    confirm_and_save(&mut window);
    window.update(Message::Edit(Field::Host, "other.test".into()));
    save_without_confirmation(&mut window);
}

#[test]
fn native_archive_form_saves_and_reopens_sources_compression_and_optional_local_copy() {
    let profile = Profile::temporary();
    let path = profile.file();
    let mut window = TestWindow::open(path.clone()).unwrap();
    window.update(Message::CommandSource(Source::RemoteArchive));
    window.update(Message::ArchiveFormat(ArchiveFormat::TarBz2));
    for (field, value) in [
        (Field::Name, "Remote backup".into()),
        (Field::Host, "example.test".into()),
        (Field::Username, "operator".into()),
        (Field::KeyFile, profile.0.join("key").display().to_string()),
        (Field::RemotePath, "/canonical/result.tar.bz2".into()),
        (
            Field::ArchiveDownload,
            profile.0.join("local copy.tar.bz2").display().to_string(),
        ),
        (Field::ArchiveLevel, "7".into()),
    ] {
        window.update(Message::Edit(field, value));
    }
    window.update(Message::EditArchiveSources(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("/canonical/one file\n/canonical/second".into())),
    )));
    window.update(Message::EditArchiveExcludes(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new(" *.tmp \ncache/?.txt\n".into())),
    )));
    window.update(Message::Save);
    assert_eq!(window.status(), Some("Job saved"));
    let action = window.selected_job().unwrap().action().clone();
    let remote = serde_json::to_value(&action).unwrap()["remote"].clone();
    assert_eq!(remote["kind"], "archive");
    assert_eq!(remote["format"], "tar_bz2");
    assert_eq!(
        remote["excludes"],
        serde_json::json!(["*.tmp", "cache/?.txt"])
    );
    assert_eq!(
        remote["sources"],
        serde_json::json!(["/canonical/one file", "/canonical/second"])
    );
    assert_eq!(remote["level"], 7);
    assert!(action.remote_action().unwrap().transfers_files());
    drop(window);

    let mut window = TestWindow::open(path).unwrap();
    assert_eq!(window.view.draft.source, Source::RemoteArchive);
    assert_eq!(window.view.draft.archive_format, ArchiveFormat::TarBz2);
    window.update(Message::Save);
    assert_eq!(window.selected_job().unwrap().action(), &action);
    window.update(Message::Edit(Field::ArchiveLevel, "0".into()));
    window.update(Message::Save);
    assert_eq!(
        window.status(),
        Some("Choose ZIP compression 0–9 or bzip2 compression 1–9")
    );

    for sources in [
        vec!["/a/report", "/b/report"],
        vec!["/canonical/result.tar.bz2"],
        vec![],
    ] {
        let mut invalid = remote.clone();
        invalid["sources"] = serde_json::json!(sources);
        assert!(
            serde_json::from_value::<RemoteAction>(invalid)
                .unwrap()
                .validate()
                .is_err()
        );
    }

    let mut invalid = remote;
    invalid["excludes"] = serde_json::json!(["unclosed["]);
    assert!(
        serde_json::from_value::<RemoteAction>(invalid)
            .unwrap()
            .validate()
            .is_err()
    );
    let legacy: RemoteAction = serde_json::from_value(serde_json::json!({
        "kind": "archive_zip", "sources": ["/source"], "target": "/target.zip", "level": 6,
    }))
    .unwrap();
    assert!(legacy.validate().is_ok());
    assert!(matches!(legacy, RemoteAction::Archive(Archive {
        format: ArchiveFormat::Zip, excludes, ..
    }) if excludes.is_empty()));
}

#[test]
fn server_archive_form_saves_and_reopens_explicit_output_metadata() {
    use serde_json::json;

    let profile = Profile::temporary();
    let path = profile.file();
    let id = JobId::new("server-archive-output").unwrap();
    let action: SshCommand = serde_json::from_value(json!({
        "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
        "remote": {"kind": "server_archive", "sources": ["/source"], "target": "/target.zip", "level": 6}
    })).unwrap();
    let job = Job::new(id.clone(), "Server archive", action).unwrap();
    let graph = JobGraph::new(vec![job]).unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();

    let mut app = TestWindow::open(path.clone()).unwrap();
    app.update(Message::Select(id.clone()));
    app.update(Message::Page(Page::Action));
    app.update(Message::Edit(Field::ArchiveUid, "1200".into()));
    app.update(Message::Edit(Field::ArchiveGid, "1400".into()));
    app.update(Message::Edit(Field::ArchiveMode, "4755".into()));
    app.update(Message::Save);
    assert!(app.status().unwrap().contains("three octal digits"));
    app.update(Message::Edit(Field::ArchiveMode, "000".into()));
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    let action = app.selected_job().unwrap().action().clone();
    drop(app);

    let mut app = TestWindow::open(path).unwrap();
    app.update(Message::Select(id));
    app.update(Message::Save);
    assert_eq!(app.selected_job().unwrap().action(), &action);
    let RemoteAction::ServerArchive(archive) = action.remote_action().unwrap() else {
        panic!("expected server-only archive job");
    };
    let spec = archive.server_spec().unwrap();
    assert_eq!(
        serde_json::to_value(spec).unwrap()["output"],
        json!({"uid": 1200, "gid": 1400, "mode": 0})
    );
    assert!(!action.remote_action().unwrap().transfers_files());
    assert!(RemoteAction::Archive(archive.clone()).validate().is_err());
    assert!(
        RemoteAction::PrivilegedArchive(privileged::PrivilegedArchive {
            archive: archive.clone(),
            approval: None,
        })
        .validate()
        .is_err()
    );

    let local = profile.0.join("local copy.zip");
    app.update(Message::Edit(
        Field::ArchiveDownload,
        local.to_str().unwrap().into(),
    ));
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    let action = app.selected_job().unwrap().action().clone();
    let remote = action.remote_action().unwrap();
    assert!(remote.transfers_files() && remote.writes_local_file());
    let RemoteAction::ServerArchive(archive) = remote else {
        unreachable!()
    };
    assert_eq!(archive.download.as_deref(), Some(local.as_path()));
    assert!(archive.server_spec().unwrap().report_payload);
    drop(app);

    let mut app = TestWindow::open(profile.file()).unwrap();
    app.update(Message::Save);
    assert_eq!(app.selected_job().unwrap().action(), &action);
}

#[test]
fn server_password_zip_form_saves_only_the_vault_reference() {
    use serde_json::json;

    let profile = Profile::temporary();
    let path = profile.file();
    let job_id = JobId::new("server-password-zip").unwrap();
    let action = serde_json::from_value::<SshCommand>(json!({
        "connection": {"id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}},
        "remote": {"kind": "server_archive", "sources": ["/source"], "target": "/target.zip", "level": 6},
    })).unwrap();
    let job = Job::new(job_id.clone(), "Server password ZIP", action).unwrap();
    let graph = JobGraph::new(vec![job]).unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();

    let mut app = TestWindow::open(path.clone()).unwrap();
    app.update(Message::Select(job_id.clone()));
    app.update(Message::Page(Page::Action));
    app.update(Message::ArchiveFormat(ArchiveFormat::ZipPassword));
    app.update(Message::Save);
    assert_eq!(
        app.status(),
        Some("Choose a ZIP archive password from the vault")
    );
    let id = "ab".repeat(16);
    let choice = CredentialChoice {
        id: id.clone(),
        label: "Archive password".into(),
        username: String::new(),
    };
    app.runtime.set_archive_passwords(vec![choice.clone()]);
    app.update(Message::ArchivePassword(choice));
    let local = profile.0.join("verified copy.zip");
    app.update(Message::Edit(
        Field::ArchiveDownload,
        local.to_str().unwrap().into(),
    ));
    app.update(Message::Save);
    assert_eq!(app.status(), Some("Job saved"));
    drop(app);

    let mut app = TestWindow::open(path).unwrap();
    app.update(Message::Select(job_id));
    let action = app.selected_job().unwrap().action();
    assert_eq!(action.sudo, SudoMode::Disabled);
    let RemoteAction::ServerArchive(archive) = action.remote_action().unwrap() else {
        panic!("expected server archive");
    };
    assert_eq!(archive.password_credential.as_deref(), Some(id.as_str()));
    assert_eq!(archive.download.as_deref(), Some(local.as_path()));
    let spec = archive.server_spec().unwrap();
    assert_eq!(
        spec.format,
        kortty_server_archive::protocol::CreateFormat::ZipPassword
    );
    assert!(spec.report_payload);
    assert!(!serde_json::to_string(&spec).unwrap().contains(&id));
    assert!(
        RemoteAction::PrivilegedArchive(privileged::PrivilegedArchive {
            archive: archive.clone(),
            approval: None,
        })
        .validate()
        .is_ok()
    );
}

#[test]
fn encrypted_zip_form_persists_only_a_vault_reference() {
    use kortty_vault::{CredentialInput, CredentialKind, Secret, Vault};

    let profile = Profile::temporary();
    let path = profile.file();
    let mut vault = Vault::create(profile.0.join("vault"), b"test master password").unwrap();
    let password = "test-only distinct AES ZIP password";
    let id = vault
        .save(
            CredentialInput {
                label: "Backup password".into(),
                kind: CredentialKind::ArchivePassword,
                ..Default::default()
            },
            Secret::new(password.into()),
        )
        .unwrap();
    let mut window = TestWindow::open(path.clone()).unwrap();
    window.update(Message::CommandSource(Source::RemoteArchive));
    window.update(Message::ArchiveFormat(ArchiveFormat::ZipPassword));
    for (field, value) in [
        (Field::Name, "Encrypted backup".into()),
        (Field::Host, "example.test".into()),
        (Field::Username, "operator".into()),
        (Field::KeyFile, profile.0.join("key").display().to_string()),
        (Field::RemotePath, "/target.zip".into()),
    ] {
        window.update(Message::Edit(field, value));
    }
    window.update(Message::EditArchiveSources(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("/source".into())),
    )));
    window.update(Message::Save);
    assert_eq!(
        window.status(),
        Some("Choose a ZIP archive password from the vault")
    );
    let choice = CredentialChoice {
        id: id.clone(),
        label: "Backup password".into(),
        username: String::new(),
    };
    window.runtime.set_archive_passwords(vec![choice.clone()]);
    window.update(Message::ArchivePassword(choice));
    window.update(Message::Save);
    assert_eq!(window.status(), Some("Job saved"));
    drop(window);
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains(&id));
    assert!(!persisted.contains(password));
    let mut reopened = TestWindow::open(path).unwrap();
    assert_eq!(
        reopened.view.draft.archive_password.as_deref(),
        Some(id.as_str())
    );
    reopened.update(Message::ArchiveFormat(ArchiveFormat::Zip));
    reopened.update(Message::Save);
    assert!(matches!(
        reopened.selected_job().unwrap().action().remote_action(),
        Ok(RemoteAction::Archive(Archive {
            format: ArchiveFormat::Zip,
            password_credential: None,
            ..
        }))
    ));
}

#[test]
fn scheduler_persists_a_reviewed_saved_route_without_authentication_details() {
    use crate::connection_store::{Auth, Connection, SavedLogin};
    use crate::ssh_dialog::LoginTarget;

    let profile = Profile::temporary();
    let path = profile.file();
    let connections_path = profile.0.join("connections.json");
    let mut store = ConnectionStore::open(connections_path).unwrap();
    let mut saved = Connection {
        id: 0,
        name: "Target profile".into(),
        tag: String::new(),
        login: SavedLogin {
            preferences: Default::default(),
            endpoint: LoginTarget {
                host: "target.example.test.".into(),
                port: 22,
                username: "operator".into(),
            },
            auth: Auth::VaultPassword("vault-reference-test".into()),
            keyboard_interactive: false,
        },
        jump: None,
        emulation: "xterm-256color".into(),
        tunnels: Vec::new(),
        incognito: false,
        isolation: Default::default(),
        inherit_isolation: false,
        startup: Default::default(),
        logging: Default::default(),
        terminal_effect: None,
        command_timestamps: None,
        tab_color: None,
        paste_warning: None,
        window_geometry: None,
        terminal: Default::default(),
    };
    let id = store.save(saved.clone()).unwrap();
    saved.id = id;
    let reference = ConnectionRef::from(&saved);
    let mut window = TestWindow::open(path.clone()).unwrap();
    window.runtime.set_connections(Some(&store));
    window.update(Message::ConnectionSelected(
        window.runtime.connection_choices[1].clone(),
    ));
    window.update(Message::Edit(Field::Name, "Saved connection job".into()));
    window.update(Message::EditCommand(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("true".into())),
    )));
    window.update(Message::Save);
    assert_eq!(window.status(), Some("Job saved"));
    let action = window.selected_job().unwrap().action();
    assert_eq!(action.connection.as_ref(), Some(&reference));
    let json = serde_json::to_value(action).unwrap();
    assert!(json.get("key_file").is_none());
    assert!(!json.to_string().contains("vault-reference-test"));
    drop(window);

    let mut window = TestWindow::open(path.clone()).unwrap();
    saved.login.endpoint.port = 2222;
    store.save(saved).unwrap();
    window.runtime.set_connections(Some(&store));
    assert_ne!(
        window.runtime.connection_choices[1].reference(),
        Some(&reference)
    );
    window.update(Message::Save);
    assert_eq!(
        window.selected_job().unwrap().action().connection.as_ref(),
        Some(&reference)
    );
    window.update(Message::ConnectionSelected(
        window.runtime.connection_choices[1].clone(),
    ));
    window.update(Message::Save);
    assert_eq!(
        window
            .selected_job()
            .unwrap()
            .action()
            .connection
            .as_ref()
            .unwrap()
            .target
            .port,
        2222
    );

    let mut target = store.get(id).unwrap().clone();
    target.id = 0;
    target.name = "Second target".into();
    target.login.endpoint.host = "second.example.test".into();
    let second = store.save(target.clone()).unwrap();
    target.name = "Third target".into();
    target.login.endpoint.host = "third.example.test".into();
    let third = store.save(target).unwrap();
    window.runtime.set_connections(Some(&store));
    let choice = |window: &TestWindow, id| {
        window
            .runtime
            .connection_choices
            .iter()
            .find(|choice| {
                choice
                    .reference()
                    .is_some_and(|reference| reference.id == id)
            })
            .unwrap()
            .clone()
    };
    window.update(Message::ConnectionAdded(choice(&window, third)));
    window.update(Message::ConnectionAdded(choice(&window, second)));
    window.update(Message::ConnectionAdded(choice(&window, third)));
    window.update(Message::Save);
    assert_eq!(window.status(), Some("Job saved"));
    drop(window);

    let mut window = TestWindow::open(path).unwrap();
    let action = window.selected_job().unwrap().action().clone();
    assert_eq!(
        action
            .targets()
            .map(|reference| reference.id)
            .collect::<Vec<_>>(),
        [id, third, second]
    );
    let mut invalid = action.clone();
    invalid
        .additional_connections
        .push(invalid.connection.clone().unwrap());
    assert!(
        invalid
            .validate_target_selection()
            .unwrap_err()
            .contains("once")
    );
    invalid.additional_connections.pop();
    invalid.virtual_terminal = Some(virtual_terminal::Options::default());
    assert!(
        invalid
            .validate_target_selection()
            .unwrap_err()
            .contains("recordings")
    );
    invalid.virtual_terminal = None;
    invalid.remote = Some(RemoteAction::Download {
        local: profile.0.join("destination"),
        remote: "/remote/source".to_owned().try_into().unwrap(),
        folder: true,
    });
    assert!(
        invalid
            .validate_target_selection()
            .unwrap_err()
            .contains("download")
    );

    let mut changed = store.get(second).unwrap().clone();
    changed.login.endpoint.port = 9922;
    store.save(changed).unwrap();
    assert!(
        matches!(crate::job_executor::Credentials::resolve_batch(&action, &store, &crate::clipboard_policy::ServerAccess::default(), |_, _, _| panic!("a stale selection must block before vault lookup")), Err(error) if error.contains("route changed"))
    );
    store.delete(second).unwrap();
    assert!(
        matches!(crate::job_executor::Credentials::resolve_batch(&action, &store, &crate::clipboard_policy::ServerAccess::default(), |_, _, _| panic!("a missing selection must block before vault lookup")), Err(error) if error.contains("missing"))
    );
    window.update(Message::ConnectionRemoved(second));
    window.update(Message::Save);
    assert_eq!(
        window
            .selected_job()
            .unwrap()
            .action()
            .targets()
            .map(|reference| reference.id)
            .collect::<Vec<_>>(),
        [id, third]
    );
}

#[test]
fn sftp_actions_reject_ambiguous_paths_and_incompatible_shell_settings() {
    for path in [
        "relative",
        "/",
        "/parent/..",
        "/parent//target",
        "/parent/\\..\\target",
        "/parent/CON",
        "/parent/target.",
        "/file:stream",
    ] {
        let document = serde_json::json!({"kind": "mkdir", "path": path});
        assert!(
            serde_json::from_value::<RemoteAction>(document).is_err(),
            "{path}"
        );
    }
    let mut action: SshCommand = serde_json::from_value(serde_json::json!({
        "host":"example.test", "port":22, "username":"operator",
        "key_file":Profile::temporary().0.join("key"),
        "remote":{"kind":"chmod", "path":"/allowed/file", "mode":4096},
    }))
    .unwrap();
    assert!(action.remote_action().is_err());
    action.remote =
        serde_json::from_value(serde_json::json!({"kind":"mkdir", "path":"/allowed/path"}))
            .unwrap();
    assert!(action.remote_action().is_ok());
    action.virtual_terminal = Some(virtual_terminal::Options::default());
    assert!(action.remote_action().is_err());
    action.virtual_terminal = None;
    action.command = "echo conflicting action".into();
    assert!(action.remote_action().is_err());

    for remote in [
        serde_json::json!({"kind":"copy_remote", "from":"/allowed/file", "to":"/allowed/file"}),
        serde_json::json!({"kind":"chown", "path":"/allowed/file"}),
        serde_json::json!({"kind":"chown", "path":"/allowed/file", "uid":4294967295u32}),
    ] {
        action.command.clear();
        action.remote = serde_json::from_value(remote).unwrap();
        assert!(action.remote_action().is_err());
    }
    for invalid in ["root", "+1", "4294967295"] {
        assert!(remote::owner_id_from_text(invalid).is_err());
    }
    assert_eq!(remote::owner_id_from_text(" 0 "), Ok(Some(0)));
    assert_eq!(remote::owner_id_from_text(""), Ok(None));
}

#[test]
fn native_form_saves_and_reopens_the_script_source_and_separate_argv() {
    let profile = Profile::temporary();
    let path = profile.file();
    let mut window = TestWindow::open(path.clone()).unwrap();
    for (field, value) in [
        (Field::Name, "Script job".to_owned()),
        (Field::Host, "example.test".to_owned()),
        (Field::Username, "operator".to_owned()),
        (Field::KeyFile, profile.0.join("key").display().to_string()),
    ] {
        window.update(Message::Edit(field, value));
    }
    window.update(Message::CommandSource(Source::Script));
    window.update(Message::ScriptLanguage(Language::Python));
    let body = "import sys\nprint(sys.argv[1:])";
    let paste =
        |value: &str| text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(value.into())));
    window.update(Message::EditCommand(paste(body)));
    window.update(Message::EditScriptArguments(paste(
        "two words\n\n apostrophe' \n--flag",
    )));
    window.update(Message::Save);
    assert_eq!(window.status(), Some("Job saved"));
    let action = window.selected_job().unwrap().action();
    let script = action.script.as_ref().unwrap();
    assert_eq!(script.language, Language::Python);
    assert_eq!(script.body, body);
    assert_eq!(script.arguments, ["two words", " apostrophe' ", "--flag"]);
    assert!(action.command.is_empty());
    let expected = action.clone();
    drop(window);

    let mut window = TestWindow::open(path).unwrap();
    assert_eq!(window.view.draft.source, Source::Script);
    assert_eq!(window.view.draft.script_language, Language::Python);
    window.update(Message::Save);
    assert_eq!(window.selected_job().unwrap().action(), &expected);
    window.runtime.set_execution_allowed(true);
    let request = window.update(Message::Run(false)).pop().unwrap();
    assert_eq!(request.action.as_ref(), &expected);
    let encoded = request.action.command_for_exec(None).unwrap();
    assert!(encoded.starts_with("exec python3 - 'two words'"));
}

#[test]
fn native_form_reopens_stable_snippet_link_and_missing_library_does_not_break_other_jobs() {
    let profile = Profile::temporary();
    let path = profile.file();
    let mut window = TestWindow::open(path.clone()).unwrap();
    let mut library = Library::open(window.runtime.snippet_path.clone()).unwrap();
    let snippet = |name: &str, content: &str| crate::snippets::Input {
        name: name.into(),
        language: "bash".into(),
        category: String::new(),
        content: content.into(),
    };
    let id = library.save(None, snippet("Probe", "exit 99")).unwrap();
    window.runtime.refresh_snippets();
    for (field, value) in [
        (Field::Name, "Shared snippet job".to_owned()),
        (Field::Host, "example.test".to_owned()),
        (Field::Username, "operator".to_owned()),
        (Field::KeyFile, profile.0.join("key").display().to_string()),
    ] {
        window.update(Message::Edit(field, value));
    }
    window.update(Message::CommandSource(Source::Snippet));
    window.update(Message::SnippetSelected(
        window.runtime.snippet_choices[0].clone(),
    ));
    window.update(Message::EditScriptArguments(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("one literal argument".into())),
    )));
    window.update(Message::Save);
    assert_eq!(window.status(), Some("Job saved"));
    drop(window);

    let mut window = TestWindow::open(path.clone()).unwrap();
    assert_eq!(window.view.draft.source, Source::Snippet);
    assert_eq!(window.view.draft.snippet_id, Some(id));
    library
        .save(Some(id), snippet("Renamed", "exit 23"))
        .unwrap();
    window.runtime.set_execution_allowed(true);
    let request = window.update(Message::Run(false)).pop().unwrap();
    let command = request
        .action
        .command_for_exec(Some(window.runtime.snippet_path()))
        .unwrap();
    assert!(command.starts_with("exec bash -s -- 'one literal argument'"));
    assert!(command.contains("\nexit 23\n"));
    assert!(!command.contains("exit 99"));
    library.remove(id).unwrap();
    drop(window);

    let window = TestWindow::open(path).unwrap();
    assert!(window.runtime.snippet_choices.is_empty());
    let action = window.selected_job().unwrap().action();
    assert_eq!(action.snippet.as_ref().unwrap().id, id);
    assert_eq!(
        action
            .command_for_exec(Some(window.runtime.snippet_path()))
            .unwrap_err(),
        "The linked snippet is missing"
    );
}

#[test]
fn scheduler_picker_replaces_preselected_targets_and_keeps_cancel_private() {
    use target_picker::{Choice, Event};
    let scratch = Profile::temporary();
    let mut store = ConnectionStore::open(scratch.0.join("connections.json")).unwrap();
    let first = store.save(grouped_connection("first.example")).unwrap();
    let second = store.save(grouped_connection("second.example")).unwrap();
    let mut app = TestWindow::open(scratch.file()).unwrap();
    app.runtime.set_connections(Some(&store));
    let choice = |app: &TestWindow, id| {
        app.runtime
            .connection_choices
            .iter()
            .find(|choice| {
                choice
                    .reference()
                    .is_some_and(|reference| reference.id == id)
            })
            .unwrap()
            .clone()
    };
    app.update(Message::ConnectionSelected(choice(&app, first)));
    app.update(Message::TargetPicker(Event::Open));
    assert!(
        app.view.draft.target_picker.as_ref().unwrap().selected()
            == [Choice::Connection(choice(&app, first))]
    );
    app.update(Message::TargetPicker(Event::Clear));
    app.update(Message::TargetPicker(Event::Select(
        Choice::Connection(choice(&app, second)),
        true,
    )));
    app.update(Message::TargetPicker(Event::Close));
    assert!(
        app.view
            .draft
            .selected_picker_targets(&app.runtime.connection_choices)
            == [Choice::Connection(choice(&app, first))]
    );
    app.update(Message::TargetPicker(Event::Open));
    app.update(Message::TargetPicker(Event::Clear));
    app.update(Message::TargetPicker(Event::Select(
        Choice::Connection(choice(&app, second)),
        true,
    )));
    app.update(Message::TargetPicker(Event::Apply));
    assert!(app.view.draft.target_picker.is_none());
    assert!(
        app.view
            .draft
            .selected_picker_targets(&app.runtime.connection_choices)
            == [Choice::Connection(choice(&app, second))]
    );
}

#[test]
fn scheduler_picker_tree_keeps_filtered_ancestors_and_keyboard_ranges() {
    use iced::keyboard::{Modifiers, key::Named};
    use target_picker::{Choice, Event};
    let scratch = Profile::temporary();
    let mut store = ConnectionStore::open(scratch.0.join("connections.json")).unwrap();
    let root = store.create_group(None, "Fleet").unwrap();
    let child = store.create_group(Some(&root), "Staging").unwrap();
    store
        .save_in_group(grouped_connection("leaf.example"), Some(&child))
        .unwrap();
    let mut app = TestWindow::open(scratch.file()).unwrap();
    app.runtime.set_connections(Some(&store));
    app.update(Message::TargetPicker(Event::Open));
    app.update(Message::TargetPicker(Event::Search("leaf.example".into())));
    let rows = app.view.draft.target_picker.as_ref().unwrap().rows(
        &app.runtime.connection_choices,
        &app.runtime.connection_group_choices,
        &app.runtime.connection_group_paths,
    );
    assert_eq!(rows.len(), 3);
    assert!(rows[0].choice == Some(Choice::Group(root.into())));
    assert!(rows[1].choice == Some(Choice::Group(child.into())));
    assert_eq!(rows[2].depth, 2);
    app.update(Message::TargetPicker(Event::Click(
        rows[0].choice.clone().unwrap(),
    )));
    app.update(Message::TargetPicker(Event::RowKey(
        rows[0].key.clone(),
        Named::End,
        Modifiers::SHIFT,
    )));
    assert_eq!(
        app.view
            .draft
            .target_picker
            .as_ref()
            .unwrap()
            .selected()
            .len(),
        3
    );
    app.update(Message::TargetPicker(Event::Search(String::new())));
    app.update(Message::TargetPicker(Event::Expand(rows[0].key.clone())));
    let picker = app.view.draft.target_picker.as_ref().unwrap();
    assert_eq!(
        picker
            .rows(
                &app.runtime.connection_choices,
                &app.runtime.connection_group_choices,
                &app.runtime.connection_group_paths
            )
            .len(),
        1
    );
}
