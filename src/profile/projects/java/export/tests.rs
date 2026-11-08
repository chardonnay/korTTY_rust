use crate::profile::projects::tests::Fixture;
use crate::terminal_effects::Effect;

use super::*;

const PROJECT: &str = include_str!("../fixtures/interop.kortty");
const CONNECTIONS: &str = include_str!("../fixtures/connections.xml");
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
    assert_eq!(export.tabs, if cfg!(unix) { 4 } else { 3 });
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
    assert!(xml.contains("<terminalEffectPluginId>neon-city</terminalEffectPluginId>"));
    assert_eq!(xml.matches("<terminalEffectAnimationSpeed>").count(), 1);
    assert!(xml.contains("<terminalEffectAnimationSpeed>7</terminalEffectAnimationSpeed>"));
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
        speed: 1,
    };
    let neon = Selection {
        effect: Effect::NeonCity,
        speed: 7,
    };
    layout.windows[0].tab_terminal_effects = BTreeMap::from([
        (0, BTreeMap::from([(0, mother)])),
        (1, BTreeMap::from([(0, neon), (1, neon), (2, neon)])),
    ]);
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
    let duplicate =
        CONNECTIONS[source.range()].replace(PRIMARY, "186ccf8c-6393-434c-bf1d-f944e8670b58");
    let ambiguous = CONNECTIONS.replace("</connections>", &format!("{duplicate}</connections>"));
    fs::write(&catalogue, &ambiguous).unwrap();

    let export = prepare(&name, &layout, output.clone(), Some(catalogue.clone())).unwrap();
    assert_eq!(export.tabs, if cfg!(unix) { 3 } else { 2 });
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
        first.tabs.as_slice(),
        [PaneLayout::Terminal { .. }]
    ));
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
        tab_terminal_effects: BTreeMap::from([(
            0,
            BTreeMap::from([(
                1,
                Selection {
                    effect: Effect::Vhs1987,
                    speed: 4,
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
