use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::profile::{PaneLayout, SavedPosition};
use crate::session::{IbmKind, SerialConnection, SerialLineEnding, SerialSettings, Status};

use super::*;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = env::temp_dir().join(format!(
                "kortty-session-recovery-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
    }

    fn path(&self) -> PathBuf {
        self.0.join("session")
    }

    fn open(&self) -> io::Result<Store> {
        Store::open_at(self.path())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn ibm(font: u8, focused: bool) -> PaneLayout {
    PaneLayout::Ibm {
        font,
        focused,
        kind: IbmKind::Tn3270e,
    }
}

fn single(font: u8) -> WorkspaceLayout {
    WorkspaceLayout::new(vec![WindowLayout {
        tab_titles: Default::default(),
        tab_groups: Default::default(),
        tab_primary_panes: Default::default(),
        tab_terminal_effects: Default::default(),
        tab_terminal_appearance: Default::default(),
        width: 880.0,
        height: 560.0,
        position: None,
        maximized: false,
        dashboard_visible: Some(false),
        active: 0,
        tabs: vec![ibm(font, true)],
    }])
}

fn written(layout: WorkspaceLayout) -> Snapshot {
    Snapshot {
        layout,
        clean_exit: true,
        output: None,
    }
}

#[test]
fn encrypted_session_output_restores_to_exact_panes_and_managed_denial_purges() -> io::Result<()> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let scratch = Scratch::new()?;
    let path = scratch.0.join("vault.bin");
    let vault = kortty_vault::Vault::create(&path, b"private recovery test password").unwrap();
    let mut layout = single(13);
    layout.windows[0].tabs.push(ibm(17, true));
    let mut snapshot = written(layout);
    snapshot.output = Some(
        STANDARD.encode(
            vault
                .seal_recovery_output(
                    &serde_json::to_vec(&snapshot.layout)?,
                    br#"["first pane fixture","second pane fixture"]"#,
                )
                .unwrap(),
        ),
    );
    let store = scratch.open()?;
    store.write(Some(&snapshot))?;
    let bytes = fs::read_to_string(scratch.path().join("last-session.json"))?;
    assert!(!bytes.contains("pane fixture"));
    let mut workspace = Workspace {
        vault: Some(crate::vault_window::VaultWindow::with_unlocked_fixture(
            path, vault,
        )),
        session_restore: Restore::load_from(store, None, true)?,
        ..Workspace::default()
    };
    workspace.window_settings.restore_output.enabled = true;
    let (id, _) = workspace.open_window(false);
    let _ = workspace.update_session_restore(Message::Restore(id));
    for pane in workspace.sessions.values() {
        assert_eq!(
            pane.restored_output.as_ref().map(|text| text.as_str()),
            Some(if pane.font == 13 {
                "first pane fixture"
            } else {
                "second pane fixture"
            })
        );
    }
    assert_eq!(workspace.sessions.len(), 2);
    workspace.clipboard.policy.session_restore_output = Some(false);
    workspace.reconcile_session_output(false, None);
    assert!(workspace.window_settings.restore_output.enabled);
    assert!(
        workspace
            .sessions
            .values()
            .all(|pane| pane.restored_output.is_none())
    );
    workspace
        .session_restore
        .finish(None, None)
        .unwrap()
        .recv_blocking()
        .unwrap();
    assert!(scratch.open()?.previous()?.unwrap().output.is_none());
    Ok(())
}

#[test]
fn startup_asks_by_default_and_downgrades_auto_after_an_interruption() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let store = scratch.open()?;
    assert_eq!(store.restore_mode()?, RestoreMode::Ask);
    store.write(Some(&written(single(13))))?;

    let mut restore = Restore::load_from(store, None, false)?;
    assert!(restore.offered && !restore.automatic);
    restore.finish(None, None).unwrap().recv_blocking().unwrap();

    let store = scratch.open()?;
    store.save_mode(RestoreMode::Auto)?;
    store.write(Some(&written(single(13))))?;
    let mut restore = Restore::load_from(store, None, false)?;
    assert!(restore.automatic && !restore.offered);
    // Simulate ending after startup without an accepted clean snapshot.
    restore.finish(None, None).unwrap().recv_blocking().unwrap();

    let store = scratch.open()?;
    assert!(!store.previous()?.unwrap().clean_exit);
    let mut restore = Restore::load_from(store, None, false)?;
    assert!(restore.offered && !restore.automatic);
    assert!(restore.previous.is_some());
    restore.finish(None, None).unwrap().recv_blocking().unwrap();
    Ok(())
}

#[test]
fn auto_startup_opens_the_saved_workspace_without_an_extra_default_tab() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let store = scratch.open()?;
    let expected = single(21);
    store.save_mode(RestoreMode::Auto)?;
    store.write(Some(&written(expected.clone())))?;
    let explicit = scratch.0.join("layout.json");
    fs::write(&explicit, "explicit layout remains separate")?;

    let mut workspace = Workspace {
        session_restore: Restore::load_from(store, None, false)?,
        ..Workspace::default()
    };
    let (id, _opened) = workspace.startup_session_window();
    assert_eq!(workspace.windows.len(), 1);
    assert_eq!(workspace.windows[&id].tabs.len(), 1);
    assert_eq!(
        workspace.capture_session_layout(),
        Some((expected.clone(), None))
    );
    assert!(workspace.session_restore.previous.is_none());
    assert_eq!(workspace.sessions.len(), 1);
    assert_eq!(
        workspace.sessions.values().next().unwrap().frame.status,
        Status::Disconnected
    );
    workspace
        .session_restore
        .finish(Some(expected), None)
        .unwrap()
        .recv_blocking()
        .unwrap();
    assert_eq!(
        fs::read_to_string(explicit)?,
        "explicit layout remains separate"
    );
    Ok(())
}

#[test]
fn managed_startup_restore_preserves_manual_restore_and_the_saved_preference() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let store = scratch.open()?;
    store.save_mode(RestoreMode::Auto)?;
    let expected = single(21);
    store.write(Some(&written(expected.clone())))?;
    let mut workspace = Workspace {
        session_restore: Restore::load_from(store, Some(RestoreMode::Off), false)?,
        ..Workspace::default()
    };
    workspace.clipboard.policy.session_restore_mode = Some(RestoreMode::Off);
    assert_eq!(
        workspace.session_restore.preference(),
        Some(RestoreMode::Off)
    );
    assert!(workspace.session_restore.can_restore());
    assert!(!workspace.session_restore.offered && !workspace.session_restore.automatic);
    let _ = workspace.update_session_restore(Message::Mode(RestoreMode::Ask));
    assert_eq!(
        workspace.session_restore.preference(),
        Some(RestoreMode::Off)
    );

    let (id, _) = workspace.open_window(false);
    let _ = workspace.update_session_restore(Message::Restore(id));
    assert_eq!(
        workspace.capture_session_layout(),
        Some((expected.clone(), None))
    );
    workspace
        .session_restore
        .finish(Some(expected), None)
        .unwrap()
        .recv_blocking()
        .unwrap();

    let store = scratch.open()?;
    assert_eq!(store.restore_mode()?, RestoreMode::Auto);
    let mut ask = Restore::load_from(store, Some(RestoreMode::Ask), false)?;
    assert!(ask.offered && !ask.automatic);
    ask.finish(None, None).unwrap().recv_blocking().unwrap();

    let store = scratch.open()?;
    let mut interrupted = Restore::load_from(store, Some(RestoreMode::Auto), false)?;
    assert!(interrupted.offered && !interrupted.automatic);
    interrupted
        .finish(None, None)
        .unwrap()
        .recv_blocking()
        .unwrap();
    Ok(())
}

#[test]
fn one_writer_flushes_the_latest_layout_and_restore_mode_before_releasing_the_lock()
-> io::Result<()> {
    let scratch = Scratch::new()?;
    let mut restore = Restore::load_from(scratch.open()?, None, false)?;
    assert!(scratch.open().is_err());
    restore.writer.as_ref().unwrap().mode(RestoreMode::Off);
    restore.save_layout(single(14), None);
    restore.save_layout(single(15), None);
    let final_layout = single(18);
    restore
        .finish(Some(final_layout.clone()), None)
        .unwrap()
        .recv_blocking()
        .unwrap();

    let store = scratch.open()?;
    assert_eq!(store.previous()?, Some(written(final_layout)));
    assert_eq!(store.restore_mode()?, RestoreMode::Off);
    drop(store);

    let mut restore = Restore::load_from(scratch.open()?, None, false)?;
    assert!(!restore.offered && !restore.automatic);
    assert!(restore.previous.is_some());
    restore.finish(None, None).unwrap().recv_blocking().unwrap();
    Ok(())
}

#[test]
fn missing_or_invalid_files_do_not_replace_a_previous_snapshot() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let store = scratch.open()?;
    assert!(store.previous()?.is_none());
    let bad = scratch.path().join("last-session.json");
    fs::write(&bad, "invalid JSON")?;
    assert!(Restore::load_from(store, None, false).is_err());
    assert_eq!(fs::read_to_string(&bad)?, "invalid JSON");
    Ok(())
}

#[cfg(unix)]
#[test]
fn recovery_never_follows_a_snapshot_symlink() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let store = scratch.open()?;
    let outside = scratch.0.join("outside.json");
    let snapshot = serde_json::to_vec(&written(single(13)))?;
    fs::write(&outside, &snapshot)?;
    std::os::unix::fs::symlink(&outside, scratch.path().join("last-session.json"))?;
    assert!(Restore::load_from(store, None, false).is_err());
    assert_eq!(fs::read(outside)?, snapshot);
    Ok(())
}

#[test]
fn restores_two_windows_once_with_safe_offline_panes_and_saved_focus() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let store = scratch.open()?;
    let target = serde_json::from_value(serde_json::json!({
        "host": "127.0.0.1", "port": 22, "username": "fixture",
    }))?;
    let connection = SerialConnection {
        device: if cfg!(windows) {
            "COM42".into()
        } else {
            "/dev/kortty-recovery-fixture".into()
        },
        settings: SerialSettings::default(),
        line_ending: SerialLineEnding::CrLf,
        local_echo: true,
        encoding: Some(kortty_terminal::TextEncoding::Windows1252),
    };
    let layout = WorkspaceLayout::new(vec![
        WindowLayout {
            tab_titles: Default::default(),
            tab_groups: Default::default(),
            tab_primary_panes: Default::default(),
            tab_terminal_effects: Default::default(),
            tab_terminal_appearance: Default::default(),
            width: 880.0,
            height: 560.0,
            position: SavedPosition::new(100.0, 80.0),
            maximized: false,
            dashboard_visible: None,
            active: 1,
            tabs: vec![
                ibm(13, true),
                PaneLayout::Split {
                    vertical: true,
                    ratio: 0.4,
                    first: Box::new(PaneLayout::Ssh {
                        font: 16,
                        focused: false,
                        term: "xterm-256color".into(),
                        target,
                        initial_size: Default::default(),
                    }),
                    second: Box::new(PaneLayout::Serial {
                        font: 17,
                        focused: true,
                        term: "vt100".into(),
                        connection: connection.clone(),
                    }),
                },
            ],
        },
        single(19).windows.remove(0),
    ]);
    store.write(Some(&written(layout)))?;
    let mut workspace = Workspace {
        session_restore: Restore::load_from(store, None, false)?,
        ..Workspace::default()
    };
    let (first, _) = workspace.open_window(false);
    let _ = workspace.update_session_restore(Message::Restore(first));
    assert_eq!(workspace.windows.len(), 2);
    assert_eq!(workspace.windows[&first].tabs.len(), 2);
    assert_eq!(workspace.windows[&first].placement().width, 880.0);
    let active = workspace.windows[&first].tab().unwrap();
    assert_eq!(active.grid.len(), 2);
    let focused = &workspace.sessions[&active.focused_session().unwrap()];
    assert_eq!(focused.frame.serial.as_deref(), Some(&connection));
    assert_eq!(focused.font, 17);
    assert_eq!(workspace.sessions.len(), 4);
    assert!(
        workspace
            .sessions
            .values()
            .all(|pane| pane.frame.status == Status::Disconnected)
    );
    assert!(workspace.sessions.values().any(|pane| pane.ssh.is_some()));
    assert!(
        workspace
            .sessions
            .values()
            .any(|pane| pane.frame.ibm.is_some())
    );
    let _ = workspace.update_session_restore(Message::Restore(first));
    assert_eq!(workspace.windows.len(), 2);

    let captured = workspace.capture_session_layout().unwrap();
    workspace
        .session_restore
        .finish(Some(captured.0), captured.1)
        .unwrap()
        .recv_blocking()
        .unwrap();
    let bytes = fs::read_to_string(scratch.path().join("last-session.json"))?;
    for unexpected in ["password", "plaintext", "private_key", "scrollback"] {
        assert!(!bytes.contains(unexpected));
    }
    Ok(())
}

#[test]
fn closing_a_window_preserves_its_tabs_but_closing_the_last_tab_clears_recovery() -> io::Result<()>
{
    let scratch = Scratch::new()?;
    let store = scratch.open()?;
    store.write(Some(&written(single(13))))?;
    let mut workspace = Workspace {
        session_restore: Restore::load_from(store, None, false)?,
        ..Workspace::default()
    };
    let (id, _) = workspace.open_window(false);
    let _ = workspace.update_session_restore(Message::Restore(id));

    // Exercise the pre-close capture without opening a native window.
    workspace.preserve_closing_session_window(id);
    let fallback = workspace
        .session_restore
        .final_write(Some(WorkspaceLayout::new(vec![])), None);
    assert_eq!(fallback.unwrap(), Some(written(single(13))));

    let tab = workspace.windows[&id].tab().unwrap().id;
    workspace.close_tab(id, tab);
    let captured = workspace.capture_session_layout().map(|(layout, _)| layout);
    assert!(captured.as_ref().unwrap().windows.is_empty());
    workspace
        .session_restore
        .finish(captured, None)
        .unwrap()
        .recv_blocking()
        .unwrap();
    assert!(scratch.open()?.previous()?.is_none());
    Ok(())
}

#[test]
fn a_pending_recovery_checkpoint_is_still_taken_during_scheduler_drain() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let store = scratch.open()?;
    store.write(Some(&written(single(13))))?;
    let mut workspace = Workspace {
        session_restore: Restore::load_from(store, None, false)?,
        ..Workspace::default()
    };
    let (id, _) = workspace.open_window(false);
    let _ = workspace.update_session_restore(Message::Restore(id));
    workspace.sessions.values_mut().next().unwrap().font = 18;
    workspace.scheduler_draining = true;
    let _ = workspace.update(WorkspaceMessage::SessionRestore(Message::Worker(
        Event::Capture,
    )));
    assert_eq!(
        workspace.session_restore.latest,
        Some(Snapshot {
            layout: single(18),
            clean_exit: false,
            output: None,
        })
    );
    workspace
        .session_restore
        .finish(None, None)
        .unwrap()
        .recv_blocking()
        .unwrap();
    Ok(())
}

#[test]
fn failed_second_window_rolls_back_staged_workers_and_keeps_the_snapshot_for_retry()
-> io::Result<()> {
    let scratch = Scratch::new()?;
    let store = scratch.open()?;
    let connection = SerialConnection {
        device: if cfg!(windows) {
            "COM42".into()
        } else {
            "/dev/kortty-restore-rollback-test".into()
        },
        settings: SerialSettings::default(),
        line_ending: SerialLineEnding::CrLf,
        local_echo: false,
        encoding: None,
    };
    let mut layout = single(13);
    layout.windows.push(WindowLayout {
        tab_titles: Default::default(),
        tab_groups: Default::default(),
        tab_primary_panes: Default::default(),
        tab_terminal_effects: Default::default(),
        tab_terminal_appearance: Default::default(),
        width: 920.0,
        height: 620.0,
        position: None,
        maximized: false,
        dashboard_visible: None,
        active: 0,
        tabs: vec![PaneLayout::Split {
            vertical: true,
            ratio: 0.5,
            first: Box::new(PaneLayout::Serial {
                font: 14,
                focused: false,
                term: "vt100".into(),
                connection,
            }),
            second: Box::new(PaneLayout::Terminal {
                font: 14,
                focused: true,
                vt100: false,
                term: None,
                security: None,
                initial_size: Default::default(),
            }),
        }],
    });
    let original = written(layout);
    store.write(Some(&original))?;
    let mut workspace = Workspace {
        session_restore: Restore::load_from(store, None, false)?,
        ..Workspace::default()
    };
    let (existing, _) = workspace.open_window(false);
    let geometry = workspace.windows[&existing].placement();
    // The first window and first leaf of the second window stage normally. A
    // rejected local start exercises the same rollback path as a spawn failure.
    workspace.clipboard.policy.problem = Some("invalid policy fixture".into());
    let failure = workspace.restore_previous_layout(Some(existing));
    assert!(matches!(failure, Err(error) if error.contains("Sessions are locked")));
    assert_eq!(workspace.windows.len(), 1);
    assert!(workspace.windows[&existing].tabs.is_empty());
    assert_eq!(workspace.windows[&existing].placement(), geometry);
    assert!(workspace.sessions.is_empty());
    assert!(!workspace.pending_shutdown.is_empty());
    assert_eq!(workspace.session_restore.previous.as_ref(), Some(&original));
    assert!(workspace.session_restore.offered);

    workspace
        .session_restore
        .finish(Some(WorkspaceLayout::new(vec![])), None)
        .unwrap()
        .recv_blocking()
        .unwrap();
    assert_eq!(
        scratch.open()?.previous()?,
        Some(Snapshot {
            clean_exit: false,
            ..original
        })
    );
    Ok(())
}

#[test]
fn writer_flushes_a_history_only_profile_and_clears_it_without_a_layout() -> io::Result<()> {
    use crate::profile::session::{ClosedEntry, ClosedTab, RecentlyClosed};

    let scratch = Scratch::new()?;
    let stored = RecentlyClosed {
        entries: vec![ClosedEntry {
            window: true,
            tabs: vec![ClosedTab {
                saved_id: 7,
                custom_title: Some("deploy".into()),
                group: Some("Operations".into()),
            }],
        }],
    };
    let mut restore = Restore::load_from(scratch.open()?, None, false)?;
    restore.save_recently_closed(stored.clone());
    restore.finish(None, None).unwrap().recv_blocking().ok();
    let store = scratch.open()?;
    assert_eq!(store.recently_closed()?, stored);
    assert!(store.previous()?.is_none());

    let mut restore = Restore::load_from(store, None, false)?;
    assert!(!restore.can_restore());
    assert_eq!(restore.loaded_history.as_ref(), Some(&stored));
    restore.save_recently_closed(RecentlyClosed::default());
    restore.finish(None, None).unwrap().recv_blocking().ok();
    let store = scratch.open()?;
    assert!(store.recently_closed()?.entries.is_empty());
    assert!(store.previous()?.is_none());
    Ok(())
}
