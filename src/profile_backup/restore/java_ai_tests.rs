use std::path::{Path, PathBuf};

use kortty_vault::Secret;
use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

use crate::ai_manager::{Configuration, ProfileRef};
use crate::session_journal::summary_queue::{AutomationProfile, Queue};

use super::*;

const GLOBAL_SETTINGS: &[u8] =
    include_bytes!("../../ai_manager/profiles/fixtures/java-global-settings.xml");

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("kortty-java-ai-restore-{}", nonce().unwrap()));
        Directory::open(&path, true).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn java_ai_profile_backup_queues_and_replays_without_trust_or_default() {
    let scratch = Scratch::new();
    let path = scratch.0.join("java-ai-backup.zip");
    let password = Secret::new("java-archive-password".into());
    write_backup(&path, &password, GLOBAL_SETTINGS);

    let destination = scratch.0.join("profile");
    let live = destination.join(ai_backup::SETTINGS);
    let inspect = || super::super::inspect_java_password(&path, &password).unwrap();
    let preview = inspect();
    assert_eq!(preview.entries().collect::<Vec<_>>(), [ai_backup::SETTINGS]);
    let (_, imported, omitted) = preview.java_review().unwrap();
    assert!(
        imported
            .iter()
            .any(|line| line.contains("HTTPS AI profiles requiring destination review (2)"))
    );
    assert!(
        imported
            .iter()
            .any(|line| line
                .contains("CLI AI profiles requiring executable and argument review (1)"))
    );
    assert!(
        omitted
            .iter()
            .any(|line| line.contains("Java AI API keys omitted (1)"))
    );
    assert!(omitted.iter().any(|line| {
        line.contains("Invalid or unsupported Java AI token pricing omitted from profiles (1)")
    }));
    assert!(!live.exists());

    super::super::queue_restore_at(&destination, preview, RestoreMode::JavaAdditive).unwrap();
    assert!(!live.exists());
    let pending = fs::read(destination.join(PENDING)).unwrap();
    assert!(
        !pending
            .windows(b"synthetic-source-api-key".len())
            .any(|window| window == b"synthetic-source-api-key")
    );
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    assert!(!destination.join(PENDING).exists());
    let installed = fs::read(&live).unwrap();
    let settings: serde_json::Value = serde_json::from_slice(&installed).unwrap();
    assert!(settings["profiles"]["default"].is_null());
    let profiles = settings["profiles"]["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 3);
    assert!(profiles.iter().all(|profile| {
        profile["restore_review"] == true
            && profile["trusted_local_endpoint"] == false
            && profile["trusted_remote_endpoint"] == false
            && profile["credential_env"] == ""
    }));
    assert_eq!(
        profiles[0]["endpoint"],
        "https://example.test/openai/v1/chat/completions"
    );
    assert_eq!(profiles[2]["connection_mode"], "cli");
    assert_eq!(profiles[2]["cli"]["path"], "/fixture/cli-not-installed");
    assert!(profiles[2]["cli"]["fingerprint"].is_null());
    assert_eq!(
        profiles[0]["usage_budget"]["input_micros_per_million"],
        1_234_567
    );
    assert_eq!(
        profiles[0]["usage_budget"]["output_micros_per_million"],
        25_000
    );
    assert_eq!(profiles[0]["usage_budget"]["currency"], "CHF");
    assert!(profiles[1]["usage_budget"]["input_micros_per_million"].is_null());
    assert_eq!(
        profiles[1]["usage_budget"]["output_micros_per_million"],
        4_250_000
    );
    assert_eq!(profiles[1]["usage_budget"]["currency"], "EUR");
    assert_eq!(profiles[2]["usage_budget"]["input_micros_per_million"], 0);
    assert!(profiles[2]["usage_budget"]["output_micros_per_million"].is_null());
    let ai = Configuration::load_from(&destination).unwrap();
    assert!(ai.terminal_chat_transport(3).is_err());
    assert!(ai.reviewed_agent_cli_profiles().next().is_none());

    super::super::queue_restore_at(&destination, inspect(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    assert_eq!(fs::read(live).unwrap(), installed);
}

fn write_backup(path: &Path, password: &Secret, contents: &[u8]) {
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    zip.start_file(
        "global-settings.xml",
        SimpleFileOptions::default()
            .with_aes_encryption_bytes(AesMode::Aes256, password.as_str().as_bytes()),
    )
    .unwrap();
    zip.write_all(contents).unwrap();
    zip.finish().unwrap();
}

#[test]
fn java_connection_ai_assignments_restore_inactive_with_exact_profile_provenance() {
    use crate::connection_store::Store;

    const SETTINGS: &[u8] = br#"<globalSettings><aiProfiles>
        <profile><id>java-chat</id><name>Imported chat</name>
            <apiUrl>http://127.0.0.1:12345/v1</apiUrl><modelSelectionMode>DEFAULT</modelSelectionMode>
        </profile>
    </aiProfiles></globalSettings>"#;
    const CONNECTIONS: &[u8] = br#"<connections>
        <connection><name>Imported SSH</name><host>ssh.example.test</host><username>fixture</username>
            <aiProfileId>java-chat</aiProfileId></connection>
        <connection><name>Imported shell</name><protocol>LOCAL_SHELL</protocol>
            <aiProfileId>java-chat</aiProfileId></connection>
        <connection><name>Unresolved shell</name><protocol>LOCAL_SHELL</protocol>
            <aiProfileId>missing-chat</aiProfileId></connection>
    </connections>"#;
    let scratch = Scratch::new();
    let path = scratch.0.join("connection-ai.zip");
    let password = Secret::new("java-archive-password".into());
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    for (name, bytes) in [
        ("global-settings.xml", SETTINGS),
        ("connections.xml", CONNECTIONS),
    ] {
        zip.start_file(
            name,
            SimpleFileOptions::default()
                .with_aes_encryption_bytes(AesMode::Aes256, password.as_str().as_bytes()),
        )
        .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    let destination = scratch.0.join("profile");
    let inspect = || super::super::inspect_java_password(&path, &password).unwrap();
    let preview = inspect();
    let (_, _, omitted) = preview.java_review().unwrap();
    assert!(
        omitted.iter().any(|line| line
            == "Java connection AI assignments require review in Connection Manager (2)")
    );
    assert!(
        omitted.iter().any(|line| line
            == "Java connection AI assignments have no matching profile in the backup (1)")
    );
    super::super::queue_restore_at(&destination, preview, RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&destination, || false).unwrap());

    let connections = destination.join("connections.json");
    let store = Store::open(connections.clone()).unwrap();
    let config = Configuration::load_from(&destination).unwrap();
    let source = config.java_profile_source(&ProfileRef::Numeric(1)).unwrap();
    assert!(config.interactive_chat_transport(1).is_err());
    let ssh = store
        .connections()
        .iter()
        .find(|entry| entry.name == "Imported SSH")
        .unwrap();
    let shell = store
        .local_profiles()
        .iter()
        .find(|entry| entry.name == "Imported shell")
        .unwrap();
    for terminal in [&ssh.terminal, &shell.terminal] {
        assert!(terminal.ai_profile.is_none());
        assert_eq!(
            terminal.java_ai_profile.as_ref().unwrap().source(),
            Some(source)
        );
    }
    let unresolved = store
        .local_profiles()
        .iter()
        .find(|entry| entry.name == "Unresolved shell")
        .unwrap();
    let hint = unresolved.terminal.java_ai_profile.as_ref().unwrap();
    assert_eq!(hint.id(), "missing-chat");
    assert!(hint.source().is_none());
    assert!(unresolved.terminal.ai_profile.is_none());

    let expected_ssh = ssh.clone();
    let mut existing_ssh = expected_ssh.clone();
    existing_ssh.terminal.java_ai_profile = None;
    let mut existing_shell = shell.clone();
    existing_shell.terminal.java_ai_profile = None;
    let mut existing_unresolved = unresolved.clone();
    existing_unresolved.terminal.dismiss_java_ai_profile();
    let mut store = store;
    store.save(existing_ssh.clone()).unwrap();
    store.save_local(existing_shell.clone()).unwrap();
    store.save_local(existing_unresolved.clone()).unwrap();
    super::super::queue_restore_at(&destination, inspect(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    let mut store = Store::open(connections.clone()).unwrap();
    assert_eq!(store.connections(), &[expected_ssh]);
    assert_eq!(store.local_profiles().len(), 2);
    assert_eq!(
        store.local(existing_unresolved.id),
        Some(&existing_unresolved)
    );
    let mut supplemented = store.local(existing_shell.id).unwrap().clone();
    assert_eq!(
        supplemented
            .terminal
            .java_ai_profile
            .as_ref()
            .unwrap()
            .source(),
        Some(source)
    );
    assert!(supplemented.terminal.ai_profile.is_none());
    supplemented.terminal.dismiss_java_ai_profile();
    store.save_local(supplemented).unwrap();
    existing_ssh.terminal.ai_profile = Some(ProfileRef::Numeric(9));
    store.save(existing_ssh).unwrap();

    let installed = fs::read(&connections).unwrap();
    super::super::queue_restore_at(&destination, inspect(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    assert_eq!(fs::read(connections).unwrap(), installed);
}

#[test]
fn java_journal_roles_replay_without_replacing_native_preferences_or_granting_access() {
    use serde_json::json;

    const REGULAR: &str = "45d49b0d-764e-4fc8-9108-b831ba701f88";
    const AUTOMATION: &str = "7f87d66d-5ad2-4ff0-9b07-4b91604626bd";
    let scratch = Scratch::new();
    let path = scratch.0.join("java-journal-roles.zip");
    let password = Secret::new("java-archive-password".into());
    write_backup(&path, &password, GLOBAL_SETTINGS);

    let destination = scratch.0.join("profile");
    let live = destination.join(ai_backup::SETTINGS);
    let directory = Directory::open(&destination, true).unwrap();
    let local = json!({
        "profiles": {
            "profiles": [{
                "id": 7, "name": "Native local model", "model": "fixture-local",
                "endpoint": "http://127.0.0.1:11434"
            }],
            "default": 7,
            "last_id": 7
        }
    });
    let local = serde_json::to_vec(&local).unwrap();
    directory.publish_copy(ai_backup::SETTINGS, &local).unwrap();
    let mut queue = Queue::load(&destination).unwrap();
    let mut preferences = queue.preferences().clone();
    preferences.automation_profile = Some(AutomationProfile::Local);
    queue.set_preferences(preferences.clone()).unwrap();

    let inspect = || super::super::inspect_java_password(&path, &password).unwrap();
    super::super::queue_restore_at(&destination, inspect(), RestoreMode::JavaAdditive).unwrap();
    assert_eq!(fs::read(&live).unwrap(), local);
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    assert!(!destination.join(PENDING).exists());
    let installed = fs::read(&live).unwrap();
    let ai = Configuration::load_from(&destination).unwrap();
    assert_eq!(
        ai.default_ai_profile_reference(),
        Some(ProfileRef::Numeric(7))
    );
    assert_eq!(ai.journal_summary_default_id(), Some(7));
    for (automation, java_id, profile) in [(false, REGULAR, 8), (true, AUTOMATION, 10)] {
        let hint = ai.imported_journal_summary_profile(automation).unwrap();
        assert_eq!(hint.id(), java_id);
        let source = ai
            .java_profile_source(&ProfileRef::Numeric(profile))
            .unwrap();
        assert_eq!(hint.source(), Some(source));
        assert_eq!(
            ai.resolve_imported_journal_summary_profile(automation),
            Some(u128::from(profile))
        );
    }
    assert!(ai.journal_summary_https_revision(8).is_err());
    assert!(ai.journal_summary_cli_revision(10).is_err());
    let queue = Queue::load(&destination).unwrap();
    assert_eq!(queue.preferences(), &preferences);
    assert!(queue.pending().is_empty());
    assert!(queue.preferences().automation_grant().is_none());

    super::super::queue_restore_at(&destination, inspect(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    assert_eq!(fs::read(live).unwrap(), installed);
}

#[test]
fn java_journal_role_only_backup_remains_unresolved_after_replay() {
    let scratch = Scratch::new();
    let path = scratch.0.join("java-journal-role-only.zip");
    let password = Secret::new("java-archive-password".into());
    write_backup(
        &path,
        &password,
        br#"<globalSettings>
            <sessionJournalAiProfileId>journal-only</sessionJournalAiProfileId>
            <automationJournalAiProfileId>automation-only</automationJournalAiProfileId>
        </globalSettings>"#,
    );
    let destination = scratch.0.join("profile");
    let live = destination.join(ai_backup::SETTINGS);
    let inspect = || super::super::inspect_java_password(&path, &password).unwrap();
    let preview = inspect();
    assert_eq!(preview.entries().collect::<Vec<_>>(), [ai_backup::SETTINGS]);
    super::super::queue_restore_at(&destination, preview, RestoreMode::JavaAdditive).unwrap();
    assert!(!live.exists());
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    let installed = fs::read(&live).unwrap();
    let ai = Configuration::load_from(&destination).unwrap();
    assert!(ai.default_ai_profile_reference().is_none());
    for (automation, java_id) in [(false, "journal-only"), (true, "automation-only")] {
        let hint = ai.imported_journal_summary_profile(automation).unwrap();
        assert_eq!(hint.id(), java_id);
        assert!(hint.source().is_none());
        assert!(
            ai.resolve_imported_journal_summary_profile(automation)
                .is_none()
        );
    }
    super::super::queue_restore_at(&destination, inspect(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    assert_eq!(fs::read(live).unwrap(), installed);
}

#[test]
fn java_loopback_ai_backup_restores_only_reviewable_endpoints() {
    use serde_json::json;

    let scratch = Scratch::new();
    let path = scratch.0.join("java-local-ai.zip");
    let password = Secret::new("java-archive-password".into());
    write_backup(
        &path,
        &password,
        br#"<globalSettings>
            <sessionJournalAiProfileId>local-inference</sessionJournalAiProfileId>
            <automationJournalAiProfileId>studio-https-auto</automationJournalAiProfileId>
            <aiBraveSearchMcpPluginId>mcp/backup-fixture</aiBraveSearchMcpPluginId>
            <encryptedAiBraveSearchApiKey>test-web-key-not-imported</encryptedAiBraveSearchApiKey>
            <aiProfiles>
                <profile><id>local-inference</id><name>Local inference</name>
                    <model>local-model</model><apiUrl>http://127.0.0.1:11434/v1/chat/completions</apiUrl>
                    <internetAccessMode>KORTTY_TAVILY_TOOL</internetAccessMode>
                    <encryptedApiKey>test-key-not-imported</encryptedApiKey></profile>
                <profile><id>studio-inference</id><name>Studio inference</name>
                    <model>studio-model</model><apiUrl>http://[::1]:1234/api/v1/chat</apiUrl>
                    <internetAccessMode>BRAVE_SEARCH_MCP</internetAccessMode></profile>
                <profile><id>studio-compatible-auto</id><name>Studio compatible Auto</name>
                    <apiUrl>http://127.0.0.1:1234/v1/chat/completions</apiUrl>
                    <modelSelectionMode>AUTO</modelSelectionMode><model>vendor/preferred</model>
                    <encryptedApiKey>test-auto-key-not-imported</encryptedApiKey></profile>
                <profile><id>studio-localhost-auto</id><name>Studio localhost Auto</name>
                    <apiUrl>http://LOCALHOST:1234/v1/</apiUrl></profile>
                <profile><id>studio-https-auto</id><name>Studio HTTPS Auto</name>
                    <apiUrl>https://localhost:1234/api/v1/chat</apiUrl>
                    <internetAccessMode>BRAVE_SEARCH_MCP</internetAccessMode>
                    <modelSelectionMode>AUTO</modelSelectionMode><model>vendor/preferred</model>
                    <encryptedApiKey>test-tls-key-not-imported</encryptedApiKey></profile>
                <profile><id>studio-localhost-manual</id><name>Studio localhost Manual</name>
                    <apiUrl>http://localhost:1234/api/v1/chat</apiUrl>
                    <internetAccessMode>BRAVE_SEARCH_MCP</internetAccessMode>
                    <model>vendor/manual</model></profile>
                <profile><id>studio-https-manual</id><name>Studio HTTPS Manual</name>
                    <apiUrl>https://localhost:1234/api/v1/chat</apiUrl>
                    <internetAccessMode>BRAVE_SEARCH_MCP</internetAccessMode>
                    <modelSelectionMode>MANUAL</modelSelectionMode><model>vendor/manual</model>
                    <encryptedApiKey>test-manual-key-not-imported</encryptedApiKey></profile>
                <profile><id>studio-https-origin</id><name>Studio HTTPS compatible origin</name>
                    <apiUrl>https://localhost:1234/</apiUrl><model>vendor/manual</model></profile>
                <profile><id>network-inference</id><name>Other HTTP</name>
                    <model>other-model</model><apiUrl>http://192.0.2.55:11434/v1</apiUrl></profile>
            </aiProfiles>
        </globalSettings>"#,
    );
    let destination = scratch.0.join("profile");
    let live = destination.join(ai_backup::SETTINGS);
    let inspect = || super::super::inspect_java_password(&path, &password).unwrap();
    let preview = inspect();
    let (_, imported, _) = preview.java_review().unwrap();
    assert!(
        imported
            .iter()
            .any(|line| line.contains("Loopback AI profiles requiring endpoint review (5)"))
    );
    assert!(
        imported
            .iter()
            .any(|line| { line.contains("HTTPS AI profiles requiring destination review (3)") })
    );
    assert!(imported.iter().any(|line| {
        line.contains("Web provider suggestions requiring profile review and opt-in (5)")
    }));
    super::super::queue_restore_at(&destination, preview, RestoreMode::JavaAdditive).unwrap();
    assert!(!live.exists());
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    let settings = fs::read(&live).unwrap();
    let stored: serde_json::Value = serde_json::from_slice(&settings).unwrap();
    let profiles = stored["profiles"]["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 8);
    assert!(profiles[0].get("connection_mode").is_none());
    assert_eq!(profiles[1]["connection_mode"], "lm-studio");
    assert_eq!(profiles[2]["connection_mode"], "lm-studio-compatible");
    assert_eq!(profiles[2]["model_mode"], "auto");
    assert_eq!(profiles[2]["model"], "vendor/preferred");
    assert_eq!(profiles[2]["endpoint"], "127.0.0.1:1234");
    assert_eq!(profiles[3]["connection_mode"], "lm-studio-compatible");
    assert_eq!(profiles[3]["model_mode"], "auto");
    assert_eq!(profiles[3]["model"], "");
    assert_eq!(
        profiles[3]["endpoint"],
        "http://localhost:1234/v1/chat/completions"
    );
    assert_eq!(profiles[4]["connection_mode"], "lm-studio");
    assert_eq!(profiles[4]["model_mode"], "auto");
    assert_eq!(profiles[4]["model"], "vendor/preferred");
    assert_eq!(
        profiles[4]["endpoint"],
        "https://localhost:1234/api/v1/chat"
    );
    for (profile, mode, endpoint) in [
        (
            &profiles[5],
            "lm-studio",
            "http://localhost:1234/api/v1/chat",
        ),
        (
            &profiles[6],
            "lm-studio",
            "https://localhost:1234/api/v1/chat",
        ),
        (
            &profiles[7],
            "lm-studio-compatible",
            "https://localhost:1234/v1/chat/completions",
        ),
    ] {
        assert_eq!(profile["connection_mode"], mode);
        assert_eq!(profile["model"], "vendor/manual");
        assert_eq!(profile["endpoint"], endpoint);
        assert_ne!(profile["model_mode"], "auto");
    }
    assert_eq!(profiles[0]["java_web_hint"], json!({"mode":"tavily"}));
    for index in [1, 4, 5, 6] {
        assert_eq!(
            profiles[index]["java_web_hint"],
            json!({"mode":"brave", "plugin":"mcp/backup-fixture"})
        );
    }
    assert!(profiles.iter().all(|profile| {
        profile["restore_review"] == true
            && profile["trusted_local_endpoint"] == false
            && profile["trusted_remote_endpoint"].as_bool() != Some(true)
            && profile["credential_env"] == ""
            && profile.get("lm_web").is_none_or(serde_json::Value::is_null)
            && profile
                .get("tavily_search")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
    }));
    let config = Configuration::load_from(&destination).unwrap();
    for (automation, id) in [(false, 1), (true, 5)] {
        assert_eq!(
            config.resolve_imported_journal_summary_profile(automation),
            Some(id)
        );
        assert!(config.terminal_chat_transport(id).is_err());
        assert!(config.agent_transport(id).is_err());
    }
    for id in 1..=8 {
        assert!(config.terminal_chat_transport(id).is_err());
        assert!(config.agent_transport(id).is_err());
    }
    assert!(config.journal_summary_https_revision(5).is_err());
    assert!(config.reviewed_agent_https_profiles().next().is_none());
    assert_eq!(config.lm_studio_auto_selections().count(), 0);
    assert!(config.default_ai_profile_reference().is_none());
    assert!(config.terminal_chat_profile_id().is_none());
    assert!(!String::from_utf8_lossy(&settings).contains("test-key-not-imported"));
    assert!(!String::from_utf8_lossy(&settings).contains("test-web-key-not-imported"));
    assert!(!String::from_utf8_lossy(&settings).contains("test-auto-key-not-imported"));
    assert!(!String::from_utf8_lossy(&settings).contains("test-tls-key-not-imported"));
    assert!(!String::from_utf8_lossy(&settings).contains("test-manual-key-not-imported"));
    super::super::queue_restore_at(&destination, inspect(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    assert_eq!(fs::read(live).unwrap(), settings);
}

#[test]
fn java_embedded_ai_backup_preserves_only_reviewable_model_provenance() {
    let scratch = Scratch::new();
    let path = scratch.0.join("java-embedded-ai.zip");
    let password = Secret::new("java-archive-password".into());
    let options = SimpleFileOptions::default()
        .with_aes_encryption_bytes(AesMode::Aes256, password.as_str().as_bytes());
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    zip.start_file("global-settings.xml", options).unwrap();
    zip.write_all(
        br#"<globalSettings>
        <sessionJournalAiProfileId>portable-gguf</sessionJournalAiProfileId>
        <automationJournalAiProfileId>portable-mlx</automationJournalAiProfileId>
        <aiProfiles>
            <profile><id>portable-gguf</id><name>My Java GGUF</name>
                <connectionMode>EMBEDDED_LLAMA_CPP</connectionMode>
                <embeddedModelId>qwen-gguf.8b</embeddedModelId>
                <model>aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</model>
            </profile>
            <profile><id>portable-mlx</id><name>My Java MLX</name>
                <connectionMode>EMBEDDED_MLX</connectionMode>
                <embeddedModelId>qwen_mlx-8b</embeddedModelId>
            </profile>
        </aiProfiles>
    </globalSettings>"#,
    )
    .unwrap();
    zip.start_file("llm/models.xml", options).unwrap();
    zip.write_all(
        br#"<llamaModels><schemaVersion>1</schemaVersion>
        <model><id>qwen-gguf.8b</id><displayName>Registry GGUF</displayName>
        <modelPath>/private/fixture/qwen.gguf</modelPath>
        <serverExecutablePath>/private/fixture/llama-server</serverExecutablePath></model>
    </llamaModels>"#,
    )
    .unwrap();
    zip.start_file("llm/mlx-models.json", options).unwrap();
    zip.write_all(br#"{"schemaVersion":1,"models":[{"id":"qwen_mlx-8b","displayName":"Registry MLX","modelDirectory":"/private/fixture/mlx"}]}"#).unwrap();
    zip.finish().unwrap();

    let destination = scratch.0.join("profile");
    let live = destination.join(ai_backup::SETTINGS);
    let inspect = || super::super::inspect_java_password(&path, &password).unwrap();
    let preview = inspect();
    let (_, imported, _) = preview.java_review().unwrap();
    assert!(
        imported
            .iter()
            .any(|line| line
                .contains("Embedded AI profiles requiring a registered native model (2)"))
    );
    assert!(
        imported
            .iter()
            .any(|line| line.contains("Local model suggestions requiring review (2)"))
    );
    assert_eq!(preview.entries().collect::<Vec<_>>(), [ai_backup::SETTINGS]);
    super::super::queue_restore_at(&destination, preview, RestoreMode::JavaAdditive).unwrap();
    assert!(!live.exists());
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    let installed = fs::read(&live).unwrap();
    let installed_text = std::str::from_utf8(&installed).unwrap();
    assert!(!installed_text.contains("/private/fixture"));
    assert!(!installed_text.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
    let stored: serde_json::Value = serde_json::from_slice(&installed).unwrap();
    let model_hints = stored["java_model_hints"].as_array().unwrap();
    assert_eq!(model_hints.len(), 2);
    assert_eq!(model_hints[0]["id"], "qwen-gguf.8b");
    assert_eq!(model_hints[1]["id"], "qwen_mlx-8b");
    let profiles = stored["profiles"]["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 2);
    for (profile, mode, original) in [
        (&profiles[0], "managed", "qwen-gguf.8b"),
        (&profiles[1], "managed-mlx", "qwen_mlx-8b"),
    ] {
        assert_eq!(profile["java_embedded_model"], original);
        assert_eq!(profile["connection_mode"], mode);
        assert_eq!(profile["restore_review"], true);
        assert_eq!(profile["model"], "");
    }
    let config = Configuration::load_from(&destination).unwrap();
    assert!(config.default_ai_profile_reference().is_none());
    assert!(config.reviewed_local_ai_profiles().next().is_none());
    for (automation, id) in [(false, 1), (true, 2)] {
        assert_eq!(
            config.resolve_imported_journal_summary_profile(automation),
            Some(id)
        );
        assert!(config.terminal_chat_transport(id).is_err());
        assert!(config.agent_transport(id).is_err());
    }
    super::super::queue_restore_at(&destination, inspect(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    assert_eq!(fs::read(live).unwrap(), installed);
}

#[test]
fn java_model_registries_without_profiles_keep_distinct_kinds_and_existing_settings() {
    let scratch = Scratch::new();
    let source = scratch.0.join("java-models.zip");
    let destination = scratch.0.join("profile");
    let password = Secret::new("fixture".into());
    let write_registries = |gguf_name: &str| {
        let options = SimpleFileOptions::default()
            .with_aes_encryption_bytes(AesMode::Aes256, password.as_str().as_bytes());
        let mut zip = ZipWriter::new(File::create(&source).unwrap());
        zip.start_file("llm/models.xml", options).unwrap();
        write!(
            zip,
            r#"<llamaModels><schemaVersion>1</schemaVersion>
            <model><id>same-model</id><displayName>{gguf_name}</displayName>
             <backend>VULKAN</backend><purpose>EMBEDDING</purpose>
             <contextSize>4096</contextSize><threadCount>8</threadCount>
             <gpuLayers>5</gpuLayers><idleTimeoutMinutes>0</idleTimeoutMinutes>
             <publishedAt>2025-01-02T03:04:05Z</publishedAt>
             <modelPath>/NoAccess/archive-weight.gguf</modelPath>
             <serverExecutablePath>/NoAccess/archive-executable</serverExecutablePath></model>
            <model><id>ambiguous</id><displayName>Duplicate one</displayName></model>
            <model><id>ambiguous</id><displayName>Duplicate two</displayName></model>
        </llamaModels>"#
        )
        .unwrap();
        zip.start_file("llm/mlx-models.json", options).unwrap();
        zip.write_all(br#"{"schemaVersion":1,"models":[{"id":"same-model","displayName":"Fixture MLX","modelDirectory":"/NoAccess/archive-mlx","apiKey":"fixture-secret-do-not-save","contextSize":32768,"idleTimeoutMinutes":30,"quantizationLabel":"Q4_0"}]}"#).unwrap();
        zip.finish().unwrap();
    };
    let live = destination.join(ai_backup::SETTINGS);
    let mut baseline = serde_json::to_value(Configuration::default()).unwrap();
    baseline["general"]["timeout_minutes"] = 12.into();
    Directory::open(&destination, true)
        .unwrap()
        .publish_copy(ai_backup::SETTINGS, &serde_json::to_vec(&baseline).unwrap())
        .unwrap();

    let inspect = || super::super::inspect_java_password(&source, &password).unwrap();
    write_registries("Fixture GGUF");
    let preview = inspect();
    let (_, entries, omissions) = preview.java_review().unwrap();
    assert!(
        entries
            .iter()
            .any(|line| line.contains("Local model suggestions requiring review (2)"))
    );
    assert!(omissions.iter().any(|line| {
        line.contains("Invalid, ambiguous or excess Java model registrations omitted (2)")
    }));
    assert_eq!(preview.entries().collect::<Vec<_>>(), [ai_backup::SETTINGS]);
    super::super::queue_restore_at(&destination, preview, RestoreMode::JavaAdditive).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&live).unwrap()).unwrap(),
        baseline
    );
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    let bytes = fs::read(&live).unwrap();
    let portable = ai_backup::portable_settings(&bytes).unwrap();
    for candidate in [bytes.as_slice(), portable.as_slice()] {
        let text = std::str::from_utf8(candidate).unwrap();
        assert!(!text.contains("/NoAccess/"));
        assert!(!text.contains("fixture-secret-do-not-save"));
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(value["general"]["timeout_minutes"], 12);
        assert_eq!(value["local"], baseline["local"]);
        assert_eq!(value["profiles"], baseline["profiles"]);
        let hints = value["java_model_hints"].as_array().unwrap();
        assert_eq!(hints.len(), 2);
        assert_eq!(
            hints[0],
            serde_json::json!({
                "id": "same-model", "name": "Fixture GGUF", "published_at": "2025-01-02T03:04:05Z",
                "metadata": {"kind": "gguf", "backend": "vulkan", "purpose": "embedding", "context":4096, "threads":8, "gpu_layers":5, "idle_minutes":0}
            })
        );
        assert_eq!(
            hints[1],
            serde_json::json!({
                "id":"same-model", "name":"Fixture MLX",
                "metadata":{"kind":"mlx", "context":32768, "idle_minutes":30,"quantization":"Q4_0"}
            })
        );
    }
    assert!(
        Configuration::load_from(&destination)
            .unwrap()
            .default_ai_profile_reference()
            .is_none()
    );
    assert!(
        crate::local_ai::managed_models::Registry::new(destination.clone())
            .list()
            .unwrap()
            .is_empty()
    );
    assert!(
        crate::local_ai::mlx_models::Registry::new(&destination)
            .list()
            .unwrap()
            .is_empty()
    );

    write_registries("Conflicting GGUF name");
    super::super::queue_restore_at(&destination, inspect(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    assert_eq!(fs::read(live).unwrap(), bytes);
}

#[test]
fn java_knowledge_registry_replays_as_inactive_additive_suggestions_without_profiles() {
    let scratch = Scratch::new();
    let source = scratch.0.join("java-knowledge.zip");
    let destination = scratch.0.join("profile");
    let password = Secret::new("fixture".into());
    let options = SimpleFileOptions::default()
        .with_aes_encryption_bytes(AesMode::Aes256, password.as_str().as_bytes());
    let write_backup = |registry: &[u8]| {
        let mut zip = ZipWriter::new(File::create(&source).unwrap());
        zip.start_file(crate::ai_manager::java_knowledge::MEMBER, options)
            .unwrap();
        zip.write_all(registry).unwrap();
        zip.finish().unwrap();
    };
    let baseline = serde_json::to_value(Configuration::default()).unwrap();
    let live = destination.join(ai_backup::SETTINGS);
    write_backup(crate::ai_manager::java_knowledge::FIXTURE);
    let inspect = || super::super::inspect_java_password(&source, &password).unwrap();
    let preview = inspect();
    assert_eq!(preview.entries().collect::<Vec<_>>(), [ai_backup::SETTINGS]);
    let (_, entries, _) = preview.java_review().unwrap();
    assert!(entries.iter().any(|entry| {
        entry.contains("Knowledge store suggestions requiring review (2 stores, 2 sources)")
    }));
    super::super::queue_restore_at(&destination, preview, RestoreMode::JavaAdditive).unwrap();
    assert!(!live.exists());
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    let installed = fs::read(&live).unwrap();
    let retained = ai_backup::portable_settings(&installed).unwrap();
    for contents in [installed.as_slice(), retained.as_slice()] {
        let text = std::str::from_utf8(contents).unwrap();
        for omitted in [
            "/archive/rag-private-index",
            r"C:\\archive\\private\\source-tree",
            "/archive/private/source.txt",
            "/archive/foreign-document",
            "path-to-omit",
            "omitted-api-key",
            "foreign-digest",
            "indexedChunks",
        ] {
            assert!(
                !text.contains(omitted),
                "unexpected archive detail: {omitted}"
            );
        }
        let settings: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(settings["local"], baseline["local"]);
        assert_eq!(settings["profiles"], baseline["profiles"]);
        let stores = settings["java_knowledge_hints"].as_array().unwrap();
        assert_eq!(stores.len(), 2);
        assert_eq!(stores[0]["id"], "local-store");
        assert_eq!(stores[0]["embedding_model_id"], "embed-model");
        assert_eq!(stores[0]["autonomous_enabled"], true);
        assert_eq!(stores[0]["sources"][0]["basename"], "source-tree");
        assert_eq!(stores[0]["sources"][0]["sync"], "AUTOMATIC");
        assert_eq!(
            stores[0]["sources"][0]["includes"],
            serde_json::json!(["**/*.md"])
        );
        assert_eq!(stores[1]["qdrant_origin"], "https://rag.example:6334");
        assert_eq!(stores[1]["collection"], "java_collection");
        assert_eq!(stores[1]["sources"][0]["id"], "tree");
        assert_eq!(stores[1]["sources"][0]["basename"], "source.txt");
    }
    let changed = std::str::from_utf8(crate::ai_manager::java_knowledge::FIXTURE)
        .unwrap()
        .replace("Project notes", "Conflicting imported store");
    write_backup(changed.as_bytes());
    super::super::queue_restore_at(&destination, inspect(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    assert_eq!(fs::read(live).unwrap(), installed);
}
