use std::io::Write;
use std::time::{Duration, Instant};

use crate::connection_store::protocol::{EditorSettings, Profile};
use crate::connection_store::{LocalProfile, SerialProfile, Store, TabColor, TerminalOverrides};
use crate::session::{SerialConnection, Status, serial_test_pair};
use crate::terminal_theme::{CursorStyle, Rgb};
use kortty_terminal::{ScrollbackConfig, TerminalProfile};

use super::super::{LocalLaunchView, SessionId, Workspace};
use super::SavedTerminalPreferences;

fn local() -> LocalProfile {
    serde_json::from_value(serde_json::json!({
        "id": 0, "name": "Runtime local fixture", "shell": "default",
        "command": ["/bin/sh", "-c", "printf 'FIRST PRIVATE LAST\\n'; exec /bin/cat"],
        "emulation": "xterm-256color"
    }))
    .unwrap()
}

fn common(directory: &std::path::Path) -> EditorSettings {
    EditorSettings {
        command_timestamps: Some(true),
        tab_color: Some(TabColor::parse("#123456").unwrap()),
        paste_warning: Some(crate::clipboard_policy::PasteWarningMode::Always),
        logging: serde_json::from_value(serde_json::json!({
            "enabled": true, "folder": directory.to_str().unwrap(), "compress": false,
            "format": "text"
        }))
        .unwrap(),
        terminal: TerminalOverrides {
            cursor: Some(Rgb::from_hex(0xabcdef)),
            cursor_style: Some(CursorStyle::SteadyBar),
            selection: Some(Rgb::from_hex(0xfedcba)),
            terminal_columns: Some(103),
            terminal_rows: Some(33),
            scrollback_lines: Some(219),
            scrollback_memory_mib: Some(4),
            scrollback_compression: Some(false),
            paste_line_delay_ms: Some(37),
            confirm_close: Some(true),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn prepare(workspace: &mut Workspace, store: &Store) {
    workspace.refresh_saved_terminal_preferences(store);
    workspace.clipboard.policy = crate::clipboard_policy::parse(
        "[meta]\nschema-version = 1\n[[rule]]\n[[rule.session-journal.replace]]\npattern = 'PRIVATE'\nreplacement = 'MASKED'"
    ).unwrap();
}

fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "native runtime did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn stop(workspace: &mut Workspace, session: SessionId) {
    let _ = workspace.sessions[&session].session.stop().recv_blocking();
    assert!(
        workspace.sessions[&session]
            .session
            .logfile_notice()
            .is_none(),
        "{:?}",
        workspace.sessions[&session].session.logfile_notice()
    );
    workspace.stop_session(session);
    for done in std::mem::take(&mut workspace.pending_shutdown) {
        let _ = done.recv_blocking();
    }
}

fn logs(directory: &std::path::Path) -> String {
    std::fs::read_dir(directory)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension().is_some_and(|e| e == "log"))
                .then(|| std::fs::read_to_string(path).unwrap())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_runtime(workspace: &Workspace, id: SessionId) {
    wait(|| workspace.sessions[&id].session.frame().status == Status::Running);
    let pane = &workspace.sessions[&id];
    assert_eq!((pane.grid.columns, pane.grid.rows), (103, 33));
    assert_eq!(workspace.paste_line_delay_for(id), 37);
    assert_eq!(
        workspace.paste_warning_for(id),
        crate::clipboard_policy::PasteWarningMode::Always
    );
    assert_eq!(
        workspace.connection_color_mark(id).unwrap().color(),
        TabColor::parse("#123456").unwrap().color()
    );
    assert_eq!(
        workspace.pane_appearance(id).cursor,
        Rgb::from_hex(0xabcdef)
    );
    assert!(!workspace.pane_appearance(id).cursor_blink);
    assert_eq!(
        workspace.pane_appearance(id).selection,
        Some(Rgb::from_hex(0xfedcba))
    );
    assert_eq!(
        pane.session.scrollback_config_requested(),
        ScrollbackConfig {
            lines: 219,
            memory_mib: 4,
            compression: false
        }
    );
    assert!(workspace.pane_needs_close_confirmation(id, false));
}

#[test]
fn saved_local_runtime_applies_before_first_output_and_finalizes_masked_log() {
    let scratch = crate::snippets::tests::Profile::new();
    let folder = scratch.0.join("logs");
    std::fs::create_dir(&folder).unwrap();
    let mut store = Store::open(scratch.0.join("connections.json")).unwrap();
    let settings = common(&folder);
    let mut profile = local();
    profile.terminal = settings.terminal.clone();
    let id = store
        .save_protocol(
            None,
            Profile::Local(Box::new(profile)),
            vec![],
            None,
            Some(settings),
        )
        .unwrap();
    let mut workspace = Workspace::default();
    prepare(&mut workspace, &store);
    let session = workspace
        .store_saved_local_session(store.local(id).unwrap().clone(), None)
        .unwrap();
    assert_runtime(&workspace, session);
    let (owner, _) = workspace.open_window(false);
    workspace.add_tab(owner, session);
    assert!(workspace.windows[&owner].tab().unwrap().command_timestamps);
    wait(|| {
        workspace.sessions[&session]
            .session
            .frame()
            .screen
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .contains("FIRST PRIVATE LAST")
    });
    stop(&mut workspace, session);
    let captured = logs(&folder);
    assert!(captured.contains("FIRST MASKED LAST"), "{captured}");
    assert!(!captured.contains("PRIVATE"));
}

#[test]
fn saved_serial_runtime_uses_full_common_presentation_and_masked_logging() {
    let scratch = crate::snippets::tests::Profile::new();
    let folder = scratch.0.join("logs");
    std::fs::create_dir(&folder).unwrap();
    let (mut peer, device) = serial_test_pair().unwrap();
    let mut store = Store::open(scratch.0.join("connections.json")).unwrap();
    let settings = common(&folder);
    let mut profile = SerialProfile::new(
        "Runtime serial fixture",
        SerialConnection {
            device,
            ..Default::default()
        },
        TerminalProfile::Xterm,
    );
    profile.paste_line_delay_ms = settings.terminal.paste_line_delay_ms;
    let id = store
        .save_protocol(
            None,
            Profile::Serial(Box::new(profile)),
            vec![],
            None,
            Some(settings),
        )
        .unwrap();
    let mut workspace = Workspace::default();
    prepare(&mut workspace, &store);
    let (owner, _) = workspace.open_window(false);
    let _ = workspace.open_saved_serial(owner, store.serial(id).unwrap().clone());
    let session = workspace.windows[&owner]
        .tab()
        .unwrap()
        .focused_session()
        .unwrap();
    assert_runtime(&workspace, session);
    assert!(workspace.windows[&owner].tab().unwrap().command_timestamps);
    wait(|| workspace.sessions[&session].session.frame().status == Status::Running);
    peer.write_all(b"FIRST PRIVATE LAST\r\n").unwrap();
    wait(|| {
        workspace.sessions[&session]
            .session
            .frame()
            .screen
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .contains("FIRST PRIVATE LAST")
    });
    stop(&mut workspace, session);
    let captured = logs(&folder);
    assert!(captured.contains("FIRST MASKED LAST"), "{captured}");
    assert!(!captured.contains("PRIVATE"));
}

#[test]
fn incognito_local_and_inherited_private_launch_never_create_logs_or_reopen_origin() {
    let scratch = crate::snippets::tests::Profile::new();
    for saved_private in [false, true] {
        let folder = scratch.0.join(format!("logs-{saved_private}"));
        let mut store =
            Store::open(scratch.0.join(format!("connections-{saved_private}.json"))).unwrap();
        let mut settings = common(&folder);
        settings.incognito = saved_private;
        let id = store
            .save_protocol(
                None,
                Profile::Local(Box::new(local())),
                vec![],
                None,
                Some(settings),
            )
            .unwrap();
        let mut workspace = Workspace::default();
        prepare(&mut workspace, &store);
        let session = workspace
            .store_saved_local_session_in_view(
                store.local(id).unwrap().clone(),
                None,
                LocalLaunchView::QuickConnect(None, None, None, !saved_private, None),
            )
            .unwrap();
        let (owner, _) = workspace.open_window(false);
        workspace.add_tab_with_privacy(owner, session, !saved_private);
        let tab = workspace.windows[&owner].tab().unwrap();
        assert!(tab.incognito);
        assert!(tab.reopen_origin.is_none());
        wait(|| workspace.sessions[&session].session.frame().status == Status::Running);
        let _ = workspace.record_local_session_used(session);
        assert!(workspace.connection_usage.pending.is_empty());
        assert!(!workspace.connection_usage.active);
        stop(&mut workspace, session);
        assert!(!folder.exists());
    }
}

#[test]
fn protocol_conversion_rejects_stale_launch_and_does_not_restyle_live_local_pane() {
    let scratch = crate::snippets::tests::Profile::new();
    let mut store = Store::open(scratch.0.join("connections.json")).unwrap();
    let mut settings = common(&scratch.0.join("logs"));
    settings.logging = Default::default();
    let mut profile = local();
    profile.terminal = settings.terminal.clone();
    let id = store
        .save_protocol(
            None,
            Profile::Local(Box::new(profile)),
            vec![],
            None,
            Some(settings),
        )
        .unwrap();
    let mut workspace = Workspace::default();
    prepare(&mut workspace, &store);
    let local = store.local(id).unwrap().clone();
    let session = workspace
        .store_saved_local_session(local.clone(), None)
        .unwrap();
    let original = store.protocol_profile(id).unwrap();
    let (_, device) = serial_test_pair().unwrap();
    let mut serial = SerialProfile::new(
        "Converted serial",
        SerialConnection {
            device,
            ..Default::default()
        },
        TerminalProfile::Xterm,
    );
    serial.id = id;
    let mut changed = common(&scratch.0.join("other-logs"));
    changed.terminal.cursor = Some(Rgb::from_hex(0x000001));
    store
        .save_protocol(
            Some(&original),
            Profile::Serial(Box::new(serial)),
            vec![],
            None,
            Some(changed),
        )
        .unwrap();
    // Resolving before refresh must still detect a replaced active transport.
    assert!(
        workspace
            .saved_terminal_preferences
            .resolve_local(&local, None)
            .is_err()
    );
    workspace.refresh_saved_terminal_preferences(&store);
    assert_eq!(
        workspace.pane_appearance(session).cursor,
        Rgb::from_hex(0xabcdef)
    );
    stop(&mut workspace, session);
}

#[test]
fn native_privacy_and_serial_isolation_policy_reject_before_opening_a_window() {
    let scratch = crate::snippets::tests::Profile::new();
    let mut store = Store::open(scratch.0.join("connections.json")).unwrap();
    let mut settings = common(&scratch.0.join("logs"));
    settings.incognito = true;
    let id = store
        .save_protocol(
            None,
            Profile::Local(Box::new(local())),
            vec![],
            None,
            Some(settings),
        )
        .unwrap();
    let mut workspace = Workspace::default();
    prepare(&mut workspace, &store);
    let (owner, _) = workspace.open_window(false);
    workspace.clipboard.policy.incognito_allowed = false;
    let _ = workspace.open_saved_local_in_new_window(owner, store.local(id).unwrap().clone());
    assert_eq!(workspace.windows.len(), 1);
    assert!(workspace.sessions.is_empty());
    let (peer, device) = serial_test_pair().unwrap();
    let mut settings = common(&scratch.0.join("serial-logs"));
    settings.isolation = crate::session_security::IsolationLevel::Process;
    let serial = SerialProfile::new(
        "Isolated serial",
        SerialConnection {
            device,
            ..Default::default()
        },
        TerminalProfile::Xterm,
    );
    let id = store
        .save_protocol(
            None,
            Profile::Serial(Box::new(serial)),
            vec![],
            None,
            Some(settings),
        )
        .unwrap();
    workspace.refresh_saved_terminal_preferences(&store);
    let _ = workspace.open_saved_serial_in_new_window(owner, store.serial(id).unwrap().clone());
    assert_eq!(workspace.windows.len(), 1);
    assert!(workspace.sessions.is_empty());
    drop(peer);
    assert!(!scratch.0.join("serial-logs").exists());
}

#[test]
fn local_new_window_geometry_does_not_change_an_existing_window() {
    let scratch = crate::snippets::tests::Profile::new();
    let mut store = Store::open(scratch.0.join("connections.json")).unwrap();
    let settings = EditorSettings {
        window_geometry: Some(crate::connection_store::WindowGeometry {
            width: 980,
            height: 670,
            x: 120,
            y: 130,
            maximized: false,
        }),
        ..Default::default()
    };
    let id = store
        .save_protocol(
            None,
            Profile::Local(Box::new(local())),
            vec![],
            None,
            Some(settings),
        )
        .unwrap();
    let mut workspace = Workspace::default();
    prepare(&mut workspace, &store);
    let (owner, _) = workspace.open_window(false);
    let original = workspace.windows[&owner].size;
    let _ = workspace.open_saved_local_in_new_window(owner, store.local(id).unwrap().clone());
    assert_eq!(workspace.windows.len(), 2);
    assert_eq!(workspace.windows[&owner].size, original);
    let (&new, state) = workspace
        .windows
        .iter()
        .find(|(id, _)| **id != owner)
        .unwrap();
    assert_eq!(state.size, iced::Size::new(980.0, 670.0));
    let session = workspace.windows[&new]
        .tab()
        .unwrap()
        .focused_session()
        .unwrap();
    stop(&mut workspace, session);
    let mut cache = SavedTerminalPreferences::default();
    cache.refresh(&store);
    assert_eq!(
        cache
            .resolve_local(store.local(id).unwrap(), None)
            .unwrap()
            .settings
            .window_geometry
            .unwrap()
            .width,
        980
    );
}

#[test]
fn saved_incognito_serial_never_creates_logs_or_usage() {
    let scratch = crate::snippets::tests::Profile::new();
    let folder = scratch.0.join("logs");
    let (mut peer, device) = serial_test_pair().unwrap();
    let mut store = Store::open(scratch.0.join("connections.json")).unwrap();
    let mut settings = common(&folder);
    settings.incognito = true;
    let serial = SerialProfile::new(
        "Private serial",
        SerialConnection {
            device,
            ..Default::default()
        },
        TerminalProfile::Xterm,
    );
    let id = store
        .save_protocol(
            None,
            Profile::Serial(Box::new(serial)),
            vec![],
            None,
            Some(settings),
        )
        .unwrap();
    let mut workspace = Workspace::default();
    prepare(&mut workspace, &store);
    let (owner, _) = workspace.open_window(false);
    let _ = workspace.open_saved_serial(owner, store.serial(id).unwrap().clone());
    let tab = workspace.windows[&owner].tab().unwrap();
    assert!(tab.incognito);
    assert!(tab.reopen_origin.is_none());
    let session = tab.focused_session().unwrap();
    wait(|| workspace.sessions[&session].session.frame().status == Status::Running);
    peer.write_all(
        b"PRIVATE
",
    )
    .unwrap();
    let _ = workspace.record_serial_session_used(session);
    assert!(workspace.connection_usage.pending.is_empty());
    assert!(!workspace.connection_usage.active);
    stop(&mut workspace, session);
    assert!(!folder.exists());
}

#[test]
fn serial_folder_isolation_is_resolved_before_launch() {
    use crate::session_security::IsolationLevel;
    let scratch = crate::snippets::tests::Profile::new();
    let (peer, device) = serial_test_pair().unwrap();
    let mut store = Store::open(scratch.0.join("connections.json")).unwrap();
    let group = store.create_group(None, "Restricted").unwrap();
    store
        .set_group_isolation(&group, None, Some(IsolationLevel::Process))
        .unwrap();
    let settings = EditorSettings {
        inherit_isolation: true,
        ..Default::default()
    };
    let serial = SerialProfile::new(
        "Inherited isolation",
        SerialConnection {
            device,
            ..Default::default()
        },
        TerminalProfile::Xterm,
    );
    let id = store
        .save_protocol(
            None,
            Profile::Serial(Box::new(serial)),
            vec![],
            Some(&group),
            Some(settings),
        )
        .unwrap();
    let mut workspace = Workspace::default();
    prepare(&mut workspace, &store);
    let runtime = workspace
        .saved_terminal_preferences
        .resolve_serial(store.serial(id).unwrap())
        .unwrap();
    assert_eq!(
        workspace.serial_runtime_isolation(&runtime),
        IsolationLevel::Process
    );
    let (owner, _) = workspace.open_window(false);
    let _ = workspace.open_saved_serial(owner, store.serial(id).unwrap().clone());
    assert!(workspace.sessions.is_empty());
    drop(peer);
}
