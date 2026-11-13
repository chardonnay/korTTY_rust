use crate::profile::projects::{Name, Store};

use super::*;

// These files were written by ProjectManager.saveProject and
// XMLConnectionRepository.saveConnections from Java 1ebbe724ee84.
const FIXTURE: &str = include_str!("fixtures/interop.kortty");
const CONNECTIONS: &str = include_str!("fixtures/connections.xml");

#[test]
fn java_local_shell_integration_survives_portable_layout_serialization() {
    let project = "<project><windows><window><tabs><tab><connectionId>local</connectionId></tab></tabs></window></windows></project>";
    for enabled in [false, true] {
        let connections = format!(
            "<connections><connection id=\"local\"><protocol>LOCAL_SHELL</protocol><shellIntegrationAutoInject>{enabled}</shellIntegrationAutoInject></connection></connections>"
        );
        let imported = parse(project, Some(&connections)).unwrap();
        let saved = serde_json::to_string(&imported.layout).unwrap();
        let reopened: WorkspaceLayout = serde_json::from_str(&saved).unwrap();
        let PaneLayout::Terminal { security, .. } = &reopened.windows[0].tabs[0] else {
            panic!("expected the imported local shell");
        };
        assert_eq!(
            security
                .as_ref()
                .is_some_and(|settings| settings.shell_integration),
            enabled
        );
        let document = roxmltree::Document::parse(&connections).unwrap();
        let connection = document
            .root_element()
            .children()
            .find(|node| node.is_element())
            .unwrap();
        let Some(PaneLayout::Terminal { security, .. }) =
            routes::catalogue_pane(connection).unwrap()
        else {
            panic!("expected an eligible local catalogue route");
        };
        assert_eq!(
            security
                .as_ref()
                .is_some_and(|settings| settings.shell_integration),
            enabled
        );
    }
}

#[test]
fn java_project_does_not_replace_a_custom_integrated_shell_with_the_system_default() {
    let project = "<project><windows><window><tabs><tab><connectionId>local</connectionId></tab><tab><localShell>true</localShell></tab></tabs></window></windows></project>";
    for launch in [
        "<localShellCommand>/bin/bash -il</localShellCommand>",
        "<localShellWorkingDirectory>/tmp/example</localShellWorkingDirectory>",
        "<connectionSource>TEAMWORK</connectionSource>",
    ] {
        let connections = format!(
            "<connections><connection id=\"local\"><protocol>LOCAL_SHELL</protocol><shellIntegrationAutoInject>true</shellIntegrationAutoInject>{launch}</connection></connections>"
        );
        let imported = parse(project, Some(&connections)).unwrap();
        assert_eq!(imported.notes.get(&Note::UnsupportedConnection), Some(&1));
        assert_eq!(imported.layout.windows[0].tabs.len(), 1);
        let document = roxmltree::Document::parse(&connections).unwrap();
        let connection = document
            .root_element()
            .children()
            .find(|node| node.is_element())
            .unwrap();
        assert!(routes::catalogue_pane(connection).unwrap().is_none());
    }
}

#[test]
fn imports_and_matches_a_known_java_serial_encoding() {
    let serial = CONNECTIONS.replace(
        "<protocol>SERIAL</protocol>",
        "<protocol>SERIAL</protocol><encoding>latin9</encoding>",
    );
    let port = if cfg!(windows) {
        "COM42"
    } else {
        "/dev/ttyUSB9"
    };
    let serial = serial.replace("/dev/ttyUSB9", port);
    let project = parse(FIXTURE, Some(&serial)).unwrap();
    let pane = project.layout.windows[1]
        .tabs
        .iter()
        .find(|pane| matches!(pane, PaneLayout::Serial { .. }))
        .unwrap();
    let PaneLayout::Serial { connection, .. } = pane else {
        unreachable!()
    };
    assert_eq!(
        connection.encoding,
        Some(kortty_terminal::TextEncoding::Latin9)
    );

    let document = roxmltree::Document::parse(&serial).unwrap();
    let source = document
        .descendants()
        .find(|node| {
            node.children()
                .any(|child| child.has_tag_name("protocol") && child.text() == Some("SERIAL"))
        })
        .unwrap();
    assert!(
        matches!(routes::catalogue_pane(source).unwrap(), Some(PaneLayout::Serial { connection: candidate, .. }) if &candidate == connection)
    );
}

#[test]
fn imports_the_pinned_java_jaxb_project_and_companion_routes() {
    let fixture = crate::profile::projects::tests::Fixture::new();
    let source = fixture.0.join("java");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("interop.kortty"), FIXTURE).unwrap();
    std::fs::write(source.join("connections.xml"), CONNECTIONS).unwrap();
    let imported = read(
        &source.join("interop.kortty"),
        Some(&source.join("connections.xml")),
    )
    .unwrap();
    let [first, second] = imported.layout.windows.as_slice() else {
        panic!("expected both saved windows")
    };
    assert_eq!(
        (first.width, first.height, first.active, first.tabs.len()),
        (910.0, 680.0, 1, if cfg!(unix) { 3 } else { 2 })
    );
    assert_eq!(first.position, SavedPosition::new(35.0, 61.0));
    assert!(!first.maximized);
    assert_eq!(first.dashboard_visible, None);
    assert!(matches!(
        first.tabs[0],
        PaneLayout::Terminal { focused: true, .. }
    ));
    let toolbar_shell = &first.tab_terminal_appearance[&0];
    assert!(toolbar_shell.style.is_none());
    assert_eq!(toolbar_shell.font_zoom, None);
    let PaneLayout::Split {
        vertical: true,
        ratio,
        first: primary,
        second: nested,
    } = &first.tabs[1]
    else {
        panic!("expected left/right Java split")
    };
    assert_eq!(*ratio, 0.37);
    let PaneLayout::Ssh {
        font: 19,
        focused: true,
        term,
        target,
        ..
    } = primary.as_ref()
    else {
        panic!("expected active Java widget zero")
    };
    assert_eq!(term, "vt220");
    let target = serde_json::to_value(target).unwrap();
    assert_eq!(
        target,
        serde_json::json!({
            "host": "edge.example", "port": 2222, "username": "operator", "isolation": "process",
            "jump": {"host": "gateway.example", "port": 2200, "username": "relay"},
        })
    );
    let PaneLayout::Split {
        vertical: false,
        ratio,
        first: alternate,
        second: primary_again,
    } = nested.as_ref()
    else {
        panic!("expected top/bottom nested Java split")
    };
    assert_eq!(*ratio, 0.68);
    assert!(
        matches!(alternate.as_ref(), PaneLayout::Ssh { font: 19, focused: false, term, target, .. } if term == "dtterm" && target.label() == "ops@other.example:22")
    );
    assert!(
        matches!(primary_again.as_ref(), PaneLayout::Ssh { focused: false, target, .. } if target.label() == "operator@edge.example:2222")
    );
    assert_eq!(second.position, SavedPosition::new(980.0, 95.0));
    assert!(second.maximized);
    #[cfg(unix)]
    {
        assert_eq!(
            first.tabs[2],
            PaneLayout::LocalFile {
                path: PathBuf::from("/tmp/fixture-editor.txt"),
                tool: LocalFileTool::Editor,
            }
        );
        assert!(!imported.notes.contains_key(&Note::ToolTab));
        assert_eq!(second.active, 1);
        let PaneLayout::Serial {
            connection, term, ..
        } = &second.tabs[0]
        else {
            panic!("expected the Java serial configuration")
        };
        assert_eq!(connection.device, Path::new("/dev/ttyUSB9"));
        assert_eq!(connection.settings.baud_rate, 115200);
        assert_eq!(connection.settings.data_bits, 7);
        assert_eq!(
            connection.settings.parity,
            crate::session::SerialParity::Even
        );
        assert_eq!(
            connection.settings.stop_bits,
            crate::session::SerialStopBits::Two
        );
        assert_eq!(
            connection.line_ending,
            crate::session::SerialLineEnding::CrLf
        );
        assert!(!connection.settings.assert_dtr);
        assert!(connection.local_echo);
        assert_eq!(term, "vt100");
    }
    #[cfg(windows)]
    {
        assert_eq!(second.active, 0);
        assert_eq!(second.tabs.len(), 1);
        assert_eq!(imported.notes.get(&Note::UnsupportedConnection), Some(&1));
        assert_eq!(imported.notes.get(&Note::ToolTab), Some(&1));
    }
    assert_eq!(imported.notes.get(&Note::Mosh), Some(&1));
    assert!(!imported.notes.contains_key(&Note::Geometry));

    let store = Store::open_at(fixture.0.join("projects")).unwrap();
    let name = Name::parse("Imported fixture").unwrap();
    assert!(store.save_new(&name, &imported.layout).unwrap().is_none());
    assert_eq!(store.load(&name).unwrap(), imported.layout);
}

#[test]
fn java_project_uses_each_linked_connections_initial_size() {
    use crate::profile::InitialTerminalSize;

    let project = r#"
        <project><windows><window><tabs>
          <tab>
            <connectionId>primary</connectionId>
            <settings><terminalColumns>80</terminalColumns><terminalRows>24</terminalRows></settings>
            <splitPaneState>
              <orientation>HORIZONTAL</orientation>
              <leftChild><widgetIndex>0</widgetIndex></leftChild>
              <rightChild><widgetIndex>1</widgetIndex><connectionId>alternate</connectionId></rightChild>
            </splitPaneState>
          </tab>
          <tab><connectionId>local</connectionId></tab>
          <tab><connectionId>defaults</connectionId></tab>
          <tab>
            <localShell>true</localShell>
            <settings><terminalColumns>142</terminalColumns><terminalRows>52</terminalRows></settings>
          </tab>
        </tabs></window></windows></project>
    "#;
    for schema in ["", " schemaVersion=\"1\"", " schemaVersion=\"2\""] {
        let catalogue = format!(
            r#"<connections{schema}>
              <connection id="primary">
                <host>edge.example</host><username>operator</username>
                <settings><useGlobalSettings>true</useGlobalSettings>
                  <terminalColumns>132</terminalColumns><terminalRows>43</terminalRows>
                </settings>
              </connection>
              <connection id="alternate">
                <host>other.example</host><username>operator</username>
                <settings><terminalRows>200</terminalRows></settings>
              </connection>
              <connection id="local"><protocol>LOCAL_SHELL</protocol>
                <settings><terminalColumns>40</terminalColumns></settings>
              </connection>
              <connection id="defaults"><protocol>LOCAL_SHELL</protocol></connection>
            </connections>"#,
        );
        let imported = parse(project, Some(&catalogue)).unwrap();
        let [
            PaneLayout::Split { first, second, .. },
            PaneLayout::Terminal {
                initial_size: local,
                ..
            },
            PaneLayout::Terminal {
                initial_size: defaults,
                ..
            },
            PaneLayout::Terminal {
                initial_size: unsaved,
                ..
            },
        ] = imported.layout.windows[0].tabs.as_slice()
        else {
            panic!("expected a split, two saved local shells and an unsaved local shell");
        };
        let [
            PaneLayout::Ssh {
                initial_size: primary,
                ..
            },
            PaneLayout::Ssh {
                initial_size: alternate,
                ..
            },
        ] = [first.as_ref(), second.as_ref()]
        else {
            panic!("expected two SSH split panes");
        };
        assert_eq!(
            *primary,
            InitialTerminalSize {
                columns: Some(132),
                rows: Some(43),
            }
        );
        assert_eq!(
            *alternate,
            InitialTerminalSize {
                columns: Some(80),
                rows: Some(200),
            }
        );
        assert_eq!(
            *local,
            InitialTerminalSize {
                columns: Some(40),
                rows: Some(24),
            }
        );
        assert_eq!(
            *defaults,
            InitialTerminalSize {
                columns: Some(80),
                rows: Some(24),
            }
        );
        assert!(unsaved.is_default());
        assert!(!imported.notes.contains_key(&Note::InitialSize));
    }
}

#[test]
fn java_project_reports_invalid_initial_size_and_keeps_its_route() {
    let project = r#"
        <project><windows><window><tabs>
          <tab><connectionId>primary</connectionId></tab>
        </tabs></window></windows></project>
    "#;
    for (field, value) in [("terminalColumns", "501"), ("terminalRows", "invalid")] {
        let catalogue = format!(
            r#"<connections><connection id="primary">
                <host>edge.example</host><username>operator</username>
                <settings><{field}>{value}</{field}></settings>
            </connection></connections>"#,
        );
        let imported = parse(project, Some(&catalogue)).unwrap();
        assert_eq!(imported.notes.get(&Note::InitialSize), Some(&1));
        assert!(matches!(
            imported.layout.windows[0].tabs.as_slice(),
            [PaneLayout::Ssh { target, initial_size, .. }]
                if target.label() == "operator@edge.example:22" && initial_size.is_default()
        ));
    }
}

#[test]
fn missing_ids_are_reported_and_an_incomplete_split_keeps_the_surviving_route() {
    let standalone = parse(FIXTURE, None).unwrap();
    assert_eq!(standalone.notes.get(&Note::MissingConnection), Some(&5));
    let [first, second] = standalone.layout.windows.as_slice() else {
        panic!("expected both project windows");
    };
    assert!(matches!(
        first.tabs[0],
        PaneLayout::Terminal { focused: true, .. }
    ));
    #[cfg(unix)]
    assert!(matches!(
        first.tabs.as_slice(),
        [
            PaneLayout::Terminal { .. },
            PaneLayout::LocalFile {
                tool: LocalFileTool::Editor,
                ..
            }
        ]
    ));
    #[cfg(windows)]
    assert_eq!(first.tabs.len(), 1);
    assert!(matches!(
        second.tabs.as_slice(),
        [PaneLayout::Terminal { focused: true, .. }]
    ));

    let document = Document::parse(CONNECTIONS).unwrap();
    let first_connection = document.root_element().first_element_child().unwrap();
    let mut subset = CONNECTIONS.to_owned();
    subset.replace_range(first_connection.range(), "");
    let partial = parse(FIXTURE, Some(&subset)).unwrap();
    assert_eq!(partial.notes.get(&Note::MissingConnection), Some(&2));
    assert!(
        matches!(partial.layout.windows[0].tabs[1], PaneLayout::Ssh { focused: true, ref target, .. } if target.label() == "ops@other.example:22")
    );
}

#[test]
fn java_project_effect_applies_to_the_retained_panes_after_omissions() {
    let project = r#"
        <project><windows><window><tabs>
          <tab><tabType>SFTP_MANAGER</tabType></tab>
          <tab>
            <localShell>true</localShell>
            <terminalEffectPluginId>commodore-blue</terminalEffectPluginId>
            <terminalEffectAnimationSpeed>7.5</terminalEffectAnimationSpeed>
            <splitPaneState>
              <orientation>HORIZONTAL</orientation>
              <leftChild><widgetIndex>0</widgetIndex></leftChild>
              <rightChild>
                <orientation>VERTICAL</orientation>
                <leftChild><widgetIndex>1</widgetIndex><connectionId>missing</connectionId></leftChild>
                <rightChild><widgetIndex>2</widgetIndex></rightChild>
              </rightChild>
            </splitPaneState>
          </tab>
        </tabs></window></windows></project>
    "#;
    let imported = parse(project, None).unwrap();
    let selection = Selection {
        effect: Effect::CommodoreHeritage,
        speed: Speed::try_from(7.5).unwrap(),
    };
    assert_eq!(
        imported.layout.windows[0].tab_terminal_effects,
        BTreeMap::from([(0, BTreeMap::from([(0, selection), (1, selection)]))])
    );
    assert!(!imported.notes.contains_key(&Note::ToolTab));
    assert_eq!(imported.notes.get(&Note::MissingConnection), Some(&2));
    assert!(!imported.notes.contains_key(&Note::Appearance));

    for unsupported in [
        project.replace("commodore-blue", "external.plugin"),
        project.replace(">7.5<", ">NaN<"),
        project.replace(">7.5<", ">100<"),
        project.replace(">commodore-blue<", "><"),
    ] {
        let imported = parse(&unsupported, None).unwrap();
        assert!(imported.layout.windows[0].tab_terminal_effects.is_empty());
        assert_eq!(imported.notes.get(&Note::Appearance), Some(&1));
    }
}

#[test]
fn commands_history_files_and_credentials_are_not_carried_into_the_native_layout() {
    let project = FIXTURE
        .replace("<localShell>true</localShell>", "<localShell>true</localShell><localShellCommand>/tmp/fixture-program</localShellCommand><currentDirectory>/tmp/fixture-cwd</currentDirectory><historyFilePath>../fixture-history</historyFilePath><terminalHistory>fixture-output</terminalHistory>")
        .replace("<editorFilePath>", "<editorIsRemote>true</editorIsRemote><editorFilePath>");
    let connections = CONNECTIONS.replace("<host>edge.example</host>", "<host>edge.example</host><encryptedPassword>fixture-secret</encryptedPassword><privateKeyPath>/tmp/fixture-key</privateKeyPath>");
    let imported = parse(&project, Some(&connections)).unwrap();
    assert_eq!(imported.notes.get(&Note::SessionData), Some(&2));
    assert_eq!(imported.notes.get(&Note::ToolTab), Some(&1));
    let encoded = serde_json::to_string(&imported.layout).unwrap();
    for ignored in [
        "fixture-program",
        "fixture-cwd",
        "fixture-history",
        "fixture-output",
        "fixture-secret",
        "fixture-key",
        "fixture-editor",
    ] {
        assert!(!encoded.contains(ignored));
    }
}

#[test]
fn rejects_entities_ambiguous_ids_and_overdeep_split_trees() {
    let external = r#"<!DOCTYPE project [<!ENTITY secret SYSTEM "file:///etc/passwd">]><project><name>&secret;</name></project>"#;
    assert!(parse(external, None).is_err());
    let duplicate = CONNECTIONS.replace(
        "</connections>",
        "<connection id=\"94144051-b1fb-40fb-bec7-a160f119ae9e\"/></connections>",
    );
    assert!(
        parse(FIXTURE, Some(&duplicate))
            .unwrap_err()
            .to_string()
            .contains("duplicate connection IDs")
    );

    let mut split = "<widgetIndex>0</widgetIndex>".to_owned();
    for index in 0..=MAX_SPLIT_DEPTH {
        split = format!(
            "<orientation>HORIZONTAL</orientation><leftChild>{split}</leftChild><rightChild><widgetIndex>{}</widgetIndex></rightChild>",
            index + 1
        );
    }
    let nested = format!(
        "<project><windows><window><tabs><tab><localShell>true</localShell><splitPaneState>{split}</splitPaneState></tab></tabs></window></windows></project>"
    );
    assert!(
        parse(&nested, None)
            .unwrap_err()
            .to_string()
            .contains("split nesting")
    );
    assert!(read(Path::new("relative.kortty"), None).is_err());
}
