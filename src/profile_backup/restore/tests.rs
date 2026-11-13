use kortty_vault::{CredentialInput, CredentialKind, Secret, Vault};

use super::*;
use crate::ssh_key_catalog::{Edit, KeyDraft, KeyInput};

const ENVIRONMENTS: &[u8] = br#"{"version":1,"custom":[],"colors":{}}"#;
const OLD_JOBS: &[u8] =
    br#"{"version":1,"definitions":[],"journal":[],"last_runs":{},"high_watermark":0}"#;
const JAVA_CLEAR_KEY: &[u8] = include_bytes!("../java_secrets/fixtures/java-managed-ed25519.pem");
const JAVA_ENCRYPTED_KEY: &[u8] =
    include_bytes!("../java_secrets/fixtures/java-managed-encrypted.key");
const JAVA_AGENT_CHAT: &[u8] =
    include_bytes!("../../workspace/saved_chats/fixtures/java-agent-run.xml");
const JAVA_AGENT_CHAT_PATH: &str = "ai-chats/20f88b8a407047fda4a473b27b5cc3a2.json";
const JAVA_SWARM: &[u8] =
    include_bytes!("../../workspace/saved_chats/swarm/fixtures/java-2.26.0.xml");
const JAVA_ENVIRONMENTS: &[u8] = br##"<environments>
    <environment id="custom-a1b2c3d4" displayName="Java lab"/>
    <environment id="custom-deadbeef" displayName="New environment"/>
    <colors><color id="PRODUCTION" color="#abc"/><color id="custom-deadbeef" color="#123456"/></colors>
</environments>"##;

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

fn java_managed_keys_archive(source: &Path) -> (PathBuf, Secret) {
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    let java_field = include_str!("../java_secrets/fixtures/envelopes.txt")
        .lines()
        .next()
        .unwrap();
    let connections = format!(
        r#"<connections><connection id="encrypted-route"><name>Protected key</name><host>java-key.example</host><username>fixture</username><authMethod>PUBLIC_KEY</authMethod><sshKeyId>fe33d0a9-ab84-4351-8d5c-ece77f40432f</sshKeyId><jumpServer><enabled>true</enabled><host>jump.example</host><username>jump-user</username><encryptedPassword>{java_field}</encryptedPassword></jumpServer></connection><connection id="plain-route"><name>Plain key</name><host>plain-key.example</host><username>fixture</username><authMethod>PUBLIC_KEY</authMethod><sshKeyId>f99ec24c-9aec-4978-a57f-ec37c449ca35</sshKeyId></connection></connections>"#
    );
    let path = source.join("java-managed-keys.zip");
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    let password = Secret::new("java-archive-password".into());
    let options = SimpleFileOptions::default()
        .with_aes_encryption_bytes(AesMode::Aes256, password.as_str().as_bytes());
    for (name, bytes) in [
        (
            "ssh-keys.xml",
            include_bytes!("../java_secrets/fixtures/java-managed-ssh-keys.xml").as_slice(),
        ),
        (
            "ssh-keys/f99ec24c-9aec-4978-a57f-ec37c449ca35_fixture-ed25519.pem",
            JAVA_CLEAR_KEY,
        ),
        (
            "ssh-keys/fe33d0a9-ab84-4351-8d5c-ece77f40432f_fixture-encrypted",
            JAVA_ENCRYPTED_KEY,
        ),
        (
            "master.key",
            include_bytes!("../java_secrets/fixtures/master.properties"),
        ),
        ("ai-chats.xml", JAVA_AGENT_CHAT),
        ("swarm-chats.xml", JAVA_SWARM),
        ("environments.xml", JAVA_ENVIRONMENTS),
        ("connections.xml", connections.as_bytes()),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    (path, password)
}

#[test]
fn java_managed_keys_restore_without_a_destination_vault() {
    let source = Scratch::new();
    let (path, password) = java_managed_keys_archive(&source.0);
    let local = Scratch::new();
    let mut preview = super::super::inspect_java_password(&path, &password).unwrap();
    assert!(preview.has_java_managed_keys());
    super::super::prepare_java_credentials(&mut preview, None, None, &local.0).unwrap();
    assert_eq!(preview.java_key_summary(), Some((1, 1)));
    assert_eq!(preview.java_password_summary(), None);
    assert!(
        preview
            .java_review()
            .unwrap()
            .2
            .iter()
            .any(|note| note.contains("passwords were skipped"))
    );
    super::super::queue_restore_at(&local.0, preview, RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    assert!(!local.0.join("vault").exists());
    let keys = Catalog::open(local.0.clone()).unwrap();
    let [plain] = keys.records() else {
        panic!("expected one unencrypted managed key");
    };
    assert!(plain.passphrase.is_none());
    assert_eq!(keys.read_key(plain).unwrap().as_slice(), JAVA_CLEAR_KEY);
    let routes = crate::connection_store::Store::open(local.0.join(CONNECTIONS)).unwrap();
    assert!(routes.connections().iter().any(|route| {
        route.name == "Plain key"
            && matches!(route.login.auth, crate::connection_store::Auth::StoredKey(id) if id == plain.id)
    }));
    assert!(!routes.connections().iter().any(|route| {
        route.name == "Protected key"
            && matches!(
                route.login.auth,
                crate::connection_store::Auth::StoredKey(_)
            )
    }));
}

#[test]
fn java_managed_keys_restore_replays_and_preflights_before_touching_the_vault() {
    let source = Scratch::new();
    let (path, password) = java_managed_keys_archive(&source.0);
    let local = Scratch::new();
    let directory = Directory::open(&local.0, false).unwrap();
    directory.private_child("vault", true).unwrap();
    let vault_path = crate::profile::vault_path_at(&local.0);
    let destination_master = b"native destination password";
    let mut vault = Vault::create(&vault_path, destination_master).unwrap();
    let original_passphrase = vault
        .save(
            CredentialInput {
                label: "Existing key passphrase".into(),
                kind: CredentialKind::KeyPassphrase,
                ..CredentialInput::default()
            },
            Secret::new("existing-native-passphrase".into()),
        )
        .unwrap();
    let existing_key = source.0.join("existing-identity");
    fs::write(&existing_key, JAVA_CLEAR_KEY).unwrap();
    let mut local_keys = Catalog::open(local.0.clone()).unwrap();
    let report = local_keys
        .apply(vec![Edit::Add(KeyDraft {
            name: "Existing identity".into(),
            description: String::new(),
            input: KeyInput::Copy(existing_key),
            passphrase: Some(original_passphrase.clone()),
        })])
        .unwrap();
    let original_id = report.added[0];
    let original_record = local_keys.get(original_id).unwrap().clone();
    let original_catalogue = fs::read(local.0.join(ssh_key_catalog::CATALOG)).unwrap();
    let original_vault = fs::read(&vault_path).unwrap();

    let mut preview = super::super::inspect_java_password(&path, &password).unwrap();
    let java_master = Secret::new("päss🔑例".into());
    super::super::prepare_java_credentials(
        &mut preview,
        Some(&java_master),
        Some(&vault),
        &local.0,
    )
    .unwrap();
    assert_eq!(preview.java_key_summary(), Some((2, 0)));
    assert_eq!(preview.java_password_summary(), Some((1, 0)));
    let prepared = preview
        .java_credentials
        .as_ref()
        .unwrap()
        .keys
        .as_ref()
        .unwrap();
    let key_name = prepared.catalogue.additions[0].token.clone();
    super::super::queue_restore_at(&local.0, preview, RestoreMode::JavaAdditive).unwrap();
    let pending = fs::read(local.0.join(PENDING)).unwrap();
    assert_eq!(pending[PENDING_MAGIC_WITH_MODE.len()], JAVA_CREDENTIALS);
    for secret in [
        "fixture-only passphrase",
        java_master.as_str(),
        "café – 新🔒",
    ] {
        assert!(
            !pending
                .windows(secret.len())
                .any(|part| part == secret.as_bytes())
        );
    }
    drop(vault);

    directory
        .replace_file(
            ssh_key_catalog::CATALOG,
            &[original_catalogue.as_slice(), b"\n"].concat(),
        )
        .unwrap();
    assert!(ProfileLease::acquire(&local.0, || false).is_err());
    assert_eq!(fs::read(&vault_path).unwrap(), original_vault);
    assert!(pending_java_cancellation(&local.0).unwrap().is_some());
    directory
        .replace_file(ssh_key_catalog::CATALOG, &original_catalogue)
        .unwrap();
    let managed = directory
        .private_child(ssh_key_catalog::MANAGED, false)
        .unwrap();
    let companion = format!("{key_name}.pub");
    managed
        .publish_copy(&companion, b"foreign public companion")
        .unwrap();
    assert!(ProfileLease::acquire(&local.0, || false).is_err());
    assert_eq!(fs::read(&vault_path).unwrap(), original_vault);
    assert!(!managed.has_entry(&key_name).unwrap());
    managed.remove_copy(&companion).unwrap();
    assert!(pending_java_cancellation(&local.0).unwrap().is_some());

    let mut committing = pending;
    committing[PENDING_MAGIC_WITH_MODE.len()] = JAVA_CREDENTIALS_COMMITTING;
    directory.replace_file(PENDING, &committing).unwrap();
    managed.publish_copy(&key_name, JAVA_CLEAR_KEY).unwrap();
    assert!(pending_java_cancellation(&local.0).unwrap().is_none());
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    let installed = fs::read(local.0.join(ssh_key_catalog::CATALOG)).unwrap();
    let installed_vault = fs::read(&vault_path).unwrap();
    let agent_chat = fs::read(local.0.join(JAVA_AGENT_CHAT_PATH)).unwrap();
    let swarm = data::collect(&local.0).unwrap();
    let swarm_path = swarm
        .keys()
        .find(|path| path.starts_with("swarm-conversations/"))
        .unwrap();
    let environments = fs::read(local.0.join("environments.json")).unwrap();
    directory.publish_copy(PENDING, &committing).unwrap();
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    assert_eq!(fs::read(&vault_path).unwrap(), installed_vault);
    assert_eq!(
        fs::read(local.0.join(ssh_key_catalog::CATALOG)).unwrap(),
        installed
    );
    assert_eq!(
        fs::read(local.0.join(JAVA_AGENT_CHAT_PATH)).unwrap(),
        agent_chat
    );
    assert_eq!(
        fs::read(local.0.join(swarm_path)).unwrap(),
        swarm[swarm_path].as_slice()
    );
    assert_eq!(
        fs::read(local.0.join("environments.json")).unwrap(),
        environments
    );
    assert!(!directory.has_entry(PENDING).unwrap());

    let vault = Vault::unlock(&vault_path, destination_master).unwrap();
    assert_eq!(
        vault.secret(&original_passphrase),
        Some("existing-native-passphrase")
    );
    let keys = Catalog::open(local.0.clone()).unwrap();
    let [original, plain, encrypted] = keys.records() else {
        panic!("expected existing, clear and encrypted managed identities");
    };
    assert_eq!(original, &original_record);
    assert_eq!(keys.read_key(plain).unwrap().as_slice(), JAVA_CLEAR_KEY);
    let encrypted_bytes = keys.read_key(encrypted).unwrap();
    assert_eq!(encrypted_bytes.as_slice(), JAVA_ENCRYPTED_KEY);
    let passphrase = vault
        .secret(encrypted.passphrase.as_deref().unwrap())
        .unwrap();
    assert_eq!(passphrase, "fixture-only passphrase");
    kortty_ssh::validate_private_key(&encrypted_bytes, Some(passphrase)).unwrap();
    let routes = crate::connection_store::Store::open(local.0.join(CONNECTIONS)).unwrap();
    let route = routes
        .connections()
        .iter()
        .find(|route| route.name == "Protected key")
        .unwrap();
    let crate::connection_store::Auth::StoredKey(id) = route.login.auth else {
        panic!("expected native managed identity")
    };
    assert_eq!(id, encrypted.id);
    let crate::connection_store::Auth::VaultPassword(jump) = &route.jump.as_ref().unwrap().auth
    else {
        panic!("expected vaulted jump-host password")
    };
    assert_eq!(vault.secret(jump), Some("café – 新🔒"));
}

#[test]
fn java_swarm_backup_restores_archive_and_round_trips_native_backups() {
    use crate::workspace::saved_chats::backup;
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    let source = Scratch::new();
    let path = source.0.join("java-swarm-backup.zip");
    let password = Secret::new("archive-password".into());
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    let options = SimpleFileOptions::default()
        .with_aes_encryption_bytes(AesMode::Aes256, password.as_str().as_bytes());
    zip.start_file("swarm-chats.xml", options).unwrap();
    zip.write_all(JAVA_SWARM).unwrap();
    zip.start_file("ssh-host-keys.properties", options).unwrap();
    zip.write_all(include_bytes!(
        "../java_host_pins/fixtures/ssh-host-keys.properties"
    ))
    .unwrap();
    zip.finish().unwrap();
    let local = Scratch::new();
    let (_, local_id) = save_fixture_connection(&local.0, "local-swarm.example");
    let native_path = backup::save_swarm_draft_fixture(&local.0, &[local_id]).unwrap();
    let native_bytes = fs::read(local.0.join(&native_path)).unwrap();
    let preview = super::super::inspect_java_password(&path, &password).unwrap();
    assert!(preview.java_host_pins().is_some());
    let [archive_path]: [String; 1] = preview
        .entries()
        .map(str::to_owned)
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    assert_ne!(archive_path, native_path);
    assert!(
        preview
            .java_review()
            .unwrap()
            .1
            .iter()
            .any(|entry| entry == "Saved Swarm archives (1)")
    );
    super::super::queue_restore_at(&local.0, preview, RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    assert!(
        !local.0.join(PINS).exists(),
        "profile restore cannot approve host keys"
    );

    let archive_bytes = fs::read(local.0.join(&archive_path)).unwrap();
    let (_, name) = archive_path.split_once('/').unwrap();
    backup::validate_java_swarm(name, &archive_bytes).unwrap();
    let archive: serde_json::Value = serde_json::from_slice(&archive_bytes).unwrap();
    assert_eq!(archive["title"], "Grüße & 東京");
    assert_eq!(archive["messages"][1]["servers"][0]["elapsed_seconds"], 17);
    assert_eq!(archive["messages"][1]["servers"][0]["total_tokens"], 1205);
    assert_eq!(archive["messages"][2]["content"], "Check again?");
    assert_eq!(fs::read(local.0.join(&native_path)).unwrap(), native_bytes);

    let portable = data::collect(&local.0).unwrap();
    assert_eq!(portable[&archive_path].as_slice(), archive_bytes);
    let target = Scratch::new();
    queue_with_mode(&target.0, portable, RestoreMode::KeepExisting, false).unwrap();
    drop(ProfileLease::acquire(&target.0, || false).unwrap());
    assert_eq!(
        fs::read(target.0.join(&archive_path)).unwrap(),
        archive_bytes
    );
}

#[test]
fn java_chat_backup_replays_additively_and_preserves_local_chat_and_environment() {
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    const ORDINARY: &str = include_str!("../../workspace/saved_chats/fixtures/java-2.26.0.xml");
    const ORDINARY_PATH: &str = "ai-chats/11111111111141118111111111111111.json";
    let source = Scratch::new();
    let local = Scratch::new();
    let directory = Directory::open(&local.0, false).unwrap();
    let chats = directory.private_child("ai-chats", true).unwrap();
    let redactor = crate::workspace::saved_chats::java_xml_redactor(
        &crate::clipboard_policy::ClipboardPolicy::default().terminal_logging,
    )
    .unwrap();
    let local_chats = crate::workspace::saved_chats::backup::java_chat_entries(
        ORDINARY.replace("Grüße 東京", "Local title").as_bytes(),
        &redactor,
        |_, _| true,
    )
    .unwrap();
    let original = &local_chats.entries[ORDINARY_PATH];
    chats
        .publish_copy(ORDINARY_PATH.split_once('/').unwrap().1, original)
        .unwrap();
    let original_environment = br##"{"version":1,"custom":[{"id":"custom-a1b2c3d4","label":"Local lab"}],"colors":{"PRODUCTION":"#FFFFFF"}}"##;
    directory
        .publish_copy("environments.json", original_environment)
        .unwrap();

    let agent = std::str::from_utf8(JAVA_AGENT_CHAT).unwrap();
    let combined = format!(
        "{}{}",
        ORDINARY.split_once("</aiChats>").unwrap().0,
        agent.split_once("<aiChats>").unwrap().1,
    );
    let path = source.0.join("java-chat-backup.zip");
    let password = Secret::new("archive-password".into());
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    let options = SimpleFileOptions::default()
        .with_aes_encryption_bytes(AesMode::Aes256, password.as_str().as_bytes());
    for (name, bytes) in [
        ("ai-chats.xml", combined.as_bytes()),
        ("environments.xml", JAVA_ENVIRONMENTS),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    let preview = super::super::inspect_java_password(&path, &password).unwrap();
    assert!(!preview.has_java_passwords());
    assert!(preview.entries().any(|path| path == JAVA_AGENT_CHAT_PATH));
    assert!(preview.entries().any(|path| path == ORDINARY_PATH));
    assert!(preview.entries().any(|path| path == "environments.json"));
    super::super::queue_restore_at(&local.0, preview, RestoreMode::JavaAdditive).unwrap();
    let pending = fs::read(local.0.join(PENDING)).unwrap();
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    let bytes = fs::read(local.0.join(JAVA_AGENT_CHAT_PATH)).unwrap();
    data::ai::validate_java_chat(JAVA_AGENT_CHAT_PATH, &bytes).unwrap();
    let agent: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(agent["summary"]["title"], "Grüße from Agent");
    assert!(agent.get("agent_run").is_none());
    assert!(agent.get("agent_journal").is_none());
    assert!(agent["java"].get("agent_run_id").is_some());
    assert_eq!(
        fs::read(local.0.join(ORDINARY_PATH)).unwrap(),
        original.as_slice()
    );

    let environments = fs::read(local.0.join("environments.json")).unwrap();
    let settings: serde_json::Value = serde_json::from_slice(&environments).unwrap();
    assert_eq!(settings["custom"][0]["label"], "Local lab");
    assert_eq!(settings["custom"][1]["label"], "New environment");
    assert_eq!(settings["colors"]["PRODUCTION"], "#FFFFFF");
    assert_eq!(settings["colors"]["custom-deadbeef"], "#123456");

    directory.publish_copy(PENDING, &pending).unwrap();
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    assert_eq!(fs::read(local.0.join(JAVA_AGENT_CHAT_PATH)).unwrap(), bytes);
    assert_eq!(
        fs::read(local.0.join("environments.json")).unwrap(),
        environments
    );
    assert!(!directory.has_entry(PENDING).unwrap());
}

#[test]
fn java_password_restore_adds_supported_data_and_preserves_native_catalogues() {
    use std::io::Write;
    use zip::{AesMode, CompressionMethod, ZipWriter, write::SimpleFileOptions};

    let source = Scratch::new();
    let path = source.0.join("renamed-backup.bin");
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .with_aes_encryption_bytes(AesMode::Aes256, b"backup-pw");
    for (name, data) in [
        ("connections.xml", br#"<connections><connection id="java-route"><name>Java SSH</name><host>java.example</host><username>fixture</username></connection></connections>"#.as_slice()),
        ("themes.xml", br#"<themes><theme id="java-test"><name>Java Theme</name></theme></themes>"#),
        ("snippets.xml", include_bytes!("../../snippets/archive/fixtures/java-private-library-2.26.0.xml")),
        ("snippet-variables.xml", br#"<snippetVariables><variable><name>JAVA_BACKUP_TEST</name><value>restored</value></variable></snippetVariables>"#),
        ("projects/Java Project.kortty", br#"<project><name>Java Project</name><windows><window><tabs><tab><connectionId>java-route</connectionId></tab></tabs></window></windows></project>"#),
        ("credentials.xml", b"<credentials/>"),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
    assert_eq!(
        super::super::detect_format(&path).unwrap(),
        super::super::BackupFormat::JavaPassword
    );
    let preview =
        super::super::inspect_java_password(&path, &Secret::new("backup-pw".into())).unwrap();
    let (_, entries, omissions) = preview.java_review().unwrap();
    assert_eq!(entries.len(), 4, "{entries:?} {omissions:?}");
    assert!(
        omissions
            .iter()
            .any(|line| line.starts_with("credentials.xml:"))
    );

    let local = Scratch::new();
    let directory = Directory::open(&local.0, false).unwrap();
    let connections_path = local.0.join("connections.json");
    let initial = crate::connection_store::import::Preview::java_backup(br#"<connections><connection><name>Native SSH</name><host>native.example</host></connection></connections>"#).unwrap();
    let (initial, _) = initial.portable_java_backup().unwrap();
    directory
        .publish_copy("connections.json", &initial)
        .unwrap();
    directory
        .publish_copy("environments.json", ENVIRONMENTS)
        .unwrap();
    directory
        .publish_copy(data::WINDOW_SETTINGS, br#"{"remote_sidebar_width":222}"#)
        .unwrap();
    let snippets_path = local.0.join(snippets::FILE_NAME);
    let (initial_snippets, ..) = snippets::portable_java_backup(None, Some(br#"<snippetVariables><variable><name>LOCAL_TEST</name><value>kept</value></variable></snippetVariables>"#)).unwrap();
    directory
        .publish_copy(snippets::FILE_NAME, &initial_snippets)
        .unwrap();

    let delta = preview.snapshot;
    for _ in 0..2 {
        queue_with_mode(&local.0, delta.clone(), RestoreMode::JavaAdditive, false).unwrap();
        ProfileLease::acquire(&local.0, || false).unwrap();
    }
    let connections = crate::connection_store::Store::open(connections_path).unwrap();
    assert_eq!(
        connections
            .connections()
            .iter()
            .map(|profile| (profile.id, profile.name.as_str()))
            .collect::<Vec<_>>(),
        [(1, "Native SSH"), (2, "Java SSH")]
    );
    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(local.0.join(data::WINDOW_SETTINGS)).unwrap()).unwrap();
    assert_eq!(settings["remote_sidebar_width"], 222);
    assert_eq!(settings["themes"]["edited"][0]["name"], "Java Theme");
    let snippets = snippets::Library::open(snippets_path).unwrap();
    assert!(
        snippets
            .entries()
            .iter()
            .any(|snippet| snippet.name() == "Deploy α")
    );
    assert_eq!(snippets.variables().len(), 2);
    let path = data::existing_project_paths(&local.0).unwrap();
    assert_eq!(path.len(), 1);
    let bytes = fs::read(local.0.join(&path[0])).unwrap();
    assert!(
        std::str::from_utf8(&bytes)
            .unwrap()
            .contains("java.example")
    );
    assert_eq!(
        fs::read(local.0.join("environments.json")).unwrap(),
        ENVIRONMENTS
    );
    assert!(!directory.has_entry(PENDING).unwrap());
}

#[test]
fn java_ssh_password_restore_seals_and_replays_exact_routes() {
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    let source = Scratch::new();
    let local = Scratch::new();
    let directory = Directory::open(&local.0, false).unwrap();
    directory.private_child("vault", true).unwrap();
    let vault_path = crate::profile::vault_path_at(&local.0);
    let destination_master = b"native master password for test";
    let mut vault = Vault::create(&vault_path, destination_master).unwrap();
    let existing_id = vault
        .save(
            CredentialInput {
                label: "Existing credential".into(),
                ..CredentialInput::default()
            },
            Secret::new("existing-fixture".into()),
        )
        .unwrap();
    let initial_vault = fs::read(&vault_path).unwrap();
    let (mut catalog, original_id) = save_fixture_connection(&local.0, "preserved.example");
    let mut original = catalog.get(original_id).unwrap().clone();
    original.name = "Collision".into();
    original.login.endpoint.username = "fixture".into();
    catalog.save(original).unwrap();
    let original_plan = catalog.get(original_id).unwrap().plan();
    catalog.approve(original_id, original_plan.clone()).unwrap();
    let native = fs::read(local.0.join(CONNECTIONS)).unwrap();

    let java_field = include_str!("../java_secrets/fixtures/envelopes.txt")
        .lines()
        .next()
        .unwrap();
    let connections = format!(
        r#"<connections><connection id="stored-route"><name>Imported SSH</name><host>java.example</host><username>fixture</username><credentialId>stored-credential</credentialId><jumpServer><enabled>true</enabled><host>jump.example</host><username>jump-user</username><encryptedPassword>{java_field}</encryptedPassword></jumpServer></connection><connection id="collision-route"><name>Collision</name><host>preserved.example</host><username>fixture</username><encryptedPassword>{java_field}</encryptedPassword></connection></connections>"#
    );
    let credentials = format!(
        r#"<credentials><credential><id>stored-credential</id><name>Stored test</name><username>fixture</username><serverPattern>java.example</serverPattern><environmentId>custom-deadbeef</environmentId><passwordType>STORED</passwordType><encryptedPassword>{java_field}</encryptedPassword></credential></credentials>"#
    );
    let archive_path = source.0.join("java-passwords.zip");
    let mut archive = ZipWriter::new(File::create(&archive_path).unwrap());
    let options = SimpleFileOptions::default()
        .with_aes_encryption_bytes(AesMode::Aes256, b"archive-password");
    for (name, bytes) in [
        ("connections.xml", connections.as_bytes()),
        ("credentials.xml", credentials.as_bytes()),
        ("environments.xml", JAVA_ENVIRONMENTS),
        (
            "master.key",
            include_bytes!("../java_secrets/fixtures/master.properties"),
        ),
    ] {
        archive.start_file(name, options).unwrap();
        archive.write_all(bytes).unwrap();
    }
    archive.finish().unwrap();

    let archive_password = Secret::new("archive-password".into());
    let java_password = Secret::new("päss🔑例".into());
    let mut preview =
        super::super::inspect_java_password(&archive_path, &archive_password).unwrap();
    assert!(preview.has_java_passwords());
    assert!(
        super::super::prepare_java_credentials(
            &mut preview,
            Some(&archive_password),
            Some(&vault),
            &local.0,
        )
        .is_err()
    );
    assert!(preview.has_java_passwords());
    super::super::prepare_java_credentials(
        &mut preview,
        Some(&java_password),
        Some(&vault),
        &local.0,
    )
    .unwrap();
    assert_eq!(preview.java_password_summary(), Some((2, 1)));
    assert!(!preview.has_java_passwords());
    assert_eq!(fs::read(&vault_path).unwrap(), initial_vault);
    super::super::queue_restore_at(&local.0, preview, RestoreMode::JavaAdditive).unwrap();
    let pending = fs::read(local.0.join(PENDING)).unwrap();
    for secret in [
        java_field,
        java_password.as_str(),
        "café – 新🔒",
        "stored-credential",
    ] {
        assert!(
            !pending
                .windows(secret.len())
                .any(|bytes| bytes == secret.as_bytes())
        );
    }
    assert_eq!(fs::read(&vault_path).unwrap(), initial_vault);
    drop(vault);

    directory
        .replace_file(CONNECTIONS, &[native.as_slice(), b"\n"].concat())
        .unwrap();
    assert!(ProfileLease::acquire(&local.0, || false).is_err());
    assert_eq!(fs::read(&vault_path).unwrap(), initial_vault);
    assert!(directory.has_entry(PENDING).unwrap());
    let stale = pending_java_cancellation(&local.0).unwrap().unwrap();
    directory.remove_copy(PENDING).unwrap();
    directory.publish_copy(PENDING, &pending).unwrap();
    assert!(discard_pending_java(&stale).is_err());
    let cancellation = pending_java_cancellation(&local.0).unwrap().unwrap();
    discard_pending_java(&cancellation).unwrap();
    assert!(!directory.has_entry(PENDING).unwrap());
    assert_eq!(fs::read(&vault_path).unwrap(), initial_vault);
    directory.publish_copy(PENDING, &pending).unwrap();
    directory.replace_file(CONNECTIONS, &native).unwrap();
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    let installed_vault = fs::read(&vault_path).unwrap();
    let installed_connections = fs::read(local.0.join(CONNECTIONS)).unwrap();
    let mut committed = pending;
    committed[PENDING_MAGIC_WITH_MODE.len()] = JAVA_PASSWORDS_COMMITTING;
    directory.publish_copy(PENDING, &committed).unwrap();
    assert!(pending_java_cancellation(&local.0).unwrap().is_none());
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    assert_eq!(fs::read(&vault_path).unwrap(), installed_vault);
    assert_eq!(
        fs::read(local.0.join(CONNECTIONS)).unwrap(),
        installed_connections
    );
    assert!(!directory.has_entry(PENDING).unwrap());

    let vault = Vault::unlock(&vault_path, destination_master).unwrap();
    assert_eq!(vault.entries().len(), 3);
    assert_eq!(vault.secret(&existing_id), Some("existing-fixture"));
    let catalog = crate::connection_store::Store::open(local.0.join(CONNECTIONS)).unwrap();
    let [original, imported] = catalog.connections() else {
        panic!("expected two SSH profiles")
    };
    assert_eq!(original.id, 1);
    assert!(catalog.approved(original.id, &original_plan));
    assert!(matches!(
        original.login.auth,
        crate::connection_store::Auth::Prompt
    ));
    let crate::connection_store::Auth::VaultPassword(id) = &imported.login.auth else {
        panic!("expected vaulted SSH password");
    };
    let environment = &vault.get(id).unwrap().environment;
    assert_eq!(environment, "custom-deadbeef");
    let environments: serde_json::Value =
        serde_json::from_slice(&fs::read(local.0.join("environments.json")).unwrap()).unwrap();
    assert!(
        environments["custom"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == environment.as_str() && item["label"] == "New environment")
    );
    for login in [&imported.login, imported.jump.as_ref().unwrap()] {
        let crate::connection_store::Auth::VaultPassword(id) = &login.auth else {
            panic!("expected vault binding")
        };
        let credential = vault.get(id).unwrap();
        assert_eq!(credential.username, login.endpoint.username);
        assert!(credential.matches_host(&login.endpoint.host));
        assert_eq!(vault.secret(id), Some("café – 新🔒"));
    }
}

#[test]
fn ai_profile_restore_rechecks_endpoints_and_preserves_local_memory_and_statistics() {
    use crate::ai_manager::backup::{SETTINGS, STATISTICS};
    use crate::workspace::saved_chats::backup::save_fixtures;

    let source = Scratch::new();
    let source_directory = Directory::open(&source.0, false).unwrap();
    let (foreign_file, foreign_folder) = if cfg!(windows) {
        ("/Users/example/reference.md", "/Users/example/documents")
    } else {
        (
            r"C:\Users\example\reference.md",
            r"C:\Users\example\documents",
        )
    };
    let settings = serde_json::json!({
        "profiles": {"last_id": 1, "default": 1, "profiles": [{
            "id": 1,
            "name": "HTTPS profile",
            "connection_mode": "remote-open-ai",
            "endpoint": "https://models.example.com/v1",
            "model": "fixture-model",
            "credential_env": "FIXTURE_AI_KEY",
            "trusted_remote_endpoint": true,
            "reviewed_agent_https": true
        }]},
        "local": {
            "runtime_update": { "mode": "automatic_stable", "backend": "cpu" },
            "stores": [{
                "name": "Migrated knowledge",
                "sources": [foreign_file],
                "automatic_files": [foreign_file],
                "folders": [{
                    "root": foreign_folder,
                    "options": { "sync": "automatic", "includes": ["*.md"] },
                    "files": []
                }]
            }]
        }
    });
    source_directory
        .publish_copy(SETTINGS, settings.to_string().as_bytes())
        .unwrap();
    let memory = source_directory.private_child("memory", true).unwrap();
    memory
        .publish_copy("MEMORY.md", b"Use plain text reports.\n")
        .unwrap();
    memory
        .publish_copy("external.json", b"Machine-local approvals")
        .unwrap();
    let history = memory.private_child("history", true).unwrap();
    history
        .publish_copy("global-20261112-094535-122.md", b"A previous rule.\n")
        .unwrap();
    history
        .publish_copy("global-20261112-094535-123.md", b"api_key: abcd123456\n")
        .unwrap();
    let statistics = br#"{"version":1,"tokenizer":null,"skills":{}}"#;
    let stats = source_directory.private_child("stats", true).unwrap();
    stats.publish_copy("skill-usage.json", statistics).unwrap();
    save_fixtures(&source.0).unwrap();
    let gguf = source_directory
        .private_child("llama-models", true)
        .unwrap();
    let id = "11111111111111111111111111111111";
    let model = gguf.private_child(id, true).unwrap();
    let metadata =
        serde_json::json!({"id": id, "name": "Model", "bytes": 4096, "context_tokens": 2048});
    model
        .publish_copy("model-v1.json", metadata.to_string().as_bytes())
        .unwrap();
    model
        .publish_copy("model.gguf", b"Not included in the backup")
        .unwrap();

    let backup = data::collect(&source.0).unwrap();
    assert_eq!(backup.len(), 7);
    assert!(!backup.contains_key("memory/external.json"));
    assert!(!backup.contains_key("memory/history/global-20261112-094535-123.md"));
    assert!(!backup.contains_key(&format!("llama-models/{id}/model.gguf")));
    let raw: serde_json::Value = serde_json::from_slice(&backup[SETTINGS]).unwrap();
    let profile = &raw["profiles"]["profiles"][0];
    assert_eq!(profile["restore_review"], true);
    assert_eq!(profile["trusted_remote_endpoint"], false);
    assert_eq!(profile["credential_env"], "");
    assert!(profile.get("reviewed_agent_https").is_none());
    assert_eq!(raw["local"]["runtime_update"]["mode"], "off");
    let store = &raw["local"]["stores"][0];
    assert_eq!(store["sources"][0], foreign_file);
    assert!(store.get("automatic_files").is_none());
    assert_eq!(store["folders"][0]["root"], foreign_folder);
    assert_eq!(store["folders"][0]["options"]["sync"], "manual");

    let target = Scratch::new();
    let directory = Directory::open(&target.0, false).unwrap();
    directory
        .private_child("memory", true)
        .unwrap()
        .publish_copy("MEMORY.md", b"Use the local notes.\n")
        .unwrap();
    directory
        .private_child("stats", true)
        .unwrap()
        .publish_copy(
            "skill-usage.json",
            b"{\"version\":1,\"skills\":{},\"tokenizer\":null}",
        )
        .unwrap();
    queue(&target.0, backup.clone(), false).unwrap();
    drop(ProfileLease::acquire(&target.0, || false).unwrap());
    let installed = data::collect(&target.0).unwrap();
    assert_eq!(&*installed["memory/MEMORY.md"], b"Use the local notes.\n");
    assert_ne!(installed[STATISTICS], backup[STATISTICS]);
    for (path, bytes) in backup
        .iter()
        .filter(|(path, _)| !matches!(path.as_str(), "memory/MEMORY.md" | STATISTICS))
    {
        assert_eq!(&installed[path], bytes);
    }

    // Old archives still use their original replacement rules for the original stores.
    queue(&target.0, snapshot(), false).unwrap();
    drop(ProfileLease::acquire(&target.0, || false).unwrap());
    let after_old_backup = data::collect(&target.0).unwrap();
    for (path, bytes) in installed {
        assert_eq!(after_old_backup[&path], bytes);
    }
}

fn snippet_store(category: &str, content: &str) -> Zeroizing<Vec<u8>> {
    let data = serde_json::json!({"version": 1, "snippets": [{
        "id": "8de01df30c98452d9ae5d88ec0abf4b6",
        "name": "Fixture",
        "language": "bash",
        "category": category,
        "content": content,
    }]});
    Zeroizing::new(serde_json::to_vec(&data).unwrap())
}

#[test]
fn restore_checks_custom_header_policy_when_queued_and_again_at_startup() {
    for mode in [RestoreMode::Replace, RestoreMode::KeepExisting] {
        let target = Scratch::new();
        let root = &target.0;
        let mut backup = snapshot();
        backup.insert(
            snippets::FILE_NAME.into(),
            snippet_store("sCrIpT-HeAdEr", "# saved"),
        );
        assert_eq!(
            queue_with_header_policy(root, backup.clone(), mode, false, || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied,
        );
        assert!(!root.join(PENDING).exists());
        queue_with_header_policy(root, backup, mode, false, || true).unwrap();
        assert!(matches!(
            ProfileLease::acquire_with_header_policy(root, || false, || false),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied
        ));
        assert!(root.join(PENDING).exists());
        assert!(!root.join(snippets::FILE_NAME).exists());
        assert!(!root.join("environments.json").exists());

        let allowed = std::cell::Cell::new(true);
        assert!(matches!(
            ProfileLease::acquire_with_header_policy(root, || false, || allowed.replace(false)),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied
        ));
        assert!(root.join(PENDING).exists());
        assert!(!root.join(snippets::FILE_NAME).exists());

        ProfileLease::acquire_with_header_policy(root, || false, || true).unwrap();
        assert!(root.join(snippets::FILE_NAME).is_file());
        assert!(!root.join(PENDING).exists());
    }
}

#[test]
fn restore_distinguishes_existing_headers_from_reclassified_snippets() {
    let target = Scratch::new();
    let root = &target.0;
    let directory = Directory::open(root, false).unwrap();
    let original = snippet_store("Operations", "# local");
    directory
        .publish_copy(snippets::FILE_NAME, &original)
        .unwrap();
    let mut backup = snapshot();
    let edited = snippet_store("Script-Header", "# edited");
    backup.insert(snippets::FILE_NAME.into(), edited.clone());

    assert_eq!(
        queue_with_header_policy(root, backup.clone(), RestoreMode::Replace, false, || false)
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied,
    );
    queue_with_header_policy(
        root,
        backup.clone(),
        RestoreMode::KeepExisting,
        false,
        || false,
    )
    .unwrap();
    ProfileLease::acquire_with_header_policy(root, || false, || false).unwrap();
    assert_eq!(
        fs::read(root.join(snippets::FILE_NAME)).unwrap(),
        original.as_slice()
    );

    let established = snippet_store("script-header", "# established local header");
    directory
        .replace_file(snippets::FILE_NAME, &established)
        .unwrap();
    queue_with_header_policy(root, backup, RestoreMode::Replace, false, || false).unwrap();
    ProfileLease::acquire_with_header_policy(root, || false, || false).unwrap();
    assert_eq!(
        fs::read(root.join(snippets::FILE_NAME)).unwrap(),
        edited.as_slice()
    );
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
fn keep_existing_run_journal_retention_preserves_local_history() {
    use std::time::{Duration, SystemTime};

    use kortty_jobs::{Job, JobGraph, JobId, RunOutcome, Scheduler, SchedulerStore};

    use crate::scheduler_window::{SchedulerWindow, SshCommand};

    let legacy = Scratch::new();
    let action: SshCommand = serde_json::from_value(serde_json::json!({
        "host": "fixture.example",
        "port": 22,
        "username": "operator",
        "key_file": legacy.0.join("fixture-key"),
        "command": "true",
    }))
    .unwrap();
    let job = JobId::new("fixture").unwrap();
    let graph = JobGraph::new(vec![Job::new(job.clone(), "Fixture", action).unwrap()]).unwrap();
    let mut scheduler = Scheduler::new(graph);
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(60);
    let run = scheduler.start_manual(&job, at).unwrap().started[0].id;
    let finished = scheduler.complete(run, RunOutcome::Success, at).unwrap();
    let legacy_jobs = legacy.0.join(JOBS);
    SchedulerStore::open(&legacy_jobs)
        .unwrap()
        .commit_with_actions(scheduler.graph(), &finished.finished, &[])
        .unwrap();
    let saved_history = fs::read(&legacy_jobs).unwrap();

    let fresh = Scratch::new();
    let replay = Scratch::new();
    let incoming_jobs = data::portable_jobs(OLD_JOBS).unwrap();
    // Simulate an earlier replay publishing the portable jobs before settings.
    fs::write(replay.0.join(JOBS), &incoming_jobs).unwrap();
    let mut backup = Snapshot::from([
        (JOBS.into(), Zeroizing::new(incoming_jobs)),
        (
            data::WINDOW_SETTINGS.into(),
            Zeroizing::new(
                br#"{"job_scheduler_journal_retention_days":14,"manual_scale":140,"future_preference":{"values":[1,2]}}"#
                    .to_vec(),
            ),
        ),
    ]);
    for (root, expected_days) in [(&legacy.0, 0), (&fresh.0, 14), (&replay.0, 14)] {
        queue_with_mode(root, backup.clone(), RestoreMode::KeepExisting, false).unwrap();
        assert!(!root.join(data::WINDOW_SETTINGS).exists());
        let _lease = ProfileLease::acquire(root, || false).unwrap();
        let bytes = fs::read(root.join(data::WINDOW_SETTINGS)).unwrap();
        let preferences: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let days: u16 =
            serde_json::from_value(preferences["job_scheduler_journal_retention_days"].clone())
                .unwrap();
        assert_eq!(days, expected_days);
        assert_eq!(preferences["manual_scale"], 140);
        assert_eq!(
            preferences["future_preference"],
            serde_json::json!({"values":[1,2]})
        );
        drop(SchedulerWindow::open_with_journal_retention(root.join(JOBS), days).unwrap());
    }
    let legacy_settings = legacy.0.join(data::WINDOW_SETTINGS);
    fs::remove_file(&legacy_settings).unwrap();
    backup.insert(data::WINDOW_SETTINGS.into(), Zeroizing::new(b"[]".to_vec()));
    queue_with_mode(&legacy.0, backup, RestoreMode::KeepExisting, false).unwrap();
    let _lease = ProfileLease::acquire(&legacy.0, || false).unwrap();
    assert_eq!(fs::read(legacy_settings).unwrap(), b"[]");
    assert_eq!(fs::read(&legacy_jobs).unwrap(), saved_history);
    assert!(
        SchedulerStore::open(legacy_jobs)
            .unwrap()
            .journal_record(run)
            .is_some()
    );
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

fn save_fixture_connection(root: &Path, host: &str) -> (crate::connection_store::Store, u64) {
    use kortty_terminal::TerminalProfile;

    use crate::connection_store::{
        Auth, Connection, SavedLogin, Store, TunnelConfig, TunnelRule, TunnelTarget,
    };
    use crate::ssh_dialog::LoginTarget;

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
            window_geometry: None,
            terminal: Default::default(),
        })
        .unwrap();
    (store, id)
}

#[test]
fn keep_existing_imports_named_swarm_draft_and_revokes_different_saved_connections() {
    use crate::workspace::saved_chats::backup::save_swarm_draft_fixture;

    let source = Scratch::new();
    let (_, source_id) = save_fixture_connection(&source.0, "source.example");
    let path = save_swarm_draft_fixture(&source.0, &[source_id]).unwrap();
    let backup = data::collect(&source.0).unwrap();

    for (host, retain_route) in [("source.example", true), ("local.example", false)] {
        let target = Scratch::new();
        let (_, target_id) = save_fixture_connection(&target.0, host);
        assert_eq!(target_id, source_id);
        queue_with_mode(&target.0, backup.clone(), RestoreMode::KeepExisting, false).unwrap();
        drop(ProfileLease::acquire(&target.0, || false).unwrap());

        let installed = data::collect(&target.0).unwrap();
        let draft: serde_json::Value = serde_json::from_slice(&installed[&path]).unwrap();
        assert_eq!(draft["title"], "Saved Swarm draft");
        assert_eq!(draft["turns"], serde_json::json!([]));
        if retain_route {
            assert_eq!(installed[&path], backup[&path]);
        } else {
            assert_eq!(draft["draft_routes"], serde_json::json!([]));
        }
    }
}

#[test]
fn keep_existing_imports_jobs_only_with_their_matching_saved_connection() {
    use kortty_jobs::{Job, JobGraph, JobId, SchedulerStore};

    use crate::connection_store::Store;
    use crate::scheduler_window::{ConnectionRef, SshCommand};

    let source = Scratch::new();
    let (store, id) = save_fixture_connection(&source.0, "source.example");
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
    let (mut local, local_id) = save_fixture_connection(&matching.0, "source.example");
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
    let (_, different_id) = save_fixture_connection(&different.0, "local.example");
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
