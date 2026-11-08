use std::fs;
use std::path::PathBuf;

use kortty_ssh::ConnectOptions;

use super::*;
use crate::clipboard_policy::PasteWarningMode;
use crate::connection_store::import::{Format as ImportFormat, Preview};
use crate::connection_store::{Auth, SavedLogin, Store, TunnelConfig, TunnelRule};
use crate::session_security::IsolationLevel;
use crate::ssh_dialog::LoginTarget;
use crate::terminal_effects::{Effect, Selection};

fn fixture() -> Connection {
    Connection {
        id: 17,
        name: "Ops, \"Berlin\" & <Staging>".into(),
        tag: "kept & escaped".into(),
        login: SavedLogin {
            endpoint: LoginTarget {
                host: "server.example".into(),
                port: 2222,
                username: "deploy".into(),
            },
            auth: Auth::VaultPassword("password-vault-reference".into()),
            keyboard_interactive: false,
        },
        jump: Some(SavedLogin {
            endpoint: LoginTarget {
                host: "jump.example".into(),
                port: 22,
                username: "operator".into(),
            },
            auth: Auth::StoredKey(46_913_002),
            keyboard_interactive: false,
        }),
        emulation: "hp700-92".into(),
        tunnels: vec![
            TunnelConfig {
                enabled: true,
                description: "API & logs".into(),
                rule: TunnelRule {
                    bind: "127.0.0.1:8181".parse().unwrap(),
                    target: TunnelTarget::Local {
                        host: "api.example".into(),
                        port: 443,
                    },
                },
            },
            TunnelConfig {
                enabled: true,
                description: String::new(),
                rule: TunnelRule {
                    bind: "[::1]:9191".parse().unwrap(),
                    target: TunnelTarget::Remote {
                        host: "localhost".into(),
                        port: 443,
                    },
                },
            },
        ],
        incognito: true,
        isolation: IsolationLevel::Process,
        startup: ConnectOptions::new(19, 4).unwrap(),
        logging: Default::default(),
        terminal_effect: Some(Selection {
            effect: Effect::AmberCrt90,
            speed: 7,
        }),
        command_timestamps: None,
        tab_color: None,
        paste_warning: Some(PasteWarningMode::Always),
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!(
            "kortty-connection-export-{:016x}",
            u64::from_ne_bytes(nonce),
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn preview(&self, format: Format, entries: &[Entry<'_>]) -> Preview {
        let path = self.0.join(format!("connections.{}", format.extension()));
        fs::write(&path, encode(format, entries).unwrap()).unwrap();
        Preview::read(&path).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn kortty_xml_round_trip_preserves_shared_settings_and_removes_credentials() {
    let original = fixture();
    let entries = [Entry {
        connection: &original,
        group: "Europe/Berlin",
        os_badge: Some("windows-11"),
    }];
    let bytes = encode(Format::Kortty, &entries).unwrap();
    let text = str::from_utf8(&bytes).unwrap();
    assert!(text.contains("<connections schemaVersion=\"2\">"));
    assert!(text.contains("<terminalEmulationType>HP700_92</terminalEmulationType>"));
    assert!(text.contains("<terminalEffectPluginId>amber-crt-90</terminalEffectPluginId>"));
    assert!(text.contains("<terminalEffectAnimationSpeed>7</terminalEffectAnimationSpeed>"));
    assert!(text.contains("<osBadge>windows-11</osBadge>"));
    assert!(text.contains("&amp; &lt;Staging&gt;"));
    for excluded in [
        "password-vault-reference",
        "46913002",
        "<encryptedPassword>",
        "<privateKeyPath>",
        "<sshKeyId>",
    ] {
        assert!(!text.contains(excluded));
    }
    let scratch = Scratch::new();
    let preview = scratch.preview(Format::Kortty, &entries);
    assert_eq!(preview.format(), ImportFormat::Kortty);
    assert_eq!(preview.invalid, 0);
    let (imported, group) = preview.entries().next().unwrap();
    assert_eq!(group, "Europe/Berlin");
    let mut expected = original;
    expected.id = 0;
    expected.login.auth = Auth::Prompt;
    expected.jump.as_mut().unwrap().auth = Auth::Configured {
        method: crate::connection_store::AuthMethod::PublicKey,
        credential: None,
        key: None,
    };
    assert_eq!(imported, &expected);

    let path = scratch.0.join("connections.json");
    let mut store = Store::open(path.clone()).unwrap();
    assert_eq!(preview.apply(&mut store).unwrap().imported, 1);
    drop(store);
    let store = Store::open(path).unwrap();
    let imported = &store.connections()[0];
    assert_eq!(store.os_badge_id(imported.id), Some("windows-11"));
    assert!(!store.approved(imported.id, &imported.plan()));
}

#[test]
fn custom_symbol_reference_survives_the_portable_xml_round_trip() {
    let connection = fixture();
    let badge = "custom:7d9cf041-710d-4b27-bc89-d46f72db96bf";
    let entries = [Entry {
        connection: &connection,
        group: "",
        os_badge: Some(badge),
    }];
    let scratch = Scratch::new();
    let preview = scratch.preview(Format::Kortty, &entries);
    let mut store = Store::open(scratch.0.join("connections.json")).unwrap();
    preview.apply(&mut store).unwrap();
    assert_eq!(store.os_badge_id(store.connections()[0].id), Some(badge));
}

#[test]
fn xml_export_rejects_documents_over_the_import_node_limit_before_writing() {
    let connections: Vec<_> = (0..200)
        .map(|index| {
            let mut connection = fixture();
            connection.id = index + 1;
            connection.name = format!("Profile {index}");
            connection.tunnels = (1080..1096)
                .map(|port| TunnelConfig {
                    enabled: true,
                    description: String::new(),
                    rule: TunnelRule {
                        bind: (std::net::Ipv4Addr::LOCALHOST, port).into(),
                        target: TunnelTarget::Dynamic,
                    },
                })
                .collect();
            connection
        })
        .collect();
    let entries: Vec<_> = connections
        .iter()
        .map(|connection| Entry {
            connection,
            group: "",
            os_badge: None,
        })
        .collect();
    assert!(xml::kortty(&entries, Options::default()).len() < 1024 * 1024);
    let error = encode(Format::Kortty, &entries).unwrap_err();
    assert!(error.to_string().contains("Select fewer connections"));
    let scratch = Scratch::new();
    assert_eq!(
        scratch
            .preview(Format::Kortty, &entries[..75])
            .entries()
            .len(),
        75
    );
}

#[test]
fn reads_java_timestamps_only_when_the_connection_uses_its_own_settings() {
    let scratch = Scratch::new();
    let file = scratch.0.join("java-settings.xml");
    for (schema, uses_global, expected) in [
        ("2", false, Some(true)),
        ("2", true, None),
        ("1", false, None),
    ] {
        fs::write(&file, format!(
            "<connections schemaVersion=\"{schema}\"><connection><host>server.example</host>\
             <username>operator</username><settings><useGlobalSettings>{uses_global}</useGlobalSettings>\
             <commandTimestampsEnabled>true</commandTimestampsEnabled>\
             </settings></connection></connections>",
        )).unwrap();
        let preview = Preview::read(&file).unwrap();
        let (entry, _) = preview.entries().next().unwrap();
        assert_eq!(entry.command_timestamps, expected);
    }
}

#[test]
fn third_party_outputs_use_the_verified_profile_and_folder_shapes() {
    let original = fixture();
    let entries = [Entry {
        connection: &original,
        group: "Europe/Berlin",
        os_badge: None,
    }];
    let scratch = Scratch::new();
    let csv = scratch.preview(Format::PuttyCm, &entries);
    let (profile, group) = csv.entries().next().unwrap();
    assert_eq!(
        (profile.name.as_str(), group),
        (original.name.as_str(), "Europe/Berlin")
    );
    assert_eq!(profile.login.endpoint, original.login.endpoint);
    assert!(profile.tunnels.iter().all(|tunnel| !tunnel.enabled));
    assert_eq!(profile.tunnels.len(), 2);

    let mtputty = scratch.preview(Format::Mtputty, &entries);
    assert_eq!(mtputty.format(), ImportFormat::Mtputty);
    let (profile, group) = mtputty.entries().next().unwrap();
    assert_eq!(
        (profile.name.as_str(), group),
        (original.name.as_str(), "Europe/Berlin")
    );
    assert_eq!(profile.login.endpoint, original.login.endpoint);

    let moba = encode(Format::Mobaxterm, &entries).unwrap();
    let moba = str::from_utf8(&moba).unwrap();
    assert!(moba.starts_with("[Bookmarks]\r\nSubRep=Europe/Berlin\r\nImgNum=41\r\n"));
    assert!(moba.contains("Ops, \"Berlin\" & <Staging>=#109#0%server.example%2222%deploy%"));
    assert!(moba.contains("# Tunnel: REMOTE [::1]:9191 -> localhost:443"));
    for format in Format::ALL {
        let bytes = encode(format, &entries).unwrap();
        assert!(
            !str::from_utf8(&bytes)
                .unwrap()
                .contains("password-vault-reference")
        );
    }
}

#[test]
fn mobaxterm_tunnel_comment_cannot_be_imported_as_a_bookmark() {
    let mut connection = fixture();
    connection.tunnels[0].description = "=#109#0%other.example%22%user%placeholder".into();
    let entries = [Entry {
        connection: &connection,
        group: "",
        os_badge: None,
    }];
    let bytes = encode(Format::Mobaxterm, &entries).unwrap();
    let text = str::from_utf8(&bytes).unwrap();
    let comment = text
        .lines()
        .find(|line| line.starts_with("# Tunnel: LOCAL"))
        .unwrap();
    assert!(comment.contains("＝#109#0%other.example%22%user%placeholder"));
    assert!(!comment.contains('='));
    assert_eq!(
        text.lines().filter(|line| line.contains("=#109#")).count(),
        1
    );
}

#[test]
fn rejects_unsafe_csv_cells_and_ambiguous_mobaxterm_delimiters() {
    let mut original = fixture();
    original.name = " =formula()".into();
    let entries = [Entry {
        connection: &original,
        group: "",
        os_badge: None,
    }];
    assert!(encode(Format::PuttyCm, &entries).is_err());
    assert!(encode(Format::Kortty, &entries).is_ok());

    original.login.endpoint.username = "name%field".into();
    let entries = [Entry {
        connection: &original,
        group: "",
        os_badge: None,
    }];
    assert!(encode(Format::Mobaxterm, &entries).is_err());
    assert!(encode(Format::Kortty, &entries).is_ok());
}

#[test]
fn privacy_options_remove_usernames_jumps_and_forwards_from_each_wire_format() {
    let connection = fixture();
    let entries = [Entry {
        connection: &connection,
        group: "",
        os_badge: None,
    }];
    let options = Options {
        include_username: false,
        include_tunnels: false,
        include_jump_server: false,
    };
    for format in Format::ALL {
        let bytes = encode_with_options(format, &entries, options).unwrap();
        let text = str::from_utf8(&bytes).unwrap();
        assert!(text.contains("server.example"));
        for value in ["deploy", "operator", "jump.example", "api.example", "8181"] {
            assert!(!text.contains(value), "{format} leaked {value}");
        }
    }
    let with_jump = encode_with_options(
        Format::Kortty,
        &entries,
        Options {
            include_jump_server: true,
            ..options
        },
    )
    .unwrap();
    let text = str::from_utf8(&with_jump).unwrap();
    assert!(text.contains("<host>jump.example</host>"));
    assert!(!text.contains("operator"));

    let scratch = Scratch::new();
    let file = scratch.0.join("without-user.xml");
    fs::write(&file, with_jump).unwrap();
    let preview = Preview::read(&file).unwrap();
    assert_eq!(preview.invalid, 0);
    let (entry, _) = preview.entries().next().unwrap();
    assert!(entry.login.endpoint.username.is_empty());
    assert!(entry.login.endpoint.validate().is_err());
    let jump = entry.jump.as_ref().unwrap();
    assert!(jump.endpoint.username.is_empty());
    assert!(jump.endpoint.validate().is_err());
}
