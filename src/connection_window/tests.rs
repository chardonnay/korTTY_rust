use std::fs;

use crate::clipboard_policy::PasteWarningMode;
use crate::connection_store::{Auth, SavedLogin};
use crate::session_security::IsolationLevel;
use crate::ssh_dialog::LoginTarget;

use super::*;

mod serial_selection;

struct Profile(PathBuf);

impl Profile {
    fn new() -> Self {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!(
            "kortty-connection-manager-{:016x}",
            u64::from_ne_bytes(nonce)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> PathBuf {
        self.0.join("connections.json")
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn connection(name: &str, tag: &str) -> Connection {
    Connection {
        id: 0,
        name: name.into(),
        tag: tag.into(),
        login: SavedLogin {
            endpoint: LoginTarget {
                host: "example.test".into(),
                port: 22,
                username: "operator".into(),
            },
            auth: Auth::Prompt,
            keyboard_interactive: false,
        },
        jump: None,
        emulation: "xterm-256color".into(),
        tunnels: Vec::new(),
        incognito: false,
        isolation: IsolationLevel::None,
        inherit_isolation: false,
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

#[test]
fn saved_window_geometry_is_kept_by_edit_and_omitted_by_duplicate() {
    use crate::connection_store::WindowGeometry;

    let mut connection = connection("fixture", "");
    connection.window_geometry = Some(WindowGeometry {
        width: 900,
        maximized: true,
        ..WindowGeometry::default()
    });
    let mut editor = Draft::from_profile(&connection, None, false);
    editor.update(Edit::Name("renamed".into()));
    assert_eq!(
        editor.connection().unwrap().window_geometry,
        connection.window_geometry
    );
    let duplicate = Draft::from_profile(&connection, None, true);
    assert_eq!(duplicate.connection().unwrap().window_geometry, None);
}

fn complete_persisted(manager: &mut ConnectionWindow, message: Message) {
    use iced::futures::{StreamExt, executor::block_on};
    use iced_runtime::{Action, task};

    let mut result = task::into_stream(manager.update(message)).unwrap();
    let Some(Action::Output(completed @ Message::Completed(Ok(_)))) = block_on(result.next())
    else {
        panic!("expected a completed connection store update");
    };
    let _ = manager.update(completed);
}

#[test]
fn network_map_selects_collapsed_connections_and_restores_a_moved_folder() {
    use iced_runtime::task;

    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let from = store.create_group(None, "Original folder").unwrap();
    let to = store.create_group(None, "Destination").unwrap();
    let id = store
        .save_in_group(connection("Mapped host", ""), Some(&from))
        .unwrap();
    store
        .save_in_group(connection("Other host", ""), Some(&to))
        .unwrap();
    let mut manager = ConnectionWindow::open(profile.path()).unwrap();
    let _ = manager.update(Message::ToggleGroup(from.clone()));
    assert!(!manager.visible_row_keys().contains(&RowKey::Connection(id)));
    let _ = manager.update(Message::MapView(true));
    let _ = manager.update(Message::MapSelect(RowKey::Connection(id)));
    assert_eq!(manager.selected().unwrap().id, id);
    assert!(manager.visible_row_keys().contains(&RowKey::Connection(id)));
    complete_persisted(&mut manager, Message::MapMove(id, Some(to.clone())));
    assert_eq!(
        Store::open(profile.path()).unwrap().group_of(id),
        Some(to.as_str())
    );
    complete_persisted(&mut manager, Message::UndoFolderMove);
    assert_eq!(
        Store::open(profile.path()).unwrap().group_of(id),
        Some(from.as_str())
    );
    assert!(task::into_stream(manager.update(Message::UndoFolderMove)).is_none());
    let _ = manager.update(Message::MapView(false));
    assert_eq!(manager.selected().unwrap().id, id);
}

#[test]
fn tree_and_map_folder_moves_share_successive_undo() {
    use folder_moves::Profile as Movable;
    use iced_runtime::task;

    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let first = store.create_group(None, "First folder").unwrap();
    let second = store.create_group(Some(&first), "Second folder").unwrap();
    let ssh = store.save(connection("SSH fixture", "")).unwrap();
    let serial = store
        .save_serial(SerialProfile::new(
            "Serial fixture",
            crate::session::SerialConnection {
                device: "COM14".into(),
                ..Default::default()
            },
            kortty_terminal::TerminalProfile::Vt220,
        ))
        .unwrap();
    let mut manager = ConnectionWindow::open(profile.path()).unwrap();
    complete_persisted(
        &mut manager,
        Message::TreeMove(Movable::Serial(serial), first.clone()),
    );
    complete_persisted(
        &mut manager,
        Message::TreeMove(Movable::Ssh(ssh), first.clone()),
    );
    assert!(
        task::into_stream(manager.update(Message::TreeMove(Movable::Ssh(ssh), first.clone())))
            .is_none()
    );
    let _ = manager.update(Message::MapView(true));
    complete_persisted(&mut manager, Message::MapMove(ssh, Some(second.clone())));
    assert_eq!(manager.store.group_of(ssh), Some(second.as_str()));

    let _ = manager.update(Message::MapView(false));
    let _ = manager.update(Message::Scope(Scope::Teamwork));
    assert!(task::into_stream(manager.update(Message::UndoFolderMove)).is_none());
    assert!(
        task::into_stream(manager.update(Message::TreeMove(Movable::Ssh(ssh), first.clone())))
            .is_none()
    );
    let _ = manager.update(Message::Scope(Scope::All));
    complete_persisted(&mut manager, Message::UndoFolderMove);
    assert_eq!(manager.store.group_of(ssh), Some(first.as_str()));
    complete_persisted(&mut manager, Message::UndoFolderMove);
    assert_eq!(manager.store.group_of(ssh), None);
    assert_eq!(manager.store.group_of(serial), Some(first.as_str()));
    complete_persisted(&mut manager, Message::UndoFolderMove);
    let saved = Store::open(profile.path()).unwrap();
    assert_eq!(saved.group_of(ssh), None);
    assert_eq!(saved.group_of(serial), None);
    assert!(!manager.can_undo_folder_move());
}

#[test]
fn folder_undo_refuses_a_concurrent_edit_and_keeps_earlier_valid_moves() {
    use folder_moves::Profile as Movable;
    use iced::futures::{StreamExt, executor::block_on};
    use iced_runtime::{Action, task};

    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let first = store.create_group(None, "First folder").unwrap();
    let second = store.create_group(None, "Second folder").unwrap();
    let earlier = store.save(connection("Earlier", "")).unwrap();
    let later = store.save(connection("Later", "")).unwrap();
    let mut manager = ConnectionWindow::open(profile.path()).unwrap();
    for id in [earlier, later] {
        complete_persisted(
            &mut manager,
            Message::TreeMove(Movable::Ssh(id), first.clone()),
        );
    }

    let mut other = Store::open(profile.path()).unwrap();
    other
        .save_in_group(other.get(later).unwrap().clone(), Some(&second))
        .unwrap();
    let mut result = task::into_stream(manager.update(Message::UndoFolderMove)).unwrap();
    let Some(Action::Output(completed @ Message::Completed(Err(_)))) = block_on(result.next())
    else {
        panic!("a stale store cannot write an undo");
    };
    let _ = manager.update(completed);
    assert!(manager.can_undo_folder_move());
    complete_persisted(&mut manager, Message::Reload);
    complete_persisted(&mut manager, Message::UndoFolderMove);
    let saved = Store::open(profile.path()).unwrap();
    assert_eq!(saved.group_of(later), Some(second.as_str()));
    assert_eq!(saved.group_of(earlier), None);
    assert!(!manager.can_undo_folder_move());
}

#[test]
fn serial_profiles_are_searchable_and_duplicate_with_the_same_terminal_settings() {
    use crate::session::{SerialConnection, SerialLineEnding};
    use kortty_terminal::{TerminalProfile, TextEncoding};

    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let serial = SerialConnection {
        device: "COM18".into(),
        line_ending: SerialLineEnding::Lf,
        encoding: Some(TextEncoding::Windows1252),
        ..SerialConnection::default()
    };
    let saved = store
        .save_serial(SerialProfile::new(
            "Router",
            serial.clone(),
            TerminalProfile::Vt220,
        ))
        .unwrap();
    let mut manager = ConnectionWindow::open(profile.path()).unwrap();
    let _ = manager.update(Message::Search("com18".into()));
    assert_eq!(manager.visible_row_keys(), vec![RowKey::Serial(saved)]);
    let _ = manager.update(Message::SelectSerial(saved));
    assert_eq!(manager.selected_serial().unwrap().id, saved);
    assert_eq!(manager.export_connections_sources().unwrap().1, [saved]);

    let _ = manager.update(Message::Duplicate);
    let copied = manager.serial_draft.as_ref().unwrap().profile().unwrap();
    assert_eq!(copied.id, 0);
    assert_eq!(copied.display_name(), "Router (copy)");
    assert!(copied.matches(&serial, TerminalProfile::Vt220));
    let _ = manager.update(Message::SerialName("Edited copy".into()));
    assert!(!manager.request_close());
}

#[test]
fn grouped_serial_favorites_pins_tags_and_duplicate_are_persisted() {
    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let folder = store.create_group(None, "Hardware").unwrap();
    let mut draft = SerialProfile::new(
        "Console",
        crate::session::SerialConnection {
            device: "COM21".into(),
            ..Default::default()
        },
        kortty_terminal::TerminalProfile::Vt220,
    );
    draft.tag = "Switches".into();
    draft.highlight_set = Some("builtin.errors".into());
    let id = store.save_serial_in_group(draft, Some(&folder)).unwrap();
    let mut manager = ConnectionWindow::open(profile.path()).unwrap();
    assert_eq!(
        manager.visible_row_keys(),
        [RowKey::Group(folder.clone()), RowKey::Serial(id)]
    );
    let _ = manager.update(Message::ToggleGroup(folder.clone()));
    assert_eq!(manager.visible_row_keys(), [RowKey::Group(folder.clone())]);
    let _ = manager.update(Message::Search("switches".into()));
    assert!(manager.visible_row_keys().contains(&RowKey::Serial(id)));
    let _ = manager.update(Message::SelectSerial(id));
    complete_persisted(&mut manager, Message::ToggleFavorite);
    complete_persisted(&mut manager, Message::ToggleSerialPin);
    let _ = manager.update(Message::Scope(Scope::Favorites));
    assert_eq!(manager.visible_row_keys(), [RowKey::Serial(id)]);
    let _ = manager.update(Message::Duplicate);
    let copy = manager.serial_draft.as_ref().unwrap().profile().unwrap();
    assert!(!copy.pinned);
    assert_eq!(copy.tag, "Switches");
    assert_eq!(copy.highlight_set.as_deref(), Some("builtin.errors"));
    let _ = manager.update(Message::SerialHighlightSet("none".into()));
    assert_eq!(
        manager
            .serial_draft
            .as_ref()
            .unwrap()
            .profile()
            .unwrap()
            .highlight_set
            .as_deref(),
        Some("none")
    );
    assert_eq!(manager.draft_group.as_deref(), Some(folder.as_str()));
    complete_persisted(&mut manager, Message::Save);
    let mut store = Store::open(profile.path()).unwrap();
    assert!(store.is_favorite(id) && store.serial(id).unwrap().pinned);
    assert_eq!(store.serial_in_group(&folder).count(), 2);
    store.delete_group(&folder).unwrap();
    assert!(store.serial_profiles().is_empty());
    assert!(!store.is_favorite(id));
}

#[test]
fn local_shell_editor_persists_exact_arguments_and_keeps_cancelled_edits_private() {
    use super::local::Edit as Local;

    let directory = Profile::new();
    let mut manager = ConnectionWindow::open(directory.path()).unwrap();
    let _ = manager.update(Message::NewLocal);
    let _ = manager.update(Message::Local(Local::Name("Custom build".into())));
    let _ = manager.update(Message::Local(Local::Directory(
        directory.0.display().to_string(),
    )));
    let _ = manager.update(Message::Local(Local::Custom(true)));
    let _ = manager.update(Message::Local(Local::Program("Build tool".into())));
    let _ = manager.update(Message::Local(Local::AddArgument));
    let _ = manager.update(Message::Local(Local::Argument(
        0,
        " file with spaces ".into(),
    )));
    let _ = manager.update(Message::Local(Local::AddArgument));
    complete_persisted(&mut manager, Message::Save);
    let id = manager.selected_local().unwrap().id;
    assert_eq!(manager.export_connections_sources().unwrap().1, vec![id]);

    let mut manager = ConnectionWindow::open(directory.path()).unwrap();
    let _ = manager.update(Message::Search("Build*".into()));
    assert_eq!(manager.visible_row_keys(), vec![RowKey::Local(id)]);
    let saved = manager.selected_local().unwrap().clone();
    assert_eq!(saved.working_directory.as_ref(), Some(&directory.0));
    assert_eq!(
        saved.command.as_ref().unwrap(),
        &["Build tool", " file with spaces ", ""]
    );

    let _ = manager.update(Message::Duplicate);
    let copy = manager.local_draft.as_ref().unwrap().profile().unwrap();
    assert_eq!(copy.id, 0);
    assert_eq!(copy.command, saved.command);
    let _ = manager.update(Message::Local(Local::Name("Edited copy".into())));
    assert!(!manager.request_close());
    let _ = manager.update(Message::KeepEditing);
    let _ = manager.update(Message::CancelEdit);
    assert_eq!(manager.selected_local(), Some(&saved));

    let _ = manager.update(Message::Delete);
    complete_persisted(&mut manager, Message::ConfirmDelete(true));
    assert!(Store::open(directory.path()).unwrap().local(id).is_none());
}

#[test]
fn local_shell_editor_saves_folder_inheritance_and_keeps_it_on_duplicate() {
    use super::local::Edit as Local;
    use crate::session_security::LocalIsolation;

    let directory = Profile::new();
    let mut store = Store::open(directory.path()).unwrap();
    let group = store.create_group(None, "Development").unwrap();
    let mut manager = ConnectionWindow::open(directory.path()).unwrap();
    let _ = manager.update(Message::Scope(Scope::Group(group.clone())));
    let _ = manager.update(Message::NewLocal);
    let _ = manager.update(Message::Local(Local::Name("Build shell".into())));
    complete_persisted(&mut manager, Message::Save);
    assert_eq!(manager.store.local_in_group(&group).count(), 1);
    let id = manager.store.local_in_group(&group).next().unwrap().id;
    assert_eq!(manager.visible_row_keys(), [RowKey::Local(id)]);
    assert_eq!(
        manager.store.local(id).unwrap().isolation,
        LocalIsolation::Inherit
    );

    let _ = manager.update(Message::SelectLocal(id));
    let _ = manager.update(Message::Duplicate);
    let _ = manager.update(Message::Local(Local::Isolation(LocalIsolation::Process)));
    complete_persisted(&mut manager, Message::Save);
    let store = Store::open(directory.path()).unwrap();
    let levels: Vec<_> = store
        .local_in_group(&group)
        .map(|profile| profile.isolation)
        .collect();
    assert_eq!(levels, [LocalIsolation::Inherit, LocalIsolation::Process]);
}

#[test]
fn initial_terminal_dimensions_save_for_ssh_and_local_profiles() {
    use super::local::Edit as Local;
    use super::terminal_overrides::Edit as Terminal;

    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let ssh = store.save(connection("SSH terminal", "")).unwrap();
    let mut manager = ConnectionWindow::open(profile.path()).unwrap();
    let _ = manager.update(Message::Select(ssh));
    let _ = manager.update(Message::EditSelected);
    let _ = manager.update(Message::Edit(Edit::Terminal(Terminal::Columns(
        "39".into(),
    ))));
    assert_eq!(manager.update(Message::Save).units(), 0);
    let _ = manager.update(Message::Edit(Edit::Terminal(Terminal::Columns(
        "40".into(),
    ))));
    let _ = manager.update(Message::Edit(Edit::Terminal(Terminal::Rows("200".into()))));
    complete_persisted(&mut manager, Message::Save);

    let _ = manager.update(Message::NewLocal);
    let _ = manager.update(Message::Local(Local::Name("Local terminal".into())));
    let _ = manager.update(Message::Local(Local::Terminal(Terminal::Columns(
        "500".into(),
    ))));
    let _ = manager.update(Message::Local(Local::Terminal(Terminal::Rows(
        "201".into(),
    ))));
    assert_eq!(manager.update(Message::Save).units(), 0);
    let _ = manager.update(Message::Local(Local::Terminal(Terminal::Rows(" ".into()))));
    complete_persisted(&mut manager, Message::Save);
    let local = manager.selected_local().unwrap().id;

    let store = Store::open(profile.path()).unwrap();
    let ssh_terminal = &store.get(ssh).unwrap().terminal;
    assert_eq!(ssh_terminal.terminal_columns, Some(40));
    assert_eq!(ssh_terminal.terminal_rows, Some(200));
    let local_terminal = &store.local(local).unwrap().terminal;
    assert_eq!(local_terminal.terminal_columns, Some(500));
    assert_eq!(local_terminal.terminal_rows, None);

    let mut manager = ConnectionWindow::open(profile.path()).unwrap();
    let _ = manager.update(Message::Select(ssh));
    let _ = manager.update(Message::EditSelected);
    let _ = manager.update(Message::Edit(Edit::Terminal(Terminal::Reset)));
    complete_persisted(&mut manager, Message::Save);
    assert!(
        Store::open(profile.path())
            .unwrap()
            .get(ssh)
            .unwrap()
            .terminal
            .is_default()
    );
}

#[test]
fn connection_editor_persists_highlight_choices_for_ssh_and_local_profiles() {
    use super::local::Edit as Local;
    use super::terminal_overrides::Edit as Terminal;

    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let mut ssh = connection("SSH terminal", "");
    ssh.terminal.highlight_set = Some("missing-shared-set".into());
    let ssh = store.save(ssh).unwrap();
    let mut manager = ConnectionWindow::open(profile.path()).unwrap();
    let _ = manager.update(Message::Select(ssh));
    let _ = manager.update(Message::EditSelected);
    let _ = manager.update(Message::Edit(Edit::Name("Renamed SSH terminal".into())));
    complete_persisted(&mut manager, Message::Save);
    assert_eq!(
        manager
            .selected()
            .unwrap()
            .terminal
            .highlight_set
            .as_deref(),
        Some("missing-shared-set")
    );

    let _ = manager.update(Message::EditSelected);
    let _ = manager.update(Message::Edit(Edit::Terminal(Terminal::HighlightSet(
        "none".into(),
    ))));
    complete_persisted(&mut manager, Message::Save);

    let _ = manager.update(Message::NewLocal);
    let _ = manager.update(Message::Local(Local::Name("Local terminal".into())));
    let _ = manager.update(Message::Local(Local::Terminal(Terminal::HighlightSet(
        "ops-errors".into(),
    ))));
    complete_persisted(&mut manager, Message::Save);
    let local = manager.selected_local().unwrap().id;
    let saved = Store::open(profile.path()).unwrap();
    assert_eq!(
        saved.get(ssh).unwrap().terminal.highlight_set.as_deref(),
        Some("none")
    );
    assert_eq!(
        saved
            .local(local)
            .unwrap()
            .terminal
            .highlight_set
            .as_deref(),
        Some("ops-errors")
    );

    let _ = manager.update(Message::EditSelected);
    let _ = manager.update(Message::Local(Local::Terminal(Terminal::HighlightSet(
        String::new(),
    ))));
    complete_persisted(&mut manager, Message::Save);
    assert_eq!(
        Store::open(profile.path())
            .unwrap()
            .local(local)
            .unwrap()
            .terminal
            .highlight_set,
        None
    );
}

#[test]
fn range_selection_and_context_keep_the_same_visible_profiles() {
    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let first = store.save(connection("Alpha", "")).unwrap();
    let middle = store.save(connection("Bravo", "")).unwrap();
    let last = store.save(connection("Charlie", "")).unwrap();
    let mut manager = ConnectionWindow::open(profile.path()).unwrap();
    let _ = manager.update(Message::Select(first));
    manager.set_modifiers(Modifiers::SHIFT);
    let _ = manager.update(Message::Select(last));
    assert_eq!(manager.selected_connection_ids(), vec![first, middle, last]);
    assert!(manager.selected().is_none());
    assert_eq!(manager.connect_selection().unwrap().id, first);
    manager.set_modifiers(Modifiers::empty());
    let _ = manager.update(Message::OpenTreeContext(
        RowKey::Connection(middle),
        Point::ORIGIN,
    ));
    assert_eq!(manager.selected_connection_ids(), vec![first, middle, last]);
    let _ = manager.update(Message::Search("Bravo".into()));
    assert_eq!(manager.selected_connection_ids(), vec![middle]);
    assert_eq!(manager.selected().unwrap().id, middle);
}

#[test]
fn teamwork_favorites_toggle_mixed_selection_and_filter_the_map_and_pdf() {
    use super::tree_context::{Message as Context, Page};

    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let local = store.save(connection("Local favorite", "")).unwrap();
    assert!(store.toggle_connection_favorites(&[local], &[]).unwrap());
    let shared_file = profile.0.join("shared.xml");
    let xml = r#"<connections>
        <connection id="primary"><name>Shared primary</name><host>one.example</host>
            <username>operator</username></connection>
        <connection id="spare"><name>Shared spare</name><host>two.example</host>
            <username>operator</username></connection>
        </connections>"#;
    fs::write(&shared_file, xml).unwrap();
    let settings = teamwork::Settings {
        sources: vec![teamwork::Source {
            location: shared_file.to_str().unwrap().into(),
            enabled: true,
            kind: teamwork::SourceKind::File,
            check_interval_minutes: teamwork::DEFAULT_CHECK_INTERVAL_MINUTES,
        }],
        ..teamwork::Settings::default()
    };
    let catalog =
        teamwork::Catalog::default().refreshed(&settings.active_sources(true, &[]).unwrap());
    let identity = |name| {
        catalog
            .entries()
            .find(|entry| entry.connection.name == name)
            .unwrap()
            .identity()
            .clone()
    };
    let primary = identity("Shared primary");
    let spare = identity("Shared spare");
    let primary_row = RowKey::Shared(primary.clone());
    let mut manager = ConnectionWindow::open(profile.path()).unwrap();
    manager.set_teamwork(catalog.clone(), None);
    manager.set_open_counts(OpenCounts::default());
    assert_eq!(manager.favorite_count(), 1);

    let _ = manager.update(Message::Select(local));
    manager.set_modifiers(Modifiers::COMMAND);
    let _ = manager.update(Message::SelectShared(primary.clone()));
    manager.set_modifiers(Modifiers::empty());
    let _ = manager.update(Message::OpenTreeContext(primary_row.clone(), Point::ORIGIN));
    assert_eq!(manager.selected_connection_ids(), vec![local]);
    assert!(manager.selection.contains(&primary_row));
    manager.set_open_counts(OpenCounts::default());
    complete_persisted(
        &mut manager,
        Message::TreeContext(Context::Activate(Page::Root, 0)),
    );
    assert!(manager.store.is_favorite(local));
    assert!(manager.store.is_teamwork_favorite(&primary));
    assert!(!manager.store.is_teamwork_favorite(&spare));
    assert_eq!(manager.favorite_count(), 2);
    assert_eq!(fs::read_to_string(&shared_file).unwrap(), xml);

    let _ = manager.update(Message::Scope(Scope::Teamwork));
    assert_eq!(manager.visible_shared().len(), 2);
    let _ = manager.update(Message::Scope(Scope::Favorites));
    assert_eq!(
        manager.visible_row_keys(),
        [
            RowKey::Connection(local),
            RowKey::SharedGroup(String::new()),
            primary_row.clone()
        ]
    );
    let _ = manager.update(Message::MapView(true));
    let snapshot = manager.map_pdf_snapshot().unwrap();
    assert_eq!(snapshot.scope, "Favorites");
    assert_eq!(snapshot.connection_count, 2);
    let _ = manager.update(Message::Search("Shared".into()));
    assert_eq!(
        manager.visible_row_keys().as_slice(),
        std::slice::from_ref(&primary_row)
    );
    assert_eq!(manager.map_pdf_snapshot().unwrap().connection_count, 1);
    let _ = manager.update(Message::MapSelect(primary_row.clone()));
    let _ = manager.update(Message::MapSelect(RowKey::Shared(spare)));
    assert_eq!(manager.selected_shared().unwrap().identity(), &primary);

    let _ = manager.update(Message::Search(String::new()));
    manager.set_teamwork(teamwork::Catalog::default(), None);
    assert_eq!(manager.favorite_count(), 1);
    manager.set_teamwork(catalog, None);
    assert_eq!(manager.favorite_count(), 2);
    let _ = manager.update(Message::MapView(false));
    let _ = manager.update(Message::Select(local));
    manager.set_modifiers(Modifiers::COMMAND);
    let _ = manager.update(Message::SelectShared(primary.clone()));
    manager.set_modifiers(Modifiers::empty());
    complete_persisted(&mut manager, Message::ToggleFavorite);
    assert!(manager.visible_row_keys().is_empty());
    assert!(manager.selected_shared().is_none());
    assert_eq!(manager.favorite_count(), 0);
    let persisted = Store::open(profile.path()).unwrap();
    assert!(!persisted.is_favorite(local));
    assert!(!persisted.is_teamwork_favorite(&primary));
}

#[test]
fn teamwork_hide_confirms_separately_and_excludes_hidden_rows_from_every_scope() {
    use super::tree_context::{Message as Context, Page};

    let profile = Profile::new();
    let source = profile.0.join("shared.xml");
    let xml = r#"<connections><connection id="primary">
        <name>Shared primary</name><host>shared.example</host><username>operator</username>
        <tag>prod</tag><lastUsed>1700000000012</lastUsed>
        </connection></connections>"#;
    fs::write(&source, xml).unwrap();
    let settings = teamwork::Settings {
        sources: vec![teamwork::Source {
            location: source.to_str().unwrap().into(),
            enabled: true,
            kind: teamwork::SourceKind::File,
            check_interval_minutes: teamwork::DEFAULT_CHECK_INTERVAL_MINUTES,
        }],
        ..teamwork::Settings::default()
    };
    let catalog =
        teamwork::Catalog::default().refreshed(&settings.active_sources(true, &[]).unwrap());
    let identity = catalog.entries().next().unwrap().identity().clone();
    let shared = RowKey::Shared(identity.clone());
    let mut store = Store::open(profile.path()).unwrap();
    let local = store.save(connection("Local production", "prod")).unwrap();
    store.record_used_at(local, 1_700_000_000_010).unwrap();
    store
        .toggle_connection_favorites(&[local], std::slice::from_ref(&identity))
        .unwrap();

    let mut manager = ConnectionWindow::open(profile.path()).unwrap();
    manager.set_teamwork(catalog.clone(), None);
    manager.set_open_counts(OpenCounts {
        saved: BTreeMap::from([(local, 1)]),
        teamwork: BTreeMap::from([(identity.clone(), 2)]),
    });
    let _ = manager.update(Message::Scope(Scope::Open));
    assert_eq!(
        manager.visible_row_keys(),
        [
            RowKey::Connection(local),
            RowKey::SharedGroup(String::new()),
            shared.clone()
        ]
    );
    let _ = manager.update(Message::Scope(Scope::All));
    assert!(!manager.can_restore_teamwork());
    manager.set_teamwork_available(true);
    assert!(manager.can_restore_teamwork());
    let _ = manager.update(Message::Select(local));
    manager.set_modifiers(Modifiers::COMMAND);
    let _ = manager.update(Message::SelectShared(identity.clone()));
    manager.set_modifiers(Modifiers::empty());
    let _ = manager.update(Message::OpenTreeContext(shared.clone(), Point::ORIGIN));
    let _ = manager.update(Message::TreeContext(Context::Activate(Page::Root, 1)));
    assert_eq!(manager.confirming_teamwork_hide.len(), 1);
    assert_eq!(manager.confirming_delete, [local]);
    assert!(!manager.can_restore_teamwork());
    assert_eq!(manager.update(Message::ConfirmDelete(true)).units(), 0);
    assert!(manager.store.get(local).is_some());
    complete_persisted(&mut manager, Message::ConfirmTeamworkHide(true));
    assert_eq!(manager.confirming_delete, [local]);
    let _ = manager.update(Message::ConfirmDelete(false));
    assert!(manager.store.get(local).is_some());
    assert!(manager.can_restore_teamwork());
    assert_eq!(manager.favorite_count(), 1);
    assert_eq!(fs::read_to_string(&source).unwrap(), xml);

    let _ = manager.update(Message::MapView(true));
    for (scope, count) in [
        (Scope::All, 1),
        (Scope::Teamwork, 0),
        (Scope::Favorites, 1),
        (Scope::Recent, 1),
        (Scope::Open, 1),
        (Scope::Tag("prod".into()), 1),
    ] {
        let _ = manager.update(Message::Scope(scope));
        assert!(!manager.visible_row_keys().contains(&shared));
        assert_eq!(
            manager
                .map_pdf_snapshot()
                .map_or(0, |map| map.connection_count),
            count
        );
        let _ = manager.update(Message::MapSelect(shared.clone()));
        assert!(!manager.selection.contains(&shared));
    }
    manager.selection.replace(Some(shared.clone()));
    assert!(manager.selected_shared().is_none());

    manager.set_teamwork(teamwork::Catalog::default(), None);
    let mut persisted = Store::open(profile.path()).unwrap();
    let hidden = persisted.hidden_teamwork().next().unwrap();
    assert_eq!(hidden.identity(), &identity);
    assert_eq!(hidden.name(), "Shared primary");
    assert_eq!(hidden.host(), "shared.example");
    persisted.restore_teamwork(&identity).unwrap();
    manager.update_store(persisted);
    manager.set_teamwork(catalog, None);
    let _ = manager.update(Message::Scope(Scope::Teamwork));
    assert_eq!(manager.visible_row_keys(), [shared]);
    assert_eq!(manager.map_pdf_snapshot().unwrap().connection_count, 1);
    assert_eq!(manager.favorite_count(), 2);
}

#[test]
fn teamwork_recency_shares_the_combined_cutoff_order_and_search_with_map_pdf() {
    use crate::connection_store::RECENT_LIMIT;
    use crate::session::SerialConnection;
    use kortty_terminal::TerminalProfile;

    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let local = store.save(connection("Local SSH", "")).unwrap();
    store.record_used_at(local, 1_700_000_000_010).unwrap();
    let serial = store
        .save_serial(SerialProfile::new(
            "Local serial",
            SerialConnection {
                device: "COM19".into(),
                ..SerialConnection::default()
            },
            TerminalProfile::Vt220,
        ))
        .unwrap();
    let serial_profile = store.serial(serial).unwrap().clone();
    store
        .record_serial_used_at(&serial_profile, 1_700_000_000_008)
        .unwrap();

    let source = profile.0.join("recency.xml");
    let xml_row = |id: &str, name: &str, millis: u64| {
        format!(
            r#"<connection id="{id}"><name>{name}</name><host>shared.example</host><username>operator</username><lastUsed>{millis}</lastUsed></connection>"#,
        )
    };
    let mut xml = String::from("<connections>");
    xml.push_str(&xml_row("newest", "Shared newest", 1_700_000_000_012));
    xml.push_str(&xml_row("tie", "Shared tie", 1_700_000_000_010));
    xml.push_str(&xml_row("outside", "Outside limit", 1));
    xml.push_str(&xml_row("never", "Never used", 0));
    for index in 0..RECENT_LIMIT {
        xml.push_str(&xml_row(
            &format!("fill-{index}"),
            &format!("Filler {index}"),
            1_699_999_999_999 - index as u64,
        ));
    }
    xml.push_str("</connections>");
    fs::write(&source, &xml).unwrap();
    let settings = teamwork::Settings {
        sources: vec![teamwork::Source {
            location: source.to_str().unwrap().into(),
            enabled: true,
            kind: teamwork::SourceKind::File,
            check_interval_minutes: teamwork::DEFAULT_CHECK_INTERVAL_MINUTES,
        }],
        ..teamwork::Settings::default()
    };
    let catalog =
        teamwork::Catalog::default().refreshed(&settings.active_sources(true, &[]).unwrap());
    let row = |name| {
        RowKey::Shared(
            catalog
                .entries()
                .find(|entry| entry.connection.name == name)
                .unwrap()
                .identity()
                .clone(),
        )
    };
    let newest = row("Shared newest");
    let outside = row("Outside limit");
    let mut manager = ConnectionWindow::open(profile.path()).unwrap();
    manager.set_teamwork(catalog.clone(), None);
    let _ = manager.update(Message::Scope(Scope::Recent));
    let rows: Vec<_> = manager
        .recent_entries()
        .iter()
        .map(selection::VisualRow::key)
        .collect();
    assert_eq!(rows.len(), RECENT_LIMIT);
    assert_eq!(
        rows[..4],
        [
            newest.clone(),
            RowKey::Connection(local),
            row("Shared tie"),
            RowKey::Serial(serial)
        ]
    );
    assert!(!rows.contains(&outside));
    assert!(!rows.contains(&row("Never used")));
    let visible = manager.visible_row_keys();
    assert_eq!(visible.len(), RECENT_LIMIT + 1);
    assert!(visible.contains(&RowKey::SharedGroup(String::new())));
    assert!(rows.iter().all(|row| visible.contains(row)));

    let _ = manager.update(Message::MapView(true));
    let snapshot = manager.map_pdf_snapshot().unwrap();
    assert_eq!(snapshot.scope, "Recently used");
    assert_eq!(snapshot.connection_count, RECENT_LIMIT);
    let _ = manager.update(Message::MapSelect(newest.clone()));
    let _ = manager.update(Message::MapSelect(outside));
    assert!(manager.selection.contains(&newest));
    let _ = manager.update(Message::Search("Shared newest".into()));
    assert_eq!(manager.visible_row_keys(), [newest]);
    assert_eq!(manager.map_pdf_snapshot().unwrap().connection_count, 1);
    let _ = manager.update(Message::Search("Outside limit".into()));
    assert!(manager.visible_row_keys().is_empty());
    assert!(manager.map_pdf_snapshot().is_none());

    assert_eq!(fs::read_to_string(&source).unwrap(), xml);
    assert_eq!(
        Store::open(profile.path())
            .unwrap()
            .recent_ids()
            .collect::<Vec<_>>(),
        [local, serial]
    );
}

#[test]
fn teamwork_tree_collapses_read_only_folders_and_reveals_matching_routes() {
    let profile = Profile::new();
    let source = profile.0.join("shared.xml");
    let xml = r#"<connections>
        <connection id="root"><name>Root host</name><host>root.example</host>
          <username>fixture</username></connection>
        <connection id="direct"><name>Direct host</name><host>direct.example</host>
          <username>fixture</username><group>OPS</group></connection>
        <connection id="nested"><name>Nested host</name><host>nested.example</host>
          <username>fixture</username><group>Ops/West</group></connection>
        </connections>"#;
    fs::write(&source, xml).unwrap();
    let settings = teamwork::Settings {
        sources: vec![teamwork::Source {
            location: source.to_str().unwrap().into(),
            enabled: true,
            kind: teamwork::SourceKind::File,
            check_interval_minutes: teamwork::DEFAULT_CHECK_INTERVAL_MINUTES,
        }],
        ..Default::default()
    };
    let catalog =
        teamwork::Catalog::default().refreshed(&settings.active_sources(true, &[]).unwrap());
    let identity = |id| {
        catalog
            .entries()
            .find(|entry| entry.identity().external_id() == id)
            .unwrap()
            .identity()
            .clone()
    };
    let nested = identity("nested");
    let direct = RowKey::Shared(identity("direct"));
    let ungrouped = RowKey::Shared(identity("root"));
    let branch = RowKey::SharedGroup(String::new());
    let ops = RowKey::SharedGroup("ops".into());
    let west = RowKey::SharedGroup("ops/west".into());
    let leaf = RowKey::Shared(nested.clone());
    let mut manager = ConnectionWindow::open(profile.path()).unwrap();
    manager.set_teamwork(catalog, None);
    assert_eq!(
        manager.visible_row_keys(),
        [
            branch.clone(),
            ops.clone(),
            west.clone(),
            leaf.clone(),
            direct,
            ungrouped
        ]
    );

    let _ = manager.update(Message::SelectShared(nested.clone()));
    let _ = manager.update(Message::ToggleSharedGroup("ops".into()));
    assert!(manager.selected_shared().is_none());
    assert!(!manager.visible_row_keys().contains(&west));
    let _ = manager.update(Message::OpenTreeContext(ops.clone(), Point::ORIGIN));
    assert!(manager.selection.contains(&ops));
    assert!(manager.active_group().is_none());

    let _ = manager.update(Message::ToggleSharedGroup(String::new()));
    assert_eq!(manager.visible_row_keys(), std::slice::from_ref(&branch));
    let _ = manager.update(Message::MapView(true));
    assert_eq!(manager.map_pdf_snapshot().unwrap().connection_count, 3);
    let _ = manager.update(Message::MapSelect(leaf.clone()));
    let _ = manager.update(Message::MapView(false));
    assert_eq!(manager.selected_shared().unwrap().identity(), &nested);
    assert!(manager.visible_row_keys().contains(&west));

    let _ = manager.update(Message::ToggleSharedGroup("ops".into()));
    let _ = manager.update(Message::Search("nested.example".into()));
    assert_eq!(
        manager.visible_row_keys(),
        [branch, ops.clone(), west.clone(), leaf.clone()]
    );
    let _ = manager.update(Message::Scope(Scope::Teamwork));
    assert_eq!(manager.visible_row_keys(), [ops, west, leaf]);
}

#[test]
fn tag_prompt_accepts_worker_completion_and_preserves_failed_edits() {
    use super::tree_context::{Message as Context, Page};
    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let id = store.save(connection("Tag target", "before")).unwrap();
    let mut manager = ConnectionWindow::open(profile.path()).unwrap();
    let _ = manager.update(Message::OpenTreeContext(
        RowKey::Connection(id),
        Point::ORIGIN,
    ));
    let _ = manager.update(Message::TreeContext(Context::Activate(Page::Root, 6)));
    let _ = manager.update(Message::TreeContext(Context::Tag("after".into())));
    assert!(!manager.request_close());
    let _ = manager.update(Message::KeepEditing);
    assert!(!manager.confirming_close);
    assert!(manager.tree_context.has_prompt());
    // Test double: inject the completion of the unpolled save task.
    let _ = manager.update(Message::TreeContext(Context::SaveTag));
    assert!(manager.busy());
    let _ = manager.update(Message::Completed(Err("test save failure".into())));
    assert!(!manager.busy());
    assert!(manager.tree_context.has_prompt());
    let _ = manager.update(Message::TreeContext(Context::SaveTag));
    store.set_connection_tags(&[id], "after").unwrap();
    let _ = manager.update(Message::Completed(Ok(Box::new(
        Persisted::preserving_selection(store, "saved"),
    ))));
    assert!(!manager.busy());
    assert!(!manager.tree_context.has_prompt());
    assert_eq!(manager.store.get(id).unwrap().tag, "after");
}

#[test]
fn library_navigation_and_search_never_leave_a_hidden_connection_connectable() {
    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let root = store.save(connection("Home", "desk")).unwrap();
    let folder = store.create_group(None, "Production").unwrap();
    let nested = store.create_group(Some(&folder), "Servers").unwrap();
    let web = store
        .save_in_group(connection("Web 01", "prod"), Some(&nested))
        .unwrap();
    assert!(store.toggle_connection_favorites(&[web], &[]).unwrap());
    store.record_used(web).unwrap();
    let mut window = ConnectionWindow::open(profile.path()).unwrap();
    assert_eq!(window.selected().map(|entry| entry.id), Some(root));

    let _ = window.update(Message::Scope(Scope::Group(folder.clone())));
    assert_eq!(window.selection.single_group(), Some(folder.as_str()));
    let _ = window.update(Message::Select(web));
    assert_eq!(window.selected().map(|entry| entry.id), Some(web));
    let _ = window.update(Message::Search("old*".into()));
    assert!(window.selected().is_none());
    let _ = window.update(Message::Select(web));
    assert!(window.selected().is_none());
    let _ = window.update(Message::Search("web *".into()));
    let _ = window.update(Message::Select(web));
    assert_eq!(window.selected().map(|entry| entry.id), Some(web));

    for scope in [Scope::Favorites, Scope::Recent, Scope::Tag("PROD".into())] {
        let _ = window.update(Message::Scope(scope));
        assert_eq!(window.selected().map(|entry| entry.id), Some(web));
    }
    window.set_open_counts(OpenCounts {
        saved: BTreeMap::from([(web, 2)]),
        ..OpenCounts::default()
    });
    let _ = window.update(Message::Scope(Scope::Open));
    assert_eq!(window.selected().map(|entry| entry.id), Some(web));
    assert_eq!(window.open_count(web), 2);
    window.set_open_counts(OpenCounts::default());
    assert!(window.selected().is_none());
}

#[test]
fn overview_distinguishes_folder_selection_filtered_rows_and_all_connections() {
    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let unrelated = store.save(connection("Unrelated", "prod")).unwrap();
    let folder = store.create_group(None, "Production").unwrap();
    let child = store.create_group(Some(&folder), "Nested").unwrap();
    let db = store
        .save_in_group(connection("Database", "prod"), Some(&folder))
        .unwrap();
    let web = store
        .save_in_group(connection("Web", "prod"), Some(&child))
        .unwrap();
    let mut manager = ConnectionWindow::open(profile.path()).unwrap();

    let _ = manager.update(Message::Scope(Scope::Group(folder)));
    let _ = manager.update(Message::Search("web".into()));
    let sources = manager.overview_sources().unwrap();
    assert_eq!(sources.selected, [db, web]);
    assert_eq!(sources.shown, [web]);
    assert_eq!(sources.all, [unrelated, db, web]);
    assert_eq!(manager.export_connections_sources().unwrap().1, [db, web]);

    let _ = manager.update(Message::Select(web));
    assert_eq!(manager.overview_sources().unwrap().selected, [web]);
    assert_eq!(manager.export_connections_sources().unwrap().1, [web]);
    let _ = manager.update(Message::EditSelected);
    assert!(manager.overview_sources().is_none());
    assert!(manager.export_connections_sources().is_none());
}

#[test]
fn os_symbol_draft_preserves_existing_badge_until_save_or_explicit_clear() {
    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let id = store
        .save_in_group_with_badge(connection("Operations", ""), None, Some("freebsd"))
        .unwrap();
    let mut window = ConnectionWindow::open(profile.path()).unwrap();
    let _ = window.update(Message::EditSelected);
    let _ = window.update(Message::Edit(Edit::ToggleOsBadgePicker));
    let _ = window.update(Message::Edit(Edit::OsBadge(None)));
    assert_eq!(window.draft.as_ref().unwrap().os_badge_id(), None);
    assert_eq!(
        Store::open(profile.path()).unwrap().os_badge_id(id),
        Some("freebsd")
    );
    let _ = window.update(Message::CancelEdit);

    let _ = window.update(Message::Duplicate);
    let draft = window.draft.as_ref().unwrap();
    assert_eq!(draft.os_badge_id(), Some("freebsd"));
    assert_eq!(draft.connection().unwrap().id, 0);
}

#[test]
fn custom_symbol_resolves_in_manager_and_overview_and_keeps_dormant_reference() {
    use super::overview::{Format, Part, Snapshot};

    let profile = Profile::new();
    let root = fs::canonicalize(&profile.0).unwrap();
    let path = root.join("connections.json");
    let source = root.join("source.png");
    image::RgbaImage::from_pixel(32, 32, image::Rgba([12, 34, 56, 255]))
        .save(&source)
        .unwrap();
    let mut catalog = Catalog::load(&root).unwrap();
    let reference = catalog.add("Primary rack", "Custom", &source).unwrap();
    let mut store = Store::open(path.clone()).unwrap();
    let id = store
        .save_in_group_with_badge(connection("Operations", ""), None, Some(&reference))
        .unwrap();
    let mut manager = ConnectionWindow::open(path.clone()).unwrap();
    assert!(matches!(
        manager.connection_badge(id),
        Some(os_badge::Badge::Custom(_))
    ));
    let parts = [Part::OsBadge].into_iter().collect();
    let overview = |manager: &ConnectionWindow| {
        let sources = manager.overview_sources().unwrap();
        Snapshot::capture(sources.store, sources.custom_badges, |_| None, |_| None)
            .render(&[id], &parts, Format::Json)
            .unwrap()
    };
    assert!(overview(&manager).contains(r#""osBadge": "Primary rack""#));

    let _ = manager.update(Message::Duplicate);
    assert_eq!(
        manager.draft.as_ref().unwrap().os_badge_id(),
        Some(reference.as_str())
    );
    let _ = manager.update(Message::Edit(Edit::ToggleOsBadgePicker));
    let _ = manager.update(Message::Edit(Edit::OsBadge(Some("custom:unknown".into()))));
    assert_eq!(
        manager.draft.as_ref().unwrap().os_badge_id(),
        Some(reference.as_str())
    );
    let _ = manager.update(Message::Edit(Edit::OsBadge(None)));
    assert_eq!(manager.draft.as_ref().unwrap().os_badge_id(), None);
    let _ = manager.update(Message::CancelEdit);

    catalog.rename(&reference, "Backup rack", "Custom").unwrap();
    manager.replace_custom_badges(Arc::new(catalog.clone()));
    assert!(overview(&manager).contains(r#""osBadge": "Backup rack""#));
    catalog.remove(&reference).unwrap();
    manager.replace_custom_badges(Arc::new(catalog));
    assert!(manager.connection_badge(id).is_none());
    assert!(!overview(&manager).contains("osBadge"));
    assert_eq!(
        Store::open(path).unwrap().os_badge_id(id),
        Some(reference.as_str())
    );
}

#[test]
fn connection_tab_color_draft_validates_saves_and_can_be_removed() {
    use super::color_picker::{Edit as Picker, Target};
    use crate::terminal_theme::Rgb;

    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let id = store.save(connection("Operations", "")).unwrap();
    assert!(
        !fs::read_to_string(profile.path())
            .unwrap()
            .contains("tab_color")
    );
    let mut window = ConnectionWindow::open(profile.path()).unwrap();
    let _ = window.update(Message::EditSelected);
    let _ = window.update(Message::Edit(Edit::Page(form::Page::Settings)));
    let picker = |action| Message::Edit(Edit::ColorPicker(action));
    let _ = window.update(picker(Picker::Open(Target::Tab, None)));
    let _ = window.update(picker(Picker::Select(Some(Rgb::from_hex(0x112233)))));
    assert!(
        window
            .draft
            .as_ref()
            .unwrap()
            .connection()
            .unwrap()
            .tab_color
            .is_none()
    );
    let _ = window.update(Message::Edit(Edit::UseTabColor(true)));
    let _ = window.update(picker(Picker::Open(Target::Tab, None)));
    let _ = window.update(picker(Picker::Select(Some(Rgb::from_hex(0xAABBCC)))));
    let draft = window.draft.as_ref().unwrap().connection().unwrap();
    assert_eq!(draft.tab_color.unwrap().to_string(), "#AABBCC");
    let _ = window.update(Message::CancelEdit);
    assert!(
        Store::open(profile.path())
            .unwrap()
            .get(id)
            .unwrap()
            .tab_color
            .is_none()
    );
    store.save(draft).unwrap();
    assert!(
        fs::read_to_string(profile.path())
            .unwrap()
            .contains("\"#AABBCC\"")
    );
    let mut window = ConnectionWindow::open(profile.path()).unwrap();
    let _ = window.update(Message::EditSelected);
    assert_eq!(
        window
            .draft
            .as_ref()
            .unwrap()
            .connection()
            .unwrap()
            .tab_color
            .unwrap()
            .to_string(),
        "#AABBCC"
    );
    let _ = window.update(Message::Edit(Edit::UseTabColor(false)));
    store
        .save(window.draft.as_ref().unwrap().connection().unwrap())
        .unwrap();
    assert!(
        Store::open(profile.path())
            .unwrap()
            .get(id)
            .unwrap()
            .tab_color
            .is_none()
    );
}

#[test]
fn ssh_editor_journal_and_ai_tabs_keep_their_drafts_when_terminal_defaults_are_restored() {
    use iced::advanced::{Layout, Shell, clipboard, layout, widget::Tree};
    use iced::keyboard::{
        self, Key, Location, Modifiers,
        key::{self, Named},
    };
    use iced::{Event, Font, Renderer, Size, mouse};
    use terminal_overrides::Edit as Terminal;

    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let mut saved = connection("Build host", "");
    saved.terminal.font_size = Some(22);
    saved.terminal.ai_skills = vec![17, 19];
    let id = store.save(saved).unwrap();
    let mut window = ConnectionWindow::open(profile.path()).unwrap();
    let _ = window.update(Message::EditSelected);

    let themes = crate::terminal_theme::Themes::default();
    let mut view = window
        .editor_view(ViewChoices {
            passwords: &[],
            passphrases: &[],
            keys: &[],
            temporary_keys: false,
            vault_unlocked: false,
            ai_profiles: &[],
            java_ai_profiles: &[],
            ai_skills: &[],
            ai_agent_designs: &[],
            highlighting: HighlightChoices::default(),
            themes: &themes,
            journal_policy: JournalEditorPolicy::default(),
            terminal_style: Default::default(),
            logging_policy: Default::default(),
            incognito_allowed: true,
            serial_allowed: true,
            terminal_effects: None,
            default_paste_warning: Default::default(),
            minimum_paste_warning: PasteWarningMode::Off,
            isolation: Default::default(),
            local_isolation_default: Default::default(),
        })
        .unwrap();
    let compact = Size::new(640.0, 430.0);
    let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
    let mut tree = Tree::new(view.as_widget());
    let node = view.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, compact),
    );
    let root = Layout::new(&node);
    let body = root.children().next().unwrap();
    let form = body.children().next().unwrap().children().next().unwrap();
    let tabs: Vec<_> = form
        .children()
        .nth(1)
        .unwrap()
        .children()
        .next()
        .unwrap()
        .children()
        .map(|tab| tab.bounds())
        .collect();
    assert_eq!(tabs.last().unwrap().width, 34.0);
    assert!(tabs.iter().all(|tab| tab.x + tab.width <= compact.width));
    assert!(
        tabs.iter()
            .all(|tab| (tab.center().y - tabs[0].center().y).abs() < 1.0)
    );
    let footer = body.children().nth(2).unwrap().bounds();
    assert!(footer.height > 0.0 && footer.y + footer.height <= compact.height);

    for (named, modifiers, selected) in [
        (Named::Tab, Modifiers::CTRL, form::Page::Settings),
        (
            Named::Tab,
            Modifiers::CTRL | Modifiers::SHIFT,
            form::Page::Connection,
        ),
        (Named::PageDown, Modifiers::CTRL, form::Page::Settings),
        (Named::PageUp, Modifiers::CTRL, form::Page::Connection),
    ] {
        let key = Key::Named(named);
        let event = Event::Keyboard(keyboard::Event::KeyPressed {
            modified_key: key.clone(),
            key,
            location: Location::Standard,
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            modifiers,
            text: None,
            repeat: false,
        });
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        view.as_widget_mut().update(
            &mut tree,
            &event,
            root,
            mouse::Cursor::Unavailable,
            &renderer,
            &mut clipboard::Null,
            &mut shell,
            &node.bounds(),
        );
        assert!(shell.is_event_captured());
        assert!(
            matches!(messages.as_slice(), [Message::Edit(Edit::Page(page))] if *page == selected)
        );
    }
    drop(view);

    let _ = window.update(Message::Edit(Edit::Page(form::Page::Ai)));
    let _ = window.update(Message::Edit(Edit::Terminal(Terminal::AiSkillSearch(
        "release".into(),
    ))));
    assert!(!window.draft.as_ref().unwrap().changed);
    for edit in [
        Edit::Terminal(Terminal::AiAgentDisabled(true)),
        Edit::Page(form::Page::Journal),
        Edit::Terminal(Terminal::Journal(journal_settings::Edit::Auto(true))),
        Edit::Terminal(Terminal::Journal(journal_settings::Edit::Input(true))),
        Edit::Terminal(Terminal::Journal(journal_settings::Edit::Interval(
            "45".into(),
        ))),
        Edit::Page(form::Page::Settings),
        Edit::Terminal(Terminal::ResetTerminal),
    ] {
        let _ = window.update(Message::Edit(edit));
    }
    complete_persisted(&mut window, Message::Save);
    let reopened = Store::open(profile.path()).unwrap();
    let terminal = &reopened.get(id).unwrap().terminal;
    assert!(terminal.font_size.is_none());
    assert!(terminal.ai_agent_disabled);
    assert_eq!(terminal.ai_skills, [17, 19]);
    assert!(terminal.journal.auto_start);
    assert!(terminal.journal.capture_input);
    assert_eq!(terminal.journal.summary_interval_minutes, 45);

    let _ = window.update(Message::Duplicate);
    assert_eq!(
        window
            .draft
            .as_ref()
            .unwrap()
            .connection()
            .unwrap()
            .terminal
            .journal,
        terminal.journal
    );
    complete_persisted(&mut window, Message::Save);
    let duplicated = Store::open(profile.path()).unwrap();
    assert_eq!(duplicated.connections().len(), 2);
    assert!(
        duplicated
            .connections()
            .iter()
            .all(|saved| saved.terminal.journal == terminal.journal)
    );
}

#[test]
fn ssh_editor_ai_tab_moves_focus_and_assigns_skills_with_the_keyboard() {
    use crate::settings_search::test_input;
    use crate::terminal_agent::design::Choice;
    use iced::keyboard::{Key, key::Named};
    use iced::{Font, Renderer, Size, Theme};
    use iced_runtime::{UserInterface, user_interface::Cache};
    use terminal_overrides::Edit as Terminal;

    let profile = Profile::new();
    let mut saved = connection("Keyboard fixture", "");
    saved.terminal.ai_skills = (9000..9020).collect();
    let id = Store::open(profile.path()).unwrap().save(saved).unwrap();
    let mut window = ConnectionWindow::open(profile.path()).unwrap();
    let _ = window.update(Message::EditSelected);
    let _ = window.update(Message::Edit(Edit::Page(form::Page::Ai)));
    let themes = crate::terminal_theme::Themes::default();
    let designs = [
        Choice {
            id: String::new(),
            name: "Default".into(),
        },
        Choice {
            id: "standard".into(),
            name: "Standard".into(),
        },
    ];
    let view = keyboard_editor_view;
    let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
    let compact = Size::new(900.0, 410.0);
    let mut cache = Cache::default();
    for _ in 0..4 {
        let mut interface = UserInterface::build(
            view(&window, &themes, &designs),
            compact,
            cache,
            &mut renderer,
        );
        let events = test_input::press(&mut interface, &mut renderer, Key::Named(Named::Tab), None);
        assert!(matches!(events.as_slice(), [Message::FocusEditor(false)]));
        let focus = window.draft.as_ref().unwrap().focus_editor(false);
        test_input::apply(&mut interface, &renderer, focus);
        cache = interface.into_cache();
    }
    let mut interface = UserInterface::build(
        view(&window, &themes, &designs),
        compact,
        cache,
        &mut renderer,
    );
    let named =
        |interface: &mut UserInterface<'_, Message, Theme, Renderer>,
         renderer: &mut Renderer,
         key| { test_input::press(interface, renderer, Key::Named(key), None) };
    assert!(named(&mut interface, &mut renderer, Named::Enter).is_empty());
    assert!(named(&mut interface, &mut renderer, Named::End).is_empty());
    assert!(matches!(
        named(&mut interface, &mut renderer, Named::Enter).as_slice(),
        [Message::Edit(Edit::Terminal(Terminal::AiAgentDesign(Some(design))))] if design == "standard"
    ));
    for _ in 0..2 {
        assert!(matches!(
            named(&mut interface, &mut renderer, Named::Tab).as_slice(),
            [Message::FocusEditor(false)]
        ));
        let focus = window.draft.as_ref().unwrap().focus_editor(false);
        test_input::apply(&mut interface, &renderer, focus);
    }
    let mut navigate = named(&mut interface, &mut renderer, Named::End);
    assert!(matches!(
        navigate.as_slice(),
        [Message::Edit(Edit::Terminal(Terminal::AiSkillNavigate(
            9019
        )))]
    ));
    let cache = interface.into_cache();
    let reveal = window.update(navigate.remove(0));
    assert!(!window.draft.as_ref().unwrap().changed);

    let mut interface = UserInterface::build(
        view(&window, &themes, &designs),
        compact,
        cache,
        &mut renderer,
    );
    test_input::apply(&mut interface, &renderer, reveal);
    let mut assignment = named(&mut interface, &mut renderer, Named::Space);
    assert!(matches!(
        assignment.as_slice(),
        [Message::Edit(Edit::Terminal(Terminal::AiSkill(
            9019, false
        )))]
    ));
    let cache = interface.into_cache();
    let _ = window.update(assignment.remove(0));
    let mut interface = UserInterface::build(
        view(&window, &themes, &designs),
        compact,
        cache,
        &mut renderer,
    );
    assert!(matches!(
        named(&mut interface, &mut renderer, Named::Tab).as_slice(),
        [Message::FocusEditor(false)]
    ));
    let focus = window.draft.as_ref().unwrap().focus_editor(false);
    test_input::apply(&mut interface, &renderer, focus);
    assert!(matches!(
        named(&mut interface, &mut renderer, Named::Enter).as_slice(),
        [Message::Save]
    ));
    drop(interface);
    complete_persisted(&mut window, Message::Save);
    let reopened = Store::open(profile.path()).unwrap();
    assert_eq!(
        reopened.get(id).unwrap().terminal.ai_skills,
        (9000..9019).collect::<Vec<_>>()
    );
}

fn keyboard_editor_view<'a>(
    window: &'a ConnectionWindow,
    themes: &crate::terminal_theme::Themes,
    designs: &[crate::terminal_agent::design::Choice],
) -> iced::Element<'a, Message> {
    window
        .editor_view(ViewChoices {
            passwords: &[],
            passphrases: &[],
            keys: &[],
            temporary_keys: false,
            vault_unlocked: false,
            ai_profiles: &[],
            java_ai_profiles: &[],
            ai_skills: &[],
            ai_agent_designs: designs,
            highlighting: HighlightChoices::default(),
            themes,
            journal_policy: JournalEditorPolicy::default(),
            terminal_style: Default::default(),
            logging_policy: Default::default(),
            incognito_allowed: true,
            serial_allowed: true,
            terminal_effects: None,
            default_paste_warning: Default::default(),
            minimum_paste_warning: PasteWarningMode::Off,
            isolation: Default::default(),
            local_isolation_default: Default::default(),
        })
        .unwrap()
}

#[test]
fn ssh_editor_skips_disabled_jump_and_log_controls_in_tab_order() {
    use crate::settings_search::test_input;
    use iced::keyboard::{Key, key::Named};
    use iced::{Font, Renderer, Size, Theme};
    use iced_runtime::{UserInterface, user_interface::Cache};

    let profile = Profile::new();
    Store::open(profile.path())
        .unwrap()
        .save(connection("Private test", ""))
        .unwrap();
    let mut window = ConnectionWindow::open(profile.path()).unwrap();
    let _ = window.update(Message::EditSelected);
    let themes = crate::terminal_theme::Themes::default();
    let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
    for page in [form::Page::JumpHost, form::Page::Logging] {
        let _ = window.update(Message::Edit(Edit::Page(page)));
        let mut interface = UserInterface::build(
            keyboard_editor_view(&window, &themes, &[]),
            Size::new(900.0, 420.0),
            Cache::default(),
            &mut renderer,
        );
        let tab = |interface: &mut UserInterface<'_, Message, Theme, Renderer>,
                   renderer: &mut Renderer| {
            let messages = test_input::press(interface, renderer, Key::Named(Named::Tab), None);
            assert!(matches!(messages.as_slice(), [Message::FocusEditor(false)]));
            let focus = window.draft.as_ref().unwrap().focus_editor(false);
            test_input::apply(interface, renderer, focus);
        };
        tab(&mut interface, &mut renderer);
        let toggle = test_input::press(
            &mut interface,
            &mut renderer,
            Key::Named(Named::Space),
            None,
        );
        assert!(match page {
            form::Page::JumpHost =>
                matches!(toggle.as_slice(), [Message::Edit(Edit::UseJump(true))]),
            form::Page::Logging => matches!(
                toggle.as_slice(),
                [Message::Edit(Edit::Logging(
                    crate::terminal_logging::Edit::Enabled(true)
                ))]
            ),
            _ => unreachable!(),
        });
        tab(&mut interface, &mut renderer);
        let save = test_input::press(
            &mut interface,
            &mut renderer,
            Key::Named(Named::Enter),
            None,
        );
        assert!(matches!(save.as_slice(), [Message::Save]));
    }
}

#[test]
fn connection_paste_warning_round_trips_explicit_and_inherited_values() {
    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let id = store.save(connection("Lab", "")).unwrap();
    assert!(
        !fs::read_to_string(profile.path())
            .unwrap()
            .contains("paste_warning")
    );
    let mut window = ConnectionWindow::open(profile.path()).unwrap();
    let _ = window.update(Message::EditSelected);
    let _ = window.update(Message::Edit(Edit::PasteWarning(Some(
        PasteWarningMode::Off,
    ))));
    let edited = window.draft.as_ref().unwrap().connection().unwrap();
    let _ = window.update(Message::CancelEdit);
    assert!(window.store.get(id).unwrap().paste_warning.is_none());
    store.save(edited).unwrap();

    let mut window = ConnectionWindow::open(profile.path()).unwrap();
    assert_eq!(
        window.store.get(id).unwrap().paste_warning,
        Some(PasteWarningMode::Off)
    );
    let _ = window.update(Message::EditSelected);
    let _ = window.update(Message::Edit(Edit::PasteWarning(None)));
    store
        .save(window.draft.as_ref().unwrap().connection().unwrap())
        .unwrap();
    assert!(
        Store::open(profile.path())
            .unwrap()
            .get(id)
            .unwrap()
            .paste_warning
            .is_none()
    );
}

#[test]
fn folder_tab_color_dialog_validates_and_discards_stale_drafts() {
    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let parent = store.create_group(None, "Production").unwrap();
    let folder = store.create_group(Some(&parent), "Database").unwrap();
    let parent_color = TabColor::PRESETS[0].0;
    store.set_group_color(&parent, Some(parent_color)).unwrap();
    let mut window = ConnectionWindow::open(profile.path()).unwrap();
    let _ = window.update(Message::SelectGroup(folder.clone()));
    let _ = window.update(Message::EditGroupColor);
    let _ = window.update(Message::GroupColorEnabled(true));
    let Some(FolderDialog::Color { draft, .. }) = &window.folder else {
        panic!("expected folder color draft");
    };
    assert_eq!(draft.value(), Ok(Some(parent_color)));
    let _ = window.update(Message::GroupColorInput("invalid".into()));
    assert_eq!(window.update(Message::CommitGroup).units(), 0);
    assert!(!window.busy());
    assert!(!window.request_close());
    let _ = window.update(Message::KeepEditing);
    let _ = window.update(Message::CancelGroup);
    assert_eq!(
        Store::open(profile.path()).unwrap().group_color(&folder),
        None
    );

    let _ = window.update(Message::EditGroupColor);
    assert_eq!(window.update(Message::CommitGroup).units(), 0);
    assert!(window.folder.is_none());

    let _ = window.update(Message::EditGroupColor);
    let _ = window.update(Message::GroupColorEnabled(true));
    let _ = window.update(Message::GroupColorInput("#abc".into()));
    let Some(FolderDialog::Color { draft, .. }) = &window.folder else {
        panic!("expected folder color draft")
    };
    let color = draft.value().unwrap();
    assert_eq!(window.update(Message::CommitGroup).units(), 1);
    // Test double for Iced delivery. The store commit itself uses real filesystem IO.
    store.set_group_color(&folder, color).unwrap();
    let _ = window.update(Message::Completed(Ok(Box::new(Persisted {
        store: store.clone(),
        selection: SelectionChange::Replace(None),
        scope: Some(Scope::Group(folder.clone())),
        status: "Folder tab color saved.",
        folder_rename: None,
    }))));
    assert_eq!(window.store.group_color(&folder), color);
    assert!(window.folder.is_none());

    let _ = window.update(Message::EditGroupColor);
    let _ = window.update(Message::GroupColorEnabled(false));
    store.set_group_color(&folder, Some(parent_color)).unwrap();
    window.update_store(store);
    assert!(window.folder.is_none());
    assert_eq!(window.update(Message::CommitGroup).units(), 0);
    assert_eq!(window.store.group_color(&folder), Some(parent_color));
}

#[test]
fn an_external_add_preserves_the_draft_and_a_conflicting_edit_blocks_save() {
    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let id = store.save(connection("Original", "desk")).unwrap();
    let mut window = ConnectionWindow::open(profile.path()).unwrap();
    let _ = window.update(Message::EditSelected);
    let _ = window.update(Message::Edit(Edit::Name("Unsaved name".into())));

    let added = store
        .save(connection("Added in Quick Connect", ""))
        .unwrap();
    window.update_store(store.clone());
    assert!(window.store.get(added).is_some());
    assert!(!window.draft_conflict);
    assert_eq!(
        window.draft.as_ref().unwrap().connection().unwrap().name,
        "Unsaved name"
    );

    let mut changed = store.get(id).unwrap().clone();
    changed.login.endpoint.port = 2222;
    store.save(changed).unwrap();
    window.update_store(store);
    assert!(window.draft_conflict);
    assert_eq!(window.update(Message::Save).units(), 0);
    assert!(!window.request_close());
    let _ = window.update(Message::KeepEditing);
    let _ = window.update(Message::CancelEdit);
    assert_eq!(window.selected().unwrap().login.endpoint.port, 2222);
    assert_eq!(window.selected().unwrap().name, "Original");
}

#[test]
fn a_changed_folder_invalidates_the_pending_delete_confirmation() {
    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let folder = store.create_group(None, "Production").unwrap();
    let mut window = ConnectionWindow::open(profile.path()).unwrap();
    let _ = window.update(Message::Scope(Scope::Group(folder.clone())));
    let _ = window.update(Message::DeleteGroup);
    assert!(matches!(
        &window.folder,
        Some(FolderDialog::Delete { count: 0, .. })
    ));
    let child = store
        .create_group(Some(&folder), "New empty folder")
        .unwrap();
    window.update_store(store);
    assert!(window.folder.is_none());
    assert_eq!(window.update(Message::CommitGroup).units(), 0);
    assert!(window.store.has_group(&child));
}

#[test]
fn connection_manager_pointer_drags_accept_local_folders_and_refuse_teamwork_sources() {
    use folder_moves::Profile as Movable;
    use iced::advanced::{Layout, Shell, clipboard, layout, widget::Tree};
    use iced::{Event, Font, Renderer, Size, mouse};

    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let destination = store.create_group(None, "Destination").unwrap();
    let id = store.save(connection("Writable", "")).unwrap();
    let shared_file = profile.0.join("shared.xml");
    fs::write(
        &shared_file,
        r#"<connections><connection id="one"><name>Shared</name><host>shared.example.test</host><username>operator</username></connection></connections>"#,
    )
    .unwrap();
    let settings = teamwork::Settings {
        sources: vec![teamwork::Source {
            location: shared_file.to_str().unwrap().into(),
            enabled: true,
            kind: teamwork::SourceKind::File,
            check_interval_minutes: teamwork::DEFAULT_CHECK_INTERVAL_MINUTES,
        }],
        ..Default::default()
    };
    let catalog =
        teamwork::Catalog::default().refreshed(&settings.active_sources(true, &[]).unwrap());
    let mut manager = ConnectionWindow::open(profile.path()).unwrap();
    manager.set_teamwork(catalog, None);
    let keys = manager.visible_row_keys();
    assert!(matches!(keys.last(), Some(RowKey::Shared(_))));
    let row_index = |key: &RowKey| keys.iter().position(|row| row == key).unwrap();
    let themes = crate::terminal_theme::Themes::default();
    let mut view = manager.view(ViewChoices {
        temporary_keys: false,
        vault_unlocked: false,
        journal_policy: JournalEditorPolicy::default(),
        terminal_style: Default::default(),
        logging_policy: Default::default(),
        serial_allowed: true,
        passwords: &[],
        passphrases: &[],
        keys: &[],
        ai_profiles: &[],
        java_ai_profiles: &[],
        ai_skills: &[],
        ai_agent_designs: &[],
        highlighting: HighlightChoices::default(),
        themes: &themes,
        incognito_allowed: true,
        terminal_effects: Some(&Effect::ALL),
        default_paste_warning: Default::default(),
        minimum_paste_warning: PasteWarningMode::Off,
        isolation: Default::default(),
        local_isolation_default: Default::default(),
    });
    let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
    let mut tree = Tree::new(view.as_widget());
    let node = view.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, Size::new(1240.0, 760.0)),
    );
    let layout = Layout::new(&node);
    let workbench = layout.children().nth(2).unwrap();
    let results = workbench
        .children()
        .nth(2)
        .unwrap()
        .children()
        .next()
        .unwrap();
    let rows = results
        .children()
        .nth(1)
        .unwrap()
        .children()
        .next()
        .unwrap();
    let sidebar = workbench
        .children()
        .next()
        .unwrap()
        .children()
        .next()
        .unwrap();
    let sidebar_rows: Vec<_> = sidebar.children().next().unwrap().children().collect();
    let sidebar_destination = sidebar_rows[sidebar_rows.len() - 3].bounds().center();
    let sidebar_teamwork = sidebar_rows.last().unwrap().bounds().center();
    let row_point = |key| {
        rows.children()
            .nth(row_index(&key))
            .unwrap()
            .bounds()
            .center()
    };
    let start = row_point(RowKey::Connection(id));
    let group = row_point(RowKey::Group(destination.clone()));
    let shared = rows.children().last().unwrap().bounds().center();
    let mut drag = |start, end| {
        let mut messages = Vec::new();
        for event in [
            mouse::Event::CursorMoved { position: start },
            mouse::Event::ButtonPressed(mouse::Button::Left),
            mouse::Event::CursorMoved { position: end },
            mouse::Event::ButtonReleased(mouse::Button::Left),
        ] {
            // A platform event batch can report the final cursor for every event.
            view.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(event),
                layout,
                mouse::Cursor::Available(end),
                &renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
                &node.bounds(),
            );
        }
        messages
    };
    for target in [group, sidebar_destination] {
        assert!(matches!(
            drag(start, target).as_slice(),
            [Message::TreeMove(Movable::Ssh(source), folder)] if *source == id && *folder == destination
        ));
    }
    assert!(
        drag(shared, sidebar_destination)
            .iter()
            .all(|message| !matches!(message, Message::TreeMove(..)))
    );
    assert!(
        drag(start, sidebar_teamwork)
            .iter()
            .all(|message| !matches!(message, Message::TreeMove(..)))
    );
    let mut messages = Vec::new();
    for event in [
        mouse::Event::ButtonPressed(mouse::Button::Left),
        mouse::Event::ButtonReleased(mouse::Button::Left),
    ] {
        view.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(event),
            layout,
            mouse::Cursor::Available(sidebar_teamwork),
            &renderer,
            &mut clipboard::Null,
            &mut Shell::new(&mut messages),
            &node.bounds(),
        );
    }
    assert!(matches!(
        messages.as_slice(),
        [Message::Scope(Scope::Teamwork)]
    ));
}

#[test]
fn native_workbench_keeps_reference_side_widths_at_default_and_minimum_size() {
    use iced::advanced::{Layout, layout, widget::Tree};
    use iced::{Font, Renderer, Size};

    let profile = Profile::new();
    let mut window = ConnectionWindow::open(profile.path()).unwrap();
    window.set_teamwork_auth(Some(teamwork::DefaultAuth::Ordered(Default::default())));
    let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
    let mut workbench_y = 0.0;
    for (width, height, scope) in [
        (1240.0, 760.0, Scope::All),
        (940.0, 520.0, Scope::All),
        (940.0, 520.0, Scope::Teamwork),
    ] {
        let _ = window.update(Message::Scope(scope.clone()));
        let mut view = window.view(ViewChoices {
            temporary_keys: false,
            vault_unlocked: false,
            journal_policy: JournalEditorPolicy::default(),
            terminal_style: Default::default(),
            logging_policy: Default::default(),
            serial_allowed: true,
            passwords: &[],
            passphrases: &[],
            keys: &[],
            ai_profiles: &[],
            java_ai_profiles: &[],
            ai_skills: &[],
            ai_agent_designs: &[],
            highlighting: HighlightChoices::default(),
            themes: &crate::terminal_theme::Themes::default(),
            incognito_allowed: true,
            terminal_effects: Some(&Effect::ALL),
            default_paste_warning: Default::default(),
            minimum_paste_warning: PasteWarningMode::Off,
            isolation: Default::default(),
            local_isolation_default: Default::default(),
        });
        let mut tree = Tree::new(view.as_widget());
        let layout = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(width, height)),
        );
        let layout = Layout::new(&layout);
        let workbench = layout.children().nth(2).unwrap();
        let widths: Vec<_> = workbench
            .children()
            .map(|pane| pane.bounds().width)
            .collect();
        assert_eq!(workbench.bounds().x, 12.0);
        assert_eq!(workbench.bounds().width, width - 24.0);
        assert_eq!(widths, [210.0, 4.0, width - 612.0, 4.0, 370.0]);
        if scope == Scope::Teamwork {
            assert_eq!(workbench.bounds().y, workbench_y);
            let center = workbench.children().nth(2).unwrap();
            let body = center.children().next().unwrap();
            let sections: Vec<_> = body.children().map(|child| child.bounds()).collect();
            assert_eq!(sections.len(), 3);
            let [header, auth, list] = sections.as_slice() else {
                unreachable!()
            };
            assert!(auth.x >= center.bounds().x);
            assert!(auth.x + auth.width <= center.bounds().x + center.bounds().width);
            assert!(auth.y >= header.y + header.height && auth.height <= 200.0);
            assert!(list.y >= auth.y + auth.height && list.height >= 100.0);
        } else {
            workbench_y = workbench.bounds().y;
        }
    }
}

#[test]
fn long_scope_titles_keep_list_and_map_visible_at_minimum_width() {
    use iced::advanced::{Layout, layout, widget::Tree};
    use iced::{Font, Renderer, Size};

    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let long_scope = "W".repeat(96);
    let folder = store.create_group(None, &long_scope).unwrap();
    store
        .save_in_group(connection("Mapped host", &long_scope), Some(&folder))
        .unwrap();
    let mut window = ConnectionWindow::open(profile.path()).unwrap();
    let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
    let switch_widths = |window: &ConnectionWindow| {
        let mut view = window.view(ViewChoices {
            temporary_keys: false,
            vault_unlocked: false,
            journal_policy: JournalEditorPolicy::default(),
            terminal_style: Default::default(),
            logging_policy: Default::default(),
            serial_allowed: true,
            passwords: &[],
            passphrases: &[],
            keys: &[],
            ai_profiles: &[],
            java_ai_profiles: &[],
            ai_skills: &[],
            ai_agent_designs: &[],
            highlighting: HighlightChoices::default(),
            themes: &crate::terminal_theme::Themes::default(),
            incognito_allowed: true,
            terminal_effects: Some(&Effect::ALL),
            default_paste_warning: Default::default(),
            minimum_paste_warning: PasteWarningMode::Off,
            isolation: Default::default(),
            local_isolation_default: Default::default(),
        });
        let mut tree = Tree::new(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(940.0, 760.0)),
        );
        let workbench = Layout::new(&node).children().nth(2).unwrap();
        let center = workbench.children().nth(2).unwrap();
        let results = center.children().next().unwrap();
        let header = results.children().next().unwrap();
        let switch = header.children().last().unwrap();
        let widths: Vec<_> = switch
            .children()
            .map(|button| {
                let button = button.bounds();
                let center = center.bounds();
                assert!(button.x >= center.x && button.x + button.width <= center.x + center.width);
                assert!(button.width > 0.0);
                button.width
            })
            .collect();
        assert_eq!(widths.len(), 2);
        widths
    };
    let expected = switch_widths(&window);
    for scope in [Scope::Group(folder), Scope::Tag(long_scope.to_lowercase())] {
        let _ = window.update(Message::Scope(scope));
        assert_eq!(switch_widths(&window), expected);
        let _ = window.update(Message::MapView(true));
        assert_eq!(switch_widths(&window), expected);
        assert_eq!(window.map_pdf_snapshot().unwrap().connection_count, 1);
        let _ = window.update(Message::MapView(false));
    }
}

#[test]
fn editor_retains_the_last_valid_speed_and_duplicates_the_saved_effect() {
    use crate::terminal_effects::{Selection, Speed};

    let profile = Profile::new();
    let mut saved = connection("Console", "");
    let effect = Effect::AmberCrt90;
    let imported_speed = Speed::try_from(2.5).unwrap();
    saved.terminal_effect = Some(Selection {
        effect,
        speed: imported_speed,
    });
    Store::open(profile.path()).unwrap().save(saved).unwrap();
    let mut window = ConnectionWindow::open(profile.path()).unwrap();
    let _ = window.update(Message::Duplicate);
    assert_eq!(window.editing_terminal_effect(), Some(effect));
    assert_eq!(
        window
            .draft
            .as_ref()
            .unwrap()
            .connection()
            .unwrap()
            .terminal_effect,
        Some(Selection {
            effect,
            speed: imported_speed
        })
    );
    for value in ["", "8.75", "0", "100", "∅"] {
        let _ = window.update(Message::Edit(Edit::TerminalEffectSpeed(value.to_owned())));
    }
    let draft = window.draft.as_ref().unwrap().connection().unwrap();
    assert_eq!(draft.id, 0);
    assert_eq!(
        draft.terminal_effect,
        Some(Selection {
            effect,
            speed: Speed::try_from(8.75).unwrap()
        })
    );
    let _ = window.update(Message::Edit(Edit::TerminalEffect(None)));
    let draft = window.draft.as_ref().unwrap().connection().unwrap();
    assert_eq!(draft.terminal_effect, None);
    let _ = window.update(Message::CancelEdit);
    assert_eq!(
        window.selected().unwrap().terminal_effect,
        Some(Selection {
            effect,
            speed: imported_speed
        })
    );
}

#[test]
fn saved_ssh_terminal_settings_switches_sources_without_losing_the_editor_draft() {
    use crate::connection_store::{TerminalAppearance, TerminalStyle, TerminalStylePreference};
    use crate::terminal_theme::Rgb;
    use kortty_terminal::TextEncoding;

    let mut saved = connection("SSH fixture", "");
    saved.command_timestamps = Some(false);
    saved.terminal.font_size = Some(23);
    saved.terminal.encoding = Some(TextEncoding::Windows1252);
    saved.terminal.terminal_columns = Some(88);
    saved.terminal.paste_line_delay_ms = Some(15);
    let mut draft = form::Draft::from_profile(&saved, None, false);
    let defaults = Box::new(TerminalStyle {
        appearance: TerminalAppearance {
            font_size: Some(17),
            foreground: Some(Rgb::from_hex(0x112233)),
            ..TerminalAppearance::default()
        },
        command_timestamps: Some(true),
        ..TerminalStyle::default()
    });
    assert_eq!(draft.connection().unwrap().terminal.font_size, Some(23));
    draft.update(Edit::Terminal(terminal_overrides::Edit::FontSize(
        "25".into(),
    )));
    draft.update(Edit::UseOwnTerminalSettings(None));
    let global = draft.connection().unwrap();
    assert!(global.terminal.font_size.is_none());
    assert!(global.command_timestamps.is_none());
    assert_eq!(global.terminal.encoding, saved.terminal.encoding);
    assert_eq!(global.terminal.terminal_columns, Some(88));
    assert_eq!(global.terminal.paste_line_delay_ms, Some(15));
    let Some(TerminalStylePreference::Global(retained)) = &global.terminal.style_preference else {
        panic!("the saved own settings must be retained");
    };
    assert_eq!(retained.appearance.font_size, Some(23));
    assert_eq!(retained.command_timestamps, Some(false));
    draft.update(Edit::Terminal(terminal_overrides::Edit::FontSize(
        "99".into(),
    )));
    draft.update(Edit::UseOwnTerminalSettings(Some(defaults.clone())));
    assert_eq!(draft.connection().unwrap().terminal.font_size, Some(25));
    let mut reopened = form::Draft::from_profile(&global, None, false);
    assert!(reopened.connection().unwrap().terminal.font_size.is_none());
    reopened.update(Edit::UseOwnTerminalSettings(Some(defaults)));
    let custom = reopened.connection().unwrap();
    assert_eq!(custom.terminal.font_size, Some(17));
    assert_eq!(custom.terminal.foreground, Some(Rgb::from_hex(0x112233)));
    assert_eq!(custom.command_timestamps, Some(true));
}

#[test]
fn saved_ssh_color_picker_keeps_unapplied_values_out_of_the_connection() {
    use super::color_picker::{Edit as Picker, Target};
    use super::custom_color::{Change, Mode};
    use crate::terminal_theme::Rgb;

    let mut saved = connection("SSH colors", "");
    let original = Rgb::from_hex(0xC0FFEE);
    let custom = Rgb::from_hex(0x123456);
    saved.terminal.foreground = Some(original);
    saved.terminal.background = Some(Rgb::from_hex(0x010203));
    let mut draft = form::Draft::from_profile(&saved, None, false);
    draft.update(Edit::Page(form::Page::Settings));
    let edit = Edit::ColorPicker;

    draft.update(edit(Picker::Open(Target::Foreground, Some(original))));
    let mut editor = draft.begin_custom_color().unwrap();
    editor.update(Change::Mode(Mode::Web));
    editor.update(Change::Hex("#12".into()));
    drop(draft.finish_custom_color(&editor, true, false));
    assert_eq!(
        draft.connection().unwrap().terminal.foreground,
        Some(original)
    );
    assert!(!draft.changed);
    editor.update(Change::Hex(custom.to_string()));
    drop(draft.finish_custom_color(&editor, false, false));
    assert!(!draft.changed);

    drop(draft.finish_custom_color(&editor, true, true));
    assert!(draft.changed);
    assert_eq!(
        draft.connection().unwrap().terminal.foreground,
        Some(custom)
    );

    draft.update(edit(Picker::Open(Target::Background, None)));
    draft.update(edit(Picker::Select(None)));
    let terminal = draft.connection().unwrap().terminal;
    assert_eq!(terminal.foreground, Some(custom));
    assert_eq!(terminal.background, None);
}

#[test]
fn editor_preserves_timestamp_visibility_in_duplicates_and_keeps_discarded_edits_local() {
    let profile = Profile::new();
    let mut saved = connection("Console", "");
    saved.command_timestamps = Some(false);
    Store::open(profile.path()).unwrap().save(saved).unwrap();
    let mut window = ConnectionWindow::open(profile.path()).unwrap();

    let _ = window.update(Message::Duplicate);
    let duplicate = window.draft.as_ref().unwrap().connection().unwrap();
    assert_eq!(duplicate.id, 0);
    assert_eq!(duplicate.command_timestamps, Some(false));

    for visibility in [Some(true), None] {
        let _ = window.update(Message::Edit(Edit::CommandTimestamps(visibility)));
        let draft = window.draft.as_ref().unwrap();
        assert!(draft.changed);
        assert_eq!(draft.connection().unwrap().command_timestamps, visibility);
    }

    let _ = window.update(Message::CancelEdit);
    assert_eq!(window.selected().unwrap().command_timestamps, Some(false));
}
