use super::*;
use crate::session::{GridSize, IbmKind, Session};

fn add_disconnected(workspace: &mut Workspace, owner: window::Id, incognito: bool) -> u64 {
    let session = workspace.store_session(
        Session::disconnected_ibm(IbmKind::Tn3270e),
        GridSize::default(),
        14,
    );
    workspace.add_tab_with_privacy(owner, session, incognito);
    workspace.windows[&owner].tab().unwrap().id
}

fn open_menu(workspace: &mut Workspace, owner: window::Id, tab: u64) -> u64 {
    let _ = workspace.update(Desktop::TabContext(
        owner,
        Message::Open(tab, Point::new(8.0, 8.0)),
    ));
    workspace.tab_context.menu.as_ref().unwrap().request
}

#[test]
fn tab_journal_submenu_keeps_order_keyboard_and_policy_guards() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let tab = add_disconnected(&mut workspace, owner, false);
    let incognito = add_disconnected(&mut workspace, owner, true);
    let root = workspace.tab_context_menu_actions(owner, tab);
    let branch = root
        .iter()
        .position(|(action, _)| *action == Action::JournalMenu)
        .unwrap();
    assert_eq!(root[branch - 1].0, Action::Sftp);
    assert_eq!(root[branch + 1].0, Action::MultiExec);
    assert!(workspace.tab_journal_actions(owner, incognito).is_none());

    let request = open_menu(&mut workspace, owner, tab);
    let step = |workspace: &mut Workspace, key| {
        let _ = workspace.tab_context_key(owner, &Key::Named(key)).unwrap();
    };
    for _ in 0..root.len() {
        step(&mut workspace, Named::ArrowDown);
        if workspace.tab_context.menu.as_ref().unwrap().selected == Some(Action::JournalMenu) {
            break;
        }
    }
    assert_eq!(
        workspace.tab_context.menu.as_ref().unwrap().selected,
        Some(Action::JournalMenu)
    );
    step(&mut workspace, Named::ArrowRight);
    let menu = workspace.tab_context.menu.as_ref().unwrap();
    assert!(menu.journal);
    assert_eq!(
        workspace.tab_context_level_actions(menu),
        [
            (Action::Journal(TabJournalAction::Start(None)), false),
            (Action::Journal(TabJournalAction::Screenshot(None)), false),
            (Action::Journal(TabJournalAction::Note(None)), false),
        ]
    );
    step(&mut workspace, Named::ArrowLeft);
    assert_eq!(
        workspace.tab_context.menu.as_ref().unwrap().selected,
        Some(Action::JournalMenu)
    );
    step(&mut workspace, Named::Enter);
    assert!(workspace.tab_context.menu.as_ref().unwrap().journal);
    workspace.clipboard.policy.session_journal_allowed = false;
    assert!(workspace.tab_journal_actions(owner, tab).is_none());
    let _ = workspace.update(Desktop::TabContext(
        owner,
        Message::Invoke(request, Action::Journal(TabJournalAction::Start(None))),
    ));
    assert!(workspace.journal_captures.is_empty());
    assert_eq!(workspace.windows[&owner].active_tab_id(), Some(incognito));
}

#[test]
fn tab_journal_records_note_and_screenshot_for_the_clicked_background_tab() {
    use std::fs;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use iced::widget::text_editor;
    use kortty_terminal::TerminalProfile;

    use crate::session_journal::{self, Status};
    use crate::workspace::journal_capture_access::Message as CaptureMessage;

    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let scratch = Scratch(std::env::temp_dir().canonicalize().unwrap().join(format!(
        "kortty-tab-journal-{}",
        crate::private_files::nonce().unwrap(),
    )));
    fs::create_dir(&scratch.0).unwrap();
    fn wait(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "journal context-menu fixture did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let mut workspace = Workspace::default();
    workspace.window_settings.session_journal.directory =
        scratch.0.join("journals").to_str().unwrap().into();
    let (owner, _) = workspace.open_window(false);
    let grid = GridSize::default();
    let shell = Session::start(grid, TerminalProfile::Xterm).unwrap();
    wait(|| shell.journal_available());
    let session = workspace.store_session(shell, grid, 14);
    workspace.add_tab(owner, session);
    let background = workspace.windows[&owner].active_tab_id().unwrap();
    let selected = add_disconnected(&mut workspace, owner, false);

    let invoke = |workspace: &mut Workspace, action| {
        let request = open_menu(workspace, owner, background);
        let _ = workspace.update(Desktop::TabContext(owner, Message::OpenJournal(request)));
        let _ = workspace.update(Desktop::TabContext(
            owner,
            Message::Invoke(request, Action::Journal(action)),
        ));
    };
    invoke(&mut workspace, TabJournalAction::Start(None));
    let (TabJournalAction::Stop(run), true) =
        workspace.tab_journal_actions(owner, background).unwrap()[0]
    else {
        panic!("clicked tab did not start recording");
    };
    wait(|| !workspace.journal_metadata_captures().is_empty());
    let capture = workspace.journal_metadata_captures().pop().unwrap();
    // Test doubles for delivery of the real startup and attachment work to Iced.
    let _ = workspace.update(Desktop::JournalCapture(CaptureMessage::Started(
        background, run,
    )));
    let _ = workspace.update(Desktop::JournalCapture(CaptureMessage::Attached(
        background,
        run,
        session,
        Ok(()),
    )));
    let _ = workspace.update(Desktop::SessionUpdate(session));
    assert_eq!(workspace.windows[&owner].active_tab_id(), Some(selected));
    assert!(
        workspace
            .tab_journal_actions(owner, selected)
            .unwrap()
            .iter()
            .all(|(_, enabled)| !enabled)
    );

    invoke(&mut workspace, TabJournalAction::Note(Some(run)));
    let dialog = workspace.tab_journal_note_for_owner(owner).unwrap();
    assert_eq!(workspace.owner_dialog(owner).unwrap().0, dialog);
    assert_eq!(
        workspace.tab_journal_note_title(dialog),
        Some(JOURNAL_NOTE_TITLE)
    );
    let input = text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(
        "Note for the clicked tab".into(),
    )));
    let _ = workspace.update(Desktop::TabContext(
        dialog,
        Message::JournalNote(Note::Input(input)),
    ));
    let _ = workspace.update(Desktop::TabContext(
        dialog,
        Message::JournalNote(Note::Save),
    ));
    assert!(workspace.tab_journal_note_for_owner(owner).is_none());
    assert_eq!(workspace.windows[&owner].active_tab_id(), Some(selected));
    invoke(&mut workspace, TabJournalAction::Screenshot(Some(run)));
    wait(|| {
        session_journal::read(capture.directory()).is_ok_and(|journal| {
            journal
                .timeline
                .iter()
                .any(|entry| entry.screenshot.is_some())
        })
    });
    invoke(&mut workspace, TabJournalAction::Stop(run));
    wait(|| capture.done().is_closed());
    assert_eq!(capture.status(), Status::Complete);
    let journal = session_journal::read(capture.directory()).unwrap();
    assert!(
        journal
            .timeline
            .iter()
            .any(|entry| entry.kind == "USER_NOTE" && entry.text == "Note for the clicked tab")
    );
    assert!(
        journal
            .timeline
            .iter()
            .any(|entry| entry.kind == "SCREENSHOT")
    );
    assert_eq!(workspace.windows[&owner].active_tab_id(), Some(selected));
    let done = workspace.sessions[&session].session.stop();
    wait(|| done.is_closed());
}

#[test]
fn menu_keeps_reference_order_and_rejects_stale_owner_actions() {
    let mut workspace = Workspace::default();
    let (source, _) = workspace.open_window(false);
    let (other, _) = workspace.open_window(false);
    let first = add_disconnected(&mut workspace, source, false);
    let second = add_disconnected(&mut workspace, source, false);
    assert_eq!(
        workspace.tab_context_actions(source, first),
        [
            (Action::Rename, true),
            (Action::Duplicate, false),
            (Action::Reconnect, true),
            (Action::Sftp, false),
            (Action::MultiExec, false),
            (Action::CloseOthers, true),
            (Action::CloseRight, true),
            (Action::ReopenClosed, false),
        ],
    );
    assert_eq!(
        workspace.tab_context_actions(source, second)[6],
        (Action::CloseRight, false),
    );
    let stale = open_menu(&mut workspace, source, first);
    assert_eq!(workspace.windows[&source].tab().unwrap().id, second);
    let _ = workspace.transfer_at(source, Some(other), first, None);
    let _ = workspace.update(Desktop::TabContext(
        source,
        Message::Invoke(stale, Action::CloseOthers),
    ));
    assert!(workspace.window_has_tab(source, second));
    assert!(workspace.window_has_tab(other, first));
    assert!(workspace.tab_context.menu.is_none());
}

#[test]
fn multi_exec_targets_the_clicked_tab_and_rejects_its_old_owner_after_a_move() {
    use kortty_terminal::TerminalProfile;

    let mut workspace = Workspace::default();
    let (source, _) = workspace.open_window(false);
    let (destination, _) = workspace.open_window(false);
    let mut add_tab = || {
        let grid = GridSize::default();
        let session = Session::ssh(None, grid, TerminalProfile::Xterm, true, None).unwrap();
        let session = workspace.store_session(session, grid, 14);
        workspace.add_tab(source, session);
        workspace.windows[&source].tab().unwrap().id
    };
    let first = add_tab();
    let focused = add_tab();

    let request = open_menu(&mut workspace, source, first);
    let _ = workspace.update_tab_context(source, Message::Invoke(request, Action::MultiExec));
    assert!(workspace.multi_exec_checked(source, MultiExecAction::TabAt(first)));
    assert!(!workspace.multi_exec_checked(source, MultiExecAction::TabAt(focused)));
    assert_eq!(workspace.windows[&source].active_tab_id(), Some(focused));

    let stale = open_menu(&mut workspace, source, first);
    let _ = workspace.transfer_at(source, Some(destination), first, None);
    let _ = workspace.update_tab_context(source, Message::Invoke(stale, Action::MultiExec));
    assert!(workspace.multi_exec_checked(destination, MultiExecAction::TabAt(first)));

    let request = open_menu(&mut workspace, destination, first);
    let _ = workspace.update_tab_context(destination, Message::Invoke(request, Action::MultiExec));
    assert!(!workspace.multi_exec_checked(destination, MultiExecAction::TabAt(first)));
}

#[test]
fn rename_normalizes_text_persists_by_tab_and_resets_to_automatic() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let named = add_disconnected(&mut workspace, owner, false);
    let secret = add_disconnected(&mut workspace, owner, true);
    let rename = |workspace: &mut Workspace, target, name: &str| {
        let request = open_menu(workspace, owner, target);
        let _ = workspace.update(Desktop::TabContext(
            owner,
            Message::Invoke(request, Action::Rename),
        ));
        let id = workspace.rename_dialog_for_owner(owner).unwrap();
        let _ = workspace.update(Desktop::TabContext(id, Message::RenameInput(name.into())));
        let _ = workspace.update(Desktop::TabContext(id, Message::RenameFinish(true)));
    };
    rename(&mut workspace, named, "  Safe\r\nTab\u{202e}\u{1b}  ");
    rename(&mut workspace, secret, "Incognito custom title");
    assert_eq!(workspace.windows[&owner].tab().unwrap().id, secret);
    let layout = workspace.capture_layout().unwrap();
    assert_eq!(layout.windows[0].tabs.len(), 1);
    assert_eq!(
        layout.windows[0].tab_titles.get(&0).map(String::as_str),
        Some("Safe Tab")
    );
    let json = serde_json::to_string(&layout).unwrap();
    assert!(!json.contains("Incognito custom title"));
    let layout: profile::WorkspaceLayout = serde_json::from_str(&json).unwrap();
    let (restored, _) = workspace.restore_layout_into(&layout, None).unwrap();
    assert_eq!(
        workspace
            .tab_names(workspace.windows[&restored].tab().unwrap())
            .0,
        "Safe Tab"
    );
    let original = workspace.windows[&owner]
        .terminal_tabs()
        .find(|tab| tab.id == named)
        .unwrap();
    let automatic = workspace.automatic_tab_names(original).0;
    rename(&mut workspace, named, &automatic);
    assert!(
        workspace.windows[&owner].tabs[0]
            .terminal()
            .unwrap()
            .custom_title
            .is_none()
    );
    assert_eq!(
        profile::normalize_tab_title(&"😀".repeat(121))
            .chars()
            .count(),
        120
    );
}

#[test]
fn tab_groups_follow_the_clicked_tab_through_dialogs_transfer_and_restore() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let (destination, _) = workspace.open_window(false);
    let first = add_disconnected(&mut workspace, owner, false);
    let second = add_disconnected(&mut workspace, owner, false);
    let active = add_disconnected(&mut workspace, owner, false);
    let private = add_disconnected(&mut workspace, owner, true);
    let _ = workspace.update(Desktop::SelectTab(owner, active));
    let session = workspace
        .terminal_tab(owner, first)
        .unwrap()
        .focused_session();
    let invoke = |workspace: &mut Workspace, owner, tab, action| {
        let request = open_menu(workspace, owner, tab);
        let _ = workspace.update(Desktop::TabContext(owner, Message::Invoke(request, action)));
    };
    let edit = |workspace: &mut Workspace, owner, value: &str, commit| {
        let dialog = workspace.rename_dialog_for_owner(owner).unwrap();
        let _ = workspace.update(Desktop::TabContext(
            dialog,
            Message::RenameInput(value.into()),
        ));
        let _ = workspace.update(Desktop::TabContext(dialog, Message::RenameFinish(commit)));
    };
    invoke(&mut workspace, owner, first, Action::NewGroup);
    edit(&mut workspace, owner, "Zulu", true);
    invoke(
        &mut workspace,
        owner,
        second,
        Action::AssignGroup("Zulu".into()),
    );
    invoke(&mut workspace, owner, private, Action::NewGroup);
    edit(&mut workspace, owner, "Only Incognito", true);
    assert!(
        !workspace
            .tab_group_actions(owner, active)
            .iter()
            .any(|(action, _)| { *action == Action::AssignGroup("Only Incognito".into()) })
    );
    invoke(&mut workspace, owner, second, Action::RenameGroup);
    edit(&mut workspace, owner, "Canceled", false);
    invoke(&mut workspace, owner, second, Action::RenameGroup);
    edit(&mut workspace, owner, "  Alpha\u{202e} ", true);
    let window = &workspace.windows[&owner];
    let order = window.terminal_tabs().map(|tab| tab.id).collect::<Vec<_>>();
    assert_eq!(order, [active, second, first, private]);
    assert_eq!(window.active_tab_id(), Some(active));
    for id in [first, second] {
        assert_eq!(
            workspace.terminal_tab(owner, id).unwrap().group.as_deref(),
            Some("Alpha")
        );
    }

    let ungrouped = add_disconnected(&mut workspace, destination, false);
    let stale = open_menu(&mut workspace, owner, first);
    let _ = workspace.transfer(owner, Some(destination), first);
    let _ = workspace.update(Desktop::TabContext(
        owner,
        Message::Invoke(stale, Action::NoGroup),
    ));
    let tab = workspace.terminal_tab(destination, first).unwrap();
    assert_eq!(tab.group.as_deref(), Some("Alpha"));
    assert_eq!(tab.focused_session(), session);
    assert_eq!(workspace.windows[&destination].active_tab_id(), Some(first));
    invoke(&mut workspace, owner, second, Action::NoGroup);
    assert!(
        workspace
            .terminal_tab(owner, second)
            .unwrap()
            .group
            .is_none()
    );

    let layout = workspace.capture_layout_ordered(Some(destination)).unwrap();
    assert_eq!(layout.windows[0].active, 1);
    assert_eq!(workspace.windows[&destination].tabs[0].id(), ungrouped);
    let bytes = serde_json::to_vec(&layout).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("Only Incognito"));
    let layout: profile::WorkspaceLayout = serde_json::from_slice(&bytes).unwrap();
    let (restored, _) = workspace.restore_layout_into(&layout, None).unwrap();
    let tab = workspace.windows[&restored].tab().unwrap();
    assert_eq!(tab.group.as_deref(), Some("Alpha"));
}

#[test]
fn saved_tab_group_changes_only_when_the_connection_folder_changes() {
    use crate::connection_store::{Auth, Connection, SavedLogin, Store};
    use crate::ssh_dialog::LoginTarget;

    let scratch = crate::snippets::tests::Profile::new();
    let mut store = Store::open(scratch.0.join("connections.json")).unwrap();
    let parent = store.create_group(None, &"P".repeat(96)).unwrap();
    let common = "s".repeat(23);
    let folder = store
        .create_group(Some(&parent), &format!("{common}-Production"))
        .unwrap();
    let destination = store
        .create_group(Some(&parent), &format!("{common}-Operations"))
        .unwrap();
    assert_eq!(&folder[..120], &destination[..120]);
    let login = LoginTarget {
        host: "fixture.invalid".into(),
        port: 22,
        username: "test".into(),
    };
    let saved_id = store
        .save_in_group(
            Connection {
                id: 0,
                name: "A saved connection".into(),
                tag: String::new(),
                login: SavedLogin {
                    endpoint: login.clone(),
                    auth: Auth::Prompt,
                    keyboard_interactive: false,
                },
                jump: None,
                emulation: "xterm-256color".into(),
                tunnels: Vec::new(),
                incognito: false,
                isolation: Default::default(),
                startup: Default::default(),
                logging: Default::default(),
                terminal_effect: None,
                command_timestamps: None,
                tab_color: None,
                paste_warning: None,
            },
            Some(&folder),
        )
        .unwrap();
    let mut target = serde_json::to_value(login).unwrap();
    target["saved"] = saved_id.into();
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let tab = add_disconnected(&mut workspace, owner, false);
    let session = workspace
        .terminal_tab(owner, tab)
        .unwrap()
        .focused_session()
        .unwrap();
    workspace.sessions.get_mut(&session).unwrap().ssh =
        Some(serde_json::from_value(target).unwrap());
    workspace.apply_saved_tab_groups(&store);
    assert_eq!(
        workspace.terminal_tab(owner, tab).unwrap().group.as_deref(),
        Some(folder.as_str())
    );

    workspace.set_tab_group(owner, tab, Some("Manual group".into()));
    workspace.apply_saved_tab_groups(&store);
    assert_eq!(
        workspace.terminal_tab(owner, tab).unwrap().group.as_deref(),
        Some("Manual group")
    );
    let connection = store.get(saved_id).unwrap().clone();
    store.save_in_group(connection, Some(&destination)).unwrap();
    workspace.apply_saved_tab_groups(&store);
    assert_eq!(
        workspace.terminal_tab(owner, tab).unwrap().group.as_deref(),
        Some(destination.as_str())
    );
    assert!(workspace.capture_layout().unwrap().validate().is_ok());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn serial_batch_asks_once_and_does_not_close_tabs_added_after_confirmation() {
    use std::time::{Duration, Instant};

    use kortty_terminal::TerminalProfile;
    use rustix::io::{FdFlags, fcntl_setfd};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

    use crate::session::{SerialConnection, Status};

    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let anchor = add_disconnected(&mut workspace, owner, false);
    let peer = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    fcntl_setfd(&peer, FdFlags::CLOEXEC).unwrap();
    grantpt(&peer).unwrap();
    unlockpt(&peer).unwrap();
    let device = ptsname(&peer, Vec::new()).unwrap().into_string().unwrap();
    let session = Session::start_serial(
        SerialConnection {
            device: device.into(),
            ..SerialConnection::default()
        },
        GridSize::default(),
        TerminalProfile::Xterm,
        true,
        None,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    while session.frame().status != Status::Running {
        assert!(
            Instant::now() < deadline,
            "serial fixture failed: {:?}",
            session.frame().status
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let session = workspace.store_session(session, GridSize::default(), 14);
    workspace.add_tab(owner, session);
    let serial = workspace.windows[&owner].tab().unwrap().id;
    let sibling = add_disconnected(&mut workspace, owner, false);
    let invoke = |workspace: &mut Workspace| {
        let request = open_menu(workspace, owner, anchor);
        let _ = workspace.update(Desktop::TabContext(
            owner,
            Message::Invoke(request, Action::CloseRight),
        ));
        let Some((token, SerialClose::TabBatch(_))) =
            workspace.windows[&owner].pending_serial_close
        else {
            panic!("opening serial fixture did not require one batch confirmation");
        };
        token
    };
    let token = invoke(&mut workspace);
    let _ = workspace.update(Desktop::ConfirmSerialClose(owner, token + 1, true));
    assert!(workspace.window_has_tab(owner, serial));
    assert!(workspace.window_has_tab(owner, sibling));
    let _ = workspace.update(Desktop::ConfirmSerialClose(owner, token, false));
    assert!(!workspace.tab_context.closing.contains_key(&owner));
    assert!(workspace.window_has_tab(owner, serial));

    let token = invoke(&mut workspace);
    let new = add_disconnected(&mut workspace, owner, false);
    let _ = workspace.update(Desktop::ConfirmSerialClose(owner, token, true));
    assert_eq!(workspace.windows[&owner].tab().unwrap().id, anchor);
    assert!(!workspace.window_has_tab(owner, serial));
    assert!(!workspace.window_has_tab(owner, sibling));
    assert!(workspace.window_has_tab(owner, new));
    assert!(!workspace.sessions.contains_key(&session));
}

#[cfg(unix)]
#[test]
fn tab_watch_real_output_marks_a_moved_tab_and_focus_clears_it() {
    use kortty_terminal::TerminalProfile;

    use std::{
        thread,
        time::{Duration, Instant},
    };

    let mut workspace = Workspace::default();
    workspace.window_settings.terminal_silence_seconds = 5;
    let (owner, _) = workspace.open_window(false);
    let (destination, _) = workspace.open_window(false);
    let grid = GridSize::default();
    let session = workspace.store_session(
        Session::start(grid, TerminalProfile::Xterm).unwrap(),
        grid,
        14,
    );
    workspace.add_tab(owner, session);
    let tab = workspace.windows[&owner].active_tab_id().unwrap();
    let request = open_menu(&mut workspace, owner, tab);
    let _ = workspace.update(Desktop::TabContext(
        owner,
        Message::Invoke(request, Action::Monitor(WatchKind::Silence)),
    ));
    assert!(
        workspace.windows[&owner]
            .tab()
            .unwrap()
            .watch
            .enabled(WatchKind::Silence)
    );
    let _ = workspace.transfer(owner, Some(destination), tab);

    let terminal = &workspace.sessions[&session].session;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let frame = terminal.frame();
        if frame
            .screen
            .row(0)
            .is_some_and(|row| row.iter().any(|cell| !cell.text.trim().is_empty()))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no output from local shell ({})",
            crate::workspace::tests::describe_test_pty(terminal, &frame),
        );
        thread::sleep(Duration::from_millis(5));
    }
    terminal
        .send(b"printf '%s%s\\n' 'watch-' 'fixture'\r".to_vec())
        .unwrap();
    while !terminal
        .frame()
        .screen
        .cells
        .iter()
        .map(|cell| cell.text.as_str())
        .collect::<String>()
        .contains("watch-fixture")
    {
        assert!(
            Instant::now() < deadline,
            "fixture output did not reach the terminal ({})",
            crate::workspace::tests::describe_test_pty(terminal, &terminal.frame()),
        );
        thread::sleep(Duration::from_millis(5));
    }
    let after_output = Instant::now();
    let _ = workspace.poll_tab_watches(after_output);
    let request = workspace.poll_tab_watches(after_output + Duration::from_secs(5));
    assert_eq!(request.units(), 1);
    let tab = workspace.windows[&destination].tab().unwrap();
    assert_eq!(
        tab.watch.attention_text().as_deref(),
        Some("No output in this tab for 5 seconds.")
    );
    let _ = workspace.update(Desktop::Window(destination, window::Event::Focused));
    assert!(
        workspace.windows[&destination]
            .tab()
            .unwrap()
            .watch
            .attention_text()
            .is_none()
    );
    let _ = workspace.sessions[&session].session.stop().recv_blocking();
}
