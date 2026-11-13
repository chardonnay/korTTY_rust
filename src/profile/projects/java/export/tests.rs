use crate::profile::projects::tests::Fixture;
use crate::terminal_effects::Effect;

use super::*;

const PROJECT: &str = include_str!("../fixtures/interop.kortty");
const CONNECTIONS: &str = include_str!("../fixtures/connections.xml");
const REMOTE_TOOLS: &str = include_str!("../fixtures/remote-tools.kortty");
const PRIMARY: &str = "94144051-b1fb-40fb-bec7-a160f119ae9e";
const ALTERNATE: &str = "bf733aa6-04db-4f8e-b8c5-b8a0d3e41710";

fn paths() -> (Fixture, PathBuf, PathBuf) {
    let fixture = Fixture::new();
    fs::create_dir(&fixture.0).unwrap();
    let catalogue = fixture.0.join("connections.xml");
    fs::write(&catalogue, CONNECTIONS).unwrap();
    let output = fixture.0.join("exported.kortty");
    (fixture, catalogue, output)
}

#[test]
fn java_project_export_rechecks_primary_catalogue_appearance() {
    use crate::terminal_theme::{Rgb, Themes};

    let (_fixture, catalogue, output) = paths();
    let source = CONNECTIONS
        .replacen(
            "<useGlobalSettings>true</useGlobalSettings>",
            "<useGlobalSettings>false</useGlobalSettings><themeId>missing-java-theme</themeId>",
            1,
        )
        .replacen("<backgroundColor>#1E1E1E", "<backgroundColor>#112233", 1);
    fs::write(&catalogue, &source).unwrap();
    let layout = super::super::parse(PROJECT, Some(&source)).unwrap().layout;
    let encoded = serde_json::to_vec(&layout).unwrap();
    let layout: WorkspaceLayout = serde_json::from_slice(&encoded).unwrap();
    let appearance = &layout.windows[0].tab_terminal_appearance[&1];
    assert_eq!(appearance.font_zoom, Some(19));
    assert_eq!(
        appearance
            .apply(Default::default(), &Themes::default())
            .background,
        Rgb::from_hex(0x112233)
    );

    let name = Name::parse("Catalogue appearance").unwrap();
    let prepared = prepare(&name, &layout, output.clone(), Some(catalogue.clone())).unwrap();
    let edited = source.replacen("#112233", "#445566", 1);
    fs::write(&catalogue, edited).unwrap();
    assert!(
        prepared
            .publish()
            .unwrap_err()
            .to_string()
            .contains("appearance changed")
    );
    assert!(!output.exists());
    let mismatched = prepare(&name, &layout, output.clone(), Some(catalogue.clone())).unwrap();
    assert!(
        mismatched
            .notes
            .iter()
            .any(|note| note.contains("different terminal appearance"))
    );

    fs::write(&catalogue, source).unwrap();
    prepared.publish().unwrap();
    let restored = super::super::read(&output, Some(&catalogue))
        .unwrap()
        .layout;
    assert_eq!(&restored.windows[0].tab_terminal_appearance[&1], appearance);
}

#[test]
fn exports_exact_catalogue_links_and_round_trips() {
    let (_fixture, catalogue, output) = paths();
    let mut layout = super::super::parse(PROJECT, Some(CONNECTIONS))
        .unwrap()
        .layout;
    add_effect_fixture(&mut layout);
    layout.windows[0].dashboard_visible = Some(false);
    layout.windows[1].dashboard_visible = Some(true);
    layout.windows[0]
        .tab_titles
        .insert(0, "\u{a0}Local <&>".into());
    layout.windows[0]
        .tab_groups
        .get_or_insert_with(Default::default)
        .insert(0, "Production <EU>".into());
    let name = Name::parse("Native Prüfung").unwrap();
    let export = prepare(&name, &layout, output.clone(), Some(catalogue.clone())).unwrap();
    assert_eq!(export.windows, 2);
    assert_eq!(export.tabs, if cfg!(unix) { 5 } else { 3 });
    #[cfg(unix)]
    assert_eq!(
        export.notes,
        ["Window 1, tab 3 includes local file path /tmp/fixture-editor.txt"]
    );
    #[cfg(windows)]
    assert!(export.notes.is_empty(), "{:?}", export.notes);
    assert!(!output.exists());
    export.publish().unwrap();

    let xml = fs::read_to_string(&output).unwrap();
    assert!(xml.contains("<name>Native Prüfung</name>"));
    assert!(xml.contains("<autoReconnect>true</autoReconnect>"));
    assert!(xml.contains("<maximized>true</maximized>"));
    assert!(xml.contains("<dashboardVisible>false</dashboardVisible>"));
    assert!(xml.contains("<dashboardVisible>true</dashboardVisible>"));
    assert!(xml.contains("<tabTitle>\u{a0}Local &lt;&amp;&gt;</tabTitle>"));
    assert!(xml.contains("<group>Production &lt;EU&gt;</group>"));
    assert!(xml.contains("<terminalEffectPluginId>mother</terminalEffectPluginId>"));
    assert!(xml.contains("<terminalEffectPluginId>typewriter-noir</terminalEffectPluginId>"));
    assert_eq!(xml.matches("<terminalEffectAnimationSpeed>").count(), 1);
    assert!(xml.contains("<terminalEffectAnimationSpeed>2.5</terminalEffectAnimationSpeed>"));
    for id in [PRIMARY, ALTERNATE] {
        assert!(xml.contains(&format!("<connectionId>{id}</connectionId>")));
    }
    for excluded in [
        "edge.example",
        "gateway.example",
        "encryptedPassword",
        "currentDirectory",
    ] {
        assert!(!xml.contains(excluded));
    }
    assert_eq!(
        super::super::read(&output, Some(&catalogue))
            .unwrap()
            .layout,
        layout
    );
    assert_eq!(
        export.publish().unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read_to_string(&output).unwrap(), xml);
}

fn add_effect_fixture(layout: &mut WorkspaceLayout) {
    let mother = Selection {
        effect: Effect::Mother,
        speed: Speed::DEFAULT,
    };
    let typewriter = Selection {
        effect: Effect::TypewriterNoir,
        speed: Speed::try_from(2.5).unwrap(),
    };
    layout.windows[0].tab_terminal_effects = BTreeMap::from([
        (0, BTreeMap::from([(0, mother)])),
        (
            1,
            BTreeMap::from([(0, typewriter), (1, typewriter), (2, typewriter)]),
        ),
    ]);
}

#[test]
fn java_remote_tools_round_trip_order_selection_routes_and_zoom() {
    let (_fixture, catalogue, output) = paths();
    let imported = super::super::parse(REMOTE_TOOLS, Some(CONNECTIONS)).unwrap();
    let window = &imported.layout.windows[0];
    assert_eq!(window.active, 2);
    let [
        PaneLayout::RemoteTool {
            target: browser,
            tool:
                RemoteProjectTool::Sftp {
                    local,
                    remote,
                    idle_minutes,
                },
        },
        PaneLayout::RemoteTool {
            target: editor,
            tool:
                RemoteProjectTool::File {
                    path,
                    tool: LocalFileTool::Editor,
                },
        },
        PaneLayout::RemoteTool {
            target: image,
            tool:
                RemoteProjectTool::File {
                    tool: LocalFileTool::Image { zoom },
                    ..
                },
        },
    ] = window.tabs.as_slice()
    else {
        panic!("expected three remote tools");
    };
    assert_eq!(local.as_deref(), cfg!(unix).then_some(Path::new("/tmp")));
    assert_eq!(remote.as_deref(), Some("/srv/deployment"));
    assert_eq!(*idle_minutes, Some(15));
    assert_eq!(path, "/etc/fixture ü.txt");
    assert_eq!(*zoom, 1.75);
    assert!(browser.matches_imported_route(image));
    assert!(!browser.matches_imported_route(editor));
    let name = Name::parse("Native remote tools").unwrap();
    assert!(prepare(&name, &imported.layout, output.clone(), None).is_err());
    let export = prepare(
        &name,
        &imported.layout,
        output.clone(),
        Some(catalogue.clone()),
    )
    .unwrap();
    assert!(
        export
            .xml
            .contains(&format!("<connectionId>{PRIMARY}</connectionId>"))
    );
    assert!(
        export
            .xml
            .contains(&format!("<connectionId>{ALTERNATE}</connectionId>"))
    );
    assert!(!export.xml.contains("encryptedPassword"));
    fs::write(
        &catalogue,
        CONNECTIONS.replace("other.example", "changed.example"),
    )
    .unwrap();
    assert!(export.publish().is_err());
    assert!(!output.exists());
    // Remote tools use the saved route without requesting a terminal grid.
    fs::write(
        &catalogue,
        CONNECTIONS.replacen("<terminalColumns>80", "<terminalColumns>501", 1),
    )
    .unwrap();
    export.publish().unwrap();
    assert_eq!(
        super::super::read(&output, Some(&catalogue))
            .unwrap()
            .layout,
        imported.layout
    );
}

#[test]
fn java_export_does_not_replace_a_selected_windows_shell_with_the_default() {
    use crate::session_security::LocalSettings;
    use kortty_pty::LocalShell;

    let mut layout = super::super::parse(PROJECT, Some(CONNECTIONS))
        .unwrap()
        .layout;
    let PaneLayout::Terminal { security, .. } = &mut layout.windows[0].tabs[0] else {
        panic!("expected an unsaved local terminal in the project fixture");
    };
    *security = Some(LocalSettings {
        shell: LocalShell::CommandPrompt,
        ..LocalSettings::default()
    });
    let pane = &layout.windows[0].tabs[0];
    assert!(matches!(
        link(pane, &Catalogue::default(), true),
        Err(reason) if reason.contains("selected Windows shell")
    ));
}

#[test]
fn requires_a_catalogue_and_omits_a_tab_with_ambiguous_routes() {
    let (_fixture, catalogue, output) = paths();
    let layout = super::super::parse(PROJECT, Some(CONNECTIONS))
        .unwrap()
        .layout;
    let name = Name::parse("Ambiguous fixture").unwrap();
    assert!(
        prepare(&name, &layout, output.clone(), None)
            .unwrap_err()
            .to_string()
            .contains("choose the Java connections.xml")
    );

    let xml = document(CONNECTIONS, CONNECTION_BYTES).unwrap();
    let source = xml.root_element().first_element_child().unwrap();
    let duplicate = CONNECTIONS[source.range()]
        .replace(PRIMARY, "186ccf8c-6393-434c-bf1d-f944e8670b58")
        .replace("<terminalRows>24", "<terminalRows>201");
    let ambiguous = CONNECTIONS.replace("</connections>", &format!("{duplicate}</connections>"));
    fs::write(&catalogue, &ambiguous).unwrap();

    let export = prepare(&name, &layout, output.clone(), Some(catalogue.clone())).unwrap();
    assert_eq!(export.tabs, if cfg!(unix) { 4 } else { 2 });
    assert!(
        export
            .notes
            .iter()
            .any(|note| note.contains("tab 2 omitted") && note.contains("ambiguous"))
    );
    assert!(
        export
            .notes
            .iter()
            .any(|note| note.contains("original selection was omitted"))
    );
    export.publish().unwrap();
    let first = &super::super::read(&output, Some(&catalogue))
        .unwrap()
        .layout
        .windows[0];
    assert_eq!(first.active, 0);
    assert!(matches!(
        first.tabs.first(),
        Some(PaneLayout::Terminal { .. })
    ));
    #[cfg(unix)]
    assert!(matches!(
        first.tabs.as_slice(),
        [PaneLayout::Terminal { .. }, PaneLayout::LocalFile { .. }]
    ));
    #[cfg(windows)]
    assert_eq!(first.tabs.len(), 1);
}

#[test]
fn java_export_revalidates_required_size_on_a_split_with_an_inherited_link() {
    let (_fixture, catalogue, output) = paths();
    let mut layout = super::super::parse(PROJECT, Some(CONNECTIONS))
        .unwrap()
        .layout;
    let PaneLayout::Split { first, second, .. } = &mut layout.windows[0].tabs[1] else {
        panic!("expected a split with two panes on the same route");
    };
    let PaneLayout::Ssh { initial_size, .. } = first.as_mut() else {
        panic!("expected the first SSH pane");
    };
    *initial_size = InitialTerminalSize::default();
    let PaneLayout::Split { second, .. } = second.as_mut() else {
        panic!("expected a nested split");
    };
    let PaneLayout::Ssh { initial_size, .. } = second.as_mut() else {
        panic!("expected a second pane on the same SSH route");
    };
    *initial_size = InitialTerminalSize {
        columns: Some(132),
        rows: None,
    };

    let name = Name::parse("Split grid fixture").unwrap();
    let preview = || prepare(&name, &layout, output.clone(), Some(catalogue.clone())).unwrap();
    assert!(preview().notes.iter().any(|note| {
        note.contains("tab 2 omitted") && note.contains("different initial columns or rows")
    }));
    let matching = CONNECTIONS.replacen("<terminalColumns>80", "<terminalColumns>132", 1);
    fs::write(&catalogue, &matching).unwrap();
    let export = preview();
    assert_eq!(
        export
            .xml
            .matches(&format!("<connectionId>{PRIMARY}</connectionId>"))
            .count(),
        1,
    );
    assert_eq!(
        export
            .bindings
            .iter()
            .filter(|binding| binding.id == PRIMARY)
            .count(),
        2,
    );

    fs::write(&catalogue, CONNECTIONS).unwrap();
    assert!(
        export
            .publish()
            .unwrap_err()
            .to_string()
            .contains("mappings changed")
    );
    assert!(!output.exists());
    fs::write(&catalogue, matching).unwrap();
    export.publish().unwrap();
}

#[test]
fn java_export_binds_and_revalidates_the_exact_local_shell_integration_choice() {
    let (_fixture, catalogue, output) = paths();
    let project = "<project><windows><window><tabs><tab><connectionId>integrated</connectionId></tab></tabs></window></windows></project>";
    let connections = "<connections><connection id=\"integrated\"><protocol>LOCAL_SHELL</protocol><shellIntegrationAutoInject>true</shellIntegrationAutoInject></connection></connections>";
    fs::write(&catalogue, connections).unwrap();
    let layout = super::super::parse(project, Some(connections))
        .unwrap()
        .layout;
    let name = Name::parse("Integrated local shell").unwrap();
    assert!(prepare(&name, &layout, output.clone(), None).is_err());
    let export = prepare(&name, &layout, output.clone(), Some(catalogue.clone())).unwrap();
    assert!(
        export
            .xml
            .contains("<connectionId>integrated</connectionId>")
    );
    assert!(!export.xml.contains("<localShell>true</localShell>"));
    fs::write(
        &catalogue,
        connections.replace(
            "<shellIntegrationAutoInject>true</shellIntegrationAutoInject>",
            "<shellIntegrationAutoInject>false</shellIntegrationAutoInject>",
        ),
    )
    .unwrap();
    assert!(
        export
            .publish()
            .unwrap_err()
            .to_string()
            .contains("mappings changed")
    );
    assert!(!output.exists());
    fs::write(&catalogue, connections).unwrap();
    export.publish().unwrap();
    let reopened = super::super::read(&output, Some(&catalogue)).unwrap();
    let PaneLayout::Terminal {
        security: Some(settings),
        ..
    } = &reopened.layout.windows[0].tabs[0]
    else {
        panic!("expected the integrated local shell");
    };
    assert!(settings.shell_integration);
}

#[test]
fn java_export_links_custom_local_size_to_an_exact_catalogue_entry() {
    let (_fixture, catalogue, output) = paths();
    let name = Name::parse("Local grid fixture").unwrap();
    let mut layout = super::super::parse(PROJECT, None).unwrap().layout;
    layout.windows.truncate(1);
    let window = &mut layout.windows[0];
    window.position = None;
    window.active = 0;
    window.tabs.truncate(1);
    window.tab_titles.clear();
    window.tab_groups = None;
    window.tab_primary_panes.clear();
    window.tab_terminal_effects.clear();
    window.tab_terminal_appearance.retain(|&tab, _| tab == 0);
    let PaneLayout::Terminal { initial_size, .. } = &mut window.tabs[0] else {
        panic!("expected an unsaved local terminal");
    };
    *initial_size = InitialTerminalSize {
        columns: Some(80),
        rows: Some(24),
    };
    let default = prepare(&name, &layout, output.clone(), None).unwrap();
    assert!(default.xml.contains("<localShell>true</localShell>"));

    let PaneLayout::Terminal { initial_size, .. } = &mut layout.windows[0].tabs[0] else {
        unreachable!("the local pane remains selected");
    };
    initial_size.columns = Some(132);
    let error = prepare(&name, &layout, output.clone(), None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires a matching local shell")
    );
    assert!(!output.exists());

    let local_catalogue = r#"<connections schemaVersion="2">
        <connection id="local-grid">
            <protocol>LOCAL_SHELL</protocol>
            <settings><terminalColumns>132</terminalColumns></settings>
        </connection>
    </connections>"#;
    fs::write(&catalogue, local_catalogue).unwrap();
    let export = prepare(&name, &layout, output.clone(), Some(catalogue)).unwrap();
    assert!(
        export
            .xml
            .contains("<connectionId>local-grid</connectionId>")
    );
    assert!(!export.xml.contains("<localShell>"));
    export.publish().unwrap();
}

#[test]
fn revalidates_routes_after_preview_and_never_replaces_an_existing_file() {
    let (_fixture, catalogue, output) = paths();
    let layout = super::super::parse(PROJECT, Some(CONNECTIONS))
        .unwrap()
        .layout;
    let name = Name::parse("Changed fixture").unwrap();
    let export = prepare(&name, &layout, output.clone(), Some(catalogue.clone())).unwrap();
    for changed in [
        CONNECTIONS.replace("gateway.example", "changed.example"),
        CONNECTIONS.replace("<username>relay</username>", "<username> relay</username>"),
        CONNECTIONS.replace(
            "<host>edge.example</host>",
            "<host>edge.example</host><disableHostKeyCheck>true</disableHostKeyCheck>",
        ),
    ] {
        fs::write(&catalogue, changed).unwrap();
        assert!(
            export
                .publish()
                .unwrap_err()
                .to_string()
                .contains("mappings changed")
        );
        assert!(!output.exists());
    }
    fs::write(&catalogue, CONNECTIONS).unwrap();
    fs::write(&output, b"existing file").unwrap();
    assert_eq!(
        export.publish().unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read(&output).unwrap(), b"existing file");
}

#[test]
fn previews_split_focus_and_font_loss_before_creating_a_file() {
    let (_fixture, _catalogue, output) = paths();
    let layout = super::super::parse(PROJECT, None).unwrap().layout;
    let mut first = layout.windows[0].tabs[0].clone();
    if let PaneLayout::Terminal { focused, .. } = &mut first {
        *focused = false;
    }
    let mut second = layout.windows[0].tabs[0].clone();
    if let PaneLayout::Terminal { font, .. } = &mut second {
        *font = 23;
    }
    let layout = WorkspaceLayout::new(vec![WindowLayout {
        tab_titles: Default::default(),
        tab_groups: Default::default(),
        tab_primary_panes: Default::default(),
        tab_terminal_appearance: Default::default(),
        tab_terminal_effects: BTreeMap::from([(
            0,
            BTreeMap::from([(
                1,
                Selection {
                    effect: Effect::Vhs1987,
                    speed: Speed::try_from(4.0).unwrap(),
                },
            )]),
        )]),
        width: 800.0,
        height: 600.0,
        position: None,
        maximized: false,
        dashboard_visible: None,
        active: 0,
        tabs: vec![PaneLayout::Split {
            vertical: true,
            ratio: 0.45,
            first: Box::new(first),
            second: Box::new(second),
        }],
    }]);
    let name = Name::parse("Split fixture").unwrap();
    let export = prepare(&name, &layout, output.clone(), None).unwrap();
    assert_eq!(export.tabs, 1);
    assert!(
        export
            .notes
            .iter()
            .any(|note| note.contains("first pane's size is used"))
    );
    assert!(
        export
            .notes
            .iter()
            .any(|note| note.contains("which split pane has focus"))
    );
    assert!(
        export
            .notes
            .iter()
            .any(|note| note.contains("individual pane effects were omitted"))
    );
    assert!(!export.xml.contains("<terminalEffectPluginId>"));
    assert!(!output.exists());
}

#[cfg(unix)]
#[test]
#[ignore = "requires the classpath for Java kortty at 1ebbe724ee84"]
fn pinned_java_project_manager_reads_the_rust_export() {
    let (fixture, catalogue, output) = paths();
    let mut layout = super::super::parse(PROJECT, Some(CONNECTIONS))
        .unwrap()
        .layout;
    add_effect_fixture(&mut layout);
    let name = Name::parse("Native verified").unwrap();
    prepare(&name, &layout, output.clone(), Some(catalogue))
        .unwrap()
        .publish()
        .unwrap();
    let classpath = std::env::var_os("KORTTY_JAVA_PROJECT_CLASSPATH")
        .expect("set KORTTY_JAVA_PROJECT_CLASSPATH to the pinned Java runtime classpath");
    let checker = fixture.0.join("CheckExport.java");
    fs::write(&checker, include_str!("CheckExport.java")).unwrap();
    let result = std::process::Command::new("java")
        .arg(format!("-Duser.home={}", fixture.0.display()))
        .arg("-cp")
        .arg(classpath)
        .arg(checker)
        .arg(output)
        .arg(&fixture.0)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        result.status.success(),
        "Java checker failed: {stdout}\n{stderr}"
    );
    assert!(stdout.contains("pinned Java loader and split restore plan accepted the Rust project"));
    println!("{stdout}");
}

#[test]
#[ignore = "requires the classpath for Java kortty at 1ebbe724ee84"]
fn pinned_java_project_manager_reads_the_rust_remote_tool_export() {
    let (fixture, catalogue, output) = paths();
    let local = if cfg!(windows) {
        r"C:\Users\fixture\docs"
    } else {
        "/tmp"
    };
    let project = REMOTE_TOOLS.replace(
        "<sftpLocalPath>/tmp</sftpLocalPath>",
        &format!("<sftpLocalPath>{local}</sftpLocalPath>"),
    );
    let layout = super::super::parse(&project, Some(CONNECTIONS))
        .unwrap()
        .layout;
    prepare(
        &Name::parse("Native remote tools").unwrap(),
        &layout,
        output.clone(),
        Some(catalogue),
    )
    .unwrap()
    .publish()
    .unwrap();
    let classpath = std::env::var_os("KORTTY_JAVA_PROJECT_CLASSPATH")
        .expect("set KORTTY_JAVA_PROJECT_CLASSPATH to the pinned Java runtime classpath");
    let checker = fixture.0.join("CheckExport.java");
    fs::write(&checker, include_str!("CheckExport.java")).unwrap();
    let result = std::process::Command::new("java")
        .arg(format!("-Duser.home={}", fixture.0.display()))
        .arg("-cp")
        .arg(classpath)
        .arg(checker)
        .arg(output)
        .arg(&fixture.0)
        .arg("remote-tools")
        .arg(local)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "Java checker failed: {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout)
            .contains("pinned Java loader accepted the Rust remote tool tabs")
    );
}
