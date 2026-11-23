use std::fs;
use std::time::{Duration, UNIX_EPOCH};

use kortty_jobs::{Job, JobGraph, JobId, RunOutcome, Scheduler};
use kortty_terminal::TerminalProfile;
use serde_json::{Value, json};

use super::*;
use crate::connection_store::{
    Auth, Connection, SavedLogin, Store, TunnelConfig, TunnelRule, TunnelTarget,
};
use crate::profile::{InitialTerminalSize, PaneLayout, WindowLayout, WorkspaceLayout};
use crate::snippets::{Input, Library, tests::Profile};
use crate::ssh_dialog::LoginTarget;
use crate::ssh_key_catalog::{Catalog, Edit, KeyDraft, KeyInput};

#[test]
fn portable_selection_keeps_native_stores_and_projects_excludes_device_state() {
    let temporary = Profile::new();
    let root = temporary.0.canonicalize().unwrap();
    fs::write(root.join("window-settings.json"), b"{}").unwrap();
    let mut markers = markers::Settings::default();
    markers
        .add_definition(
            "Deployment",
            [0x7c, 0x3a, 0xed],
            crate::session_journal::entry_edit::LegacyMarker::Info,
        )
        .unwrap();
    markers.save(&root, &markers::Settings::default()).unwrap();
    Library::open(root.join("snippets.json"))
        .unwrap()
        .save(
            None,
            Input {
                name: "Fixture".into(),
                language: "bash".into(),
                category: String::new(),
                content: "printf backup".into(),
            },
        )
        .unwrap();
    fs::create_dir(root.join("projects")).unwrap();
    let layout = WorkspaceLayout::new(vec![WindowLayout {
        width: 900.0,
        height: 660.0,
        position: None,
        maximized: false,
        dashboard_visible: None,
        active: 0,
        tabs: vec![PaneLayout::Terminal {
            font: 13,
            focused: true,
            vt100: false,
            term: None,
            security: None,
            initial_size: InitialTerminalSize {
                columns: Some(132),
                rows: None,
            },
        }],
        tab_titles: Default::default(),
        tab_groups: Default::default(),
        tab_primary_panes: Default::default(),
        tab_terminal_effects: Default::default(),
        tab_terminal_appearance: Default::default(),
    }]);
    let project = "projects/44656d6f.json";
    fs::write(root.join(project), serde_json::to_vec(&layout).unwrap()).unwrap();
    for path in [
        "projects/projects.lock",
        "projects/untitled.json",
        "layout.json",
        "last-window.json",
        "control.json",
        "kortty-policy.toml",
    ] {
        fs::write(root.join(path), b"test-only excluded contents").unwrap();
    }
    for directory in [
        "session",
        "recordings",
        "job-recordings",
        "logs",
        "snippet-drafts",
    ] {
        fs::create_dir(root.join(directory)).unwrap();
        fs::write(root.join(directory).join("runtime.txt"), b"local output").unwrap();
    }
    let snapshot = collect(&root).unwrap();
    assert_eq!(
        snapshot.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            markers::FILE,
            project,
            "snippets.json",
            "window-settings.json"
        ]
    );
    assert_eq!(
        snapshot[markers::FILE].as_slice(),
        fs::read(root.join(markers::FILE)).unwrap()
    );
    assert_eq!(existing_project_paths(&root).unwrap(), [project]);
}

#[test]
fn theme_background_backup_is_bound_to_settings_and_restores_privately() {
    use super::super::restore::{self, ProfileLease, RestoreMode};
    use crate::terminal_theme::{Scheme, Themes};

    let source = Profile::new();
    let root = source.0.canonicalize().unwrap();
    let prepare = |name: &str, color: [u8; 4]| {
        let input = root.join(name);
        ::image::RgbaImage::from_pixel(4, 3, ::image::Rgba(color))
            .save(&input)
            .unwrap();
        let prepared = background_image::prepare(&input).unwrap();
        prepared.publish(&root).unwrap();
        prepared.id()
    };
    let asset = prepare("selected.png", [37, 51, 72, 255]);
    let unused = prepare("unused.png", [9, 15, 21, 255]);
    let mut themes = Themes::default();
    let id = themes.store(None, "Artwork", Scheme::default()).unwrap();
    assert!(themes.set_background_asset(id, Some(asset)));
    let settings = serde_json::to_vec(&json!({ "themes": themes })).unwrap();
    fs::write(root.join(WINDOW_SETTINGS), &settings).unwrap();
    let snapshot = collect(&root).unwrap();
    assert!(snapshot.contains_key(&asset.relative_path()));
    assert!(!snapshot.contains_key(&unused.relative_path()));

    let mut incomplete = snapshot.clone();
    incomplete.remove(&asset.relative_path());
    assert!(validate(&incomplete).is_err());
    let mut altered = snapshot.clone();
    altered.get_mut(&asset.relative_path()).unwrap().push(0);
    assert!(validate(&altered).is_err());

    for mode in [RestoreMode::Replace, RestoreMode::KeepExisting] {
        let target = Profile::new();
        let destination = target.0.canonicalize().unwrap();
        restore::queue_with_mode(&destination, snapshot.clone(), mode, false).unwrap();
        let _lease = ProfileLease::acquire(&destination, || false).unwrap();
        assert_eq!(collect(&destination).unwrap(), snapshot);
    }

    let target = Profile::new();
    let destination = target.0.canonicalize().unwrap();
    fs::write(destination.join(WINDOW_SETTINGS), b"{}").unwrap();
    restore::queue_with_mode(&destination, snapshot, RestoreMode::KeepExisting, false).unwrap();
    let _lease = ProfileLease::acquire(&destination, || false).unwrap();
    assert!(!destination.join(asset.relative_path()).exists());
}

#[test]
fn managed_keys_include_only_catalogued_copies_and_their_companions() {
    let temporary = Profile::new();
    let root = temporary.0.canonicalize().unwrap();
    let source = root.join("source-key");
    let external = root.join("external-key");
    fs::write(&source, b"test-only managed key").unwrap();
    fs::write(source.with_extension("pub"), b"test-only public key").unwrap();
    fs::write(&external, b"test-only external key").unwrap();
    let mut catalog = Catalog::open(root.clone()).unwrap();
    let draft = |name: &str, input| KeyDraft {
        name: name.into(),
        description: String::new(),
        input,
        passphrase: None,
    };
    let id = catalog
        .apply(vec![
            Edit::Add(draft("Managed", KeyInput::Copy(source))),
            Edit::Add(draft("External", KeyInput::External(external))),
        ])
        .unwrap()
        .added[0];
    let managed = catalog.display_path(id).unwrap();
    let name = managed.file_name().unwrap().to_str().unwrap();
    let primary = format!("ssh-keys/{name}");
    let companion = format!("{primary}.pub");
    let orphan = "ssh-keys/00000000000000000000000000000000";
    fs::write(root.join(orphan), b"test-only orphan").unwrap();

    let mut snapshot = collect(&root).unwrap();
    assert_eq!(snapshot.len(), 3);
    assert_eq!(snapshot[&primary].as_slice(), b"test-only managed key");
    assert_eq!(snapshot[&companion].as_slice(), b"test-only public key");
    assert!(snapshot.contains_key(KEY_CATALOG));
    assert!(
        !snapshot
            .values()
            .any(|bytes| bytes.as_slice() == b"test-only external key")
    );
    fs::write(root.join(&companion), b"").unwrap();
    assert!(collect(&root).unwrap()[&companion].is_empty());

    let key = snapshot.remove(&primary).unwrap();
    assert!(validate(&snapshot).is_err());
    snapshot.insert(primary, key);
    snapshot.remove(&companion);
    assert!(validate(&snapshot).is_ok());
    snapshot.insert(orphan.into(), Zeroizing::new(vec![1]));
    assert!(validate(&snapshot).is_err());
}

#[test]
fn portable_projections_keep_definitions_and_revoke_device_history_and_approvals() {
    let temporary = Profile::new();
    let root = temporary.0.canonicalize().unwrap();
    let mut connections = Store::open(root.join("connections.json")).unwrap();
    let connection_id = connections
        .save(Connection {
            id: 0,
            name: "Backup fixture".into(),
            tag: String::new(),
            login: SavedLogin {
                endpoint: LoginTarget {
                    host: "server.example".into(),
                    port: 22,
                    username: "operator".into(),
                },
                auth: Auth::Prompt,
                keyboard_interactive: false,
            },
            jump: None,
            emulation: TerminalProfile::Xterm.term_name().into(),
            tunnels: vec![TunnelConfig {
                enabled: true,
                description: "Fixture service".into(),
                rule: TunnelRule {
                    bind: "127.0.0.1:15432".parse().unwrap(),
                    target: TunnelTarget::Local {
                        host: "db.example".into(),
                        port: 5432,
                    },
                },
            }],
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
        })
        .unwrap();
    let saved = connections.get(connection_id).unwrap().clone();
    let plan = saved.plan();
    connections.approve(connection_id, plan.clone()).unwrap();
    assert!(connections.approved(connection_id, &plan));
    let reference = crate::scheduler_window::ConnectionRef::from(&saved);
    let action: SshCommand = serde_json::from_value(json!({
        "connection": reference,
        "command": "true",
    }))
    .unwrap();
    let job_id = JobId::new("fixture").unwrap();
    let mut scheduler = Scheduler::new(
        JobGraph::new(vec![Job::new(job_id.clone(), "Fixture", action).unwrap()]).unwrap(),
    );
    let started_at = UNIX_EPOCH + Duration::from_secs(1);
    let ticket = scheduler.start_manual(&job_id, started_at).unwrap().started[0].id;
    let finished = scheduler
        .complete(
            ticket,
            RunOutcome::Success,
            started_at + Duration::from_secs(1),
        )
        .unwrap();
    let mut jobs = SchedulerStore::open(root.join("jobs.json")).unwrap();
    jobs.commit_with_actions(
        scheduler.graph(),
        &finished.finished,
        &[(ticket, "local detail".into())],
    )
    .unwrap();
    jobs.commit_records_with_recordings(scheduler.graph(), &[], &[], &[(ticket, [1; 16])])
        .unwrap();
    assert_eq!(jobs.journal().len(), 1);

    let snapshot = collect(&root).unwrap();
    let projected: Value = serde_json::from_slice(&snapshot["connections.json"]).unwrap();
    assert_eq!(projected["approvals"], json!({}));
    assert_eq!(projected["connections"][0]["id"], connection_id);
    let projected: Value = serde_json::from_slice(&snapshot["jobs.json"]).unwrap();
    assert_eq!(projected["definitions"].as_array().unwrap().len(), 1);
    assert_eq!(projected["actions"]["fixture"]["command"], "true");
    assert_eq!(projected["journal"], json!([]));
    assert_eq!(projected["last_runs"], json!({}));
    assert_eq!(projected["high_watermark"], 0);
    assert!(projected.get("journal_details").is_none());
    assert!(projected.get("journal_recordings").is_none());
    assert!(connections.approved(connection_id, &plan));
    assert_eq!(jobs.journal().len(), 1);

    let mut untrusted = snapshot;
    untrusted.insert(
        "connections.json".into(),
        Zeroizing::new(fs::read(connections.path()).unwrap()),
    );
    assert!(validate(&untrusted).is_err());
}

#[cfg(unix)]
#[test]
fn portable_reads_refuse_symlinked_sources_and_projects() {
    use std::os::unix::fs::symlink;

    let temporary = Profile::new();
    let root = temporary.0.canonicalize().unwrap();
    let outside = root.join("test-only-target");
    fs::write(&outside, b"{}").unwrap();
    let settings = root.join("window-settings.json");
    symlink(&outside, &settings).unwrap();
    assert!(collect(&root).is_err());
    fs::remove_file(&settings).unwrap();
    let target = root.join("test-project-directory");
    fs::create_dir(&target).unwrap();
    let project_dir = root.join("projects");
    symlink(&target, &project_dir).unwrap();
    assert!(collect(&root).is_err());
    assert!(existing_project_paths(&root).is_err());
}

#[test]
fn portable_jobs_remove_device_local_sftp_approvals_and_disable_privileged_replacement() {
    let profile = Profile::new();
    let path = profile.0.join("jobs.json");
    let mut jobs = SchedulerStore::open(&path).unwrap();
    let id = JobId::new("reviewed").unwrap();
    let action = json!({
        "connection": {"id": 7, "target": {"host": "peer.example", "port": 22, "username": "operator"}},
        "remote": {"kind": "sync", "direction": "download", "local": profile.0.join("data"),
            "remote": "/data", "excludes": "*.log", "approval": "0123456789abcdef0123456789abcdef"}
    });
    let elevated_id = JobId::new("privileged").unwrap();
    let replacement_id = JobId::new("privileged-replacement").unwrap();
    let repeated_id = JobId::new("privileged-recurring-replacement").unwrap();
    let merge_id = JobId::new("privileged-folder-merge").unwrap();
    let merge_upload_id = JobId::new("privileged-folder-upload-merge").unwrap();
    let archive_id = JobId::new("privileged-archive").unwrap();
    let recurring_id = JobId::new("recurring").unwrap();
    let mut recurring = action.clone();
    recurring["remote"]["standing"] = json!({
        "days": 7, "limits": {"max_firings": 12, "max_actions": 64, "max_bytes": 33554432},
    });
    let mut elevated = action.clone();
    elevated["sudo"] = json!("vault");
    elevated["remote"] = json!({
        "kind": "privileged", "direction": "upload", "local": profile.0.join("binary"),
        "remote": "/protected/file", "approval": "abcdef0123456789abcdef0123456789",
    });
    let mut replacement = elevated.clone();
    replacement["remote"]["direction"] = json!("download");
    replacement["remote"]["replace_existing"] = json!(true);
    let mut repeated = replacement.clone();
    repeated["remote"]["standing"] = json!({
        "days": 7, "limits": {"max_firings": 12, "max_actions": 1, "max_bytes": 1_073_741_824},
    });
    let mut folder = elevated.clone();
    folder["remote"] = json!({
        "kind": "privileged_tree_download", "direction": "download", "local": profile.0.join("data"),
        "remote": "/protected/tree", "replace_changed": true, "approval": "a1".repeat(16),
    });
    let mut folder_upload = folder.clone();
    folder_upload["remote"]["kind"] = json!("privileged_tree_upload");
    folder_upload["remote"]["direction"] = json!("upload");
    let mut archive = action.clone();
    archive["sudo"] = json!("passwordless");
    archive["remote"] = json!({
        "kind": "privileged_archive",
        "archive": {
            "sources": ["/protected/logs"], "target": "/archive/logs.tar.bz2",
            "format": "tar_bz2", "level": 7, "excludes": ["*.tmp"]
        },
        "approval": "76543210fedcba9876543210fedcba98",
    });
    let graph = JobGraph::new(vec![
        Job::new(id, "Reviewed folder copy", action.clone()).unwrap(),
        Job::new(recurring_id, "Recurring folder copy", recurring.clone())
            .unwrap()
            .with_schedule(kortty_jobs::JobSchedule::default()),
        Job::new(elevated_id, "Privileged upload", elevated.clone()).unwrap(),
        Job::new(
            replacement_id.clone(),
            "Privileged replacement",
            replacement.clone(),
        )
        .unwrap(),
        Job::new(
            repeated_id.clone(),
            "Recurring privileged replacement",
            repeated.clone(),
        )
        .unwrap()
        .with_schedule(kortty_jobs::JobSchedule::default().with_enabled(true)),
        Job::new(merge_id.clone(), "Privileged folder merge", folder.clone()).unwrap(),
        Job::new(
            merge_upload_id.clone(),
            "Privileged folder upload merge",
            folder_upload.clone(),
        )
        .unwrap(),
        Job::new(archive_id, "Privileged archive", archive.clone()).unwrap(),
    ])
    .unwrap();
    jobs.commit_with_actions(&graph, &[], &[]).unwrap();
    let portable = super::portable_jobs(&fs::read(&path).unwrap()).unwrap();
    let projected: Value = serde_json::from_slice(&portable).unwrap();
    let restored = SchedulerStore::decode_backup::<SshCommand>(&portable).unwrap();
    assert!(!restored.job(&replacement_id).unwrap().enabled());
    let repeating = restored.job(&repeated_id).unwrap();
    assert!(!repeating.enabled());
    assert!(repeating.schedule().unwrap().enabled());
    assert!(!restored.job(&merge_id).unwrap().enabled());
    assert!(!restored.job(&merge_upload_id).unwrap().enabled());
    for (id, mut expected) in [
        ("reviewed", action),
        ("recurring", recurring),
        ("privileged", elevated),
        ("privileged-replacement", replacement),
        ("privileged-recurring-replacement", repeated),
        ("privileged-folder-merge", folder),
        ("privileged-folder-upload-merge", folder_upload),
        ("privileged-archive", archive),
    ] {
        expected["remote"]
            .as_object_mut()
            .unwrap()
            .remove("approval");
        assert_eq!(projected["actions"][id], expected);
    }
}

#[test]
fn portable_grouped_rsync_jobs_keep_destinations_and_require_new_review() {
    use crate::scheduler_window::{Message, SchedulerWindow};

    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let program = root.join("rsync");
    let first = root.join("first");
    let second = root.join("second");
    let primary = json!({
        "id": 1, "target": {"host": "first.example", "port": 22, "username": "operator"},
    });
    let additional = json!({
        "id": 2, "target": {"host": "second.example", "port": 22, "username": "operator"},
    });
    let digest = [7_u8; 32];
    let approval = |reference, destination: &std::path::Path| {
        json!({
            "program": program, "digest": digest, "reference": reference, "pins": ["SHA256:fixture"],
            "download": {"source": "/incoming/file", "destination": destination},
        })
    };
    let action: SshCommand = serde_json::from_value(json!({
        "connection": primary, "additional_connections": [additional],
        "remote": {
            "kind": "rsync_download", "program": program, "local": first, "remote": "/incoming/file",
            "approval": approval(&primary, &first),
            "routes": [{
                "profile": 2, "destination": second, "approval": approval(&additional, &second),
            }],
        },
    }))
    .unwrap();
    assert!(action.require_executable().is_ok());
    let id = JobId::new("grouped-rsync").unwrap();
    let graph =
        JobGraph::new(vec![Job::new(id.clone(), "Grouped rsync", action).unwrap()]).unwrap();
    let path = root.join("jobs.json");
    let mut jobs = SchedulerStore::open(&path).unwrap();
    jobs.commit_with_actions(&graph, &[], &[]).unwrap();
    let snapshot = collect(&root).unwrap();
    let backup = &snapshot["jobs.json"];
    let projected: Value = serde_json::from_slice(backup).unwrap();
    let remote = &projected["actions"][id.as_str()]["remote"];
    assert!(remote.get("approval").is_none());
    assert!(remote["routes"][0].get("approval").is_none());
    assert_eq!(remote["local"], json!(first));
    assert_eq!(remote["routes"][0]["destination"], json!(second));
    let decoded = SchedulerStore::decode_backup::<SshCommand>(backup).unwrap();
    let restored = decoded.job(&id).unwrap();
    assert!(restored.enabled());
    assert!(restored.action().require_executable().is_err());

    let restored_path = root.join("restored.json");
    fs::write(&restored_path, backup.as_slice()).unwrap();
    let mut scheduler = SchedulerWindow::open(restored_path.clone()).unwrap();
    scheduler.set_execution_allowed(true);
    let mut view = scheduler.new_view();
    scheduler.update_view(&mut view, Message::Select(id));
    assert!(
        scheduler
            .update_view(&mut view, Message::Run(false))
            .is_empty()
    );
    scheduler.update_view(&mut view, Message::ScheduleEnabled(false));
    scheduler.update_view(&mut view, Message::Save);
    let edited: Value = serde_json::from_slice(&fs::read(restored_path).unwrap()).unwrap();
    assert_eq!(edited["definitions"][0]["schedule"]["enabled"], false);
    assert!(
        scheduler
            .update_view(&mut view, Message::Run(false))
            .is_empty()
    );
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn portable_one_use_rsync_existing_local_file_restores_as_a_disabled_draft() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let program = root.join("rsync");
    let destination = root.join("preserved.txt");
    let source = "/incoming/preserved.txt";
    let reference = json!({
        "id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}
    });
    let settings = json!({"delete": false, "include_source_name": false});
    let mut manifest = kortty_server_archive::rsync_tree::Manifest::default();
    manifest.add("", Some((4, [1; 32]))).unwrap();
    let inventory = manifest.finish();
    let action: SshCommand = serde_json::from_value(json!({
        "connection": reference,
        "remote": {
            "kind": "rsync_download", "program": program, "local": destination, "remote": source,
            "existing_download": settings,
            "approval": {
                "program": program, "digest": vec![2; 32], "reference": reference,
                "pins": ["SHA256:fixture"],
                "download": {"source": source, "destination": destination},
                "existing_file_download": {"destination": destination, "snapshot": {
                    "inventory": inventory, "fingerprint": vec![3; 32], "root_identity": vec![4; 32],
                }},
            },
        },
    }))
    .unwrap();
    assert!(action.require_executable().is_ok());
    let id = JobId::new("ordinary-existing-file").unwrap();
    let job = Job::new(id.clone(), "Replace existing local file once", action).unwrap();
    assert!(job.enabled());
    let graph = JobGraph::new(vec![job]).unwrap();
    let bytes = SchedulerStore::encode_backup(&graph).unwrap();
    let portable = super::portable_jobs(&bytes).unwrap();
    let decoded = SchedulerStore::decode_backup::<SshCommand>(&portable).unwrap();
    let restored = decoded.job(&id).unwrap();
    assert!(!restored.enabled());
    assert!(restored.action().require_executable().is_err());
    let payload: serde_json::Value = serde_json::from_slice(&portable).unwrap();
    let transfer = &payload["actions"][id.as_str()]["remote"];
    assert!(transfer.get("approval").is_none());
    assert_eq!(transfer["existing_download"], settings);
    assert_eq!(transfer["local"], json!(destination));
    assert_eq!(transfer["remote"], source);

    let path = root.join("restored-jobs.json");
    fs::write(&path, portable).unwrap();
    crate::scheduler_window::SchedulerWindow::open(path).unwrap();
}

#[test]
fn portable_recurring_rsync_job_keeps_limits_and_removes_authority() {
    let profile = Profile::new();
    let path = profile.0.canonicalize().unwrap();
    let settings = crate::scheduler_window::rsync::standing::Settings::default();
    let action = serde_json::from_value(json!({
        "connection": {
            "id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}
        },
        "remote": {
            "kind": "rsync_upload", "program": path.join("rsync"), "local": path.join("source"),
            "remote": "/incoming/tree", "directory": true,
            "existing": {"delete": false, "include_source_name": false},
            "standing": {"settings": settings, "approval": "a".repeat(32)}
        }
    }))
    .unwrap();
    let id = JobId::new("portable-standing-rsync").unwrap();
    let graph: JobGraph<SshCommand> = JobGraph::new(vec![
        Job::new(id.clone(), "Recurring rsync", action).unwrap(),
    ])
    .unwrap();
    let path = path.join("jobs.json");
    let mut store = SchedulerStore::open(&path).unwrap();
    store.commit_with_actions(&graph, &[], &[]).unwrap();
    let portable = super::portable_jobs(&fs::read(&path).unwrap()).unwrap();
    let restored = SchedulerStore::decode_backup::<SshCommand>(&portable).unwrap();
    let job = restored.job(&id).unwrap();
    assert!(!job.enabled());
    let (transfer, _) = job
        .action()
        .remote
        .as_ref()
        .unwrap()
        .rsync_transfer()
        .unwrap();
    let config = transfer.standing.as_ref().unwrap();
    assert!(config.approval.is_none());
    assert!(config.settings == settings);
}

#[test]
fn portable_passwordless_rsync_keeps_elevation_and_discards_the_helper_proof() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let program = root.join("rsync");
    let local = root.join("source");
    let reference = json!({
        "id": 1, "target": {"host": "server.example", "port": 22, "username": "operator"}
    });
    let remote = "/protected/new";
    let action = serde_json::from_value(json!({
        "connection": reference,
        "remote": {
            "kind": "rsync_upload", "program": program, "local": local, "remote": remote,
            "elevation": "passwordless",
            "approval": {
                "program": program, "digest": vec![1; 32], "reference": reference,
                "pins": ["SHA256:fixture"],
                "upload": {"source": local, "destination": remote, "bytes": 0, "digest": vec![2; 32]},
                "privileged": {
                    "spec": {"path": remote, "operation": {"direction": "upload", "directory": false,
                        "replace": false, "delete": "retain"}},
                    "policy": vec![3; 32], "fingerprint": vec![4; 32], "inventory": null,
                    "handoff": "/run/kortty/rsync-handoff"
                }
            }
        }
    }))
    .unwrap();
    let id = JobId::new("portable-passwordless-rsync").unwrap();
    let graph: JobGraph<SshCommand> = JobGraph::new(vec![
        Job::new(id.clone(), "Passwordless rsync", action).unwrap(),
    ])
    .unwrap();
    let path = root.join("jobs.json");
    let mut store = SchedulerStore::open(&path).unwrap();
    store.commit_with_actions(&graph, &[], &[]).unwrap();
    let backup = super::portable_jobs(&fs::read(path).unwrap()).unwrap();
    let restored = SchedulerStore::decode_backup::<SshCommand>(&backup).unwrap();
    let job = restored.job(&id).unwrap();
    let (transfer, _) = job
        .action()
        .remote
        .as_ref()
        .unwrap()
        .rsync_transfer()
        .unwrap();
    assert!(!job.enabled());
    assert!(transfer.uses_passwordless_sudo());
    assert!(transfer.approval.is_none());
    assert!(job.action().require_executable().is_err());
}
