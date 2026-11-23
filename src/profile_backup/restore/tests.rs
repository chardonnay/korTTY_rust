use kortty_vault::{CredentialInput, CredentialKind, Secret, Vault};

use super::*;
use crate::ssh_key_catalog::{Edit, KeyDraft, KeyInput};

mod java_toolbar;

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

#[test]
fn java_manual_question_history_restores_masked_and_preserves_local_order() {
    use crate::terminal_agent::{
        Mode,
        history::{self as agent_history, Kind},
    };
    use crate::workspace::question_history::{MANUAL_FILE, TERMINAL_FILE, WORKFLOW_FILE};
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    let source = Scratch::new();
    let path = source.0.join("java-manual-questions.zip");
    let password = Secret::new("fixture archive password".into());
    let mut archive = ZipWriter::new(File::create(&path).unwrap());
    archive
        .start_file(
            "global-settings.xml",
            SimpleFileOptions::default()
                .with_aes_encryption_bytes(AesMode::Aes256, password.as_str().as_bytes()),
        )
        .unwrap();
    archive
        .write_all(
            r#"<globalSettings><guideAskHistory>
                <question>Archive &amp; copies?</question>
                <question>How does Scheduler work? password=fixture-secret</question>
                <question>How does Scheduler work? password=other-secret</question>
                <question><markup>Invalid</markup></question>
                <question>ö 東京</question>
            </guideAskHistory><aiPromptHistory>
                <prompt>Explain the output
password=fixture-secret</prompt>
            </aiPromptHistory><workflowInstructionsHistory>
                <entry>Summarize the status
password=fixture-secret</entry>
            </workflowInstructionsHistory>
            <terminalAgentInputHistorySize>101</terminalAgentInputHistorySize>
            <terminalAgentInputHistory>
                <input><prompt>Existing agent prompt</prompt><lastUsedEpochMillis>1234000</lastUsedEpochMillis></input>
                <input><prompt>Inspect the status password=fixture-secret</prompt><lastUsedEpochMillis>1233000</lastUsedEpochMillis></input>
                <input><prompt>Malformed timestamp</prompt><lastUsedEpochMillis>-1</lastUsedEpochMillis></input>
            </terminalAgentInputHistory></globalSettings>"#
                .as_bytes(),
        )
        .unwrap();
    archive.finish().unwrap();
    let preview = || super::super::inspect_java_password(&path, &password).unwrap();

    let target = Scratch::new();
    let installed = target.0.join(MANUAL_FILE);
    let local = br#"["Native guide question","Archive & copies?"]"#;
    Directory::open(&target.0, false)
        .unwrap()
        .publish_copy(MANUAL_FILE, local)
        .unwrap();
    agent_history::remember(&target.0, "Existing agent prompt".into(), Mode::Ask, false).unwrap();
    let before = agent_history::list(&target.0, false).unwrap();
    agent_history::set_limit(&target.0, &before, 5, false).unwrap();
    let reviewed = preview();
    let (_, entries, _) = reviewed.java_review().unwrap();
    assert!(
        entries
            .iter()
            .any(|line| line == "Masked manual questions for available history slots (3)")
    );
    let staged = &reviewed.snapshot[MANUAL_FILE];
    assert!(
        !std::str::from_utf8(staged)
            .unwrap()
            .contains("fixture-secret")
    );
    assert_eq!(
        serde_json::from_slice::<Vec<String>>(&reviewed.snapshot[TERMINAL_FILE]).unwrap(),
        ["Explain the output\npassword=***"]
    );
    let workflow = serde_json::to_vec(&["Summarize the status\npassword=***"]).unwrap();
    assert_eq!(reviewed.snapshot[WORKFLOW_FILE].as_slice(), workflow);
    assert!(
        entries
            .iter()
            .any(|line| line == "Masked Agent prompts (2) and a history limit for an unset store")
    );
    let staged: agent_history::Snapshot =
        serde_json::from_slice(&reviewed.snapshot[agent_history::BACKUP_PATH]).unwrap();
    assert_eq!(staged.limit, 100);
    assert_eq!(staged.entries[1].goal, "Inspect the status password=***");
    super::super::queue_restore_at(&target.0, reviewed, RestoreMode::JavaAdditive).unwrap();
    assert_eq!(fs::read(&installed).unwrap(), local);
    drop(ProfileLease::acquire(&target.0, || false).unwrap());
    let history = fs::read(&installed).unwrap();
    assert_eq!(
        serde_json::from_slice::<Vec<String>>(&history).unwrap(),
        [
            "Native guide question",
            "Archive & copies?",
            "How does Scheduler work? password=***",
            "ö 東京"
        ]
    );
    let portable = data::collect(&target.0).unwrap();
    assert_eq!(portable[MANUAL_FILE].as_slice(), history);
    assert!(portable.contains_key(TERMINAL_FILE));
    assert_eq!(portable[WORKFLOW_FILE].as_slice(), workflow);
    let agent = agent_history::list(&target.0, false).unwrap();
    assert_eq!(agent.limit, 5);
    assert_eq!(agent.entries.len(), 2);
    assert!(agent.entries[0] == before.entries[0] && agent.entries[0].kind == Kind::Ask);
    assert!(agent.entries[1].kind == Kind::Imported);
    assert_eq!(agent.entries[1].started_at, 1233000);
    assert!(
        agent_history::list(&target.0, true)
            .unwrap()
            .entries
            .is_empty()
    );
    let native_path = source.0.join("native-manual-questions.zip");
    archive::write(&native_path, &portable, &password).unwrap();
    let native = super::super::inspect_backup(&native_path, &password).unwrap();
    let reopened = Scratch::new();
    super::super::queue_restore_at(&reopened.0, native, RestoreMode::KeepExisting).unwrap();
    drop(ProfileLease::acquire(&reopened.0, || false).unwrap());
    assert_eq!(fs::read(reopened.0.join(MANUAL_FILE)).unwrap(), history);
    assert_eq!(fs::read(reopened.0.join(WORKFLOW_FILE)).unwrap(), workflow);
    assert!(agent_history::list(&reopened.0, false).unwrap() == agent);
    let fresh = Scratch::new();
    super::super::queue_restore_at(&fresh.0, preview(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&fresh.0, || false).unwrap());
    assert!(agent_history::list(&fresh.0, false).unwrap() == staged);
    super::super::queue_restore_at(&target.0, preview(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&target.0, || false).unwrap());
    assert_eq!(fs::read(installed).unwrap(), history);
    assert!(agent_history::list(&target.0, false).unwrap() == agent);
}

#[test]
fn java_openpgp_catalogue_restores_only_inactive_full_fingerprints() {
    use crate::snippets::recipients::{Action, FILE_NAME};
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    let scratch = Scratch::new();
    let path = scratch.0.join("java-openpgp.zip");
    let password = Secret::new("java-archive-password".into());
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    zip.start_file(
        "gpg-keys.xml",
        SimpleFileOptions::default()
            .with_aes_encryption_bytes(AesMode::Aes256, password.as_str().as_bytes()),
    )
    .unwrap();
    zip.write_all(
        br#"<gpgKeys>
        <key><id>v4-java</id><name>Java public recipient</name>
            <keyId>0x9876543210fedcba</keyId>
            <fingerprint>0123456789ab cdef01234567 9876543210fedcba</fingerprint>
            <publicKeyPath>/fixture/do-not-follow.asc</publicKeyPath>
            <email>fixture@example.invalid</email><createdAt>1</createdAt></key>
        <key><id>v6-java</id><name>Java v6 recipient</name>
            <keyId>AABBCCDDEEFF0011000000000000000000000000000000000000000000000000</keyId></key>
        <key><id>mismatched</id><name>Incorrect key ID</name>
            <keyId>0123456789abcdef</keyId>
            <fingerprint>1111111111111111111111111111111111111111</fingerprint></key>
        <key><id>short</id><name>Short key ID</name><keyId>9876543210fedcba</keyId></key>
        <key><id>duplicate</id><name>Duplicate fingerprint</name><keyId>10fedcba</keyId>
            <fingerprint>0123456789abcdef012345679876543210fedcba</fingerprint></key>
    </gpgKeys>"#,
    )
    .unwrap();
    zip.finish().unwrap();

    let local = Scratch::new();
    let destination = local.0.join(FILE_NAME);
    let inspect = || super::super::inspect_java_password(&path, &password).unwrap();
    let preview = inspect();
    let (_, entries, omissions) = preview.java_review().unwrap();
    assert!(
        entries
            .iter()
            .any(|line| line.contains("fingerprint verification (2)"))
    );
    assert!(omissions.iter().any(|line| {
        line.contains("invalid or missing full fingerprints or metadata omitted (2)")
    }));
    assert!(
        omissions
            .iter()
            .any(|line| line.contains("Duplicate Java OpenPGP references omitted (1)"))
    );
    super::super::queue_restore_at(&local.0, preview, RestoreMode::JavaAdditive).unwrap();
    assert!(!destination.exists());
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    assert!(Action::List(destination.clone()).run().unwrap().is_empty());
    let catalogue = Action::List(destination.clone()).run_management().unwrap();
    assert_eq!(catalogue.pending_java.len(), 2);
    assert_eq!(
        catalogue.pending_java[0].fingerprint(),
        "0123456789ABCDEF012345679876543210FEDCBA"
    );
    let bytes = fs::read(&destination).unwrap();
    let stored = std::str::from_utf8(&bytes).unwrap();
    assert!(!stored.contains("do-not-follow"));
    assert!(!stored.contains("fixture@example.invalid"));

    super::super::queue_restore_at(&local.0, inspect(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    assert_eq!(fs::read(destination).unwrap(), bytes);
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
fn java_global_memory_restore_is_additive_and_excludes_unmapped_scopes() {
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    const GLOBAL: &str = "memory/MEMORY.md";
    const HISTORY: &str = "memory/history/global-20261102-131415-019-2.md";
    const ARCHIVE: &str = "memory/archive.md";
    const SCOPED: &str = "memory/connections/remote-uuid.md";
    const SECRET_HISTORY: &str = "memory/history/global-20261102-131415-020.md";
    const STATE: &str = "memory/state.json";
    const ARCHIVE_ENTRY: &[u8] = b"<!-- kortty:archived scope=global -->\r\n\
<!-- kortty:entry id=java-archive origin=korTTY sha256=68b692b74b8eb17e8a9f4d37b85a9a6aa4c5b9c419396edd17323fb1155e0f82 created=2026-11-02 source=chat -->\r\n\
- Check the machine.\r\n\
<!-- /kortty:entry -->\r\n";
    const SCOPED_ARCHIVE: &[u8] =
        b"\xef\xbb\xbf<!-- kortty:archived scope=connection:cmVtb3RlLXV1aWQ -->\r";

    let source = Scratch::new();
    let path = source.0.join("java-memory.zip");
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    let options = SimpleFileOptions::default().with_aes_encryption_bytes(AesMode::Aes256, b"pw");
    for (name, bytes) in [
        (GLOBAL, "# Erinnerungen\r\n- Gerät prüfen.\r\n".as_bytes()),
        (HISTORY, b"Earlier notes.\n"),
        (ARCHIVE, ARCHIVE_ENTRY),
        (SCOPED, b"Connection notes.\n"),
        (STATE, b"{}"),
        (SECRET_HISTORY, b"-----BEGIN PRIVATE KEY-----\n"),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    let inspect = || super::super::inspect_java_password(&path, &Secret::new("pw".into())).unwrap();
    let preview = inspect();
    let (_, entries, omissions) = preview.java_review().unwrap();
    assert!(
        entries
            .iter()
            .any(|line| line == "Global AI Memory files (3)")
    );
    for omitted in [SCOPED, STATE, SECRET_HISTORY] {
        assert!(omissions.iter().any(|line| line.starts_with(omitted)));
    }

    let local = Scratch::new();
    super::super::queue_restore_at(&local.0, preview, RestoreMode::JavaAdditive).unwrap();
    ProfileLease::acquire(&local.0, || false).unwrap();
    assert_eq!(
        fs::read(local.0.join(GLOBAL)).unwrap(),
        "# Erinnerungen\r\n- Gerät prüfen.\r\n".as_bytes()
    );
    assert_eq!(
        fs::read(local.0.join(HISTORY)).unwrap(),
        b"Earlier notes.\n"
    );
    assert_eq!(fs::read(local.0.join(ARCHIVE)).unwrap(), ARCHIVE_ENTRY);
    for omitted in [SCOPED, STATE, SECRET_HISTORY] {
        assert!(!local.0.join(omitted).exists());
    }

    fs::write(local.0.join(GLOBAL), b"Locally edited.\n").unwrap();
    super::super::queue_restore_at(&local.0, inspect(), RestoreMode::JavaAdditive).unwrap();
    ProfileLease::acquire(&local.0, || false).unwrap();
    assert_eq!(
        fs::read(local.0.join(GLOBAL)).unwrap(),
        b"Locally edited.\n"
    );

    for (path, bytes) in [(ARCHIVE, SCOPED_ARCHIVE), (SCOPED, b"Connection notes.\n")] {
        let delta = Snapshot::from([(path.into(), Zeroizing::new(bytes.to_vec()))]);
        assert!(queue_with_mode(&local.0, delta, RestoreMode::JavaAdditive, false).is_err());
    }
}

#[test]
fn java_ssh_memory_follows_a_new_exact_route_and_replays_without_overwriting() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    const ID: &str = "f827d86d-1531-4cc5-9d0f-f6e5c1a2d850";
    const ORPHAN: &str = "bf251306-48fc-4f95-a82c-82c92b63dd17";
    const ALIASED: &str = "31c3a185-6f6a-4960-bd6a-29a615c5d56f";
    const STAMP: &str = "20261102-131415-019-2.md";
    const ARCHIVE: &str = "memory/archive.md";
    const DESTINATION: &str = "memory/connections/2.md";
    const ARCHIVED_ENTRY: &str = "<!-- kortty:entry id=fixture origin=korTTY sha256=68b692b74b8eb17e8a9f4d37b85a9a6aa4c5b9c419396edd17323fb1155e0f82 created=2026-11-02 source=chat -->\n- Check the machine.\n<!-- /kortty:entry -->\n";
    let connections = format!(
        "<connections><connection id='{ID}'><name>Java SSH</name>\
        <host>java.example</host><username>tester</username></connection>\
        <connection id='{ALIASED}'><name>Ambiguous SSH</name>\
        <host>ambiguous.example</host><username>tester</username></connection>\
        <connection id='{}'><protocol>UNRECOGNIZED</protocol></connection></connections>",
        ALIASED.to_ascii_uppercase()
    );
    let scope = |id: &str| {
        format!(
            "<!-- kortty:archived scope=connection:{} -->",
            URL_SAFE_NO_PAD.encode(id)
        )
    };
    let backup_archive = format!(
        "<!-- kortty:archived scope=global -->\n{ARCHIVED_ENTRY}\n{}\r\n{ARCHIVED_ENTRY}",
        scope(ID)
    );
    let source = Scratch::new();
    let path = source.0.join("java-ssh-memory.zip");
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    let options = SimpleFileOptions::default().with_aes_encryption_bytes(AesMode::Aes256, b"pw");
    for (name, bytes) in [
        (CONNECTIONS.replace(".json", ".xml"), connections.as_bytes()),
        (
            format!("memory/connections/{ID}.md"),
            "Machine α.\r\n".as_bytes(),
        ),
        (
            format!("memory/history/connection-{ID}-{STAMP}"),
            b"Prior notes.\n",
        ),
        (
            format!("memory/connections/{ORPHAN}.md"),
            b"Unmatched notes.\n",
        ),
        (
            format!("memory/connections/{ALIASED}.md"),
            b"Ambiguous notes.\n",
        ),
        (ARCHIVE.into(), backup_archive.as_bytes()),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    let inspect = || super::super::inspect_java_password(&path, &Secret::new("pw".into())).unwrap();
    let local = Scratch::new();
    let directory = Directory::open(&local.0, false).unwrap();
    let native = crate::connection_store::import::Preview::java_backup(b"<connections><connection><name>Native</name><host>local.example</host></connection></connections>").unwrap();
    let (native, _) = native.portable_java_backup().unwrap();
    directory.publish_copy(CONNECTIONS, &native).unwrap();
    let preview = inspect();
    let (_, _, omissions) = preview.java_review().unwrap();
    assert!(omissions.iter().any(|note| note
        == "AI Memory documents or history files omitted because Java connection filenames collide (1)"));
    let queued =
        super::super::queue_restore_at(&local.0, preview, RestoreMode::JavaAdditive).unwrap();
    assert_eq!(
        queued.scheduler_notes,
        [
            "Connection AI Memory files omitted because their scopes could not be safely imported (1)",
            "Connection AI Memory files ready to add where missing (3)",
        ]
    );
    let mut pending = fs::read(local.0.join(PENDING)).unwrap();
    assert_eq!(pending[PENDING_MAGIC_WITH_MODE.len()], JAVA_MEMORY);

    let revision = String::from_utf8(native.clone())
        .unwrap()
        .replace("local.example", "changed.example");
    directory
        .replace_file(CONNECTIONS, revision.as_bytes())
        .unwrap();
    assert!(ProfileLease::acquire(&local.0, || false).is_err());
    assert!(!local.0.join(DESTINATION).exists());
    directory.replace_file(CONNECTIONS, &native).unwrap();

    let mut forged = pending.clone();
    let index = forged
        .windows(b"Prior notes.".len())
        .position(|window| window == b"Prior notes.")
        .unwrap();
    forged[index] = b'p';
    directory.replace_file(PENDING, &forged).unwrap();
    assert!(ProfileLease::acquire(&local.0, || false).is_err());
    assert!(!local.0.join(DESTINATION).exists());
    directory.replace_file(PENDING, &pending).unwrap();

    ProfileLease::acquire(&local.0, || false).unwrap();
    let catalogue = crate::connection_store::Store::open(local.0.join(CONNECTIONS)).unwrap();
    assert_eq!(
        catalogue.get(2).unwrap().login.endpoint.host,
        "java.example"
    );
    assert_eq!(
        catalogue.get(3).unwrap().login.endpoint.host,
        "ambiguous.example"
    );
    assert_eq!(
        fs::read(local.0.join(DESTINATION)).unwrap(),
        "Machine α.\r\n".as_bytes()
    );
    assert_eq!(
        fs::read(local.0.join(format!("memory/history/connection-2-{STAMP}"))).unwrap(),
        b"Prior notes.\n"
    );
    assert_eq!(
        fs::read_to_string(local.0.join(ARCHIVE)).unwrap(),
        backup_archive.replace(&scope(ID), &scope("2"))
    );
    assert!(!local.0.join("memory/connections/1.md").exists());
    assert!(!local.0.join("memory/connections/3.md").exists());

    fs::write(local.0.join(DESTINATION), b"Locally edited.\n").unwrap();
    pending[PENDING_MAGIC_WITH_MODE.len()] = JAVA_MEMORY_COMMITTING;
    directory.publish_copy(PENDING, &pending).unwrap();
    ProfileLease::acquire(&local.0, || false).unwrap();
    assert_eq!(
        fs::read(local.0.join(DESTINATION)).unwrap(),
        b"Locally edited.\n"
    );

    super::super::queue_restore_at(&local.0, inspect(), RestoreMode::JavaAdditive).unwrap();
    ProfileLease::acquire(&local.0, || false).unwrap();
    assert_eq!(
        fs::read(local.0.join(DESTINATION)).unwrap(),
        b"Locally edited.\n"
    );
    assert!(!local.0.join("memory/connections/3.md").exists());
}

#[test]
fn java_memory_restores_legacy_ssh_and_local_shell_ids_only_for_new_profiles() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    const LOCAL_ID: &str = "Ops Café/📦";
    const LOCAL_NAME: &str = "Ops_Caf___-6880d6da";
    const SSH_ID: &str = "Ops:@Bastion";
    const SSH_NAME: &str = "Ops__Bastion-88b26434";
    const STAMP: &str = "20261102-131415-019-2.md";
    const DESTINATION: &str = "memory/connections/2.md";
    const ARCHIVE: &str = "memory/archive.md";
    let connections = format!(
        "<connections>\
            <connection id='{SSH_ID}'><name>SSH</name><host>bastion.example</host>\
                <username>tester</username></connection>\
            <connection id='\u{2003}{LOCAL_ID} '><name>Local shell</name><protocol>LOCAL_SHELL</protocol>\
                <localShellWorkingDirectory>/tmp/java-shell</localShellWorkingDirectory></connection>\
            <connection id='duplicate-one'><name>Duplicate shell</name><protocol>LOCAL_SHELL</protocol></connection>\
            <connection id='duplicate-two'><name>Duplicate shell</name><protocol>LOCAL_SHELL</protocol></connection>\
         </connections>"
    );
    let marker = |id: &str| {
        format!(
            "<!-- kortty:archived scope=connection:{} -->\r\n",
            URL_SAFE_NO_PAD.encode(id)
        )
    };
    let archived = format!("{}{}", marker(LOCAL_ID), marker(SSH_ID));
    let source = Scratch::new();
    let path = source.0.join("java-local-memory.zip");
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    let options = SimpleFileOptions::default().with_aes_encryption_bytes(AesMode::Aes256, b"pw");
    for (name, bytes) in [
        ("connections.xml".into(), connections.as_bytes()),
        (
            format!("memory/connections/{LOCAL_NAME}.md"),
            b"Local notes.\r\n",
        ),
        (
            format!("memory/history/connection-{LOCAL_NAME}-{STAMP}"),
            b"Prior shell notes.\n",
        ),
        (format!("memory/connections/{SSH_NAME}.md"), b"SSH notes.\n"),
        ("memory/connections/duplicate-one.md".into(), b"Unmapped.\n"),
        (
            "memory/connections/duplicate-two.md".into(),
            b"Also unmapped.\n",
        ),
        (ARCHIVE.into(), archived.as_bytes()),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    let inspect = || super::super::inspect_java_password(&path, &Secret::new("pw".into())).unwrap();
    let local = Scratch::new();
    let queued =
        super::super::queue_restore_at(&local.0, inspect(), RestoreMode::JavaAdditive).unwrap();
    assert!(
        queued
            .scheduler_notes
            .iter()
            .any(|note| note.ends_with("imported (2)"))
    );
    assert!(
        queued
            .scheduler_notes
            .iter()
            .any(|note| note.ends_with("missing (4)"))
    );
    ProfileLease::acquire(&local.0, || false).unwrap();
    let catalogue = crate::connection_store::Store::open(local.0.join(CONNECTIONS)).unwrap();
    assert_eq!(
        catalogue.get(1).unwrap().login.endpoint.host,
        "bastion.example"
    );
    assert_eq!(
        catalogue.local(2).unwrap().working_directory,
        Some(PathBuf::from("/tmp/java-shell"))
    );
    assert_eq!(catalogue.local(3).unwrap().name, "Duplicate shell");
    assert_eq!(
        fs::read(local.0.join(DESTINATION)).unwrap(),
        b"Local notes.\r\n"
    );
    assert_eq!(
        fs::read(local.0.join("memory/connections/1.md")).unwrap(),
        b"SSH notes.\n"
    );
    assert_eq!(
        fs::read(local.0.join(format!("memory/history/connection-2-{STAMP}"))).unwrap(),
        b"Prior shell notes.\n"
    );
    assert_eq!(
        fs::read_to_string(local.0.join(ARCHIVE)).unwrap(),
        format!("{}{}", marker("2"), marker("1"))
    );
    assert!(!local.0.join("memory/connections/3.md").exists());

    fs::write(local.0.join(DESTINATION), b"Edited locally.\n").unwrap();
    super::super::queue_restore_at(&local.0, inspect(), RestoreMode::JavaAdditive).unwrap();
    ProfileLease::acquire(&local.0, || false).unwrap();
    assert_eq!(
        fs::read(local.0.join(DESTINATION)).unwrap(),
        b"Edited locally.\n"
    );
}

#[test]
fn java_memory_retains_exact_archive_markers_when_legacy_filenames_collide() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    let prefix = "a".repeat(71);
    let first = format!("{prefix}@fixture45694");
    let second = format!("{prefix}@fixture51370");
    let stem = format!("{prefix}-81d74748");
    let connections = format!("<connections>
        <connection id='{first}'><name>Archive source</name><protocol>LOCAL_SHELL</protocol></connection>
        <connection id='{second}'><name>Other source</name><protocol>MOSH</protocol></connection>
        </connections>");
    let archive = format!(
        "<!-- kortty:archived scope=connection:{} -->\nVerified archive note.\n",
        URL_SAFE_NO_PAD.encode(&first)
    );
    let source = Scratch::new();
    let path = source.0.join("java-memory-collision.zip");
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    let options = SimpleFileOptions::default().with_aes_encryption_bytes(AesMode::Aes256, b"pw");
    for (name, bytes) in [
        ("connections.xml".into(), connections.as_bytes()),
        (
            format!("memory/connections/{stem}.md"),
            b"Ambiguous note.\n".as_slice(),
        ),
        (
            format!("memory/history/connection-{stem}-20261102-131415-019.md"),
            b"Ambiguous history.\n".as_slice(),
        ),
        ("memory/archive.md".into(), archive.as_bytes()),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    let local = Scratch::new();
    let preview = super::super::inspect_java_password(&path, &Secret::new("pw".into())).unwrap();
    let (_, _, omissions) = preview.java_review().unwrap();
    assert!(omissions.iter().any(|line| line == "AI Memory documents or history files omitted because Java connection filenames collide (2)"));
    super::super::queue_restore_at(&local.0, preview, RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    assert_eq!(
        fs::read(local.0.join("memory/archive.md")).unwrap(),
        b"<!-- kortty:archived scope=connection:MQ -->\nVerified archive note.\n"
    );
    assert!(!local.0.join("memory/connections/1.md").exists());
    assert!(
        !local
            .0
            .join("memory/history/connection-1-20261102-131415-019.md")
            .exists()
    );
}

#[test]
fn java_memory_binds_new_serial_profiles_and_retains_existing_documents() {
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    let source = Scratch::new();
    let path = source.0.join("java-serial-memory.zip");
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    let options = SimpleFileOptions::default().with_aes_encryption_bytes(AesMode::Aes256, b"pw");
    for (name, bytes) in [
        ("connections.xml", b"<connections><connection id='console'><name>Console</name><protocol>SERIAL</protocol><serial><portName>COM7</portName></serial></connection></connections>".as_slice()),
        ("memory/connections/console.md", b"Use the device console.\n".as_slice()),
        ("memory/history/connection-console-20261102-131415-019.md", b"Prior serial notes.\n".as_slice()),
        ("memory/archive.md", b"<!-- kortty:archived scope=connection:Y29uc29sZQ -->\nSerial archive.\n".as_slice()),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    let inspect = || super::super::inspect_java_password(&path, &Secret::new("pw".into())).unwrap();
    let local = Scratch::new();
    let restore = || {
        super::super::queue_restore_at(&local.0, inspect(), RestoreMode::JavaAdditive).unwrap();
        ProfileLease::acquire(&local.0, || false).unwrap();
    };
    restore();
    let mut store = crate::connection_store::Store::open(local.0.join(CONNECTIONS)).unwrap();
    assert_eq!(
        store.serial(1).unwrap().connection.device,
        PathBuf::from("COM7")
    );
    let memory = local.0.join("memory/connections/1.md");
    assert_eq!(fs::read(&memory).unwrap(), b"Use the device console.\n");
    assert_eq!(
        fs::read(
            local
                .0
                .join("memory/history/connection-1-20261102-131415-019.md")
        )
        .unwrap(),
        b"Prior serial notes.\n"
    );
    assert_eq!(
        fs::read(local.0.join("memory/archive.md")).unwrap(),
        b"<!-- kortty:archived scope=connection:MQ -->\nSerial archive.\n"
    );
    fs::write(&memory, b"Edited locally.\n").unwrap();
    restore();
    assert_eq!(fs::read(&memory).unwrap(), b"Edited locally.\n");

    let history = local
        .0
        .join("memory/history/connection-1-20261102-131415-019.md");
    fs::remove_file(&history).unwrap();
    restore();
    assert!(!history.exists());

    let reviewed = || {
        let mut preview = inspect();
        preview.prepare_java_memory_review_at(&local.0).unwrap();
        let review = preview.java_memory_review().unwrap();
        assert_eq!(review.len(), 1);
        let (source, choices, default) = review.row(0);
        assert!(source.contains("Java ID console"));
        assert!(choices.iter().any(|choice| choice.id == Some(1)));
        assert_eq!(default.unwrap().id, None);
        assert!(preview.select_java_memory(0, Some(99)).is_err());
        let review = preview.java_memory_review_mut().unwrap();
        assert_eq!(review.exact(0).unwrap().id, Some(1));
        assert!(review.can_select_exact());
        review.select_exact();
        assert_eq!(review.assignment_count(), 1);
        assert!(!review.can_select_exact());
        preview
    };
    let stale = reviewed();
    store.toggle_connection_favorites(&[1], &[]).unwrap();
    assert!(
        super::super::queue_restore_at(&local.0, stale, RestoreMode::JavaAdditive)
            .unwrap_err()
            .to_string()
            .contains("Saved connections changed")
    );

    let catalogue_before = fs::read(local.0.join(CONNECTIONS)).unwrap();
    super::super::queue_restore_at(&local.0, reviewed(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    assert_eq!(fs::read(&history).unwrap(), b"Prior serial notes.\n");
    assert_eq!(fs::read(&memory).unwrap(), b"Edited locally.\n");
    assert_eq!(
        fs::read(local.0.join(CONNECTIONS)).unwrap(),
        catalogue_before
    );

    fs::remove_file(&history).unwrap();
    super::super::queue_restore_at(&local.0, reviewed(), RestoreMode::JavaAdditive).unwrap();
    let serial = store.serial(1).unwrap().clone();
    store
        .save_serial(crate::connection_store::SerialProfile {
            name: "Changed after queue".into(),
            ..serial
        })
        .unwrap();
    let error = ProfileLease::acquire(&local.0, || false).err().unwrap();
    assert!(error.to_string().contains("Saved connections changed"));
    assert!(!history.exists());
    let mut changed = inspect();
    changed.prepare_java_memory_review_at(&local.0).unwrap();
    let review = changed.java_memory_review_mut().unwrap();
    assert!(review.exact(0).is_none());
    assert!(!review.can_select_exact());
    review.select(0, Some(1)).unwrap();
    assert_eq!(review.assignment_count(), 1);
}

#[test]
fn java_skill_statistics_restore_resolves_current_builtin_and_preserves_existing_ledger() {
    use crate::ai_manager::Configuration;
    use crate::ai_manager::backup::{JAVA_STATISTICS, SETTINGS, STATISTICS};
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    const JAVA_SETTINGS: &[u8] = br#"<globalSettings><aiSkills>
        <skill><id>java-bash</id><builtinId>builtin.shell.bash</builtinId></skill>
        <skill><id>java-python</id><builtinId>builtin.lang.python</builtinId></skill>
        <skill><id>java-python-duplicate</id><builtinId>builtin.lang.python</builtinId></skill>
        <skill><id>java-go</id><builtinId>builtin.lang.go</builtinId></skill>
        <skill><id>custom</id><name>Bash</name></skill>
        <skill><id>177</id><name>Bash</name></skill>
    </aiSkills></globalSettings>"#;
    const JAVA_STATS: &[u8] = br#"{
        "version":1,"tokenizer":"CL100K_BASE","skills":{
        "java-bash":{"name":"Original Bash","lastUsed":"2026-11-03","lastFingerprint":"01234567deadbeef|CL100K_BASE",
          "daily":{"2026-11-03":{"automatic":3,"pinned":1,"tokens":100,"surfaces":{"CHAT":2,"AGENT":1,"WORKFLOW":1}}},
          "monthly":{"2025-01":{"automatic":2,"pinned":0,"tokens":40,"surfaces":{"CHAT":2}}},
          "yearly":{"2019":{"automatic":3,"pinned":0,"tokens":90,"surfaces":{"AGENT":3}}},
          "snapshots":[{"date":"2026-11-02","tokens":237,"tokenizer":"CL100K_BASE","fingerprint":"01234567deadbeef"}]},
        "java-python":{"name":"Python"},
        "java-go":{"name":"Go","daily":{"2026-11-03":{"automatic":-1,"pinned":1,"tokens":1}}},
        "custom":{"name":"Bash","yearly":{"2021":{"automatic":1,"pinned":0,"tokens":15}}},
        "177":{"name":"Bash","yearly":{"2021":{"automatic":2,"pinned":1,"tokens":30}}},
        "deleted-skill":{"name":"Deleted skill","yearly":{"2009":{"automatic":4,"pinned":0,"tokens":75}}},
        "\u202eunsafe-id":{"name":"Invalid label"}
      }}"#;

    let source = Scratch::new();
    let path = source.0.join("java-statistics.zip");
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    let options = SimpleFileOptions::default().with_aes_encryption_bytes(AesMode::Aes256, b"pw");
    for (name, bytes) in [
        ("global-settings.xml", JAVA_SETTINGS),
        (STATISTICS, JAVA_STATS),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    let inspect = || super::super::inspect_java_password(&path, &Secret::new("pw".into())).unwrap();
    let preview = inspect();
    let (_, entries, omissions) = preview.java_review().unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry
                == "Skill usage and historical sizes for matching built-in skills (1)")
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry == "Read-only Java skill histories (4)")
    );
    assert!(
        omissions
            .iter()
            .any(|entry| entry == "Invalid Java skill histories omitted (2)")
    );
    assert!(data::validate(&preview.snapshot).is_err());

    let restricted = Scratch::new();
    let restricted_directory = Directory::open(&restricted.0, false).unwrap();
    let checks = std::cell::Cell::new(0);
    ai_backup::publish_java_statistics(
        &restricted_directory,
        &preview.snapshot[JAVA_STATISTICS],
        || {
            let previous = checks.get();
            checks.set(previous + 1);
            previous < 3
        },
    )
    .unwrap();
    assert!(checks.get() >= 4);
    assert!(!restricted.0.join(STATISTICS).exists());

    let target = Scratch::new();
    let target_directory = Directory::open(&target.0, false).unwrap();
    super::super::queue_restore_at(&target.0, preview, RestoreMode::JavaAdditive).unwrap();
    let mut config = serde_json::to_value(Configuration::default()).unwrap();
    let skills = config["skills"]["skills"].as_object_mut().unwrap();
    let original_id = skills
        .iter()
        .find_map(|(id, skill)| (skill["builtin"] == "builtin.shell.bash").then(|| id.clone()))
        .unwrap();
    let bash = skills.remove(&original_id).unwrap();
    skills.insert("177".into(), bash);
    config["skills"]["last_id"] = 177.into();
    let config = serde_json::to_vec(&config).unwrap();
    target_directory.publish_copy(SETTINGS, &config).unwrap();
    ProfileLease::acquire(&target.0, || false).unwrap();
    assert!(!target.0.join(JAVA_STATISTICS).exists());
    let installed: serde_json::Value =
        serde_json::from_slice(&fs::read(target.0.join(STATISTICS)).unwrap()).unwrap();
    let skills = installed["skills"].as_object().unwrap();
    assert_eq!(skills.len(), 1);
    let record = &skills["177"];
    assert_eq!(
        record["daily"]["2026-11-03"]["surfaces"],
        serde_json::json!({"chat": 2, "agent": 1, "workflow": 1})
    );
    assert_eq!(record["monthly"]["2025-01"]["tokens"], 40);
    assert_eq!(record["yearly"]["2019"]["tokens"], 90);
    assert!(record["last_size"].is_null());
    assert_eq!(record["snapshots"], serde_json::json!([]));
    assert_eq!(record["java_sizes"][0]["fingerprint"], "01234567deadbeef");
    assert_eq!(record["java_sizes"][0]["tokens"], 237);
    assert_eq!(installed["java_bindings"]["java-bash"]["bound"], 177);
    assert_eq!(fs::read(target.0.join(SETTINGS)).unwrap(), config);
    let history = installed["java_history"].as_object().unwrap();
    assert_eq!(history.len(), 4);
    assert_eq!(history["177"]["yearly"]["2021"]["tokens"], 30);
    assert_eq!(history["custom"]["name"], history["177"]["name"]);
    assert_eq!(history["deleted-skill"]["yearly"]["2009"]["tokens"], 75);
    assert_eq!(history["java-python"]["name"], "Python");

    let native_password = Secret::new("native-backup-password".into());
    let native_path = source.0.join("native-statistics.zip");
    let native = data::collect(&target.0).unwrap();
    archive::write(&native_path, &native, &native_password).unwrap();
    let native = super::super::inspect_backup(&native_path, &native_password).unwrap();
    let round_trip = Scratch::new();
    super::super::queue_restore_at(&round_trip.0, native, RestoreMode::KeepExisting).unwrap();
    ProfileLease::acquire(&round_trip.0, || false).unwrap();
    let restored: serde_json::Value =
        serde_json::from_slice(&fs::read(round_trip.0.join(STATISTICS)).unwrap()).unwrap();
    assert_eq!(restored, installed);

    let invalid_catalogue_path = source.0.join("java-statistics-invalid-catalogue.zip");
    let mut zip = ZipWriter::new(File::create(&invalid_catalogue_path).unwrap());
    zip.start_file("global-settings.xml", options).unwrap();
    zip.write_all(b"<globalSettings").unwrap();
    zip.start_file(STATISTICS, options).unwrap();
    zip.write_all(JAVA_STATS).unwrap();
    zip.finish().unwrap();
    let invalid_catalogue =
        super::super::inspect_java_password(&invalid_catalogue_path, &Secret::new("pw".into()))
            .unwrap();
    let (_, entries, omissions) = invalid_catalogue.java_review().unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry == "Read-only Java skill histories (5)")
    );
    assert!(omissions.iter().any(|entry| entry == "Invalid Java skill catalogue omitted. Skill histories retain their original Java IDs."));
    super::super::queue_restore_at(&round_trip.0, invalid_catalogue, RestoreMode::JavaAdditive)
        .unwrap();
    ProfileLease::acquire(&round_trip.0, || false).unwrap();
    let replayed: serde_json::Value =
        serde_json::from_slice(&fs::read(round_trip.0.join(STATISTICS)).unwrap()).unwrap();
    assert_eq!(replayed, installed);

    let mut edited = installed.clone();
    edited["skills"]["177"]["yearly"]["2019"]["tokens"] = 120.into();
    edited["java_history"]["177"]["name"] = "Locally retained historical label".into();
    let edited = serde_json::to_vec(&edited).unwrap();
    let stats = target_directory.private_child("stats", false).unwrap();
    stats.replace_file("skill-usage.json", &edited).unwrap();
    super::super::queue_restore_at(&target.0, inspect(), RestoreMode::JavaAdditive).unwrap();
    ProfileLease::acquire(&target.0, || false).unwrap();
    assert_eq!(fs::read(target.0.join(STATISTICS)).unwrap(), edited);

    let local = br#"{"version":1,"skills":{},"tokenizer":null}"#;
    stats.replace_file("skill-usage.json", local).unwrap();
    super::super::queue_restore_at(&target.0, inspect(), RestoreMode::JavaAdditive).unwrap();
    ProfileLease::acquire(&target.0, || false).unwrap();
    let merged: serde_json::Value =
        serde_json::from_slice(&fs::read(target.0.join(STATISTICS)).unwrap()).unwrap();
    assert_eq!(merged["skills"], serde_json::json!({}));
    assert!(merged["tokenizer"].is_null());
    assert_eq!(merged["java_history"], installed["java_history"]);
    assert_eq!(merged["java_bindings"]["java-bash"], "reserved");

    let damaged = b"{malformed local statistics";
    stats.replace_file("skill-usage.json", damaged).unwrap();
    super::super::queue_restore_at(&target.0, inspect(), RestoreMode::JavaAdditive).unwrap();
    ProfileLease::acquire(&target.0, || false).unwrap();
    assert_eq!(fs::read(target.0.join(STATISTICS)).unwrap(), damaged);
}

#[test]
fn java_snippet_workspace_restores_exact_tabs_and_preserves_a_native_session_on_replay() {
    use crate::snippet_window::{SnippetWindow, session as tabs};
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    const DIRECT: &str = "4c4a5560-46ba-4570-b589-107496e994b9";
    const REMAPPED: &str = "c1017ef1-b5bb-4392-a22a-90826ce01a0e";
    const CONFLICT: &str = "c0c2a9bc-8f44-4eeb-876d-98206b9ccffe";
    const MISSING: &str = "a8a311fd-3f7a-474c-97fe-59ead4247f7c";
    let snippet = |id, name, content| {
        format!(
            "<snippet><id>{id}</id><name>{name}</name><language>bash</language><content>{content}</content></snippet>"
        )
    };
    let xml = format!(
        "<snippets>{}{}{}</snippets>",
        snippet(DIRECT, "Direct", "echo original"),
        snippet(REMAPPED, "Exact existing", "echo same"),
        snippet(CONFLICT, "Edited locally", "echo old"),
    );
    let settings = format!(
        "<globalSettings><snippetWorkspaceOpenTabs>
        <snippetId>{MISSING}</snippetId><snippetId>{REMAPPED}</snippetId>
        <snippetId>{CONFLICT}</snippetId><snippetId>{DIRECT}</snippetId>
        </snippetWorkspaceOpenTabs><snippetWorkspaceActiveTab>{REMAPPED}</snippetWorkspaceActiveTab></globalSettings>"
    );
    let source = Scratch::new();
    let path = source.0.join("java-tabs.zip");
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    let options = SimpleFileOptions::default().with_aes_encryption_bytes(AesMode::Aes256, b"pw");
    for (name, content) in [("snippets.xml", &xml), ("global-settings.xml", &settings)] {
        zip.start_file(name, options).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    let inspect = || super::super::inspect_java_password(&path, &Secret::new("pw".into())).unwrap();
    let target = Scratch::new();
    let library_path = target.0.join(snippets::FILE_NAME);
    let tab_path = target.0.join(tabs::FILE_NAME);
    let mut library = snippets::Library::open(library_path.clone()).unwrap();
    let mut save = |name: &str, content: &str| {
        library
            .save(
                None,
                snippets::Input {
                    name: name.into(),
                    language: "bash".into(),
                    category: String::new(),
                    content: content.into(),
                },
            )
            .unwrap()
    };
    let remapped = save("Exact existing", "echo same");
    let conflict = save("Edited locally", "echo local");
    let direct = snippets::SnippetId::from_java_uuid(DIRECT).unwrap();
    let restore = || {
        super::super::queue_restore_at(&target.0, inspect(), RestoreMode::JavaAdditive).unwrap();
        ProfileLease::acquire(&target.0, || false).unwrap();
    };
    restore();

    let expected = tabs::State {
        ids: vec![remapped, direct],
        active: Some(remapped),
    };
    assert_eq!(tabs::load(&tab_path).unwrap(), expected);
    let mut reopened = SnippetWindow::open(library_path).unwrap();
    reopened.restore_tabs(&[]);
    assert_eq!(reopened.tab_session(), expected);
    assert_eq!(
        snippets::Library::open(target.0.join(snippets::FILE_NAME))
            .unwrap()
            .get(conflict)
            .unwrap()
            .content(),
        "echo local"
    );

    let native = tabs::State {
        ids: vec![conflict],
        active: Some(conflict),
    };
    native.write(&tab_path).unwrap();
    let before = fs::read(&tab_path).unwrap();
    restore();
    assert_eq!(fs::read(&tab_path).unwrap(), before);
}

#[test]
fn java_workflow_header_defaults_follow_exact_restored_snippets_and_policy() {
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    let saved = [
        (
            "BASH",
            "4c4a5560-46ba-4570-b589-107496e994b9",
            "Bash Header",
            "bash",
            "# Same",
        ),
        (
            "PYTHON",
            "c1017ef1-b5bb-4392-a22a-90826ce01a0e",
            "Python Header",
            "python",
            "# Python",
        ),
        (
            "RUBY",
            "c0c2a9bc-8f44-4eeb-876d-98206b9ccffe",
            "Ruby Header",
            "ruby",
            "# Archived",
        ),
        (
            "POWERSHELL",
            "a8a311fd-3f7a-474c-97fe-59ead4247f7c",
            "Other Header",
            "bash",
            "# Bash",
        ),
        (
            "PERL",
            "a8a311fd-3f7a-474c-97fe-59ead4247f7e",
            "Perl Header",
            "perl",
            "# Perl",
        ),
    ];
    let mut catalog = String::from("<snippets>");
    let mut settings = String::from("<globalSettings><workflowHeaderDefaults>");
    for (language, id, name, metadata, content) in saved {
        catalog.push_str(&format!(
            "<snippet><id>{id}</id><name>{name}</name><category>Script-Header</category><language>{metadata}</language><content>{content}</content></snippet>"
        ));
        settings.push_str(&format!(
            "<headerDefault><language>{language}</language><headerSnippetId>{id}</headerSnippetId></headerDefault>"
        ));
    }
    catalog.push_str("</snippets>");
    settings.push_str("</workflowHeaderDefaults></globalSettings>");
    let source = Scratch::new();
    let archive = source.0.join("java-workflow-headers.zip");
    let mut zip = ZipWriter::new(File::create(&archive).unwrap());
    let options = SimpleFileOptions::default().with_aes_encryption_bytes(AesMode::Aes256, b"pw");
    for (path, contents) in [("snippets.xml", catalog), ("global-settings.xml", settings)] {
        zip.start_file(path, options).unwrap();
        zip.write_all(contents.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    let inspect =
        || super::super::inspect_java_password(&archive, &Secret::new("pw".into())).unwrap();
    let target = Scratch::new();
    let mut library = snippets::Library::open(target.0.join(snippets::FILE_NAME)).unwrap();
    let mut save = |name: &str, language: &str, content: &str| {
        library
            .save(
                None,
                snippets::Input {
                    name: name.into(),
                    language: language.into(),
                    category: "Script-Header".into(),
                    content: content.into(),
                },
            )
            .unwrap()
    };
    let remapped = save("Bash Header", "bash", "# Same");
    let _ = save("Ruby Header", "ruby", "# Edited");
    let native = save("Locally chosen", "perl", "# Kept");
    let path = target.0.join(data::WINDOW_SETTINGS);
    fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "workflow_header_defaults": {"perl": native}
        }))
        .unwrap(),
    )
    .unwrap();

    let restore = || {
        super::super::queue_restore_at(&target.0, inspect(), RestoreMode::JavaAdditive).unwrap();
        ProfileLease::acquire_with_header_policy(&target.0, || false, || true).unwrap();
    };
    restore();
    let raw = fs::read(&path).unwrap();
    let result: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    let direct = snippets::SnippetId::from_java_uuid(saved[1].1).unwrap();
    assert_eq!(
        result["workflow_header_defaults"],
        serde_json::json!({
            "bash": remapped, "python": direct, "perl": native
        })
    );
    assert!(result.get("java_display_preferences").is_none());
    restore();
    assert_eq!(fs::read(&path).unwrap(), raw);

    let denied = Scratch::new();
    let directory = Directory::open(&denied.0, false).unwrap();
    let backup = inspect();
    assert!(
        prepare_java_updates(&directory, &backup.snapshot, &|| false)
            .unwrap()
            .catalogues
            .is_empty()
    );
}

#[test]
fn java_snippet_analysis_restore_binds_the_catalogue_and_round_trips_in_native_backups() {
    use crate::snippet_analysis::model::{Subject, content_hash};
    use crate::snippet_analysis::store::Store as AnalysisStore;
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    const ID: &str = "4c4a5560-46ba-4570-b589-107496e994b9";
    const ORPHAN: &str = "c1017ef1-b5bb-4392-a22a-90826ce01a0e";
    const RECORD: &str = "c0c2a9bc-8f44-4eeb-876d-98206b9ccffe";
    const RUN: &str = "a8a311fd-3f7a-474c-97fe-59ead4247f7c";
    const CONTENT: &str = "echo preserved\n";
    let snippets_xml = format!(
        "<snippets><snippet><id>{ID}</id><name>fixture</name><language>bash</language><content>{CONTENT}</content></snippet></snippets>"
    );
    let hash = content_hash(CONTENT).unwrap();
    let member = |subject: &str| {
        serde_json::json!({
            "schemaVersion":1,"snippetId":subject,"revision":1,"updatedAt":200,
            "records":[{"id":RECORD,"snippetId":subject,"purpose":"ANALYSIS","analyzedAt":100,"updatedAt":200,"pinned":true,
              "source":{"sha256":hash,"language":"bash","reportLanguageCode":"en","codeTextLanguageCode":"en","content":hash,"contentTruncated":false,"lineCount":1,"snippetName":"fixture"},
              "provenance":{"profileId":"a-java-id","profileName":"Java profile","model":"fixture"},
              "summary":"Analysis text",
              "applyRuns":[{"id":RUN,"outcome":"FAILED","failureKey":"workflow.timeout",
                "request":{"baseSha256":hash,"baseContent":hash,"hardeningOptions":["STRICT_MODE"],"headerText":"private-fixture"},
                "checkpoint":{"completedStages":1,"totalStages":2,"content":hash},
                "stats":{"completedItems":1,"totalItems":2,"retries":1},
                "provenance":{"profileId":"apply-profile","profileName":"Java apply profile"}}]}],
            "blobs":{(hash.clone()): CONTENT}
        })
    };
    let source = Scratch::new();
    let path = source.0.join("java-analysis.zip");
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    let options = SimpleFileOptions::default().with_aes_encryption_bytes(AesMode::Aes256, b"pw");
    zip.start_file("snippets.xml", options).unwrap();
    zip.write_all(snippets_xml.as_bytes()).unwrap();
    for subject in [ID, ORPHAN] {
        zip.start_file(format!("snippet-analyses/{subject}.json"), options)
            .unwrap();
        zip.write_all(&serde_json::to_vec(&member(subject)).unwrap())
            .unwrap();
    }
    zip.finish().unwrap();
    let inspect = || super::super::inspect_java_password(&path, &Secret::new("pw".into())).unwrap();
    let preview = inspect();
    let (_, entries, omissions) = preview.java_review().unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry == "Snippet analysis histories (1)")
    );
    assert!(
        omissions
            .iter()
            .any(|note| note.contains(ORPHAN) && note.contains("no matching subject"))
    );
    assert!(
        omissions
            .iter()
            .any(|note| note == "Unsupported or active Java analysis details omitted (1)")
    );

    let id = snippets::SnippetId::try_from(ID.replace('-', "")).unwrap();
    let subject = Subject::Snippet(id);
    let name = format!("{}.json", String::from(id));
    let history_path = format!("snippet-analyses/{name}");
    let target = Scratch::new();
    super::super::queue_restore_at(&target.0, preview, RestoreMode::JavaAdditive).unwrap();
    ProfileLease::acquire(&target.0, || false).unwrap();
    let store = AnalysisStore::new(target.0.clone());
    let history = store.load(subject).unwrap();
    let current = history.current().unwrap();
    assert!(current.pinned);
    assert_eq!(current.analysis.summary, "Analysis text");
    assert_eq!(current.source.content.as_deref(), Some(CONTENT));
    assert!(current.apply_runs.is_empty() && current.apply_recovery.is_none());
    let java_history = current.java_history.as_ref().unwrap();
    let details = java_history.apply_runs[0].details.as_ref().unwrap();
    assert_eq!(details.failure_key.as_deref(), Some("workflow.timeout"));
    assert_eq!(details.progress.as_ref().unwrap().retries, 1);
    assert_eq!(
        details.provenance.as_ref().unwrap().profile_id,
        "apply-profile"
    );
    let native = data::collect(&target.0).unwrap();
    assert!(
        !std::str::from_utf8(&native[&history_path])
            .unwrap()
            .contains("private-fixture")
    );

    let denied = Scratch::new();
    let directory = Directory::open(&denied.0, false).unwrap();
    directory
        .publish_copy(snippets::FILE_NAME, &native[snippets::FILE_NAME])
        .unwrap();
    let calls = std::cell::Cell::new(0);
    data::analysis::install(&directory, &native, || {
        let call = calls.get();
        calls.set(call + 1);
        (call < 2).then_some(5 * 1024 * 1024)
    })
    .unwrap();
    assert!(calls.get() >= 3);
    assert!(!denied.0.join(&history_path).exists());
    calls.set(0);
    let error = data::analysis::install(&directory, &native, || {
        let call = calls.get();
        calls.set(call + 1);
        Some(if call < 2 { 5 * 1024 * 1024 } else { 0 })
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert!(!denied.0.join(&history_path).exists());
    data::analysis::install(&directory, &native, || Some(0)).unwrap();
    let history = AnalysisStore::new(denied.0.clone()).load(subject).unwrap();
    let capped = history.current().unwrap();
    assert!(capped.source.content_truncated);
    assert!(capped.source.content.is_none());
    assert_eq!(capped.analysis.summary, "Analysis text");
    let capped_java = capped.java_history.as_ref().unwrap();
    assert!(capped_java.attachments.is_empty());
    assert_eq!(capped_java.apply_runs[0].details.as_ref(), Some(details));

    let differing = Scratch::new();
    let directory = Directory::open(&differing.0, false).unwrap();
    let local_xml = snippets_xml.replace("echo preserved", "echo edited locally");
    let (local_snippets, ..) =
        snippets::portable_java_backup(Some(local_xml.as_bytes()), None).unwrap();
    directory
        .publish_copy(snippets::FILE_NAME, &local_snippets)
        .unwrap();
    super::super::queue_restore_at(&differing.0, inspect(), RestoreMode::JavaAdditive).unwrap();
    ProfileLease::acquire(&differing.0, || false).unwrap();
    assert!(!differing.0.join(&history_path).exists());
    queue_with_mode(
        &differing.0,
        native.clone(),
        RestoreMode::KeepExisting,
        false,
    )
    .unwrap();
    ProfileLease::acquire(&differing.0, || false).unwrap();
    assert!(!differing.0.join(&history_path).exists());

    let replay = Scratch::new();
    queue_with_mode(&replay.0, native.clone(), RestoreMode::KeepExisting, false).unwrap();
    ProfileLease::acquire(&replay.0, || false).unwrap();
    let original = fs::read(replay.0.join(&history_path)).unwrap();
    let mut local: serde_json::Value = serde_json::from_slice(&original).unwrap();
    local["history"]["records"][0]["analysis"]["summary"] = "Locally edited".into();
    let local = serde_json::to_vec(&local).unwrap();
    let directory = Directory::open(&replay.0, false).unwrap();
    directory
        .private_child("snippet-analyses", false)
        .unwrap()
        .replace_file(&name, &local)
        .unwrap();
    super::super::queue_restore_at(&replay.0, inspect(), RestoreMode::JavaAdditive).unwrap();
    ProfileLease::acquire(&replay.0, || false).unwrap();
    queue_with_mode(&replay.0, native, RestoreMode::Replace, false).unwrap();
    ProfileLease::acquire(&replay.0, || false).unwrap();
    assert_eq!(fs::read(replay.0.join(history_path)).unwrap(), local);
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
fn java_display_preferences_add_only_unset_fields_and_keep_theme_replay_safe() {
    use serde_json::json;
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    const SETTINGS: &[u8] = br#"<globalSettings>
        <openToolWindowsAsTabs>true</openToolWindowsAsTabs>
        <connectionColorBorderEnabled>false</connectionColorBorderEnabled>
        <splitPaneFocusRingEnabled>false</splitPaneFocusRingEnabled>
        <tabSwitchMostRecentFirst>true</tabSwitchMostRecentFirst>
        <tabTitleFromShellEnabled>false</tabTitleFromShellEnabled>
        <shellIntegrationEnabled>false</shellIntegrationEnabled>
        <terminalDragDropEnabled>false</terminalDragDropEnabled>
        <commandTimestampsEnabled>true</commandTimestampsEnabled>
        <terminalLinkDetectionEnabled>false</terminalLinkDetectionEnabled>
        <jobSchedulerMenuStatusEnabled>false</jobSchedulerMenuStatusEnabled>
        <terminalBackgroundTransparency>24</terminalBackgroundTransparency>
        <terminalSilenceSeconds>110</terminalSilenceSeconds>
        <terminalBellNotificationsEnabled>true</terminalBellNotificationsEnabled>
        <commandFinishedNotificationsEnabled>false</commandFinishedNotificationsEnabled>
        <commandFinishedNotificationSeconds>45</commandFinishedNotificationSeconds>
        <remoteTerminalNotificationsEnabled>false</remoteTerminalNotificationsEnabled>
        <aiRunToastsEnabled>false</aiRunToastsEnabled>
        <sftpParallelTransfers>4</sftpParallelTransfers>
        <sftpResumePartialTransfers>false</sftpResumePartialTransfers>
        <sftpKeepPartialOnCancel>true</sftpKeepPartialOnCancel>
        <sftpConflictDefault>overwrite</sftpConflictDefault>
        <sftpAutoCloseMinutes>15</sftpAutoCloseMinutes>
        <sftpDefaultZipPath>/srv/archives&amp;reports</sftpDefaultZipPath>
        <sftpDefaultZipCompression>9</sftpDefaultZipCompression>
        <sftpDiffAiFontSize>19</sftpDiffAiFontSize>
        <sessionJournalPageSchemeId>ocean</sessionJournalPageSchemeId>
        <sessionJournalPageTheme>dark</sessionJournalPageTheme>
        <sessionJournalFontScalePercent>135</sessionJournalFontScalePercent>
        <sessionJournalPageUiFont>Fixture Sans</sessionJournalPageUiFont>
        <sessionJournalPageMonoFont>Fixture Mono</sessionJournalPageMonoFont>
        <sessionJournalClickableLinks>false</sessionJournalClickableLinks>
        <sessionJournalLogFormat>YAML</sessionJournalLogFormat>
        <sessionJournalStoragePath>/fixture/private-journals</sessionJournalStoragePath>
        <pdfWatermarkEnabled>true</pdfWatermarkEnabled>
        <pdfWatermarkText>Archiv &amp; Pr&#252;fung</pdfWatermarkText>
        <pdfWatermarkColor>#123ABC</pdfWatermarkColor>
        <exportFooterEnabled>false</exportFooterEnabled>
        <exportFooterText>Test footer</exportFooterText>
        <terminalCopyOnSelectEnabled>false</terminalCopyOnSelectEnabled>
        <pasteWarningMode>off</pasteWarningMode>
        <pasteLargeWarningKiB>8</pasteLargeWarningKiB>
        <pasteLineDelayMs>35</pasteLineDelayMs>
        <terminalQuickSelectAlphabet>asdf</terminalQuickSelectAlphabet>
        <terminalQuickSelectPatterns>
            <pattern>[A-Z]{2}-\d+</pattern>
            <pattern>\Qbuild+tag\E</pattern>
        </terminalQuickSelectPatterns>
        <rememberWindowGeometry>false</rememberWindowGeometry>
        <rememberDashboardState>false</rememberDashboardState>
        <sessionRestoreMode>auto</sessionRestoreMode>
        <sessionRestoreScrollback>true</sessionRestoreScrollback>
        <sessionRestoreScrollbackLines>1600</sessionRestoreScrollbackLines>
        <useFixedWindowGeometry>true</useFixedWindowGeometry>
        <fixedWindowGeometry>
            <x>-128.5</x><y>-80</y><width>1400</width><height>900</height>
            <maximized>true</maximized><fullScreen>true</fullScreen>
        </fixedWindowGeometry>
        <codeAnalysisDiagramAutoGenerate>false</codeAnalysisDiagramAutoGenerate>
        <codeAnalysisProposeModularization>true</codeAnalysisProposeModularization>
        <codeAnalysisFontSize>22</codeAnalysisFontSize>
        <snippetAnalysisHistoryMaxSize>9</snippetAnalysisHistoryMaxSize>
        <snippetHistoryMaxSize>42</snippetHistoryMaxSize>
        <snippetAnalysisMaxStoredContentBytes>262145</snippetAnalysisMaxStoredContentBytes>
        <codeAnalysisHardeningExpanded>true</codeAnalysisHardeningExpanded>
        <snippetHardeningOptions>STRICT_MODE,SAFE_MODE</snippetHardeningOptions>
        <snippetInputHardeningEnabled>true</snippetInputHardeningEnabled>
        <snippetInputHardeningOptions>PARAM_VALIDATION,FILE_SIZE_LIMIT</snippetInputHardeningOptions>
        <snippetInputHardeningMaxFileSizeMb>25</snippetInputHardeningMaxFileSizeMb>
        <codeAnalysisDiagramOptionsExpanded>true</codeAnalysisDiagramOptionsExpanded>
        <codeAnalysisProjectDiagramVisible>false</codeAnalysisProjectDiagramVisible>
        <snippetDiagramLayout>horizontal</snippetDiagramLayout>
        <snippetStructurePanelVisible>true</snippetStructurePanelVisible>
        <snippetStructurePanelWidth>851.25</snippetStructurePanelWidth>
        <snippetAnalysisPanelVisible>true</snippetAnalysisPanelVisible>
        <snippetAnalysisPanelWidth>605.5</snippetAnalysisPanelWidth>
        <snippetAiProgressDockVisible>false</snippetAiProgressDockVisible>
        <snippetDiagramColorMode>light</snippetDiagramColorMode>
        <snippetDiagramBackgroundColor>#ABCDEF</snippetDiagramBackgroundColor>
        <snippetAnalysisExportIncludeCode>true</snippetAnalysisExportIncludeCode>
        <editorCursorStyle>UNDERSCORE</editorCursorStyle>
        <logfileViewerHighlightLinks>false</logfileViewerHighlightLinks>
        <logfileViewerHighlightKeywords>false</logfileViewerHighlightKeywords>
        <logfileViewerKeywordSetId>builtin.network</logfileViewerKeywordSetId>
        <editorCursorColor>#445566</editorCursorColor>
        <snippetFontFamily>Fixture Mono</snippetFontFamily><snippetFontSize>19</snippetFontSize>
        <snippetLineNumbers>true</snippetLineNumbers>
        <snippetWordWrap>false</snippetWordWrap>
        <customSnippetCodeLanguages>
            <language> Gleam </language>
            <language>GLEAM</language>
            <language>pwsh</language>
            <language>bad value</language>
            <language>Fennel</language>
        </customSnippetCodeLanguages>
        <snippetForegroundColor>#010203</snippetForegroundColor>
        <selectedSnippetEditorProfileId>custom-fixture</selectedSnippetEditorProfileId>
        <snippetEditorProfiles><profile><id>custom-fixture</id><builtIn>false</builtIn>
            <foregroundColor>#AABBCC</foregroundColor><backgroundColor>#102030</backgroundColor>
            <cursorStyle>LINE</cursorStyle><cursorColor>#DDEEFF</cursorColor>
        </profile></snippetEditorProfiles>
        <requireMasterPasswordOnStartup>true</requireMasterPasswordOnStartup>
        <lastWindowGeometry><x>4800</x><y>3010</y><width>732</width><height>600</height></lastWindowGeometry>
        <showTerminalScrollbar>false</showTerminalScrollbar>
        <hideTerminalScrollbarsInFullscreen>true</hideTerminalScrollbarsInFullscreen>
        <workflowScriptFontSize>21</workflowScriptFontSize>
        <uiFontScaleAuto>true</uiFontScaleAuto><uiFontScalePercent>132</uiFontScalePercent>
        <guideFontScalePercent>210</guideFontScalePercent>
        <terminalRemoteSidebarWidth>450.5</terminalRemoteSidebarWidth>
        <terminalRemoteSidebarPosition>RIGHT</terminalRemoteSidebarPosition>
        <terminalFontSize>26</terminalFontSize>
    </globalSettings>"#;
    let source = Scratch::new();
    let path = source.0.join("java-display.zip");
    let mut archive = ZipWriter::new(File::create(&path).unwrap());
    let options = SimpleFileOptions::default()
        .with_aes_encryption_bytes(AesMode::Aes256, b"archive-password");
    for (name, bytes) in [
        ("global-settings.xml", SETTINGS),
        (
            "themes.xml",
            br#"<themes><theme id="java-display"><name>Restored Theme</name></theme></themes>"#
                .as_slice(),
        ),
    ] {
        archive.start_file(name, options).unwrap();
        archive.write_all(bytes).unwrap();
    }
    archive.finish().unwrap();

    let local = Scratch::new();
    let directory = Directory::open(&local.0, false).unwrap();
    let initial = br#"{"open_tool_windows_as_tabs":false,"show_terminal_scrollbar":true,
        "remote_sidebar_position":"left","terminal_font":19,"terminal_font_family":"Fixture Mono",
        "terminal_link_detection_enabled":true,
        "terminal_notifications":{"bell":false,"command_seconds":90},
        "sftp_archive":{"level":3},"sftp_resume_partial_transfers":true,
        "journal_appearance":{"theme":"light"},
        "session_journal":{"directory":"/fixture/native-journals","clickable_links":true},
        "export_branding":{"footer_text":"Local footer"},
        "paste_warning":"always","quick_select":{"alphabet":"xy"},
        "restore_output":{"enabled":false,"lines":900},
        "startup":{"remember_geometry":true,"fixed":{"height":820}},
        "snippet_analysis_auto_diagram":true,
        "editor_caret":{"shape":"block"},"snippet_appearance":{"font_size":26,"background":[1,2,3]},
        "logfile_presentation":{"links":true},"custom_snippet_code_languages":["zig","gleam"],
        "snippet_diagram_appearance":{"mode":"dark"},
        "snippet_structure":{"visible":false},
        "snippet_analysis_panel":{"width":777},
        "snippet_hardening":{"classic":[]},
        "legacy_editor_font":{"name":"Preserved","weight":500}}"#;
    directory
        .publish_copy(data::WINDOW_SETTINGS, initial)
        .unwrap();
    let installed_path = local.0.join(data::WINDOW_SETTINGS);
    let inspect = || {
        super::super::inspect_java_password(&path, &Secret::new("archive-password".into())).unwrap()
    };
    let preview = inspect();
    let (_, entries, _) = preview.java_review().unwrap();
    let count = crate::workspace::backup_data::portable_java_preferences(SETTINGS, None)
        .unwrap()
        .count;
    assert!(
        entries
            .iter()
            .any(|line| line == &format!("Global preferences for unset fields ({count})"))
    );
    assert!(
        entries
            .iter()
            .any(|line| line.contains("Include source code by default"))
    );
    assert!(
        entries
            .iter()
            .any(|line| line.contains("Paste warning mode in this backup is Off"))
    );
    assert!(entries.iter().any(|line| {
        line.contains("SFTP conflict default in this backup is Replace regular files")
    }));
    assert!(entries.iter().any(|line| {
        line == "Inactive Java session restore preferences for review in Window settings (3)"
    }));
    let pending: serde_json::Value =
        serde_json::from_slice(&preview.snapshot[data::WINDOW_SETTINGS]).unwrap();
    assert_eq!(pending["java_display_preferences"]["manual_scale"], 210);
    assert!(pending.get("remote_sidebar_position").is_none());
    super::super::queue_restore_at(&local.0, preview, RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    let mut active: serde_json::Value =
        serde_json::from_slice(&fs::read(&installed_path).unwrap()).unwrap();
    for (key, value) in [
        ("open_tool_windows_as_tabs", json!(false)),
        ("connection_color_border_enabled", json!(false)),
        ("split_pane_focus_ring_enabled", json!(false)),
        ("tab_switch_most_recent_first", json!(true)),
        ("tab_title_from_shell_enabled", json!(false)),
        ("automatic_local_shell_integration", json!(false)),
        ("terminal_drag_drop", json!(false)),
        ("command_timestamps_enabled", json!(true)),
        ("terminal_link_detection_enabled", json!(true)),
        ("restore_output", json!({"enabled":false,"lines":900})),
        (
            "java_session_restore",
            json!({"mode":"auto","output":true,"lines":1600}),
        ),
        ("job_menu_status_enabled", json!(false)),
        ("terminal_transparency", json!(24)),
        ("terminal_silence_seconds", json!(110)),
        ("snippet_analysis_auto_diagram", json!(true)),
        ("snippet_analysis_auto_modularization", json!(true)),
        ("snippet_analysis_report_font_size", json!(22)),
        ("snippet_analysis_history_limit", json!(9)),
        ("snippet_history_limit", json!(42)),
        ("snippet_analysis_content_limit", json!(262145)),
        ("snippet_analysis_hardening_expanded", json!(true)),
        (
            "snippet_hardening",
            json!({
                "classic": [],
                "inputEnabled": true,
                "input": ["parameterValidation", "fileSizeLimit"],
                "maxFileSizeMib": 25
            }),
        ),
        ("snippet_analysis_diagram_options_expanded", json!(true)),
        ("snippet_analysis_project_diagram_visible", json!(false)),
        ("snippet_diagram_horizontal", json!(true)),
        ("snippet_structure", json!({"visible":false,"width":800})),
        (
            "snippet_analysis_panel",
            json!({"visible":true,"width":777}),
        ),
        ("snippet_apply_progress_dock", json!(false)),
        (
            "snippet_diagram_appearance",
            json!({"mode":"dark","background":[171,205,239]}),
        ),
        ("snippet_analysis_export_include_code", json!(true)),
        ("editor_caret", json!({"shape":"block","color":[68,85,102]})),
        (
            "logfile_presentation",
            json!({"links":true,"keywords":false,"keyword_set":"builtin.network"}),
        ),
        (
            "snippet_appearance",
            json!({"font_family":"Fixture Mono","font_size":26,
            "foreground":[170,187,204],"background":[1,2,3],"cursor_style":"line","cursor_color":[221,238,255],
            "line_numbers":true,"word_wrap":false}),
        ),
        (
            "startup",
            json!({"remember_geometry":true,"remember_dashboard":false,
            "use_fixed_geometry":true,"fixed":{"width":1400,"height":820,"x":-129,"y":-80,"maximized":true},
            "java_last_geometry":{"width":732,"height":600,"x":4800,"y":3010,"maximized":false}}),
        ),
        ("terminal_copy_on_select_enabled", json!(false)),
        ("paste_warning", json!("always")),
        ("paste_large_warning_kib", json!(8)),
        ("paste_line_delay_ms", json!(35)),
        (
            "quick_select",
            json!({"alphabet":"xy","java_patterns":true,"patterns":[r"[A-Z]{2}-\d+",r"\Qbuild+tag\E"]}),
        ),
        (
            "journal_appearance",
            json!({"scheme":"ocean","theme":"light","scale":135,
            "ui_font":"Fixture Sans","mono_font":"Fixture Mono"}),
        ),
        (
            "session_journal",
            json!({"directory":"/fixture/native-journals",
            "clickable_links":true,"log_format":"YAML"}),
        ),
        (
            "export_branding",
            json!({"watermark_enabled":true,"watermark_text":"Archiv & Prüfung",
            "watermark_color":[18,58,188],"footer_enabled":false,"footer_text":"Local footer"}),
        ),
        ("sftp_parallel_transfers", json!(4)),
        ("sftp_resume_partial_transfers", json!(true)),
        ("sftp_keep_partial_on_cancel", json!(true)),
        ("sftp_conflict_default", json!("overwrite")),
        ("sftp_idle_minutes", json!(15)),
        ("sftp_comparison_ai_font_size", json!(19)),
        (
            "custom_snippet_code_languages",
            json!(["zig", "gleam", "fennel"]),
        ),
        (
            "sftp_archive",
            json!({"level":3,"remote_directory":"/srv/archives&reports"}),
        ),
        (
            "terminal_notifications",
            json!({
                "bell": false,
                "command_finished": false,
                "command_seconds": 90,
                "programs": false,
                "ai_runs": false
            }),
        ),
        ("show_terminal_scrollbar", json!(true)),
        ("hide_terminal_scrollbars_in_fullscreen", json!(true)),
        ("workflow_script_font", json!(21)),
        ("manual_scale", json!(210)),
        ("remote_sidebar_width", json!(451)),
        ("remote_sidebar_position", json!("left")),
        ("terminal_font", json!(19)),
        ("terminal_font_family", json!("Fixture Mono")),
        (
            "legacy_editor_font",
            json!({"name":"Preserved","weight":500}),
        ),
    ] {
        assert_eq!(active[key], value, "changed {key}");
    }
    assert!(active.get("java_display_preferences").is_none());
    assert_eq!(active["themes"]["edited"][0]["name"], "Restored Theme");
    active["manual_scale"] = json!(120);
    active["terminal_transparency"] = json!(40);
    active["terminal_notifications"]["programs"] = json!(true);
    active["sftp_archive"]["remote_directory"] = json!("/srv/local archives");
    active["sftp_comparison_ai_font_size"] = json!(22);
    active["journal_appearance"]["scale"] = json!(145);
    active["paste_line_delay_ms"] = json!(20);
    active["startup"]["fixed"]["x"] = json!(32);
    active["snippet_analysis_export_include_code"] = json!(false);
    active["snippet_analysis_report_font_size"] = json!(18);
    active["snippet_analysis_hardening_expanded"] = json!(false);
    active["snippet_analysis_diagram_options_expanded"] = json!(false);
    active["snippet_analysis_project_diagram_visible"] = json!(true);
    active["snippet_diagram_horizontal"] = json!(false);
    active["snippet_diagram_appearance"]["background"] = json!([230, 231, 232]);
    active["snippet_structure"]["width"] = json!(367);
    active["snippet_analysis_panel"]["visible"] = json!(false);
    active["snippet_apply_progress_dock"] = json!(true);
    active["automatic_local_shell_integration"] = json!(true);
    active["snippet_hardening"]["maxFileSizeMib"] = json!(0);
    active["snippet_appearance"]["foreground"] = json!([123, 45, 67]);
    active["snippet_appearance"]["line_numbers"] = json!(false);
    active["snippet_appearance"]["word_wrap"] = json!(true);
    active["snippet_analysis_history_limit"] = json!(2);
    active["snippet_history_limit"] = json!(5);
    active["snippet_analysis_content_limit"] = json!(0);
    let updated = serde_json::to_vec(&active).unwrap();
    directory
        .replace_file(data::WINDOW_SETTINGS, &updated)
        .unwrap();
    super::super::queue_restore_at(&local.0, inspect(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    assert_eq!(fs::read(installed_path).unwrap(), updated);
}

#[test]
fn java_preferences_restore_coding_agents_shortcuts_and_recording_on_restart() {
    use crate::recording_window::preferences::{self, Preferences, Scope};
    use serde_json::json;
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    let source = Scratch::new();
    let path = source.0.join("java-preferences.zip");
    let password = Secret::new("fixture archive password".into());
    let mut archive = ZipWriter::new(File::create(&path).unwrap());
    archive
        .start_file(
            "global-settings.xml",
            SimpleFileOptions::default()
                .with_aes_encryption_bytes(AesMode::Aes256, password.as_str().as_bytes()),
        )
        .unwrap();
    archive
        .write_all(
            br#"<globalSettings>
            <codingAgentDetectionEnabled>false</codingAgentDetectionEnabled>
            <codingAgentAppBadgeEnabled>false</codingAgentAppBadgeEnabled>
            <codingAgentNotificationsEnabled>false</codingAgentNotificationsEnabled>
            <codingAgentPanelAutoVisibility>false</codingAgentPanelAutoVisibility>
            <codingAgentPanelPlacement>LEFT</codingAgentPanelPlacement>
            <codingAgentPanelWidth>951.25</codingAgentPanelWidth>
            <terminalRemoteSidebarPosition>RIGHT</terminalRemoteSidebarPosition>
            <keyBindingOverrides>
                <binding>menu.tools.aiManager=Shortcut+Shift+F8</binding>
                <binding>menu.tools.jobScheduler=Ctrl+Shift+F7</binding>
                <binding>menu.view.snippetPalette=none</binding>
                <binding>menu.view.dashboard=Ctrl+R</binding>
                <binding>menu.foreign.unknown=F4</binding>
            </keyBindingOverrides>
            <terminalRecordingEnabled>true</terminalRecordingEnabled>
            <terminalRecordingFormat>KORTTY_REPLAY</terminalRecordingFormat>
            <terminalRecordingDefaultScope>WHOLE_TAB</terminalRecordingDefaultScope>
            <terminalRecordingAutoPauseEnabled>true</terminalRecordingAutoPauseEnabled>
            <terminalRecordingIdlePauseSeconds>73</terminalRecordingIdlePauseSeconds>
            <terminalRecordingCaptureColorsEnabled>true</terminalRecordingCaptureColorsEnabled>
            <terminalRecordingStoragePath>C:\Java\Recorded Sessions</terminalRecordingStoragePath>
            <terminalRecordingFfmpegPath>C:\Java\ffmpeg.exe</terminalRecordingFfmpegPath>
        </globalSettings>"#,
        )
        .unwrap();
    archive.finish().unwrap();
    let parse = || super::super::inspect_java_password(&path, &password).unwrap();
    let function_key = |number, shortcut, control| {
        json!({"key": {"function": number}, "shortcut": shortcut,
            "control": control, "shift": true, "alt": false})
    };
    for configured in [false, true] {
        let local = Scratch::new();
        let window_path = local.0.join(data::WINDOW_SETTINGS);
        let recording_path = local.0.join(preferences::FILE);
        let native_key = function_key(9, true, false);
        let conflicting_key = function_key(7, false, true);
        let native_recording_directory = local.0.join("native recordings");
        if configured {
            fs::write(
                &window_path,
                serde_json::to_vec(&json!({
                    "coding_agent_notifications_enabled": true,
                    "coding_agent_dock": {"placement": "right", "width": 880, "future_layout": true},
                    "key_bindings": {
                        "ai-manager": native_key, "recordings": conflicting_key,
                        "future-action": {"extension": "keep"}
                    }
                })).unwrap(),
            ).unwrap();
            fs::write(
                &recording_path,
                serde_json::to_vec(&json!({
                    "directory": native_recording_directory,
                    "include_color": false, "idle_seconds": 19,
                }))
                .unwrap(),
            )
            .unwrap();
        }
        let preview = parse();
        let (_, entries, omissions) = preview.java_review().unwrap();
        assert!(
            entries
                .iter()
                .any(|line| line == "Recording preferences for unset fields (5)")
        );
        assert!(
            omissions
                .iter()
                .any(|line| line == "Invalid or unsupported Java global preferences omitted (2)")
        );
        assert!(omissions.iter().any(|line| line
            == "Java recording storage and encoder paths require selection on this device (2)"));
        super::super::queue_restore_at(&local.0, preview, RestoreMode::JavaAdditive).unwrap();
        assert_eq!(window_path.exists(), configured);
        assert_eq!(recording_path.exists(), configured);
        drop(ProfileLease::acquire(&local.0, || false).unwrap());
        let window_bytes = fs::read(&window_path).unwrap();
        let window: serde_json::Value = serde_json::from_slice(&window_bytes).unwrap();
        assert!(window.get("java_display_preferences").is_none());
        assert_eq!(window["coding_agent_detection_enabled"], false);
        assert_eq!(window["coding_agent_app_badge_enabled"], false);
        assert_eq!(window["coding_agent_notifications_enabled"], configured);
        assert_eq!(window["coding_agent_dock_automatic"], false);
        assert_eq!(window["remote_sidebar_position"], "right");
        assert_eq!(window["coding_agent_dock"]["side"], "left");
        assert_eq!(
            window["coding_agent_dock"]["placement"],
            if configured { "right" } else { "left" }
        );
        assert_eq!(
            window["coding_agent_dock"]["width"],
            if configured { 880 } else { 951 }
        );
        let shortcuts = window["key_bindings"].as_object().unwrap();
        assert_eq!(
            shortcuts.get("snippet-palette"),
            Some(&serde_json::Value::Null)
        );
        assert!(!shortcuts.contains_key("dashboard"));
        if configured {
            assert_eq!(window["coding_agent_dock"]["future_layout"], true);
            assert_eq!(shortcuts["ai-manager"], native_key);
            assert_eq!(shortcuts["recordings"], conflicting_key);
            assert_eq!(shortcuts["future-action"], json!({"extension": "keep"}));
            assert!(!shortcuts.contains_key("scheduler"));
        } else {
            assert_eq!(shortcuts["ai-manager"], function_key(8, true, false));
            assert_eq!(shortcuts["scheduler"], conflicting_key);
        }
        let recording_bytes = fs::read(&recording_path).unwrap();
        let recording = Preferences::load_from(&recording_path).unwrap();
        assert!(recording.ffmpeg.is_none());
        assert_eq!(recording.scope, Scope::WholeTab);
        assert_eq!(recording.enabled_after_restart, !configured);
        let options = recording.capture_options();
        assert_eq!(options.include_style, !configured);
        assert_eq!(
            options.idle_after,
            Some(std::time::Duration::from_secs(if configured {
                19
            } else {
                73
            }))
        );
        if configured {
            assert_eq!(recording.directory().unwrap(), native_recording_directory);
        }
        super::super::queue_restore_at(&local.0, parse(), RestoreMode::JavaAdditive).unwrap();
        drop(ProfileLease::acquire(&local.0, || false).unwrap());
        assert_eq!(fs::read(&window_path).unwrap(), window_bytes);
        assert_eq!(fs::read(&recording_path).unwrap(), recording_bytes);
    }
}

#[test]
fn java_highlight_restore_remaps_live_connections_and_logfile_selection() {
    use crate::terminal_highlight::{Action, Configuration, Rule, RuleSet};
    use serde_json::{Value, json};
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    let source = Scratch::new();
    let password = Secret::new("fixture archive password".into());
    const SOURCE_ID: &str = "f827d86d-1531-4cc5-9d0f-f6e5c1a2d850";
    let connections = format!(
        r#"<connections>
        <connection id="{SOURCE_ID}"><name>Ops</name><host>ops.example</host>
            <username>tester</username><highlightRuleSetId>service-alerts</highlightRuleSetId></connection>
        <connection><name>Inherit</name><host>other.example</host>
            <highlightRuleSetId>missing</highlightRuleSetId></connection>
        <connection><name>Shell</name><protocol>LOCAL_SHELL</protocol>
            <highlightRuleSetId>none</highlightRuleSetId></connection>
        <connection><name>Console</name><protocol>SERIAL</protocol>
            <highlightRuleSetId>service-alerts</highlightRuleSetId>
            <serial><portName>COM3</portName></serial></connection>
    </connections>"#
    );
    let create_backup = |transactional| {
        let path = source
            .0
            .join(format!("java-highlights-{transactional}.zip"));
        let mut archive = ZipWriter::new(File::create(&path).unwrap());
        let options = SimpleFileOptions::default()
            .with_aes_encryption_bytes(AesMode::Aes256, password.as_str().as_bytes());
        archive.start_file("connections.xml", options).unwrap();
        archive.write_all(connections.as_bytes()).unwrap();
        if transactional {
            archive
                .start_file(format!("memory/connections/{SOURCE_ID}.md"), options)
                .unwrap();
            archive.write_all(b"Check this service.\n").unwrap();
        }
        archive.start_file("global-settings.xml", options).unwrap();
        archive
            .write_all(
                r##"<globalSettings>
                <logfileViewerHighlightKeywords>true</logfileViewerHighlightKeywords>
                <logfileViewerKeywordSetId>service-alerts</logfileViewerKeywordSetId>
                <terminalHighlightingEnabled>false</terminalHighlightingEnabled>
                <terminalHighlightAlternateScreen>true</terminalHighlightAlternateScreen>
                <defaultHighlightRuleSetId>service-alerts</defaultHighlightRuleSetId>
                <terminalTriggersEnabled>true</terminalTriggersEnabled>
                <highlightRuleSets>
                    <ruleSet><id>service-alerts</id><name>Service alerts</name><rules>
                        <rule><pattern>retry [0-9]+</pattern><regex>true</regex>
                            <foreground>#ed8010</foreground><scope>LINE</scope>
                            <action>RUN_SNIPPET</action><snippetId>missing</snippetId></rule>
                        <rule><pattern>exhausted</pattern><bold>true</bold>
                            <action>NOTIFY</action><notifyWithText>true</notifyWithText></rule>
                        <rule><pattern>already\p{javaUnknownProperty}</pattern><regex>true</regex></rule>
                    </rules></ruleSet>
                    <ruleSet><id>builtin.errors</id><name>Reserved</name></ruleSet>
                </highlightRuleSets>
            </globalSettings>"##
                    .as_bytes(),
            )
            .unwrap();
        archive.finish().unwrap();
        path
    };
    let simple = create_backup(false);
    let transactional = create_backup(true);

    let native = RuleSet {
        id: "service-alerts".into(),
        name: "Service alerts".into(),
        rules: vec![Rule {
            pattern: "local only".into(),
            action: Action::Notify,
            ..Rule::default()
        }],
    };
    for (collision, local_selection, transaction) in [
        (false, false, false),
        (true, false, false),
        (true, true, true),
    ] {
        let local = Scratch::new();
        let highlights = local.0.join(terminal_highlight::FILE);
        let window = local.0.join(data::WINDOW_SETTINGS);
        let path = if transaction { &transactional } else { &simple };
        if local_selection {
            fs::write(
                &highlights,
                json!({
                    "sets": [&native], "triggers_enabled": true,
                    "enabled": true, "alternate_screen": false, "default_set": "builtin.errors"
                })
                .to_string(),
            )
            .unwrap();
        } else if collision {
            fs::write(
                &highlights,
                json!({"sets": [&native], "triggers_enabled": true}).to_string(),
            )
            .unwrap();
        }
        let local_presentation = if local_selection {
            json!({"links": false, "keyword_set": "builtin.network"})
        } else {
            json!({"links": false})
        };
        fs::write(
            &window,
            json!({"logfile_presentation": local_presentation}).to_string(),
        )
        .unwrap();
        let queue = || {
            let mut preview = super::super::inspect_java_password(path, &password).unwrap();
            preview.prepare_java_highlight_review_at(&local.0).unwrap();
            let (_, entries, omissions) = preview.java_review().unwrap();
            assert_eq!(
                entries.iter().any(|line| line.starts_with(
                    "Highlight set service-alerts already exists locally. Backup references will use the separate set java-",
                )),
                collision
            );
            assert!(
                entries
                    .iter()
                    .any(|line| line == "Custom highlight sets for presentation (1)")
            );
            for message in [
                "Invalid or unsupported Java highlight sets omitted (1)",
                "Invalid or unsupported Java highlight rules omitted (1)",
                "Java highlight trigger actions require separate review (2)",
            ] {
                assert!(omissions.iter().any(|line| line == message), "{message}");
            }
            super::super::queue_restore_at(&local.0, preview, RestoreMode::JavaAdditive).unwrap();
        };
        queue();
        let connection_path = local.0.join(CONNECTIONS);
        if collision {
            let original = fs::read(&highlights).unwrap();
            fs::write(&highlights, "{}").unwrap();
            let error = ProfileLease::acquire(&local.0, || false).err().unwrap();
            assert!(error.to_string().contains("highlight selections changed"));
            assert!(!connection_path.exists());
            let cancellation = pending_java_cancellation(&local.0).unwrap().unwrap();
            fs::write(&highlights, original).unwrap();
            if !transaction {
                discard_pending_java(&cancellation).unwrap();
                queue();
            }
        }
        drop(ProfileLease::acquire(&local.0, || false).unwrap());
        let saved_bytes = fs::read(&highlights).unwrap();
        let saved = Configuration::decode(saved_bytes.clone()).unwrap();
        assert_eq!(saved.sets.len(), 1 + usize::from(collision));
        assert_eq!(saved.enabled, local_selection);
        assert_eq!(saved.alternate_screen, !local_selection);
        assert_eq!(saved.triggers_enabled, collision);
        let imported = saved.sets.last().unwrap();
        if collision {
            assert_eq!(saved.sets[0], native);
            assert!(imported.id.starts_with("java-"));
        } else {
            assert_eq!(imported.id, "service-alerts");
        }
        assert_eq!(imported.name, "Service alerts");
        assert_eq!(
            saved.default_set.as_deref(),
            Some(if local_selection {
                "builtin.errors"
            } else {
                &imported.id
            })
        );
        let catalogue = crate::connection_store::Store::open(connection_path.clone()).unwrap();
        assert_eq!(catalogue.connections().len(), 2);
        assert_eq!(
            catalogue.connections()[0].terminal.highlight_set.as_deref(),
            Some(imported.id.as_str())
        );
        assert!(catalogue.connections()[1].terminal.highlight_set.is_none());
        assert_eq!(
            catalogue.local_profiles()[0]
                .terminal
                .highlight_set
                .as_deref(),
            Some("none")
        );
        assert_eq!(
            catalogue.serial_profiles()[0].highlight_set.as_deref(),
            Some(imported.id.as_str())
        );
        assert_eq!(imported.rules.len(), 2);
        assert_eq!(imported.rules[0].pattern, "retry [0-9]+");
        assert_eq!(imported.rules[0].foreground.as_deref(), Some("#ed8010"));
        assert!(imported.rules[0].whole_line);
        assert!(imported.rules[1].bold);
        assert!(
            imported
                .rules
                .iter()
                .all(|rule| rule.action == Action::None && !rule.notify_with_text)
        );
        let window_bytes = fs::read(&window).unwrap();
        let settings: Value = serde_json::from_slice(&window_bytes).unwrap();
        assert_eq!(
            settings["logfile_presentation"],
            json!({
                "links": false,
                "keywords": true,
                "keyword_set": if local_selection { "builtin.network" } else { imported.id.as_str() },
            })
        );
        let connection_bytes = fs::read(&connection_path).unwrap();
        queue();
        drop(ProfileLease::acquire(&local.0, || false).unwrap());
        assert_eq!(fs::read(&highlights).unwrap(), saved_bytes);
        assert_eq!(fs::read(&window).unwrap(), window_bytes);
        assert_eq!(fs::read(&connection_path).unwrap(), connection_bytes);
    }
}

#[test]
fn java_marker_restore_matches_entries_preserves_local_rules_and_replays() {
    use crate::session_journal::entry_edit::LegacyMarker;
    use crate::session_journal::markers::{self, Definition, Rule, Settings};
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    let source = Scratch::new();
    let path = source.0.join("java-markers.zip");
    let password = Secret::new("fixture archive password".into());
    let mut archive = ZipWriter::new(File::create(&path).unwrap());
    archive
        .start_file(
            "global-settings.xml",
            SimpleFileOptions::default()
                .with_aes_encryption_bytes(AesMode::Aes256, password.as_str().as_bytes()),
        )
        .unwrap();
    let settings = r#"<globalSettings>
        <sessionJournalMarkers>
            <marker><id>review-needed</id><name>Überprüfung &amp; 東京</name><color>#2a73d2</color>
                <builtIn>false</builtIn><legacyMarker>IMPORTANT</legacyMarker></marker>
            <marker><id>needs-follow-up</id><color>89dc3a</color></marker>
            <marker><id>error</id><color>#000000</color><builtIn>true</builtIn></marker>
        </sessionJournalMarkers>
        <sessionJournalMarkerRules>
            <rule><id>one</id><markerId>review-needed</markerId><pattern>^\d+\s+\Qneeds review+\E$</pattern>
                <regex>true</regex><ignoreCase>true</ignoreCase></rule>
            <rule><id>two</id><markerId>error</markerId><pattern>needs review+</pattern></rule>
            <rule><id>three</id><markerId>review-needed</markerId><pattern>unless\p{javaUnknownProperty}</pattern>
                <regex>true</regex><ignoreCase>false</ignoreCase></rule>
            <rule><id>four</id><markerId>needs-follow-up</markerId><pattern>(?&lt;=old)info</pattern>
                <regex>true</regex><enabled>0</enabled></rule>
            <rule><id>five</id><markerId>does-not-exist</markerId><pattern>missing marker</pattern></rule>
            <rule><id>six</id><markerId>none</markerId><pattern>dismiss</pattern></rule>
        </sessionJournalMarkerRules>
        <sessionJournalMarkerRulesEnabled>true</sessionJournalMarkerRulesEnabled>
    </globalSettings>"#;
    archive.write_all(settings.as_bytes()).unwrap();
    archive.finish().unwrap();
    let parse = || super::super::inspect_java_password(&path, &password).unwrap();

    for configured in [false, true] {
        let local = Scratch::new();
        let marker_path = local.0.join(markers::FILE);
        let native = Definition {
            id: "review-needed".to_owned(),
            name: "Native review".to_owned(),
            color: Some([16, 32, 48]),
            legacy: LegacyMarker::Info,
        };
        let first_rule = Rule {
            marker_id: "review-needed".to_owned(),
            pattern: "native report".to_owned(),
            ..Rule::default()
        };
        if configured {
            Settings {
                definitions: vec![native.clone()],
                rules: vec![first_rule.clone()],
                enabled: false,
            }
            .save(&local.0, &Settings::default())
            .unwrap();
        }
        let preview = parse();
        let (_, entries, omissions) = preview.java_review().unwrap();
        assert!(
            entries
                .iter()
                .any(|line| line == "Journal marker definitions (2) and rules (5)")
        );
        assert!(
            omissions
                .iter()
                .any(|line| line == "Invalid or unsupported Java marker settings omitted (2)")
        );
        assert!(omissions.iter().any(
            |line| line == "Java journal marker rules kept inactive pending pattern review (1)"
        ));
        super::super::queue_restore_at(&local.0, preview, RestoreMode::JavaAdditive).unwrap();
        assert_eq!(marker_path.exists(), configured);
        if configured {
            let directory = Directory::open(&local.0, false).unwrap();
            let lock = markers::lock_settings(&directory).unwrap();
            assert!(ProfileLease::acquire(&local.0, || false).is_err());
            assert!(directory.has_entry(PENDING).unwrap());
            assert_eq!(
                Settings::load(&local.0).unwrap().0.rules.as_slice(),
                std::slice::from_ref(&first_rule)
            );
            drop(lock);
        }
        drop(ProfileLease::acquire(&local.0, || false).unwrap());
        let prefix = usize::from(configured);
        let (saved, compiled) = Settings::load(&local.0).unwrap();
        assert_eq!(saved.enabled, !configured);
        assert_eq!(compiled.enabled(), !configured);
        assert_eq!(saved.definitions.len(), 2 + prefix);
        assert_eq!(saved.rules.len(), 5 + prefix);
        if configured {
            assert_eq!(saved.definitions[0], native);
            assert_eq!(saved.rules[0], first_rule);
            assert!(saved.definitions[prefix].id.starts_with("java-"));
            assert_eq!(
                compiled.first_match("native report").unwrap().id,
                "review-needed"
            );
        } else {
            assert_eq!(saved.definitions[0].id, "review-needed");
        }
        let imported = &saved.definitions[prefix];
        assert_eq!(imported.name, "Überprüfung & 東京");
        assert_eq!(imported.color, Some([42, 115, 210]));
        assert_eq!(imported.legacy, LegacyMarker::Important);
        assert_eq!(compiled.find("error").unwrap().color, None);
        assert_eq!(saved.rules[prefix].marker_id, imported.id);
        assert!(saved.rules[prefix].java_regex && saved.rules[prefix].enabled);
        assert_eq!(saved.rules[prefix + 1].marker_id, "error");
        assert_eq!(
            saved.rules[prefix + 2].pattern,
            r"unless\p{javaUnknownProperty}"
        );
        assert!(!saved.rules[prefix + 2].enabled && !saved.rules[prefix + 2].ignore_case);
        assert_eq!(saved.rules[prefix + 3].pattern, "(?<=old)info");
        assert!(!saved.rules[prefix + 3].enabled);
        assert_eq!(
            compiled.first_match("7 NEEDS REVIEW+\r\n").unwrap(),
            imported
        );
        assert_eq!(compiled.first_match("٩ needs review+").unwrap().id, "error");
        assert_eq!(compiled.first_match("dismiss").unwrap().id, "none");
        assert!(compiled.first_match("unlessfail").is_none());

        let original = fs::read(&marker_path).unwrap();
        let portable = data::collect(&local.0).unwrap();
        assert_eq!(portable[markers::FILE].as_slice(), original.as_slice());
        super::super::queue_restore_at(&local.0, parse(), RestoreMode::JavaAdditive).unwrap();
        drop(ProfileLease::acquire(&local.0, || false).unwrap());
        assert_eq!(fs::read(&marker_path).unwrap(), original);
    }
}

#[test]
fn java_os_symbol_definitions_survive_without_connections_and_require_an_image() {
    use crate::connection_store::custom_os_badges::{Catalog, java_catalog};
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    let source = Scratch::new();
    let local = Scratch::new();
    let image = source.0.join("chosen.png");
    image::RgbaImage::from_pixel(24, 24, image::Rgba([20_u8, 120, 190, 255]))
        .save(&image)
        .unwrap();
    let mut catalog = Catalog::load(&local.0).unwrap();
    let existing = catalog.add("Native symbol", "Custom", &image).unwrap();
    let id = existing.strip_prefix("custom:").unwrap();
    let restore = |name: &str, filename: &str| {
        let path = source.0.join("definitions.zip");
        let mut zip = ZipWriter::new(File::create(&path).unwrap());
        zip.start_file(
            "global-settings.xml",
            SimpleFileOptions::default()
                .with_aes_encryption_bytes(AesMode::Aes256, b"fixture password"),
        )
        .unwrap();
        write!(
            zip,
            r#"<globalSettings><customOsBadges>
            <badge id="{id}" name="{name}" file="{filename}"/>
            <badge id="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee" name="Unsafe" file="../chosen.png"/>
            <badge id="bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee" name="First" file="a.png"/>
            <badge id="bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee" name="Second" file="b.png"/>
            </customOsBadges></globalSettings>"#
        )
        .unwrap();
        zip.finish().unwrap();
        let preview =
            super::super::inspect_java_password(&path, &Secret::new("fixture password".into()))
                .unwrap();
        assert!(
            preview
                .java_review()
                .unwrap()
                .1
                .iter()
                .any(|entry| entry == "Java OS symbol definitions awaiting a local image (1)")
        );
        assert!(!preview.snapshot.contains_key(CONNECTIONS));
        super::super::queue_restore_at(&local.0, preview, RestoreMode::JavaAdditive).unwrap();
        drop(ProfileLease::acquire(&local.0, || false).unwrap());
    };
    restore("Unused Java symbol", "source.png");
    assert!(!local.0.join(CONNECTIONS).exists());
    let hints = java_catalog::load(&local.0).unwrap();
    assert_eq!(hints.len(), 1);
    let hint = hints.into_iter().next().unwrap();
    assert_eq!(hint.reference, existing);
    assert_eq!(hint.file.as_deref(), Some("source.png"));
    assert_eq!(catalog.entries().len(), 1);
    assert!(
        catalog
            .adopt_java(
                "Imported symbol",
                "Custom",
                &source.0.join("missing.png"),
                hint.clone()
            )
            .is_err()
    );
    let adopted = catalog
        .adopt_java("Imported symbol", "Custom", &image, hint.clone())
        .unwrap();
    assert_ne!(adopted, existing);
    catalog
        .rename(&adopted, "Renamed symbol", "Custom")
        .unwrap();
    restore("Unused Java symbol", "source.png");
    restore("Other Java symbol", "other.png");

    let hints = java_catalog::load(&local.0).unwrap();
    assert_eq!(hints.len(), 2);
    assert!(hints.iter().all(|entry| entry.reference == existing));
    let mut reloaded = Catalog::load(&local.0).unwrap();
    assert_eq!(reloaded.find(&adopted).unwrap().java_origin(), Some(&hint));
    assert_eq!(reloaded.find(&adopted).unwrap().name(), "Renamed symbol");
    assert!(
        reloaded
            .adopt_java("Duplicate", "Custom", &image, hint)
            .is_err()
    );
    assert_eq!(reloaded.find(&existing).unwrap().java_origin(), None);
    let snapshot = data::collect(&local.0).unwrap();
    assert_eq!(
        java_catalog::decode(&snapshot[java_catalog::PATH]).unwrap(),
        hints
    );
    let other = Scratch::new();
    let directory = Directory::open(&other.0, false).unwrap();
    merge::apply(&directory, &snapshot, &|| false, &|| false).unwrap();
    assert_eq!(java_catalog::load(&other.0).unwrap(), hints);
    assert!(Catalog::load(&other.0).unwrap().entries().is_empty());
}

#[test]
fn java_os_badge_restore_requires_a_local_image_choice_even_when_the_id_exists() {
    use crate::connection_store::{Store, custom_os_badges::Catalog};
    use crate::connection_window::{ConnectionWindow, Message};
    use iced::futures::{StreamExt, executor::block_on};
    use iced_runtime::{Action, task};
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    let local = Scratch::new();
    let source = Scratch::new();
    let collision_png = source.0.join("collision.png");
    let original_png = source.0.join("original.png");
    for (path, color) in [
        (&collision_png, image::Rgba([190_u8, 80, 40, 255])),
        (&original_png, image::Rgba([35_u8, 114, 190, 255])),
    ] {
        image::RgbaImage::from_pixel(24, 24, color)
            .save(path)
            .unwrap();
    }
    let mut catalog = Catalog::load(&local.0).unwrap();
    let collision = catalog
        .add("Existing local symbol", "Custom", &collision_png)
        .unwrap();
    let java_id = collision.strip_prefix("custom:").unwrap();
    let missing = "custom:aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let connections = format!(
        r#"
        <connections>
            <connection id="mapped"><name>Java router</name><host>router.example.test</host><username>fixture</username><osBadge>{collision}</osBadge></connection>
            <connection id="missing"><name>Unknown image</name><host>unknown.example.test</host><username>fixture</username><osBadge>{missing}</osBadge></connection>
            <connection id="builtin"><name>Built-in image</name><host>builtin.example.test</host><username>fixture</username><osBadge>debian</osBadge></connection>
        </connections>"#
    );
    let settings = format!(
        r#"<globalSettings><customOsBadges>
        <badge id="{java_id}" name="Java blue router" file="{java_id}.png"/>
        <badge id="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee" name="Unsafe original path" file="../missing.png"/>
    </customOsBadges></globalSettings>"#
    );
    let path = source.0.join("symbols.zip");
    let mut archive = ZipWriter::new(File::create(&path).unwrap());
    for (name, bytes) in [
        ("connections.xml", &connections),
        ("global-settings.xml", &settings),
    ] {
        archive
            .start_file(
                name,
                SimpleFileOptions::default()
                    .with_aes_encryption_bytes(AesMode::Aes256, b"archive-password"),
            )
            .unwrap();
        archive.write_all(bytes.as_bytes()).unwrap();
    }
    archive.finish().unwrap();
    let inspect = || {
        super::super::inspect_java_password(&path, &Secret::new("archive-password".into())).unwrap()
    };
    assert!(
        inspect()
            .java_review()
            .unwrap()
            .1
            .iter()
            .any(|line| line.contains("OS symbol assignments"))
    );
    super::super::queue_restore_at(&local.0, inspect(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&local.0, || false).unwrap());

    let path = local.0.join(CONNECTIONS);
    let store = Store::open(path.clone()).unwrap();
    let find = |host: &str| {
        store
            .connections()
            .iter()
            .find(|entry| entry.login.endpoint.host == host)
            .unwrap()
            .id
    };
    let id = find("router.example.test");
    let unresolved = find("unknown.example.test");
    let hint = store.java_badge_hint(id).unwrap();
    assert_eq!(hint.reference, collision);
    assert_eq!(hint.name.as_deref(), Some("Java blue router"));
    assert_eq!(
        hint.file.as_deref(),
        Some(format!("{java_id}.png").as_str())
    );
    assert_eq!(store.os_badge_id(id), None);
    assert_eq!(
        store.java_badge_hint(unresolved).unwrap().reference,
        missing
    );
    assert_eq!(store.java_badge_hint(unresolved).unwrap().file, None);
    assert_eq!(
        store.os_badge_id(find("builtin.example.test")),
        Some("debian")
    );

    let adopted = catalog
        .add("Imported blue router", "Custom", &original_png)
        .unwrap();
    assert_ne!(adopted, collision);
    let mut manager = ConnectionWindow::open(path.clone()).unwrap();
    let _ = manager.update(Message::Select(unresolved));
    let _ = manager.update(Message::ToggleOsBadgePicker(unresolved));
    assert!(
        task::into_stream(manager.update(Message::SetOsBadge(unresolved, Some(missing.into()))))
            .is_none()
    );
    let _ = manager.update(Message::Select(id));
    let _ = manager.update(Message::ToggleOsBadgePicker(id));
    let mut operation =
        task::into_stream(manager.update(Message::SetOsBadge(id, Some(adopted.clone())))).unwrap();
    let Some(Action::Output(completed @ Message::Completed(Ok(_)))) = block_on(operation.next())
    else {
        panic!("expected a verified local OS symbol assignment");
    };
    let _ = manager.update(completed);
    drop(manager);
    super::super::queue_restore_at(&local.0, inspect(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    let reopened = Store::open(path).unwrap();
    assert_eq!(reopened.os_badge_id(id), Some(adopted.as_str()));
    assert!(reopened.java_badge_hint(id).is_none());
    assert_eq!(reopened.os_badge_id(unresolved), None);
    assert!(Catalog::load(&local.0).unwrap().find(&collision).is_some());
}

#[test]
fn java_connection_library_restore_remaps_only_unambiguous_insertions() {
    use crate::connection_store::{Store, TabColor, import::Preview as ConnectionPreview};
    use crate::connection_window::{ConnectionWindow, Message, Scope};
    use iced::futures::{StreamExt, executor::block_on};
    use iced_runtime::{Action, task};
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    const INITIAL: &[u8] = br#"<connections>
        <connection><name>Local favorite</name><host>local.example</host><username>fixture</username><group>Work</group></connection>
        <connection><name>Existing match</name><host>existing.example</host><username>fixture</username><group>Work</group></connection>
        <connection><name>Different settings</name><host>different.example</host><username>fixture</username><group>Work</group><terminalEmulationType>VT100</terminalEmulationType></connection>
        <connection><name>Existing shell</name><protocol>LOCAL_SHELL</protocol></connection>
    </connections>"#;
    const CONNECTIONS_XML: &[u8] = br#"<connections>
        <connection id="new-ssh"><name>Java SSH</name><host>new.example</host><username>fixture</username><group> Work / Imported </group></connection>
        <connection id="already-here"><name>Existing match</name><host>existing.example</host><username>fixture</username><group>Work</group></connection>
        <connection id="different"><name>Different settings</name><host>different.example</host><username>fixture</username><group>Work</group><terminalEmulationType>VT220</terminalEmulationType></connection>
        <connection id="existing-shell"><name>Existing shell</name><protocol>LOCAL_SHELL</protocol></connection>
        <connection id="teamwork"><name>Teamwork entry</name><host>team.example</host><username>fixture</username><connectionSource>TEAMWORK</connectionSource></connection>
        <connection id="duplicate-id"><name>First duplicate ID</name><host>first.example</host><username>fixture</username></connection>
        <connection id="duplicate-id"><name>Second duplicate ID</name><host>second.example</host><username>fixture</username><group>Work/Conflict</group></connection>
        <connection id="shadow-a"><name>Shared endpoint</name><host>shadow.example</host><username>fixture</username></connection>
        <connection id="shadow-b"><name>Shared endpoint</name><host>shadow.example</host><username>fixture</username></connection>
        <connection id="new-serial"><name>Java serial</name><protocol>SERIAL</protocol><serial><portName>COM19</portName></serial></connection>
        <connection id="new-shell"><name>Java shell</name><protocol>LOCAL_SHELL</protocol></connection>
    </connections>"#;
    const SETTINGS: &[u8] = br##"<globalSettings>
        <favoriteConnections>
            <connection>new-ssh</connection><connection>already-here</connection><connection>teamwork</connection>
            <connection>duplicate-id</connection><connection>shadow-a</connection><connection>new-serial</connection><connection>new-shell</connection>
            <connection>new</connection><connection>stale</connection>
            <connection>different</connection><connection>existing-shell</connection>
        </favoriteConnections>
        <connectionGroupColors>
            <group path="Work" color="#bbb"/>
            <group path=" Work / Imported " color="#3af"/>
            <group path="Work/Conflict" color="#1a2"/>
            <group path=" Work / Conflict " color="#2a4"/>
            <group path="Orphan" color="#abc"/>
        </connectionGroupColors>
    </globalSettings>"##;
    let source = Scratch::new();
    let archive_path = source.0.join("java-library.zip");
    let mut archive = ZipWriter::new(File::create(&archive_path).unwrap());
    let options = SimpleFileOptions::default()
        .with_aes_encryption_bytes(AesMode::Aes256, b"archive-password");
    for (name, bytes) in [
        ("connections.xml", CONNECTIONS_XML),
        ("global-settings.xml", SETTINGS),
    ] {
        archive.start_file(name, options).unwrap();
        archive.write_all(bytes).unwrap();
    }
    archive.finish().unwrap();

    let local = Scratch::new();
    let directory = Directory::open(&local.0, false).unwrap();
    let (initial, _) = ConnectionPreview::java_backup(INITIAL)
        .unwrap()
        .portable_java_backup()
        .unwrap();
    directory.publish_copy(CONNECTIONS, &initial).unwrap();
    let connection_path = local.0.join(CONNECTIONS);
    let mut store = Store::open(connection_path.clone()).unwrap();
    let native_id = store.connections()[0].id;
    assert!(
        store
            .toggle_connection_favorites(&[native_id], &[])
            .unwrap()
    );
    store
        .set_group_color("Work", TabColor::parse("#900"))
        .unwrap();
    drop(store);

    let inspect = || {
        super::super::inspect_java_password(&archive_path, &Secret::new("archive-password".into()))
            .unwrap()
    };
    for _ in 0..2 {
        super::super::queue_restore_at(&local.0, inspect(), RestoreMode::JavaAdditive).unwrap();
        drop(ProfileLease::acquire(&local.0, || false).unwrap());
    }
    let mut store = Store::open(connection_path.clone()).unwrap();
    let favorites: Vec<_> = store
        .connections()
        .iter()
        .filter(|entry| store.is_favorite(entry.id))
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(favorites, ["Local favorite", "Java SSH"]);
    let existing = store
        .connections()
        .iter()
        .find(|entry| entry.name == "Existing match")
        .unwrap()
        .id;
    let differing = store
        .connections()
        .iter()
        .find(|entry| entry.name == "Different settings")
        .unwrap()
        .id;
    assert!(store.has_java_favorite(existing));
    assert!(!store.has_java_favorite(differing));
    assert!(
        !store
            .connections()
            .iter()
            .any(|entry| entry.login.endpoint.host == "team.example")
    );
    let serial = store
        .serial_profiles()
        .iter()
        .find(|profile| profile.name == "Java serial")
        .unwrap();
    assert!(store.is_favorite(serial.id));
    let shell = store
        .local_profiles()
        .iter()
        .find(|profile| profile.name == "Java shell")
        .unwrap();
    assert!(store.is_favorite(shell.id));
    assert!(!store.has_java_favorite(shell.id));
    let imported_shell = shell.id;
    let existing_shell = store
        .local_profiles()
        .iter()
        .find(|entry| entry.name == "Existing shell")
        .unwrap()
        .id;
    assert!(store.has_java_favorite(existing_shell));
    store.review_java_favorite(existing_shell, false).unwrap();
    assert_eq!(store.group_color("Work"), TabColor::parse("#900"));
    assert_eq!(store.group_color("Work/Imported"), TabColor::parse("#3af"));
    assert_eq!(store.group_color("Work/Conflict"), None);
    assert!(!store.has_group("Orphan"));
    let hints = store.java_group_colors();
    assert_eq!(hints.len(), 4);
    assert_eq!(
        hints
            .iter()
            .filter(|hint| hint.source == "Work/Conflict")
            .count(),
        2
    );
    let orphan = hints
        .iter()
        .find(|hint| hint.source == "Orphan")
        .unwrap()
        .clone();

    let imported = store
        .connections()
        .iter()
        .find(|entry| entry.name == "Java SSH")
        .unwrap()
        .id;
    assert!(!store.toggle_connection_favorites(&[imported], &[]).unwrap());
    store
        .set_group_color("Work/Imported", TabColor::parse("#098"))
        .unwrap();
    drop(store);

    let mut editor = ConnectionWindow::open(connection_path.clone()).unwrap();
    let _ = editor.update(Message::SelectGroup("Work".into()));
    let _ = editor.update(Message::EditGroupColor);
    let _ = editor.update(Message::PickJavaGroupColor(orphan.clone()));
    let _ = editor.update(Message::CancelGroup);
    assert_eq!(
        Store::open(connection_path.clone())
            .unwrap()
            .group_color("Work"),
        TabColor::parse("#900")
    );
    let _ = editor.update(Message::EditGroupColor);
    let _ = editor.update(Message::PickJavaGroupColor(orphan.clone()));
    let mut operation = task::into_stream(editor.update(Message::CommitGroup)).unwrap();
    let Some(Action::Output(completed @ Message::Completed(Ok(_)))) = block_on(operation.next())
    else {
        panic!("expected a reviewed group color assignment");
    };
    let _ = editor.update(completed);
    let _ = editor.update(Message::Select(existing));
    let mut favorite =
        task::into_stream(editor.update(Message::ReviewJavaFavorite(existing, true))).unwrap();
    let Some(Action::Output(completed @ Message::Completed(Ok(_)))) = block_on(favorite.next())
    else {
        panic!("expected an explicitly accepted Java favorite");
    };
    let _ = editor.update(completed);
    let _ = editor.update(Message::Scope(Scope::Favorites));
    let _ = editor.update(Message::SelectLocal(imported_shell));
    assert_eq!(
        editor.selected_local().map(|profile| profile.id),
        Some(imported_shell)
    );
    let mut store = Store::open(connection_path.clone()).unwrap();
    assert!(store.is_favorite(existing));
    assert!(!store.has_java_favorite(existing));
    assert!(!store.toggle_connection_favorites(&[existing], &[]).unwrap());
    drop(store);
    drop(editor);

    super::super::queue_restore_at(&local.0, inspect(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    let store = Store::open(connection_path).unwrap();
    assert!(!store.is_favorite(imported));
    assert!(!store.has_java_favorite(imported));
    assert!(!store.is_favorite(existing));
    assert!(!store.has_java_favorite(existing));
    assert!(!store.is_favorite(existing_shell));
    assert!(!store.has_java_favorite(existing_shell));
    assert_eq!(store.group_color("Work/Imported"), TabColor::parse("#098"));
    assert_eq!(store.group_color("Work"), Some(orphan.color));
    assert!(!store.java_group_colors().contains(&orphan));
}

#[test]
fn java_group_colors_restore_from_settings_alone_without_creating_folders() {
    use crate::connection_store::Store;
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    let source = Scratch::new();
    let local = Scratch::new();
    let path = source.0.join("java-colors.zip");
    let mut archive = ZipWriter::new(File::create(&path).unwrap());
    archive
        .start_file(
            "global-settings.xml",
            SimpleFileOptions::default()
                .with_aes_encryption_bytes(AesMode::Aes256, b"archive-password"),
        )
        .unwrap();
    archive
        .write_all(
            br##"<globalSettings>
        <connectionGroupColors><group path="Old/Empty" color="#abc"/></connectionGroupColors>
    </globalSettings>"##,
        )
        .unwrap();
    archive.finish().unwrap();
    let preview =
        super::super::inspect_java_password(&path, &Secret::new("archive-password".into()))
            .unwrap();
    super::super::queue_restore_at(&local.0, preview, RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    let store = Store::open(local.0.join(CONNECTIONS)).unwrap();
    assert!(store.connections().is_empty());
    assert!(!store.has_group("Old/Empty"));
    assert_eq!(
        store.java_group_colors().iter().next().unwrap().source,
        "Old/Empty"
    );
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
        r#"<connections><connection id="stored-route"><name>Imported SSH</name><host>java.example</host><username>fixture</username><group>Java/Protected</group><credentialId>stored-credential</credentialId><jumpServer><enabled>true</enabled><host>jump.example</host><username>jump-user</username><encryptedPassword>{java_field}</encryptedPassword></jumpServer></connection><connection id="collision-route"><name>Collision</name><host>preserved.example</host><username>fixture</username><encryptedPassword>{java_field}</encryptedPassword></connection></connections>"#
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
        ("global-settings.xml", br##"<globalSettings><favoriteConnections><connection>stored-route</connection><connection>collision-route</connection></favoriteConnections><connectionGroupColors><group path="Java/Protected" color="#3af"/></connectionGroupColors></globalSettings>"##),
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
    assert!(!catalog.is_favorite(original.id));
    assert!(catalog.is_favorite(imported.id));
    assert_eq!(
        catalog.group_color("Java/Protected").unwrap().to_string(),
        "#33AAFF"
    );
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
