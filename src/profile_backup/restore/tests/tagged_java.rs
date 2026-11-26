use super::*;
use crate::connection_store::{Auth, Store};
use crate::scheduler_window::{ConnectionRef, SshCommand};
use crate::session_security::IsolationLevel;
use kortty_jobs::{Job, JobGraph, JobId, SchedulerStore};

const LEADER: &str = "java-3f9bab16184b4e13bf71e37331b8843a";
const FOLLOWER: &str = "java-0bd3734742024c5a912d4fe43c09f755";
const INDEPENDENT: &str = "java-d0000000000000000000000000000003";

#[test]
#[ignore = "requires the owned mixed archive from the Java v2.26.0 BackupManager fixture"]
fn java_tagged_mixed_backup_restores_fresh_and_conflicting_profiles() {
    let run = std::env::temp_dir().canonicalize().unwrap();
    let archive = PathBuf::from(std::env::var_os("KORTTY_TAGGED_MIXED_BACKUP").unwrap())
        .canonicalize()
        .unwrap();
    assert_eq!(archive, run.join("archive/kortty-backup.zip"));
    let password = Secret::new("Owned mixed backup ZIP 2026".into());
    let source_master = Secret::new("Owned Java source master 2026".into());
    let new_master = crate::vault_window::test_vault_password();
    for destination in ["fresh", "collision"] {
        let root = run
            .join(destination)
            .join("Library/Application Support/kortty");
        assert!(
            !root.exists(),
            "the destination must be a new fixture profile"
        );
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        let directory = Directory::open(&root, true).unwrap();
        directory.private_child("vault", true).unwrap();
        let vault_path = crate::profile::vault_path_at(&root);
        let mut vault = Vault::create(&vault_path, &new_master[..]).unwrap();
        let collision = (destination == "collision").then(|| seed_collision(&root, &mut vault));
        let before_vault = fs::read(&vault_path).unwrap();
        let before_jobs = fs::read(root.join(JOBS)).ok();
        let before_connections = fs::read(root.join(CONNECTIONS)).ok();

        let mut preview = super::super::super::inspect_java_password(&archive, &password).unwrap();
        let (format, entries, omissions) = preview.java_review().unwrap();
        assert_eq!(format, "Java password backup (AES-256)");
        assert!(entries.iter().any(|entry| entry.contains("Scheduler")));
        assert!(preview.has_java_passwords());
        println!("{destination} preview: {entries:?}; omissions: {omissions:?}");
        assert!(!directory.has_entry(PENDING).unwrap());
        super::super::super::prepare_java_credentials(
            &mut preview,
            Some(&source_master),
            Some(&vault),
            &root,
        )
        .unwrap();
        assert_eq!(preview.java_password_summary(), Some((1, 1)));

        let queued =
            super::super::super::queue_restore_at(&root, preview, RestoreMode::JavaAdditive)
                .unwrap();
        assert!(queued.durable);
        println!("{destination} queue notices: {:?}", queued.scheduler_notes);
        if collision.is_some() {
            for notice in [
                "Scheduler jobs omitted due to local conflicts (1)",
                "Scheduler jobs omitted due to missing prerequisites (1)",
            ] {
                assert!(queued.scheduler_notes.iter().any(|note| note == notice));
            }
        }
        assert_eq!(fs::read(&vault_path).unwrap(), before_vault);
        assert_eq!(fs::read(root.join(CONNECTIONS)).ok(), before_connections);
        assert_eq!(fs::read(root.join(JOBS)).ok(), before_jobs);
        let pending = fs::read(root.join(PENDING)).unwrap();
        for secret in [
            password.as_str(),
            source_master.as_str(),
            "Owned route password 2026",
        ] {
            assert!(
                !pending
                    .windows(secret.len())
                    .any(|bytes| bytes == secret.as_bytes())
            );
        }
        drop(vault);
        drop(ProfileLease::acquire(&root, || false).unwrap());
        assert!(!directory.has_entry(PENDING).unwrap());

        let vault = Vault::unlock(&vault_path, &new_master[..]).unwrap();
        verify_installed(&root, &vault, collision.as_ref());
        let installed = [CONNECTIONS, JOBS, VAULT, data::WINDOW_SETTINGS]
            .map(|file| fs::read(root.join(file)).unwrap());
        let mut replay = super::super::super::inspect_java_password(&archive, &password).unwrap();
        super::super::super::prepare_java_credentials(
            &mut replay,
            Some(&source_master),
            Some(&vault),
            &root,
        )
        .unwrap();
        assert_eq!(replay.java_password_summary(), Some((0, 2)));
        let _ = super::super::super::queue_restore_at(&root, replay, RestoreMode::JavaAdditive)
            .unwrap();
        drop(vault);
        drop(ProfileLease::acquire(&root, || false).unwrap());
        assert_eq!(
            [CONNECTIONS, JOBS, VAULT, data::WINDOW_SETTINGS]
                .map(|file| fs::read(root.join(file)).unwrap()),
            installed
        );
    }
}

struct Collision {
    connection: crate::connection_store::Connection,
    credential: String,
}

fn seed_collision(root: &Path, vault: &mut Vault) -> Collision {
    let (mut catalog, id) = save_fixture_connection(root, "secondary.example");
    let mut connection = catalog.get(id).unwrap().clone();
    connection.name = "Secondary".into();
    connection.login.endpoint.username = "fixture".into();
    catalog.create_group(None, "Java").unwrap();
    catalog.save_in_group(connection, Some("Java")).unwrap();
    catalog
        .set_group_isolation("Java", None, Some(IsolationLevel::None))
        .unwrap();
    let connection = catalog.get(id).unwrap().clone();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "connection": ConnectionRef::from(&connection),
        "command": "printf owned-local-job",
    }))
    .unwrap();
    let job = Job::new(JobId::new(LEADER).unwrap(), "Owned local job", action).unwrap();
    let jobs = JobGraph::new(vec![job]).unwrap();
    SchedulerStore::open(root.join(JOBS))
        .unwrap()
        .commit_with_actions(&jobs, &[], &[])
        .unwrap();
    fs::write(
        root.join(data::WINDOW_SETTINGS),
        br#"{"default_session_isolation":"none","temporary_ssh_key_enabled":false}"#,
    )
    .unwrap();
    let credential = vault
        .save(
            CredentialInput {
                label: "Owned imported login".into(),
                username: "fixture".into(),
                server_pattern: "primary.example".into(),
                environment: "TEST".into(),
                ..CredentialInput::default()
            },
            Secret::new("Owned local route secret 2026".into()),
        )
        .unwrap();
    Collision {
        connection,
        credential,
    }
}

fn verify_installed(root: &Path, vault: &Vault, collision: Option<&Collision>) {
    let catalog = Store::open(root.join(CONNECTIONS)).unwrap();
    let [first, second] = catalog.connections() else {
        panic!("expected the two source routes and no duplicate local route");
    };
    let (primary, secondary) = if first.name == "Primary" {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(catalog.group_of(primary.id), Some("Java/Imported"));
    assert_eq!(catalog.group_of(secondary.id), Some("Java"));
    assert_eq!(
        catalog.group_isolation("Java/Imported"),
        Some(IsolationLevel::Process)
    );
    assert!(matches!(&secondary.login.auth, Auth::Prompt));
    let Auth::VaultPassword(id) = &primary.login.auth else {
        panic!("expected the reviewed Java password to be bound to the imported connection");
    };
    assert_eq!(vault.secret(id), Some("Owned route password 2026"));
    let bound = vault.get(id).unwrap();
    assert_eq!(bound.username, primary.login.endpoint.username);
    assert!(bound.matches_host(&primary.login.endpoint.host));
    assert!(!bound.matches_host(&secondary.login.endpoint.host));

    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join(data::WINDOW_SETTINGS)).unwrap()).unwrap();
    assert_eq!(settings["java_security"]["default_isolation"], "process");
    assert_eq!(settings["java_security"]["temporary_ssh_keys"], true);
    assert_ne!(settings["temporary_ssh_key_enabled"], true);

    let store = SchedulerStore::open(root.join(JOBS)).unwrap();
    let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
    let graph = scheduler.graph();
    let job = |id| graph.job(&JobId::new(id).unwrap());
    let independent = job(INDEPENDENT).unwrap();
    assert!(!independent.enabled());
    assert_eq!(
        independent.action().connection,
        Some(ConnectionRef::from(primary))
    );
    let leader = job(LEADER).unwrap();
    if let Some(collision) = collision {
        assert_eq!(
            vault.secret(&collision.credential),
            Some("Owned local route secret 2026")
        );
        assert_eq!(secondary.id, collision.connection.id);
        assert_eq!(secondary.tunnels, collision.connection.tunnels);
        assert_eq!(catalog.group_isolation("Java"), Some(IsolationLevel::None));
        assert_eq!(settings["default_session_isolation"], "none");
        assert_eq!(leader.name(), "Owned local job");
        assert!(leader.enabled());
        assert!(job(FOLLOWER).is_none());
        assert_eq!(graph.jobs().len(), 2);
    } else {
        assert_eq!(
            catalog.group_isolation("Java"),
            Some(IsolationLevel::Process)
        );
        assert!(!leader.enabled());
        assert_eq!(
            leader.action().connection,
            Some(ConnectionRef::from(primary))
        );
        let follower = job(FOLLOWER).unwrap();
        assert!(!follower.enabled());
        assert_eq!(follower.dependencies(), std::slice::from_ref(leader.id()));
        assert_eq!(graph.jobs().len(), 3);
    }
    assert!(store.journal().is_empty());
}
