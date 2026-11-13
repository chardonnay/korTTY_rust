use std::fs;
use std::path::PathBuf;

use kortty_ssh::ConnectOptions;

use super::*;
use crate::clipboard_policy::PasteWarningMode;
use crate::connection_store::import::{Format as ImportFormat, Preview};
use crate::connection_store::{Auth, SavedLogin, Store, TunnelConfig, TunnelRule};
use crate::session_security::IsolationLevel;
use crate::ssh_dialog::LoginTarget;
use crate::terminal_effects::{Effect, Selection, Speed};

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
            effect: Effect::TypewriterNoir,
            speed: Speed::try_from(2.5).unwrap(),
        }),
        command_timestamps: None,
        tab_color: None,
        paste_warning: Some(PasteWarningMode::Always),
        window_geometry: None,
        terminal: Default::default(),
    }
}

#[test]
fn custom_theme_export_keeps_the_library_id_and_field_inheritance() {
    use crate::connection_store::ThemeOverride;
    use crate::terminal_theme::{Rgb, Scheme, java};

    let mut themes = Themes::default();
    let scheme = Scheme {
        foreground: Rgb::from_hex(0x123456),
        background: Rgb::from_hex(0xAABBCC),
        ..Scheme::default()
    };
    let id = themes.store(None, "Portable theme", scheme).unwrap();
    let java_id = themes.java_id(id).unwrap();
    let library = java::encode(&themes).unwrap();
    assert!(
        String::from_utf8(library)
            .unwrap()
            .contains(&format!("<theme id=\"{java_id}\">"))
    );

    let mut connection = fixture();
    connection.terminal.theme = Some(ThemeOverride::from(id));
    connection.terminal.foreground = Some(Rgb::from_hex(0xFEDCBA));
    let entries = [Entry {
        connection: &connection,
        group: "",
        os_badge: None,
    }];
    let export = |themes: &Themes| {
        let bytes = encode_profiles(
            Format::Kortty,
            &entries,
            &[],
            &[],
            Options::default(),
            Some(themes),
        )
        .unwrap();
        String::from_utf8(bytes).unwrap()
    };
    let xml = export(&themes);
    assert!(xml.contains(&format!("<themeId>{java_id}</themeId>")));
    assert!(xml.contains("<foregroundColor>#FEDCBA</foregroundColor>"));
    assert!(!xml.contains("<backgroundColor>"));
    assert!(!xml.contains("<fontFamily>"));
}

#[test]
fn saved_window_geometry_stays_out_of_portable_connection_export() {
    let scratch = Scratch::new();
    let mut connection = fixture();
    connection.window_geometry = Some(crate::connection_store::WindowGeometry::default());
    let entries = [Entry {
        connection: &connection,
        group: "",
        os_badge: None,
    }];
    for format in Format::ALL {
        let preview = scratch.preview(format, &entries);
        let (imported, _) = preview.entries().next().unwrap();
        assert_eq!(imported.window_geometry, None, "{format:?}");
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
    assert!(text.contains("<terminalEffectPluginId>typewriter-noir</terminalEffectPluginId>"));
    assert!(text.contains("<terminalEffectAnimationSpeed>2.5</terminalEffectAnimationSpeed>"));
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
fn local_shell_integration_xml_round_trip_retains_opt_in_and_windows_presets() {
    let scratch = Scratch::new();
    let mut profiles = Vec::new();
    for (index, (shell, command)) in [
        (kortty_pty::LocalShell::Default, None),
        (
            kortty_pty::LocalShell::Default,
            Some(vec![
                r"C:\Program Files\Git\bin\bash.exe".into(),
                "--login".into(),
                "-i".into(),
            ]),
        ),
        (kortty_pty::LocalShell::PowerShell, None),
        (kortty_pty::LocalShell::CommandPrompt, None),
    ]
    .into_iter()
    .enumerate()
    {
        profiles.push(LocalProfile {
            id: index as u64 + 1,
            name: format!("Integrated shell {index}"),
            shell,
            command,
            working_directory: None,
            shell_integration: true,
            emulation: "xterm-256color".into(),
            terminal: Default::default(),
        });
    }
    let locals = profiles
        .iter()
        .map(|profile| LocalEntry {
            profile,
            group: "Shells",
        })
        .collect::<Vec<_>>();
    let data =
        encode_profiles(Format::Kortty, &[], &locals, &[], Options::default(), None).unwrap();
    let xml = str::from_utf8(&data).unwrap();
    assert_eq!(
        xml.matches("<shellIntegrationAutoInject>true</shellIntegrationAutoInject>")
            .count(),
        2
    );
    assert_eq!(
        xml.matches("<rustLocalShellIntegration>true</rustLocalShellIntegration>")
            .count(),
        2
    );
    let path = scratch.0.join("integrated.xml");
    fs::write(&path, data).unwrap();
    let preview = Preview::read(&path).unwrap();
    assert_eq!(preview.invalid, 0);
    let saved_path = scratch.0.join("saved.json");
    let mut store = Store::open(saved_path.clone()).unwrap();
    assert_eq!(preview.apply(&mut store).unwrap().imported, profiles.len());
    drop(store);
    let saved = Store::open(saved_path).unwrap();
    for (actual, expected) in saved.local_profiles().iter().zip(&profiles) {
        assert_eq!(actual.shell, expected.shell);
        assert_eq!(actual.command, expected.command);
        assert!(actual.shell_integration);
    }
}

#[test]
fn local_shell_xml_round_trip_keeps_java_compatible_and_exact_rust_launches() {
    let scratch = Scratch::new();
    let directory = scratch.0.join("local project");
    let java = LocalProfile {
        id: 5,
        name: "Java-compatible local profile".into(),
        shell: Default::default(),
        command: Some(vec![
            directory.join("program.exe").to_str().unwrap().into(),
            "two words".into(),
        ]),
        working_directory: Some(directory),
        shell_integration: false,
        emulation: "vt100".into(),
        terminal: crate::connection_store::TerminalOverrides {
            font_size: Some(22),
            terminal_columns: Some(132),
            terminal_rows: Some(43),
            paste_line_delay_ms: Some(0),
            strict_terminal_mode: Some(true),
            ..Default::default()
        },
    };
    let mut rust = java.clone();
    rust.id = 6;
    rust.name = "Exact native arguments".into();
    rust.command
        .as_mut()
        .unwrap()
        .extend([String::new(), "日本語 \"quoted\"\r\n".into()]);
    let locals = [
        LocalEntry {
            profile: &java,
            group: "Development",
        },
        LocalEntry {
            profile: &rust,
            group: "Development",
        },
    ];
    let ssh = fixture();
    let entries = [Entry {
        connection: &ssh,
        group: "Remote",
        os_badge: None,
    }];
    let data = encode_profiles(
        Format::Kortty,
        &entries,
        &locals,
        &[],
        Options::default(),
        None,
    )
    .unwrap();
    let xml = str::from_utf8(&data).unwrap();
    assert_eq!(xml.matches("<protocol>LOCAL_SHELL</protocol>").count(), 1);
    assert_eq!(xml.matches("<rustLocalConnection>").count(), 1);
    assert!(xml.contains("<localShellWorkingDirectory>"));
    assert_eq!(
        xml.matches("<terminalColumns>132</terminalColumns>")
            .count(),
        2
    );
    assert_eq!(xml.matches("<terminalRows>43</terminalRows>").count(), 2);
    assert!(
        encode_profiles(
            Format::Mtputty,
            &entries,
            &locals,
            &[],
            Options::default(),
            None
        )
        .is_err()
    );

    let path = scratch.0.join("connections.xml");
    fs::write(&path, data).unwrap();
    let preview = Preview::read(&path).unwrap();
    let store_path = scratch.0.join("connections.json");
    let mut store = Store::open(store_path.clone()).unwrap();
    assert_eq!(preview.invalid, 0);
    assert_eq!(preview.apply(&mut store).unwrap().imported, 3);
    let mut store = Store::open(store_path).unwrap();
    assert_eq!(store.connections().len(), 1);
    assert_eq!(store.local_profiles().len(), 2);
    for (saved, mut expected) in store.local_profiles().iter().zip([java, rust]) {
        expected.id = saved.id;
        assert_eq!(saved, &expected);
        assert_eq!(store.group_of(saved.id), Some("Development"));
    }
    assert_eq!(preview.apply(&mut store).unwrap().already_saved, 3);
}

#[test]
fn java_serial_xml_round_trips_device_line_settings_encoding_and_folder() {
    use crate::session::{
        SerialConnection, SerialFlowControl, SerialLineEnding, SerialParity, SerialSettings,
        SerialStopBits,
    };
    use kortty_terminal::{TerminalProfile, TextEncoding};
    let scratch = Scratch::new();
    let mut profile = SerialProfile::new(
        "Console & status",
        SerialConnection {
            device: "/dev/cu.usbserial-1410".into(),
            settings: SerialSettings {
                baud_rate: 230_400,
                data_bits: 5,
                parity: SerialParity::Mark,
                stop_bits: SerialStopBits::OnePointFive,
                flow_control: SerialFlowControl::DsrDtr,
                assert_dtr: false,
                assert_rts: false,
            },
            line_ending: SerialLineEnding::CrLf,
            local_echo: true,
            encoding: Some(TextEncoding::Windows1252),
        },
        TerminalProfile::Vt220,
    );
    profile.tag = "Lab <bench>".into();
    profile.paste_line_delay_ms = Some(125);
    profile.strict_terminal_mode = Some(false);
    let serials = [SerialEntry {
        profile: &profile,
        group: "Hardware/Lab",
    }];
    let xml =
        encode_profiles(Format::Kortty, &[], &[], &serials, Options::default(), None).unwrap();
    let text = str::from_utf8(&xml).unwrap();
    for expected in [
        "<protocol>SERIAL</protocol>",
        "<serial>",
        "<stopBits>ONE_POINT_FIVE</stopBits>",
        "<flowControl>DSR_DTR</flowControl>",
        "<lineEnding>CRLF</lineEnding>",
        "Lab &lt;bench&gt;",
    ] {
        assert!(text.contains(expected), "missing {expected}");
    }
    assert!(
        encode_profiles(
            Format::PuttyCm,
            &[],
            &[],
            &serials,
            Options::default(),
            None
        )
        .is_err()
    );
    let path = scratch.0.join("serial.xml");
    fs::write(&path, xml).unwrap();
    let preview = Preview::read(&path).unwrap();
    assert_eq!(preview.invalid, 0);
    let mut store = Store::open(scratch.0.join("connections.json")).unwrap();
    assert_eq!(preview.apply(&mut store).unwrap().imported, 1);
    let restored = &store.serial_profiles()[0];
    profile.id = restored.id;
    assert_eq!(restored, &profile);
    assert_eq!(store.group_of(restored.id), Some("Hardware/Lab"));
    assert_eq!(preview.apply(&mut store).unwrap().already_saved, 1);
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
    assert!(
        xml::kortty(&entries, &[], &[], Options::default(), None)
            .unwrap()
            .len()
            < 1024 * 1024
    );
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
fn java_terminal_settings_preserve_explicit_overrides_and_omitted_defaults() {
    use crate::connection_store::ThemeOverride;
    use crate::terminal_theme::{BoldTextMode, CursorStyle, Preset, Rgb};
    use kortty_terminal::TextEncoding;

    let scratch = Scratch::new();
    let file = scratch.0.join("java-terminal.xml");
    fs::write(
        &file,
        concat!(
            "<connections schemaVersion=\"2\"><connection><host>server.example</host>",
            "<username>operator</username><settings><useGlobalSettings>false</useGlobalSettings>",
            "<themeId>nord</themeId><fontFamily>Courier New</fontFamily><fontSize>18</fontSize>",
            "<foregroundColor>#AA0011</foregroundColor><cursorStyle>STEADY_VERTICAL_BAR</cursorStyle>",
            "<ansiPaletteCustomized>true</ansiPaletteCustomized><ansiRed>#121212</ansiRed>",
            "<selectionColor>#234567</selectionColor><boldTextMode>BRIGHT_COLOR</boldTextMode>",
            "<terminalColorsEnabled>false</terminalColorsEnabled><encoding>ISO-8859-15</encoding>",
            "<terminalColumns>500</terminalColumns><terminalRows>10</terminalRows>",
            "<scrollbackLines>100000</scrollbackLines><scrollbackMemoryMb>50</scrollbackMemoryMb>",
            "<scrollbackCompression>false</scrollbackCompression>",
            "<closeWithoutConfirmation>false</closeWithoutConfirmation>",
            "</settings></connection></connections>",
        ),
    )
    .unwrap();
    let preview = Preview::read(&file).unwrap();
    let (connection, group) = preview.entries().next().unwrap();
    let terminal = &connection.terminal;
    assert_eq!(terminal.theme, Some(ThemeOverride::BuiltIn(Preset::Nord)));
    assert_eq!(terminal.font_family.as_deref(), Some("Courier New"));
    assert_eq!(terminal.font_size, Some(18));
    assert_eq!(terminal.foreground, Some(Rgb::from_hex(0xAA0011)));
    assert_eq!(terminal.background, None);
    assert_eq!(terminal.cursor_style, Some(CursorStyle::SteadyBar));
    assert_eq!(terminal.ansi[1], Some(Rgb::from_hex(0x121212)));
    assert_eq!(terminal.selection, Some(Rgb::from_hex(0x234567)));
    assert_eq!(terminal.bold, Some(BoldTextMode::BrightColor));
    assert_eq!(terminal.sgr_enabled, Some(false));
    assert_eq!(terminal.encoding, Some(TextEncoding::Latin9));
    assert_eq!(terminal.terminal_columns, Some(500));
    assert_eq!(terminal.terminal_rows, Some(10));
    assert_eq!(terminal.scrollback_lines, Some(100_000));
    assert_eq!(terminal.scrollback_memory_mib, Some(50));
    assert_eq!(terminal.scrollback_compression, Some(false));
    assert_eq!(terminal.confirm_close, Some(true));
    let reimport = scratch.preview(
        Format::Kortty,
        &[Entry {
            connection,
            group,
            os_badge: None,
        }],
    );
    assert_eq!(&reimport.entries().next().unwrap().0.terminal, terminal);
}

#[test]
fn java_unknown_theme_id_and_fallback_colors_round_trip_exactly() {
    use crate::connection_store::ThemeOverride;
    use crate::terminal_theme::{Rgb, Themes};

    let scratch = Scratch::new();
    let file = scratch.0.join("java-theme.xml");
    let id = "D627EFC2-c70f-4214-9620-bda63766a80b";
    fs::write(
        &file,
        format!(
            "<connections schemaVersion=\"2\"><connection><host>server.example</host>\
         <username>operator</username><settings><useGlobalSettings>false</useGlobalSettings>\
         <themeId>{id}</themeId><foregroundColor>#F0E1C2</foregroundColor>\
         <backgroundColor>#102030</backgroundColor></settings></connection></connections>",
        ),
    )
    .unwrap();
    let preview = Preview::read(&file).unwrap();
    let (connection, group) = preview.entries().next().unwrap();
    let theme = &connection.terminal;
    assert_eq!(theme.theme, Some(ThemeOverride::Java { java: id.into() }));
    assert!(theme.scheme(&Themes::default()).is_none());
    assert_eq!(theme.foreground, Some(Rgb::from_hex(0xF0E1C2)));
    assert_eq!(theme.background, Some(Rgb::from_hex(0x102030)));
    let reimport = scratch.preview(
        Format::Kortty,
        &[Entry {
            connection,
            group,
            os_badge: None,
        }],
    );
    assert_eq!(&reimport.entries().next().unwrap().0.terminal, theme);

    let mut native = connection.clone();
    native.terminal.theme = Some(ThemeOverride::Custom { custom: 7 });
    let reimport = scratch.preview(
        Format::Kortty,
        &[Entry {
            connection: &native,
            group,
            os_badge: None,
        }],
    );
    native.terminal.theme = None;
    assert_eq!(
        &reimport.entries().next().unwrap().0.terminal,
        &native.terminal
    );
}

#[test]
fn java_initial_grid_can_override_one_dimension_while_appearance_inherits() {
    let scratch = Scratch::new();
    let file = scratch.0.join("java-initial-grid.xml");
    fs::write(
        &file,
        concat!(
            "<connections schemaVersion=\"2\"><connection><host>server.example</host>",
            "<username>operator</username><settings><useGlobalSettings>true</useGlobalSettings>",
            "<fontSize>18</fontSize><terminalColumns>40</terminalColumns>",
            "</settings></connection></connections>",
        ),
    )
    .unwrap();
    let preview = Preview::read(&file).unwrap();
    assert_eq!(preview.invalid, 0);
    let (connection, group) = preview.entries().next().unwrap();
    assert_eq!(connection.terminal.terminal_columns, Some(40));
    assert_eq!(connection.terminal.terminal_rows, None);
    assert_eq!(connection.terminal.font_size, None);

    let entries = [Entry {
        connection,
        group,
        os_badge: None,
    }];
    let xml = String::from_utf8(encode(Format::Kortty, &entries).unwrap()).unwrap();
    assert!(xml.contains("<useGlobalSettings>true</useGlobalSettings>"));
    assert!(xml.contains("<terminalColumns>40</terminalColumns>"));
    assert!(!xml.contains("<terminalRows>"));
    let reimport = scratch.preview(Format::Kortty, &entries);
    assert_eq!(
        reimport.entries().next().unwrap().0.terminal,
        connection.terminal
    );
}

#[test]
fn java_initial_grid_rejects_out_of_range_dimensions() {
    let scratch = Scratch::new();
    let file = scratch.0.join("java-invalid-grid.xml");
    for (field, value) in [
        ("terminalColumns", 39),
        ("terminalColumns", 501),
        ("terminalRows", 9),
        ("terminalRows", 201),
    ] {
        fs::write(
            &file,
            format!(
                "<connections schemaVersion=\"2\"><connection><host>server.example</host>\
                 <username>operator</username><settings><useGlobalSettings>true</useGlobalSettings>\
                 <{field}>{value}</{field}></settings></connection></connections>"
            ),
        )
        .unwrap();
        let preview = Preview::read(&file).unwrap();
        assert_eq!(preview.invalid, 1, "{field}={value}");
        assert!(preview.entries().next().is_none());
    }
}

#[test]
fn java_scrollback_memory_repairs_explicit_legacy_values_and_preserves_inheritance() {
    let scratch = Scratch::new();
    let file = scratch.0.join("java-scrollback.xml");
    for (schema, uses_global, value, expected) in [
        (
            2,
            false,
            "<scrollbackMemoryMb>-1</scrollbackMemoryMb>",
            Some(10),
        ),
        (
            2,
            false,
            "<scrollbackMemoryMb>0</scrollbackMemoryMb>",
            Some(10),
        ),
        (
            2,
            false,
            "<scrollbackMemoryMb>500</scrollbackMemoryMb>",
            Some(50),
        ),
        (2, false, "", None),
        (2, true, "<scrollbackMemoryMb>20</scrollbackMemoryMb>", None),
        (
            1,
            false,
            "<scrollbackMemoryMb>20</scrollbackMemoryMb>",
            None,
        ),
    ] {
        fs::write(
            &file,
            format!(
                "<connections schemaVersion=\"{schema}\"><connection><host>server.example</host>\
                 <username>operator</username><settings><useGlobalSettings>{uses_global}</useGlobalSettings>\
                 {value}</settings></connection></connections>"
            ),
        )
        .unwrap();
        let preview = Preview::read(&file).unwrap();
        let (connection, _) = preview.entries().next().unwrap();
        assert_eq!(connection.terminal.scrollback_memory_mib, expected);
        assert_eq!(connection.terminal.scrollback_lines, None);
        assert_eq!(connection.terminal.scrollback_compression, None);
    }
}

#[test]
fn java_ssh_keepalive_uses_transport_fields_independent_of_terminal_inheritance() {
    use kortty_ssh::KeepAlive;

    let scratch = Scratch::new();
    let file = scratch.0.join("java-ssh-keepalive.xml");
    for (schema, uses_global, fields, expected) in [
        (2, false, "", None),
        (
            1,
            true,
            "<sshKeepAliveEnabled>false</sshKeepAliveEnabled>",
            Some((false, 60)),
        ),
        (
            2,
            true,
            "<sshKeepAliveInterval>-10</sshKeepAliveInterval>",
            Some((true, 5)),
        ),
        (
            2,
            false,
            "<sshKeepAliveInterval>900</sshKeepAliveInterval>",
            Some((true, 600)),
        ),
    ] {
        fs::write(&file, format!(
            "<connections schemaVersion=\"{schema}\"><connection><host>server.example</host>\
             <username>operator</username><settings><useGlobalSettings>{uses_global}</useGlobalSettings>\
             {fields}</settings></connection></connections>"
        )).unwrap();
        let preview = Preview::read(&file).unwrap();
        let (connection, group) = preview.entries().next().unwrap();
        let expected = expected.map(|(enabled, seconds)| KeepAlive::new(enabled, seconds).unwrap());
        assert_eq!(connection.startup.keepalive(), expected);
        assert!(connection.terminal.is_default());
        let exported = scratch.preview(
            Format::Kortty,
            &[Entry {
                connection,
                group,
                os_badge: None,
            }],
        );
        let (imported, _) = exported.entries().next().unwrap();
        assert_eq!(imported.startup.keepalive(), expected);
        assert!(imported.terminal.is_default());
    }
}

#[test]
fn java_terminal_input_overrides_preserve_explicit_zero_false_and_inheritance() {
    let scratch = Scratch::new();
    let file = scratch.0.join("java-terminal-input.xml");
    for (fields, expected) in [
        ("", (None, None)),
        (
            "<pasteLineDelayMs>0</pasteLineDelayMs><strictTerminalMode>false</strictTerminalMode>",
            (Some(0), Some(false)),
        ),
        (
            "<pasteLineDelayMs>2000</pasteLineDelayMs><strictTerminalMode>true</strictTerminalMode>",
            (Some(1000), Some(true)),
        ),
        ("<pasteLineDelayMs>-10</pasteLineDelayMs>", (Some(0), None)),
    ] {
        fs::write(&file, format!(
            "<connections schemaVersion=\"2\"><connection><host>server.example</host>\
             <username>operator</username>{fields}<settings><useGlobalSettings>true</useGlobalSettings>\
             </settings></connection></connections>"
        )).unwrap();
        let preview = Preview::read(&file).unwrap();
        let (connection, group) = preview.entries().next().unwrap();
        assert_eq!(
            (
                connection.terminal.paste_line_delay_ms,
                connection.terminal.strict_terminal_mode
            ),
            expected
        );
        let exported = scratch.preview(
            Format::Kortty,
            &[Entry {
                connection,
                group,
                os_badge: None,
            }],
        );
        let (imported, _) = exported.entries().next().unwrap();
        assert_eq!(imported.terminal, connection.terminal);
    }
}

#[test]
fn java_agent_disable_round_trips_independently_of_terminal_presentation() {
    let scratch = Scratch::new();
    let file = scratch.0.join("java-agent-disabled.xml");
    for settings in [
        "",
        "<settings><useGlobalSettings>true</useGlobalSettings></settings>",
        "<settings><useGlobalSettings>false</useGlobalSettings><fontSize>18</fontSize></settings>",
    ] {
        fs::write(
            &file,
            format!(
                "<connections schemaVersion=\"2\"><connection><host>server.example</host>\
                 <username>operator</username><aiAgentDisabled>true</aiAgentDisabled>\
                 {settings}</connection></connections>"
            ),
        )
        .unwrap();
        let preview = Preview::read(&file).unwrap();
        let (connection, group) = preview.entries().next().unwrap();
        assert!(connection.terminal.ai_agent_disabled);
        let reimport = scratch.preview(
            Format::Kortty,
            &[Entry {
                connection,
                group,
                os_badge: None,
            }],
        );
        assert!(
            reimport
                .entries()
                .next()
                .unwrap()
                .0
                .terminal
                .ai_agent_disabled
        );
    }
}

#[test]
fn java_export_omits_machine_local_agent_design() {
    let mut connection = fixture();
    let export = |connection: &Connection| {
        encode(
            Format::Kortty,
            &[Entry {
                connection,
                group: "",
                os_badge: None,
            }],
        )
        .unwrap()
    };
    let baseline = export(&connection);
    connection.terminal.ai_agent_design = Some("floating".into());
    assert_eq!(export(&connection), baseline);
}

#[test]
fn java_highlight_selection_round_trips_with_inherited_terminal_settings() {
    let scratch = Scratch::new();
    let file = scratch.0.join("java-highlighting.xml");
    for settings in [
        "",
        "<settings><useGlobalSettings>true</useGlobalSettings></settings>",
    ] {
        fs::write(
            &file,
            format!(
                "<connections schemaVersion=\"2\"><connection><host>server.example</host>\
             <username>operator</username><highlightRuleSetId>builtin.errors</highlightRuleSetId>\
             {settings}</connection></connections>"
            ),
        )
        .unwrap();
        let imported = Preview::read(&file).unwrap();
        let (connection, group) = imported.entries().next().unwrap();
        assert_eq!(
            connection.terminal.highlight_set.as_deref(),
            Some("builtin.errors")
        );
        assert!(connection.terminal.font_size.is_none());
        let entries = [Entry {
            connection,
            group,
            os_badge: None,
        }];
        let xml = encode(Format::Kortty, &entries).unwrap();
        assert!(!std::str::from_utf8(&xml).unwrap().contains("<settings>"));
        let exported = scratch.preview(Format::Kortty, &entries);
        assert_eq!(
            exported
                .entries()
                .next()
                .unwrap()
                .0
                .terminal
                .highlight_set
                .as_deref(),
            Some("builtin.errors")
        );
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
