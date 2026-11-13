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
fn portable_jobs_remove_only_device_local_sftp_approval() {
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
        Job::new(archive_id, "Privileged archive", archive.clone()).unwrap(),
    ])
    .unwrap();
    jobs.commit_with_actions(&graph, &[], &[]).unwrap();
    let projected: Value =
        serde_json::from_slice(&super::portable_jobs(&fs::read(&path).unwrap()).unwrap()).unwrap();
    for (id, mut expected) in [
        ("reviewed", action),
        ("recurring", recurring),
        ("privileged", elevated),
        ("privileged-archive", archive),
    ] {
        expected["remote"]
            .as_object_mut()
            .unwrap()
            .remove("approval");
        assert_eq!(projected["actions"][id], expected);
    }
}
