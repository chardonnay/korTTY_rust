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
    complete_persisted(&mut manager, Message::MapUndo);
    assert_eq!(
        Store::open(profile.path()).unwrap().group_of(id),
        Some(from.as_str())
    );
    assert!(task::into_stream(manager.update(Message::MapUndo)).is_none());
    let _ = manager.update(Message::MapView(false));
    assert_eq!(manager.selected().unwrap().id, id);
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
    manager.set_open_counts(BTreeMap::new());
    assert_eq!(manager.favorite_count(), 1);

    let _ = manager.update(Message::Select(local));
    manager.set_modifiers(Modifiers::COMMAND);
    let _ = manager.update(Message::SelectShared(primary.clone()));
    manager.set_modifiers(Modifiers::empty());
    let _ = manager.update(Message::OpenTreeContext(primary_row.clone(), Point::ORIGIN));
    assert_eq!(manager.selected_connection_ids(), vec![local]);
    assert!(manager.selection.contains(&primary_row));
    manager.set_open_counts(BTreeMap::new());
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
        [RowKey::Connection(local), primary_row.clone()]
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
    let rows = manager.visible_row_keys();
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
    window.set_open_counts(BTreeMap::from([(web, 2)]));
    let _ = window.update(Message::Scope(Scope::Open));
    assert_eq!(window.selected().map(|entry| entry.id), Some(web));
    assert_eq!(window.open_count(web), 2);
    window.set_open_counts(BTreeMap::new());
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
    let _ = window.update(Message::Edit(Edit::UseTabColor(true)));
    let _ = window.update(Message::Edit(Edit::TabColor("#abcd".into())));
    assert!(window.draft.as_ref().unwrap().connection().is_err());
    let _ = window.update(Message::Edit(Edit::TabColor(" #Abc ".into())));
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
fn native_workbench_keeps_reference_side_widths_at_default_and_minimum_size() {
    use iced::advanced::{Layout, layout, widget::Tree};
    use iced::{Font, Renderer, Size};

    let profile = Profile::new();
    let window = ConnectionWindow::open(profile.path()).unwrap();
    let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
    for width in [1240.0, 940.0] {
        let mut view = window.view(ViewChoices {
            journal_max_parts: None,
            serial_allowed: true,
            passwords: &[],
            passphrases: &[],
            keys: &[],
            ai_profiles: &[],
            ai_skills: &[],
            ai_agent_designs: &[],
            highlighting: HighlightChoices::default(),
            themes: &crate::terminal_theme::Themes::default(),
            incognito_allowed: true,
            terminal_effects: Some(&Effect::ALL),
            default_paste_warning: Default::default(),
            minimum_paste_warning: PasteWarningMode::Off,
        });
        let mut tree = Tree::new(view.as_widget());
        let layout = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(width, 760.0)),
        );
        let layout = Layout::new(&layout);
        let workbench = layout.children().nth(2).unwrap();
        let widths: Vec<_> = workbench
            .children()
            .map(|pane| pane.bounds().width)
            .collect();
        assert_eq!(widths, [210.0, 4.0, width - 588.0, 4.0, 370.0]);
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
            journal_max_parts: None,
            serial_allowed: true,
            passwords: &[],
            passphrases: &[],
            keys: &[],
            ai_profiles: &[],
            ai_skills: &[],
            ai_agent_designs: &[],
            highlighting: HighlightChoices::default(),
            themes: &crate::terminal_theme::Themes::default(),
            incognito_allowed: true,
            terminal_effects: Some(&Effect::ALL),
            default_paste_warning: Default::default(),
            minimum_paste_warning: PasteWarningMode::Off,
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
