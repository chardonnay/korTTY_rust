use kortty_vault::{CredentialInput, CredentialKind, Secret, Vault};

use super::*;
use crate::ssh_key_catalog::{Edit, KeyDraft, KeyInput};

const ENVIRONMENTS: &[u8] = br#"{"version":1,"custom":[],"colors":{}}"#;
const OLD_JOBS: &[u8] =
    br#"{"version":1,"definitions":[],"journal":[],"last_runs":{},"high_watermark":0}"#;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let base = std::env::current_dir().unwrap().join("target/.scratch");
        fs::create_dir_all(&base).unwrap();
        let path = base
            .canonicalize()
            .unwrap()
            .join(format!("kortty-backup-restore-{}", nonce().unwrap()));
        Directory::open(&path, true).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn snapshot() -> Snapshot {
    Snapshot::from([(
        "environments.json".into(),
        Zeroizing::new(ENVIRONMENTS.to_vec()),
    )])
}

#[test]
fn vault_and_managed_key_are_restored_as_one_replacement() {
    let source = Scratch::new();
    let target = Scratch::new();
    let source_dir = Directory::open(&source.0, false).unwrap();
    source_dir.private_child("vault", true).unwrap();
    let master = b"backup-vault-fixture-master";
    let passphrase = "test-only managed key passphrase";
    let mut vault = Vault::create(source.0.join("vault/vault.bin"), master).unwrap();
    let credential = vault
        .save(
            CredentialInput {
                label: "fixture key".into(),
                kind: CredentialKind::KeyPassphrase,
                ..CredentialInput::default()
            },
            Secret::new(passphrase.into()),
        )
        .unwrap();
    drop(vault);

    let register = |root: &Path, name: &str, value: &[u8], passphrase: Option<String>| {
        let source_file = root.join("local-fixture-key");
        fs::write(&source_file, value).unwrap();
        fs::write(
            source_file.with_extension("pub"),
            b"test-only public companion",
        )
        .unwrap();
        let mut catalog = Catalog::open(root.to_owned()).unwrap();
        let id = catalog
            .apply(vec![Edit::Add(KeyDraft {
                name: name.into(),
                description: String::new(),
                input: KeyInput::Copy(source_file),
                passphrase,
            })])
            .unwrap()
            .added[0];
        (id, catalog.display_path(id).unwrap())
    };
    let restored_key = b"test-only source private key bytes";
    let (id, source_key) = register(
        &source.0,
        "Backed-up key",
        restored_key,
        Some(credential.clone()),
    );
    let (previous_id, previous_key) = register(
        &target.0,
        "Previous key",
        b"test-only old private key",
        None,
    );
    assert_eq!(id, previous_id);

    let collected = data::collect(&source.0).unwrap();
    assert_eq!(collected.len(), 4);
    queue(&target.0, collected, false).unwrap();
    ProfileLease::acquire(&target.0, || false).unwrap();

    let catalog = Catalog::open(target.0.clone()).unwrap();
    let record = catalog.get(id).unwrap();
    assert_eq!(record.name, "Backed-up key");
    assert_eq!(record.passphrase.as_deref(), Some(credential.as_str()));
    let restored = catalog.display_path(id).unwrap();
    assert_eq!(restored.file_name(), source_key.file_name());
    assert_eq!(catalog.read_key(record).unwrap().as_slice(), restored_key);
    assert_eq!(
        fs::read(restored.with_extension("pub")).unwrap(),
        b"test-only public companion"
    );
    assert!(!previous_key.exists());
    assert!(!previous_key.with_extension("pub").exists());
    let vault = Vault::unlock(target.0.join("vault/vault.bin"), master).unwrap();
    assert_eq!(vault.secret(&credential), Some(passphrase));

    let empty = Scratch::new();
    queue_with_mode(
        &empty.0,
        data::collect(&source.0).unwrap(),
        RestoreMode::KeepExisting,
        false,
    )
    .unwrap();
    ProfileLease::acquire(&empty.0, || false).unwrap();
    let catalog = Catalog::open(empty.0.clone()).unwrap();
    assert_eq!(
        catalog
            .read_key(catalog.get(id).unwrap())
            .unwrap()
            .as_slice(),
        restored_key
    );
    let vault = Vault::unlock(empty.0.join("vault/vault.bin"), master).unwrap();
    assert_eq!(vault.secret(&credential), Some(passphrase));

    let different = Scratch::new();
    Directory::open(&different.0, false)
        .unwrap()
        .private_child("vault", true)
        .unwrap();
    let vault_path = different.0.join("vault/vault.bin");
    drop(Vault::create(vault_path.clone(), b"different local master passphrase").unwrap());
    let old_vault = fs::read(&vault_path).unwrap();
    let mut backup = data::collect(&source.0).unwrap();
    backup.extend(snapshot());
    queue_with_mode(&different.0, backup, RestoreMode::KeepExisting, false).unwrap();
    ProfileLease::acquire(&different.0, || false).unwrap();
    assert_eq!(fs::read(&vault_path).unwrap(), old_vault);
    assert!(!different.0.join("ssh-keys.json").exists());
    assert!(!different.0.join("ssh-keys").exists());
    assert_eq!(
        fs::read(different.0.join("environments.json")).unwrap(),
        ENVIRONMENTS
    );
}

#[test]
fn keep_existing_replay_adds_missing_independent_files_and_retains_late_collisions() {
    let source = Scratch::new();
    let mut snippets = crate::snippets::Library::open(source.0.join("snippets.json")).unwrap();
    snippets
        .save(
            None,
            crate::snippets::Input {
                name: "Fixture".into(),
                language: "bash".into(),
                category: String::new(),
                content: "printf fixture".into(),
            },
        )
        .unwrap();
    let target = Scratch::new();
    let root = &target.0;
    let mut backup = data::collect(&source.0).unwrap();
    backup.extend(snapshot());
    backup.insert(
        "window-settings.json".into(),
        Zeroizing::new(b"{}".to_vec()),
    );
    fs::write(root.join("connections.json"), b"existing local data").unwrap();
    let active = ProfileLease::acquire(root, || false).unwrap();
    queue_with_mode(root, backup, RestoreMode::KeepExisting, false).unwrap();
    assert!(root.join(PENDING).is_file());
    assert!(!root.join("environments.json").exists());
    assert!(
        matches!(ProfileLease::acquire(root, || false), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
    );
    fs::write(root.join("window-settings.json"), b"added after queue").unwrap();
    drop(active);

    ProfileLease::acquire(root, || false).unwrap();
    assert_eq!(
        fs::read(root.join("environments.json")).unwrap(),
        ENVIRONMENTS
    );
    assert_eq!(
        fs::read(root.join("snippets.json")).unwrap(),
        fs::read(source.0.join("snippets.json")).unwrap()
    );
    assert_eq!(
        fs::read(root.join("window-settings.json")).unwrap(),
        b"added after queue"
    );
    assert_eq!(
        fs::read(root.join("connections.json")).unwrap(),
        b"existing local data"
    );
    assert!(!root.join(PENDING).exists());
}

#[test]
fn keep_existing_never_imports_pins_locked_at_queue_or_publication() {
    use std::cell::Cell;

    for locked_at_queue in [false, true] {
        let target = Scratch::new();
        let mut backup = snapshot();
        backup.insert(
            PINS.into(),
            Zeroizing::new(b"# source host keys\n".to_vec()),
        );
        queue_with_mode(
            &target.0,
            backup,
            RestoreMode::KeepExisting,
            locked_at_queue,
        )
        .unwrap();
        let checks = Cell::new(0);
        ProfileLease::acquire(&target.0, || {
            checks.set(checks.get() + 1);
            !locked_at_queue && checks.get() == 2
        })
        .unwrap();
        assert_eq!(checks.get(), if locked_at_queue { 0 } else { 2 });
        assert!(!target.0.join(PINS).exists());
        assert!(target.0.join("environments.json").is_file());
    }
}

#[test]
fn keep_existing_imports_jobs_only_with_their_matching_saved_connection() {
    use kortty_jobs::{Job, JobGraph, JobId, SchedulerStore};
    use kortty_terminal::TerminalProfile;

    use crate::connection_store::{
        Auth, Connection, SavedLogin, Store, TunnelConfig, TunnelRule, TunnelTarget,
    };
    use crate::scheduler_window::{ConnectionRef, SshCommand};
    use crate::ssh_dialog::LoginTarget;

    let source = Scratch::new();
    let save_profile = |root: &Path, host: &str| {
        let mut store = Store::open(root.join("connections.json")).unwrap();
        let id = store
            .save(Connection {
                id: 0,
                name: "Fixture connection".into(),
                tag: String::new(),
                login: SavedLogin {
                    endpoint: LoginTarget {
                        host: host.into(),
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
                    description: "Fixture tunnel".into(),
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
            })
            .unwrap();
        (store, id)
    };
    let (store, id) = save_profile(&source.0, "source.example");
    let connection = store.get(id).unwrap();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": ConnectionRef::from(connection),
        "command": "true",
    }))
    .unwrap();
    let graph = JobGraph::new(vec![
        Job::new(JobId::new("fixture").unwrap(), "Fixture", action).unwrap(),
    ])
    .unwrap();
    SchedulerStore::open(source.0.join(JOBS))
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();

    let matching = Scratch::new();
    let (mut local, local_id) = save_profile(&matching.0, "source.example");
    assert_eq!(id, local_id);
    let plan = local.get(local_id).unwrap().plan();
    local.approve(local_id, plan.clone()).unwrap();
    let matching_connections = matching.0.join("connections.json");
    let old = fs::read(&matching_connections).unwrap();
    queue_with_mode(
        &matching.0,
        data::collect(&source.0).unwrap(),
        RestoreMode::KeepExisting,
        false,
    )
    .unwrap();
    ProfileLease::acquire(&matching.0, || false).unwrap();
    assert_eq!(fs::read(&matching_connections).unwrap(), old);
    assert!(
        Store::open(matching_connections)
            .unwrap()
            .approved(local_id, &plan)
    );
    assert_eq!(
        fs::read(matching.0.join(JOBS)).unwrap(),
        fs::read(source.0.join(JOBS)).unwrap()
    );

    let different = Scratch::new();
    let (_, different_id) = save_profile(&different.0, "local.example");
    assert_eq!(id, different_id);
    let mut backup = data::collect(&source.0).unwrap();
    backup.extend(snapshot());
    queue_with_mode(&different.0, backup, RestoreMode::KeepExisting, false).unwrap();
    ProfileLease::acquire(&different.0, || false).unwrap();
    assert!(!different.0.join(JOBS).exists());
    assert!(different.0.join("environments.json").is_file());

    let orphan_job = Scratch::new();
    fs::copy(source.0.join(JOBS), orphan_job.0.join(JOBS)).unwrap();
    let mut backup = data::collect(&source.0).unwrap();
    backup.remove(JOBS);
    queue_with_mode(&orphan_job.0, backup, RestoreMode::KeepExisting, false).unwrap();
    ProfileLease::acquire(&orphan_job.0, || false).unwrap();
    assert!(!orphan_job.0.join("connections.json").exists());
    assert_eq!(
        fs::read(orphan_job.0.join(JOBS)).unwrap(),
        fs::read(source.0.join(JOBS)).unwrap()
    );
}

#[test]
fn authenticated_restore_waits_for_all_desktop_instances_and_preserves_local_history() {
    let scratch = Scratch::new();
    let root = &scratch.0;
    let archive_path = root.join("portable.zip");
    let password = Secret::new("backup-fixture-password".into());
    archive::write(&archive_path, &snapshot(), &password).unwrap();

    let active = ProfileLease::acquire(root, || false).unwrap();
    let second = ProfileLease::acquire(root, || false).unwrap();
    fs::write(root.join("connections.json"), b"previous connections").unwrap();
    fs::write(root.join(JOBS), OLD_JOBS).unwrap();
    fs::create_dir(root.join(RECORDINGS)).unwrap();
    fs::write(
        root.join(RECORDINGS).join("fixture.cast"),
        b"local recording",
    )
    .unwrap();
    fs::write(root.join("layout.json"), b"local session layout").unwrap();

    let loaded = archive::read(&archive_path, &password).unwrap();
    assert!(queue(root, loaded, false).unwrap());
    assert_eq!(
        queue(root, snapshot(), false).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert!(!root.join("environments.json").exists());
    for last in [active, second] {
        assert!(matches!(
            ProfileLease::acquire(root, || false),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock
        ));
        drop(last);
    }

    let restored = ProfileLease::acquire(root, || false).unwrap();
    assert_eq!(
        fs::read(root.join("environments.json")).unwrap(),
        ENVIRONMENTS
    );
    assert!(!root.join("connections.json").exists());
    assert!(!root.join(JOBS).exists());
    assert!(!root.join(PENDING).exists());
    assert_eq!(
        fs::read(root.join("layout.json")).unwrap(),
        b"local session layout"
    );
    let old = fs::read_dir(root.join(JOB_HISTORY))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(fs::read(old.join(JOBS)).unwrap(), OLD_JOBS);
    assert_eq!(
        fs::read(old.join(RECORDINGS).join("fixture.cast")).unwrap(),
        b"local recording"
    );
    drop(restored);
    ProfileLease::acquire(root, || false).unwrap();
}

#[test]
fn changed_host_pin_policy_keeps_the_pending_restore_and_live_files_intact() {
    use std::cell::Cell;

    let scratch = Scratch::new();
    let root = &scratch.0;
    let directory = Directory::open(root, false).unwrap();
    let ssh = directory.private_child("ssh", true).unwrap();
    ssh.publish_copy("known_hosts", b"protected local pins")
        .unwrap();
    assert_eq!(
        queue(root, snapshot(), true).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(!root.join(PENDING).exists());
    queue(root, snapshot(), false).unwrap();
    assert!(matches!(
        ProfileLease::acquire(root, || true),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied
    ));
    assert_eq!(fs::read(root.join(PINS)).unwrap(), b"protected local pins");
    assert!(!root.join("environments.json").exists());
    assert!(root.join(PENDING).is_file());

    let checks = Cell::new(0);
    assert!(matches!(
        ProfileLease::acquire(root, || {
            checks.set(checks.get() + 1);
            checks.get() > 1
        }),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied
    ));
    assert_eq!(checks.get(), 2);
    assert_eq!(fs::read(root.join(PINS)).unwrap(), b"protected local pins");
    assert!(root.join(PENDING).is_file());

    ProfileLease::acquire(root, || false).unwrap();
    assert!(!root.join(PINS).exists());
    assert!(root.join("environments.json").is_file());
}

#[cfg(unix)]
#[test]
fn first_launch_creates_the_profile_under_a_resolved_alias() {
    let scratch = Scratch::new();
    let parent = Scratch::new();
    let alias = scratch.0.join("profile-alias");
    std::os::unix::fs::symlink(&parent.0, &alias).unwrap();
    let lease = ProfileLease::acquire(&alias.join("preferences/kortty"), || false).unwrap();
    let canonical = parent.0.join("preferences/kortty");
    assert_eq!(lease.root(), canonical);
    assert!(canonical.join(LEASE).is_file());
}

#[cfg(unix)]
#[test]
fn lease_resolves_the_profile_alias_once() {
    let scratch = Scratch::new();
    let original = Scratch::new();
    let other = Scratch::new();
    let alias = scratch.0.join("profile-alias");
    std::os::unix::fs::symlink(&original.0, &alias).unwrap();
    let lease = ProfileLease::acquire(&alias, || false).unwrap();
    fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&other.0, &alias).unwrap();
    assert_eq!(lease.root(), original.0);
    queue(&original.0, snapshot(), false).unwrap();
    assert!(matches!(
        ProfileLease::acquire(&original.0, || false),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock
    ));
    assert!(!other.0.join(PENDING).exists());
}

#[cfg(unix)]
#[test]
fn preflight_refuses_symlinks_and_restart_can_replay_the_unchanged_marker() {
    let scratch = Scratch::new();
    let root = &scratch.0;
    let external = root.join("external-data");
    fs::write(&external, b"must stay intact").unwrap();
    std::os::unix::fs::symlink(&external, root.join("snippets.json")).unwrap();
    fs::write(root.join("connections.json"), b"previous connections").unwrap();
    queue(root, snapshot(), false).unwrap();
    assert!(ProfileLease::acquire(root, || false).is_err());
    assert!(root.join(PENDING).is_file());
    assert!(root.join("connections.json").is_file());
    assert!(!root.join("environments.json").exists());

    fs::remove_file(root.join("snippets.json")).unwrap();
    fs::write(root.join("environments.json"), ENVIRONMENTS).unwrap();
    ProfileLease::acquire(root, || false).unwrap();
    assert_eq!(fs::read(&external).unwrap(), b"must stay intact");
    assert_eq!(
        fs::read(root.join("environments.json")).unwrap(),
        ENVIRONMENTS
    );
    assert!(!root.join(PENDING).exists());
    assert!(!root.join("connections.json").exists());
}

#[cfg(unix)]
#[test]
fn keep_existing_preflights_redirected_parents_and_never_follows_existing_files() {
    use std::os::unix::fs::symlink;

    let target = Scratch::new();
    let external = Scratch::new();
    let mut backup = snapshot();
    backup.insert(
        PINS.into(),
        Zeroizing::new(b"# source host keys\n".to_vec()),
    );
    fs::write(external.0.join("known_hosts"), b"external pins").unwrap();
    symlink(&external.0, target.0.join("ssh")).unwrap();
    queue_with_mode(&target.0, backup, RestoreMode::KeepExisting, false).unwrap();
    assert!(ProfileLease::acquire(&target.0, || false).is_err());
    assert!(target.0.join(PENDING).is_file());
    assert!(!target.0.join("environments.json").exists());

    fs::remove_file(target.0.join("ssh")).unwrap();
    Directory::open(&target.0, false)
        .unwrap()
        .private_child("ssh", true)
        .unwrap();
    symlink(external.0.join("known_hosts"), target.0.join(PINS)).unwrap();
    ProfileLease::acquire(&target.0, || false).unwrap();
    assert_eq!(
        fs::read(external.0.join("known_hosts")).unwrap(),
        b"external pins"
    );
    assert!(
        fs::symlink_metadata(target.0.join(PINS))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(target.0.join("environments.json").is_file());
    assert!(!target.0.join(PENDING).exists());
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires KORTTY_BACKUP_TEST_EXE pointing to a packaged macOS application"]
fn packaged_start_applies_the_backup_before_opening_desktop_stores() {
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    struct Running(Child);

    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let path = PathBuf::from(std::env::var_os("KORTTY_BACKUP_TEST_EXE").unwrap());
    assert!(path.is_absolute() && path.is_file());
    for mode in [RestoreMode::Replace, RestoreMode::KeepExisting] {
        let home = Scratch::new();
        let root = home.0.join("Library/Application Support/kortty");
        let existing = root.join("window-settings.json");
        Directory::open(&root, true).unwrap();
        fs::write(&existing, b"{}\n").unwrap();
        queue_with_mode(&root, snapshot(), mode, false).unwrap();
        let active = Running(
            Command::new(&path)
                .env("HOME", &home.0)
                .stdin(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(8);
        while root.join(PENDING).exists() {
            assert!(
                Instant::now() < deadline,
                "the package did not apply the restore in {mode:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            fs::read(root.join("environments.json")).unwrap(),
            ENVIRONMENTS
        );
        if mode == RestoreMode::KeepExisting {
            assert_eq!(fs::read(existing).unwrap(), b"{}\n");
        }
        drop(active);
    }
}
