use super::*;
use crate::connection_window::tree_context::{Message as Context, Page};
use iced_runtime::task;

fn serial(name: &str, device: &str) -> SerialProfile {
    SerialProfile::new(
        name,
        crate::session::SerialConnection {
            device: device.into(),
            ..Default::default()
        },
        kortty_terminal::TerminalProfile::Vt220,
    )
}

#[test]
fn mixed_serial_selection_batches_library_actions_without_partial_connection_launches() {
    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let ssh = store.save(connection("Alpha", "")).unwrap();
    let first = store.save_serial(serial("Bravo", "COM14")).unwrap();
    let last = store
        .save_serial(serial("Charlie", "/dev/ttyUSB0"))
        .unwrap();
    let mut manager = ConnectionWindow::open(profile.path()).unwrap();

    let _ = manager.update(Message::Select(ssh));
    manager.set_modifiers(Modifiers::SHIFT);
    let _ = manager.update(Message::SelectSerial(last));
    manager.set_modifiers(Modifiers::empty());
    let all = vec![ssh, first, last];
    assert_eq!(manager.selected_profile_ids(), all);
    assert_eq!(manager.export_connections_sources().unwrap().1, all);
    assert!(manager.can_export_connections());
    assert!(manager.selected().is_none());
    assert!(manager.selected_serial().is_none());
    assert!(manager.connect_selection().is_none());
    for message in [
        Message::EditSelected,
        Message::Duplicate,
        Message::ToggleSerialPin,
    ] {
        assert!(task::into_stream(manager.update(message)).is_none());
    }
    assert!(manager.draft.is_none() && manager.serial_draft.is_none());

    // Right-clicking a selected serial row keeps the complete mixed selection.
    let _ = manager.update(Message::OpenTreeContext(
        RowKey::Serial(first),
        Point::ORIGIN,
    ));
    let _ = manager.update(Message::TreeContext(Context::Activate(Page::Root, 6)));
    let _ = manager.update(Message::TreeContext(Context::Tag("Console".into())));
    complete_persisted(&mut manager, Message::TreeContext(Context::SaveTag));
    assert!(
        all.iter()
            .all(|id| manager.profile_tag(*id) == Some("Console"))
    );
    assert_eq!(manager.selected_profile_ids(), all);

    let _ = manager.update(Message::OpenTreeContext(
        RowKey::Serial(last),
        Point::ORIGIN,
    ));
    complete_persisted(
        &mut manager,
        Message::TreeContext(Context::Activate(Page::Root, 2)),
    );
    assert_eq!(manager.favorite_count(), 3);
    assert!(all.iter().all(|id| manager.store.is_favorite(*id)));

    manager.set_modifiers(Modifiers::COMMAND);
    let _ = manager.update(Message::Select(ssh));
    manager.set_modifiers(Modifiers::empty());
    assert_eq!(manager.selected_profile_ids(), [first, last]);
    assert_eq!(
        manager.export_connections_sources().unwrap().1,
        [first, last]
    );
    assert!(manager.connect_selection().is_none());
    complete_persisted(&mut manager, Message::ToggleFavorite);
    assert_eq!(manager.favorite_count(), 1);
    assert!(manager.store.is_favorite(ssh));
    assert!(!manager.store.is_favorite(first) && !manager.store.is_favorite(last));

    let _ = manager.update(Message::Delete);
    assert_eq!(manager.confirming_delete, [first, last]);
    let _ = manager.update(Message::ConfirmDelete(false));
    assert_eq!(manager.store.serial_profiles().len(), 2);
    let _ = manager.update(Message::Delete);
    complete_persisted(&mut manager, Message::ConfirmDelete(true));
    let persisted = Store::open(profile.path()).unwrap();
    assert!(persisted.serial_profiles().is_empty());
    assert_eq!(persisted.get(ssh).unwrap().tag, "Console");
    assert!(persisted.is_favorite(ssh));
}

#[test]
fn serial_library_tags_and_favorites_include_serial_only_folders_and_reconcile_filters() {
    let profile = Profile::new();
    let mut store = Store::open(profile.path()).unwrap();
    let group = store.create_group(None, "Hardware").unwrap();
    let mut saved = serial("Console", "COM22");
    saved.tag = "Bench".into();
    let id = store.save_serial_in_group(saved, Some(&group)).unwrap();
    store.toggle_connection_favorites(&[id], &[]).unwrap();
    let mut manager = ConnectionWindow::open(profile.path()).unwrap();
    assert_eq!(manager.favorite_count(), 1);
    assert_eq!(manager.library_tags().collect::<Vec<_>>(), ["Bench"]);
    assert_eq!(manager.folder_profile_count(&group), 1);

    let _ = manager.update(Message::Scope(Scope::Tag("bench".into())));
    assert_eq!(manager.visible_row_keys(), [RowKey::Serial(id)]);
    let _ = manager.update(Message::SelectSerial(id));
    assert_eq!(manager.selected_serial().unwrap().id, id);
    let _ = manager.update(Message::Search("missing".into()));
    assert!(manager.selected_serial().is_none());
    assert!(manager.export_connections_sources().is_none());
    let _ = manager.update(Message::Search(String::new()));
    let _ = manager.update(Message::SelectSerial(id));
    let _ = manager.update(Message::OpenTreeContext(RowKey::Serial(id), Point::ORIGIN));
    complete_persisted(
        &mut manager,
        Message::TreeContext(Context::Activate(Page::Root, 7)),
    );
    assert!(manager.visible_row_keys().is_empty());
    assert!(manager.library_tags().next().is_none());
    assert!(manager.selected_serial().is_none());
    let _ = manager.update(Message::Scope(Scope::Favorites));
    assert_eq!(manager.visible_row_keys(), [RowKey::Serial(id)]);
}
