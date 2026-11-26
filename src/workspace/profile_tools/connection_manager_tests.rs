use crate::connection_store::{Auth, SavedLogin};
use crate::connection_window::{Edit, Message as Profile, Persisted};
use iced::futures::{StreamExt, executor::block_on};

use super::*;

impl Workspace {
    pub(in crate::workspace) fn replace_saved_temporary_key_for_test(
        &mut self,
        owner: window::Id,
        key: String,
    ) {
        let _ = self.show_connections(owner);
        let manager = self.connection_manager_for_owner(owner).unwrap();
        let _ = self.update_connections(manager, Profile::EditSelected);
        let editor = self.connection_managers[&manager].editor.unwrap();

        let previous_clipboard = std::mem::take(&mut self.clipboard.buffer);
        let internal = self.clipboard.policy.internal;
        self.clipboard.policy.internal = true;
        self.clipboard.buffer.set(key);
        let paste = self.update_connection_editor(editor, Profile::PasteTemporaryKey);
        let mut replies = iced_runtime::task::into_stream(paste).unwrap();
        let Some(iced_runtime::Action::Output(Message::Connections(
            id,
            pasted @ Profile::TemporaryKeyPasted(..),
        ))) = block_on(replies.next())
        else {
            panic!("the editor must receive its temporary key paste result");
        };
        assert_eq!(id, manager);
        let _ = self.update_connections(id, pasted);
        self.clipboard.buffer = previous_clipboard;
        self.clipboard.policy.internal = internal;
        assert!(
            self.connection_managers[&manager]
                .window
                .has_pending_temporary_key()
        );

        let save = self.update_connection_editor(editor, Profile::Save);
        let mut replies = iced_runtime::task::into_stream(save).unwrap();
        while let Some(action) = block_on(replies.next()) {
            if let iced_runtime::Action::Output(Message::Connections(
                id,
                saved @ Profile::CredentialSaveCompleted(..),
            )) = action
            {
                assert_eq!(id, manager);
                let _ = self.update_connections(id, saved);
                return;
            }
        }
        panic!("the editor must complete its credential save");
    }
}

pub(super) fn seed(store: &mut Store, name: &str) -> u64 {
    store
        .save(Connection {
            id: 0,
            name: name.into(),
            tag: String::new(),
            login: SavedLogin {
                preferences: Default::default(),
                endpoint: LoginTarget {
                    host: "localhost".into(),
                    port: 22,
                    username: "fixture".into(),
                },
                auth: Auth::Prompt,
                keyboard_interactive: false,
            },
            jump: None,
            emulation: kortty_terminal::TerminalProfile::Xterm.term_name().into(),
            tunnels: Vec::new(),
            incognito: false,
            isolation: Default::default(),
            inherit_isolation: false,
            startup: Default::default(),
            logging: Default::default(),
            terminal_effect: None,
            command_timestamps: None,
            tab_color: None,
            paste_warning: None,
            window_geometry: None,
            terminal: Default::default(),
        })
        .unwrap()
}

#[test]
fn connection_tunnel_child_owns_its_draft_until_the_connection_is_saved() {
    use crate::connection_window::tunnels::{Event, Request};
    use iced::theme::Base as _;

    let profile = crate::snippets::tests::Profile::new();
    let path = profile.0.join("connections.json");
    let mut store = Store::open(path.clone()).unwrap();
    let target = seed(&mut store, "Stored SSH target");
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let (manager, _) =
        workspace.install_connection_manager(owner, ConnectionWindow::open(path.clone()).unwrap());
    let _ = workspace.update_connections(manager, Profile::EditSelected);
    let editor = workspace.connection_managers[&manager].editor.unwrap();
    let _ = workspace.update_connection_editor(
        editor,
        Profile::Edit(Edit::Page(crate::connection_window::Page::Tunnels)),
    );
    let _ = workspace.update_connection_editor(editor, Profile::OpenTunnelEditor(Request::Add));
    let child = workspace.tunnel_for_connection_editor(editor).unwrap();
    assert_eq!(workspace.placed_dialog_owner(child), Some(editor));
    assert_eq!(workspace.title(child), "Add SSH Tunnel");
    assert_eq!(
        workspace.window_theme(child).name(),
        workspace.window_theme(editor).name()
    );
    assert!(workspace.request_profile_quit().is_some());
    assert!(workspace.quit_waiting_profile.is_none());
    let _ = workspace.update_connection_editor(editor, Profile::CancelEdit);
    assert_eq!(
        workspace.connection_editor_title(editor),
        Some("Edit connection")
    );
    let _ = workspace.update_tunnel_editor(child, Event::Cancel);
    let _ = workspace.finish_placed_dialog_closed(child);
    let _ = workspace.connection_window_closed(child);
    let _ = workspace.update_connection_editor(editor, Profile::OpenTunnelEditor(Request::Edit));
    assert!(workspace.tunnel_for_connection_editor(editor).is_none());
    let _ = workspace.update_connection_editor(editor, Profile::OpenTunnelEditor(Request::Add));
    let child = workspace.tunnel_for_connection_editor(editor).unwrap();
    let _ = workspace.update_tunnel_editor(child, Event::Save);
    let _ = workspace.finish_placed_dialog_closed(child);
    let _ = workspace.connection_window_closed(child);
    assert!(
        Store::open(path.clone())
            .unwrap()
            .get(target)
            .unwrap()
            .tunnels
            .is_empty()
    );
    let _ = workspace.update_connection_editor(editor, Profile::OpenTunnelEditor(Request::Edit));
    let child = workspace.tunnel_for_connection_editor(editor).unwrap();
    assert_eq!(workspace.title(child), "Edit SSH Tunnel");
    let _ = workspace.update_tunnel_editor(child, Event::Cancel);
    let _ = workspace.finish_placed_dialog_closed(child);
    let _ = workspace.connection_window_closed(child);
    let save = workspace.update_connection_editor(editor, Profile::Save);
    let mut actions = iced_runtime::task::into_stream(save).unwrap();
    loop {
        match block_on(actions.next()) {
            Some(iced_runtime::Action::Output(Message::Connections(id, event)))
                if id == manager =>
            {
                let _ = workspace.update_connections(id, event);
                break;
            }
            Some(_) => {}
            None => panic!("expected the saved connection result"),
        }
    }
    let published = Store::open(path).unwrap();
    let saved = published.get(target).unwrap();
    assert_eq!(saved.tunnels[0].rule.bind.port(), 8080);
    assert!(!published.approved(target, &saved.plan()));
}

#[test]
fn connection_memory_is_owned_by_the_editor_and_keeps_the_connection_draft() {
    use crate::ai_manager::{Scope, connection_memory::Event};

    let profile = crate::snippets::tests::Profile::new();
    let path = profile.0.join("connections.json");
    let mut store = Store::open(path.clone()).unwrap();
    let target = seed(&mut store, "Stored SSH target");
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let (manager, _) =
        workspace.install_connection_manager(owner, ConnectionWindow::open(path).unwrap());
    let _ = workspace.update_connections(manager, Profile::EditSelected);
    let editor = workspace.connection_managers[&manager].editor.unwrap();
    let _ = workspace.update_connection_editor(
        editor,
        Profile::Edit(Edit::Name("Unsaved editor name".into())),
    );
    let (saved_id, name) = workspace.connection_managers[&manager]
        .window
        .saved_editor_identity()
        .unwrap();
    assert_eq!(saved_id, target);
    assert_eq!(name, "Stored SSH target");
    let open = workspace.open_connection_memory_from(
        editor,
        profile.0.clone(),
        Scope::connection(target, name.clone()),
        name,
    );
    let memory = workspace.connection_memory_for_editor(editor).unwrap();
    assert_eq!(workspace.placed_dialog_owner(memory), Some(editor));
    assert_eq!(
        workspace.title(memory),
        "Connection memory · Stored SSH target"
    );
    let _ = workspace.update_connection_editor(editor, Profile::CancelEdit);
    assert_eq!(
        workspace.connection_editor_title(editor),
        Some("Edit connection")
    );
    let mut actions = iced_runtime::task::into_stream(open).unwrap();
    loop {
        match block_on(actions.next()) {
            Some(iced_runtime::Action::Output(Message::ConnectionMemory(id, event)))
                if id == memory =>
            {
                let _ = workspace.update_connection_memory(id, event);
                break;
            }
            Some(_) => {}
            None => panic!("expected the connection memory load event"),
        }
    }
    let _ = workspace.update_connection_memory(memory, Event::Close);
    let _ = workspace.connection_memory_closed(memory);
    assert_eq!(
        workspace.connection_editor_title(editor),
        Some("Edit connection")
    );
    assert!(!workspace.connection_managers[&manager].editor_closing);
    assert!(
        workspace.request_profile_quit().is_some(),
        "the connection draft is still dirty"
    );
}

#[test]
fn connection_manager_uses_independent_owners_and_keeps_a_dirty_draft_on_cancel() {
    let profile = crate::snippets::tests::Profile::new();
    let path = profile.0.join("connections.json");
    let mut workspace = Workspace::default();
    let (first, _) = workspace.open_window(false);
    let (second, _) = workspace.open_window(false);
    let (a, _) =
        workspace.install_connection_manager(first, ConnectionWindow::open(path.clone()).unwrap());
    assert_eq!(workspace.owner_dialog(first).map(|(id, _)| id), Some(a));
    assert!(workspace.owner_dialog(second).is_none());

    let (b, _) =
        workspace.install_connection_manager(second, ConnectionWindow::open(path).unwrap());
    assert_ne!(a, b);
    assert_eq!(workspace.placed_dialog_owner(a), Some(first));
    assert_eq!(workspace.placed_dialog_owner(b), Some(second));
    let _ = workspace.show_connections(first);
    let _ = workspace.show_connections(second);
    assert_eq!(workspace.connection_managers.len(), 2);

    let _ = workspace.update_connections(a, Profile::New);
    let editor = workspace.connection_managers[&a].editor.unwrap();
    assert_eq!(workspace.placed_dialog_owner(editor), Some(a));
    assert_eq!(
        workspace.owner_dialog(first).map(|(id, _)| id),
        Some(editor)
    );
    let _ = workspace.update_connection_editor(editor, Profile::Edit(Edit::Name("Draft A".into())));
    assert!(workspace.request_profile_quit().is_some());
    assert!(!workspace.connection_managers[&a].editor_closing);
    assert_eq!(workspace.quit_waiting_profile, Some(a));
    let _ = workspace.update_connection_editor(editor, Profile::KeepEditing);
    assert!(workspace.request_profile_quit().is_some());
    assert!(!workspace.connection_managers[&a].editor_closing);

    let _ = workspace.update_connection_editor(editor, Profile::KeepEditing);
    let _ = workspace.close_connection_editor(editor);
    assert!(workspace.connection_managers[&a].editor_closing);
    let _ = workspace.update(Message::Window(editor, window::Event::Closed));
    assert_eq!(workspace.owner_dialog(first).map(|(id, _)| id), Some(a));

    let _ = workspace.update_connections(a, Profile::Close);
    assert!(workspace.connection_managers[&a].closing);
    assert_eq!(workspace.connection_manager_for_owner(first), Some(a));
    let _ = workspace.update(Message::Window(a, window::Event::Closed));
    assert!(workspace.owner_dialog(first).is_none());
    assert_eq!(workspace.owner_dialog(second).map(|(id, _)| id), Some(b));
}

#[test]
fn connection_manager_waits_for_native_close_before_launching_ssh_and_sftp() {
    let profile = crate::snippets::tests::Profile::new();
    let path = profile.0.join("connections.json");
    let mut store = Store::open(path.clone()).unwrap();
    let ssh = seed(&mut store, "SSH target");
    let sftp = seed(&mut store, "SFTP target");
    let mut workspace = Workspace::default();
    let (first, _) = workspace.open_window(false);
    let (second, _) = workspace.open_window(false);
    let (a, _) =
        workspace.install_connection_manager(first, ConnectionWindow::open(path.clone()).unwrap());
    let (b, _) =
        workspace.install_connection_manager(second, ConnectionWindow::open(path).unwrap());
    let _ = workspace.update_connections(a, Profile::Select(ssh));
    let _ = workspace.update_connections(b, Profile::Select(sftp));
    let _ = workspace.update_connections(a, Profile::Connect(false));
    let _ = workspace.update_connections(b, Profile::Connect(true));
    assert!(workspace.windows[&first].pending_ssh.is_none());
    assert!(workspace.windows[&second].pending_ssh.is_none());
    assert!(matches!(
        &workspace.connection_managers[&a].launch,
        Some(ConnectionLaunch::Ssh(connection, false)) if connection.id == ssh
    ));
    assert!(matches!(
        &workspace.connection_managers[&b].launch,
        Some(ConnectionLaunch::Ssh(connection, true)) if connection.id == sftp
    ));

    let _ = workspace.update(Message::Window(b, window::Event::Closed));
    assert!(workspace.windows[&second].pending_ssh.is_some());
    assert!(workspace.windows[&first].pending_ssh.is_none());
    let _ = workspace.update(Message::Window(a, window::Event::Closed));
    assert!(workspace.windows[&first].pending_ssh.is_some());
    assert!(workspace.connection_managers.is_empty());
}

#[test]
fn saved_window_geometry_applies_only_to_the_explicit_new_ssh_window() {
    use crate::connection_store::WindowGeometry;

    let profile = crate::snippets::tests::Profile::new();
    let path = profile.0.join("connections.json");
    let mut store = Store::open(path.clone()).unwrap();
    let id = seed(&mut store, "SSH target");
    let geometry = WindowGeometry {
        width: 907,
        height: 601,
        ..WindowGeometry::default()
    };
    let mut connection = store.get(id).unwrap().clone();
    connection.window_geometry = Some(geometry);
    store.save(connection).unwrap();

    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let original = workspace.windows[&owner].placement();
    let (manager, _) =
        workspace.install_connection_manager(owner, ConnectionWindow::open(path).unwrap());
    let _ = workspace.update_connections(manager, Profile::Select(id));
    let _ = workspace.update_connections(manager, Profile::ConnectInNewWindow);
    assert!(matches!(
        &workspace.connection_managers[&manager].launch,
        Some(ConnectionLaunch::SshInNewWindow(connection)) if connection.id == id
    ));
    assert_eq!(workspace.windows.len(), 1);

    let _ = workspace.update(Message::Window(manager, window::Event::Closed));
    assert_eq!(workspace.windows.len(), 2);
    assert_eq!(workspace.windows[&owner].placement(), original);
    assert!(workspace.windows[&owner].pending_ssh.is_none());
    let (_, standalone) = workspace
        .windows
        .iter()
        .find(|(id, _)| **id != owner)
        .unwrap();
    let placement = standalone.placement();
    assert_eq!(placement.width, f32::from(geometry.width));
    assert_eq!(placement.height, f32::from(geometry.height));
    assert_eq!(placement.position, geometry.placement().position);
    assert!(standalone.pending_ssh.is_some());
}

#[test]
fn saved_local_profile_launches_from_manager_and_captures_only_a_private_reference() {
    use crate::session_security::{IsolationLevel, LocalIsolation};
    use std::time::Instant;

    let scratch = crate::snippets::tests::Profile::new();
    let path = scratch.0.join("connections.json");
    let mut store = Store::open(path.clone()).unwrap();
    #[cfg(windows)]
    let command = vec![
        std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/cmd.exe")
            .to_str()
            .unwrap()
            .into(),
        "/d".into(),
        "/q".into(),
        "/k".into(),
        "echo".into(),
        "saved-local-ready".into(),
    ];
    #[cfg(not(windows))]
    let command = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf 'saved-local-ready\\n'; exec /bin/sh".into(),
    ];
    let saved = store
        .save_local(LocalProfile {
            id: 0,
            isolation: LocalIsolation::Process,
            name: "Development Shell".into(),
            shell: Default::default(),
            command: Some(command),
            working_directory: Some(scratch.0.clone()),
            shell_integration: false,
            emulation: crate::workspace::TerminalProfile::Xterm.term_name().into(),
            terminal_effect: None,
            terminal: crate::connection_store::TerminalOverrides {
                paste_line_delay_ms: Some(200),
                strict_terminal_mode: Some(true),
                ..Default::default()
            },
        })
        .unwrap();
    let mut workspace = Workspace::default();
    workspace.window_settings.default_session_isolation = Some(IsolationLevel::Sandbox);
    let (owner, _) = workspace.open_window(false);
    let (manager, _) =
        workspace.install_connection_manager(owner, ConnectionWindow::open(path).unwrap());
    let _ = workspace.update_connections(manager, Profile::Select(saved));
    let _ = workspace.update_connections(manager, Profile::Connect(false));
    assert!(workspace.windows[&owner].tabs.is_empty());
    let _ = workspace.update(Message::Window(manager, window::Event::Closed));
    let tab = workspace.windows[&owner].tab().unwrap();
    let id = tab.focused_session().unwrap();
    assert_eq!(workspace.automatic_tab_names(tab).0, "Development Shell");
    assert!(workspace.windows[&owner].pending_ssh.is_none());
    let pane = &workspace.sessions[&id];
    assert_eq!(pane.saved_local.as_ref().unwrap().id, saved);
    assert!(pane.strict_escapes);
    assert_eq!(workspace.paste_line_delay_for(id), 200);
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let frame = pane.session.frame();
        if (0..frame.screen.rows).any(|row| {
            frame.screen.row(row).is_some_and(|cells| {
                cells
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .contains("saved-local-ready")
            })
        }) {
            break;
        }
        if Instant::now() >= deadline {
            let output: String = frame
                .screen
                .cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect();
            panic!(
                "saved local program did not produce output: {:?}, {:?}",
                frame.status,
                output.trim()
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let layout = workspace.capture_layout().unwrap();
    let crate::profile::PaneLayout::Terminal {
        security: Some(settings),
        ..
    } = &layout.windows[0].tabs[0]
    else {
        panic!("expected a local profile reference");
    };
    assert_eq!(settings.saved_profile, Some(saved));
    assert_eq!(settings.isolation, IsolationLevel::Process);
    assert!(
        !serde_json::to_string(&layout)
            .unwrap()
            .contains("saved-local-ready")
    );
    let stopped = pane.session.stop();
    let deadline = Instant::now() + Duration::from_secs(6);
    while !stopped.is_closed() {
        assert!(
            Instant::now() < deadline,
            "saved local program did not stop"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn teamwork_restore_is_owned_and_can_retry_a_stale_offline_store() {
    use crate::connection_store::teamwork::{self, HiddenEntry};
    use iced::advanced::{Layout, layout, widget::Tree};
    use iced::{Font, Renderer, Size};
    use teamwork_restore::Event;

    let profile = crate::snippets::tests::Profile::new();
    let path = profile.0.join("connections.json");
    let source = profile.0.join("shared.xml");
    std::fs::write(&source, b"<connections><connection id=\"offline\"><name>Offline fixture</name><host>ssh.example</host><port>22</port><username>ops</username></connection></connections>").unwrap();
    let settings = teamwork::Settings {
        sources: vec![teamwork::Source {
            kind: teamwork::SourceKind::File,
            enabled: true,
            location: source.to_str().unwrap().into(),
            check_interval_minutes: teamwork::DEFAULT_CHECK_INTERVAL_MINUTES,
        }],
        ..Default::default()
    };
    let catalog =
        teamwork::Catalog::default().refreshed(&settings.active_sources(true, &[]).unwrap());
    let hidden = HiddenEntry::from(catalog.entries().next().unwrap());
    let identity = hidden.identity().clone();
    let mut store = Store::open(path.clone()).unwrap();
    store.hide_teamwork(&[hidden]).unwrap();
    std::fs::remove_file(&source).unwrap();

    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let (manager, _) =
        workspace.install_connection_manager(owner, ConnectionWindow::open(path.clone()).unwrap());
    let _ = workspace.update_connections(manager, Profile::RestoreTeamwork);
    let dialog = workspace.teamwork_restore_for_owner(manager).unwrap();
    assert_eq!(workspace.placed_dialog_owner(dialog), Some(manager));
    {
        let mut view = workspace.teamwork_restore_view(dialog).unwrap();
        let mut tree = Tree::new(view.as_widget());
        let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(370.0, 290.0)),
        );
        let column = Layout::new(&node).children().next().unwrap();
        let body = column.children().nth(1).unwrap();
        let list = body.children().next().unwrap().children().nth(1).unwrap();
        let entries = list.children().next().unwrap();
        let row = entries.children().next().unwrap().bounds();
        let viewport = list.bounds();
        assert!(row.width > 0.0 && row.height > 0.0, "{row:?}");
        assert!(row.x >= viewport.x && row.y >= viewport.y, "{row:?}");
        assert!(row.x + row.width <= viewport.x + viewport.width, "{row:?}");
        assert!(
            row.y + row.height <= viewport.y + viewport.height,
            "{row:?}"
        );
    }
    let _ = workspace.update_connections(manager, Profile::Close);
    assert!(!workspace.connection_managers[&manager].closing);
    let new_local = seed(&mut store, "Concurrent local connection");

    for succeeds in [false, true] {
        let _ = workspace.update(Message::TeamworkRestore(
            dialog,
            Event::Select(identity.clone()),
        ));
        let task = workspace.update(Message::TeamworkRestore(dialog, Event::Restore));
        assert_eq!(workspace.teamwork_restore_busy(), Some(dialog));
        let _ = workspace.close_profile_tool(dialog, false);
        assert_eq!(workspace.teamwork_restore_for_owner(manager), Some(dialog));
        let mut stream = iced_runtime::task::into_stream(task).expect("restore task");
        let completed = loop {
            if let iced_runtime::Action::Output(message @ Message::TeamworkRestore(..)) =
                block_on(stream.next()).expect("restore completion")
            {
                break message;
            }
        };
        let _ = workspace.update(completed);
        let current = Store::open(path.clone()).unwrap();
        assert_eq!(current.is_teamwork_hidden(&identity), !succeeds);
        assert!(current.get(new_local).is_some());
        assert_eq!(
            workspace.teamwork_restore_for_owner(manager),
            (!succeeds).then_some(dialog)
        );
    }
}

#[test]
fn teamwork_auth_apply_serializes_drafts_across_connection_managers() {
    use crate::connection_store::teamwork::{AuthSave, DefaultAuth};
    use crate::connection_window::Scope;

    let profile = crate::snippets::tests::Profile::new();
    let path = profile.0.join("connections.json");
    let mut store = Store::open(path.clone()).unwrap();
    let mut workspace = Workspace::default();
    let (first, _) = workspace.open_window(false);
    let (second, _) = workspace.open_window(false);
    let (a, _) =
        workspace.install_connection_manager(first, ConnectionWindow::open(path.clone()).unwrap());
    let (b, _) =
        workspace.install_connection_manager(second, ConnectionWindow::open(path).unwrap());

    let saved = DefaultAuth::TemporaryKey {
        username: "shared-ops".into(),
    };
    let other = DefaultAuth::TemporaryKey {
        username: "different".into(),
    };
    for (manager, draft) in [(a, saved.clone()), (b, other)] {
        let _ = workspace.update_connections(manager, Profile::Scope(Scope::Teamwork));
        let _ = workspace.update_connections(manager, Profile::TeamworkAuthEdit(draft));
        assert!(
            workspace.connection_managers[&manager]
                .window
                .teamwork_auth_change()
                .is_some()
        );
    }

    let _ = workspace.update_connections(a, Profile::SaveTeamworkAuth);
    assert!(workspace.connection_managers[&a].window.busy());
    assert!(workspace.connection_managers[&b].window.busy());
    let latest = seed(&mut store, "Saved while applying Teamwork authentication");
    workspace.update_connection_store(store);
    for manager in [a, b] {
        let state = &workspace.connection_managers[&manager];
        assert!(!state.refresh_pending);
        assert!(
            state
                .window
                .overview_sources()
                .unwrap()
                .store
                .get(latest)
                .is_some()
        );
    }
    let _ = workspace.update_connections(b, Profile::SaveTeamworkAuth);
    assert_eq!(*workspace.teamwork_auth_default(), DefaultAuth::Prompt);
    let _ = workspace.update(Message::TeamworkAuthSaved(Ok(AuthSave::Saved)));

    assert_eq!(*workspace.teamwork_auth_default(), saved);
    for manager in [a, b] {
        let window = &workspace.connection_managers[&manager].window;
        assert!(!window.busy());
        assert!(window.teamwork_auth_change().is_none());
    }
    let _ = workspace.update_connections(b, Profile::ResetTeamworkAuth);
    let _ = workspace.update_connections(b, Profile::TeamworkAuthEdit(DefaultAuth::Prompt));
    assert_eq!(
        workspace.connection_managers[&b]
            .window
            .teamwork_auth_change(),
        Some((saved, DefaultAuth::Prompt))
    );
}

#[test]
fn connection_manager_reloads_the_catalog_after_an_overlapping_result() {
    let profile = crate::snippets::tests::Profile::new();
    let path = profile.0.join("connections.json");
    let mut store = Store::open(path.clone()).unwrap();
    let previous = seed(&mut store, "Previous");
    let snapshot = store.clone();
    let mut workspace = Workspace::default();
    let (first, _) = workspace.open_window(false);
    let (second, _) = workspace.open_window(false);
    let (a, _) =
        workspace.install_connection_manager(first, ConnectionWindow::open(path.clone()).unwrap());
    let (b, _) =
        workspace.install_connection_manager(second, ConnectionWindow::open(path).unwrap());

    let _ = workspace.update_connections(b, Profile::Reload);
    assert!(workspace.connection_managers[&b].window.busy());
    let latest = seed(&mut store, "Latest");
    workspace.update_connection_store(store);
    assert!(workspace.connection_managers[&b].refresh_pending);
    let task = workspace.update_connections(
        b,
        Profile::Completed(Ok(Box::new(Persisted::saved(snapshot, previous)))),
    );
    let mut actions = iced_runtime::task::into_stream(task).expect("catalog reload");
    let reload = loop {
        if let iced_runtime::Action::Output(message @ Message::ConnectionCatalogReloaded(..)) =
            block_on(actions.next()).expect("catalog reload result")
        {
            break message;
        }
    };
    let _ = workspace.update(reload);
    for id in [a, b] {
        let _ = workspace.update_connections(id, Profile::Select(latest));
        assert_eq!(
            workspace.connection_managers[&id]
                .window
                .selected()
                .map(|entry| entry.id),
            Some(latest)
        );
    }
}

#[test]
fn serial_refresh_focus_reply_reaches_its_editor_and_ignores_another_protocol() {
    let profile = crate::snippets::tests::Profile::new();
    let path = profile.0.join("connections.json");
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let (manager, _) =
        workspace.install_connection_manager(owner, ConnectionWindow::open(path).unwrap());
    let _ = workspace.update_connections(manager, Profile::NewSerial);
    let editor = workspace.connection_managers[&manager].editor.unwrap();
    let focus = || {
        Profile::Serial(crate::serial_dialog::Message::Focus(
            iced::widget::Id::unique(),
        ))
    };
    let mut actions =
        iced_runtime::task::into_stream(workspace.update_connections(manager, focus())).unwrap();
    assert!(matches!(
        block_on(actions.next()),
        Some(iced_runtime::Action::Widget(_))
    ));
    let _ = workspace.update_connection_editor(
        editor,
        Profile::Protocol(crate::connection_store::protocol::Kind::Local),
    );
    // Task::batch may retain an empty stream; it must emit no focus action.
    if let Some(mut actions) =
        iced_runtime::task::into_stream(workspace.update_connections(manager, focus()))
    {
        assert!(block_on(actions.next()).is_none());
    }
}
