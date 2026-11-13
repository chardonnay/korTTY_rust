use std::fs;
use std::path::PathBuf;

use crate::connection_store::{TabColor, TunnelRule};
use crate::ssh_dialog::LoginTarget;

use super::*;

struct TestProfile(PathBuf);

impl TestProfile {
    fn new() -> Self {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!(
            "kortty-overview-test-{:016x}",
            u64::from_ne_bytes(nonce)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn store(&self) -> Store {
        Store::open(self.0.join("connections.json")).unwrap()
    }
}

impl Drop for TestProfile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn login(host: &str, auth: Auth) -> SavedLogin {
    SavedLogin {
        endpoint: LoginTarget {
            host: host.into(),
            port: 22,
            username: "operator".into(),
        },
        auth,
        keyboard_interactive: false,
    }
}

fn connection(name: &str, auth: Auth) -> Connection {
    Connection {
        id: 0,
        name: name.into(),
        tag: "on".into(),
        login: login("example.test", auth),
        jump: None,
        emulation: "xterm-256color".into(),
        tunnels: Vec::new(),
        incognito: false,
        isolation: IsolationLevel::None,
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

fn tunnel(enabled: bool, target: TunnelTarget) -> TunnelConfig {
    TunnelConfig {
        enabled,
        description: if enabled { "forward" } else { "disabled" }.into(),
        rule: TunnelRule {
            bind: "127.0.0.1:18080".parse().unwrap(),
            target,
        },
    }
}

#[test]
fn inventory_allowlist_keeps_only_safe_references_in_every_format() {
    let profile = TestProfile::new();
    let mut store = profile.store();
    let group = store.create_group(None, "Production").unwrap();
    let mut selected = connection(
        "Web <1>",
        Auth::KeyFile {
            path: profile.0.join("id-example"),
            passphrase: Some("test-secret-passphrase-reference".into()),
        },
    );
    selected.jump = Some(login(
        "bastion.example.test",
        Auth::VaultPassword("test-secret-jump-reference".into()),
    ));
    selected.tab_color = TabColor::parse("#123");
    selected.tunnels = vec![
        tunnel(
            true,
            TunnelTarget::Remote {
                host: "service.example.test".into(),
                port: 443,
            },
        ),
        tunnel(true, TunnelTarget::Dynamic),
        tunnel(
            false,
            TunnelTarget::Local {
                host: "disabled.example.test".into(),
                port: 6443,
            },
        ),
    ];
    let selected = store.save_in_group(selected, Some(&group)).unwrap();
    store.set_os_badge_id(selected, Some("rocky")).unwrap();
    assert!(store.toggle_connection_favorites(&[selected], &[]).unwrap());
    store.record_used_at(selected, 1_700_000_000_000).unwrap();
    let vault = store
        .save(connection("Vault", Auth::VaultPassword("entry".into())))
        .unwrap();
    let key = store.save(connection("Key", Auth::StoredKey(17))).unwrap();
    let snapshot = Snapshot::capture(
        &store,
        &Catalog::empty(&profile.0),
        |id| (id == "entry").then(|| "Office login".into()),
        |id| (id == 17).then(|| "Registered key".into()),
    );
    let parts = Part::ALL.into_iter().collect();
    let ids = [vault, u64::MAX, selected, key];
    for format in Format::ALL {
        let rendered = snapshot.render(&ids, &parts, format).unwrap();
        assert!(rendered.contains("Office login"));
        assert!(rendered.contains("Registered key"));
        assert!(rendered.contains("id-example"));
        assert!(!rendered.contains("test-secret-"));
        assert!(!rendered.contains("disabled.example.test"));
        assert!(!rendered.contains("usageCount"));
    }

    let rendered = snapshot.render(&ids, &parts, Format::Json).unwrap();
    let document: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    let entries = document["connections"].as_array().unwrap();
    assert_eq!(document["count"], 3);
    assert_eq!(entries[0]["name"], "Vault");
    assert_eq!(entries[1]["name"], "Web <1>");
    assert_eq!(entries[1]["protocol"], "sshTcp");
    assert_eq!(entries[1]["folder"], "Production");
    assert_eq!(entries[1]["osBadge"], "Rocky Linux");
    assert_eq!(entries[1]["tabColor"], "#112233");
    assert_eq!(entries[1]["favorite"], true);
    assert_eq!(entries[1]["jumpServer"]["method"], "password");
    assert_eq!(
        entries[1]["tunnels"][0]["localHost"],
        "service.example.test"
    );
    assert_eq!(entries[1]["tunnels"][0]["remotePort"], 18080);
    assert!(entries[1]["tunnels"][1].get("remotePort").is_none());
    assert_eq!(entries[1]["terminal"]["retryCount"], 1);
    assert_eq!(entries[1]["usage"]["lastUsed"], "2023-11-14T22:13:20Z");
    assert_eq!(entries[2]["name"], "Key");

    let basics = snapshot
        .render(&[selected], &BTreeSet::new(), Format::Json)
        .unwrap();
    let document: serde_json::Value = serde_json::from_str(&basics).unwrap();
    assert_eq!(document["connections"][0].as_object().unwrap().len(), 5);
}

#[test]
fn rendering_preserves_escaping_and_repeated_sections() {
    let profile = TestProfile::new();
    let store = profile.store();
    let badges = Catalog::empty(&profile.0);
    let text = "1: <&> \" \\\nline\r\t\u{2028} 😺";
    let mut first = connection(text, Auth::Prompt);
    first.tunnels = vec![
        tunnel(true, TunnelTarget::Dynamic),
        tunnel(true, TunnelTarget::Dynamic),
    ];
    let second = connection("yes", Auth::Prompt);
    let snapshot = Snapshot {
        exported_at: Timestamp::UNIX_EPOCH,
        entries: BTreeMap::from([
            (
                1,
                Entry::capture(&first, &store, &badges, &|_| None, &|_| None, None),
            ),
            (
                2,
                Entry::capture(&second, &store, &badges, &|_| None, &|_| None, None),
            ),
        ]),
    };
    let parts = Part::defaults();
    let json = snapshot.render(&[1, 2], &parts, Format::Json).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(doc["connections"][0]["name"], text);

    let yaml = snapshot.render(&[1, 2], &parts, Format::Yaml).unwrap();
    assert!(yaml.contains("exportedAt: \"1970-01-01T00:00:00Z\""));
    assert!(yaml.contains(r#"  - name: "1: <&> \" \\\nline\r\t\u2028 😺""#));
    assert!(yaml.contains("  - name: \"yes\""));
    assert_eq!(yaml.matches("      - type: dynamic\n").count(), 2);

    let toml = snapshot.render(&[1, 2], &parts, Format::Toml).unwrap();
    let doc: toml_edit::DocumentMut = toml.parse().unwrap();
    assert!(doc["exportedAt"].is_datetime());
    let entries = doc["connections"].as_array_of_tables().unwrap();
    assert_eq!(entries.get(0).unwrap()["name"].as_str(), Some(text));
    assert_eq!(
        entries.get(0).unwrap()["tunnels"]
            .as_array_of_tables()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(entries.get(1).unwrap()["name"].as_str(), Some("yes"));

    let xml = snapshot.render(&[1, 2], &parts, Format::Xml).unwrap();
    let doc = roxmltree::Document::parse(&xml).unwrap();
    assert_eq!(doc.root_element().tag_name().name(), "korttyOverview");
    assert_eq!(doc.root_element().attribute("count"), Some("2"));
    assert_eq!(
        doc.descendants()
            .find(|node| node.has_tag_name("name"))
            .unwrap()
            .text(),
        Some(text)
    );
    assert_eq!(
        doc.descendants()
            .filter(|node| node.has_tag_name("tunnel"))
            .count(),
        2
    );
}

#[test]
fn empty_inventory_is_valid_and_inline_key_material_is_never_a_path() {
    let profile = TestProfile::new();
    let store = profile.store();
    let snapshot = Snapshot::capture(&store, &Catalog::empty(&profile.0), |_| None, |_| None);
    let parts = Part::defaults();
    for (format, expected) in [
        (Format::Json, "\"connections\": []"),
        (Format::Yaml, "connections: []"),
        (Format::Toml, "connections = []"),
        (Format::Xml, "count=\"0\"/>"),
    ] {
        assert!(
            snapshot
                .render(&[], &parts, format)
                .unwrap()
                .contains(expected)
        );
    }

    let inline_key = connection(
        "Untrusted fixture",
        Auth::KeyFile {
            path: "TEMPORARY:test-inline-key".into(),
            passphrase: None,
        },
    );
    let entry = Entry::capture(
        &inline_key,
        &store,
        &Catalog::empty(&profile.0),
        &|_| None,
        &|_| None,
        None,
    );
    let snapshot = Snapshot {
        exported_at: Timestamp::UNIX_EPOCH,
        entries: BTreeMap::from([(1, entry)]),
    };
    for format in Format::ALL {
        let text = snapshot.render(&[1], &parts, format).unwrap();
        assert!(!text.contains("test-inline-key"));
        assert!(!text.contains("privateKeyPath"));
    }
}
