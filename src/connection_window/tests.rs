use std::fs;

use crate::clipboard_policy::PasteWarningMode;
use crate::connection_store::{Auth, SavedLogin};
use crate::session_security::IsolationLevel;
use crate::ssh_dialog::LoginTarget;

use super::*;

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
    }
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
    store.set_favorite(web, true).unwrap();
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
            passwords: &[],
            passphrases: &[],
            keys: &[],
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
fn editor_retains_the_last_valid_speed_and_duplicates_the_saved_effect() {
    use crate::terminal_effects::Selection;

    let profile = Profile::new();
    let mut saved = connection("Console", "");
    let effect = Effect::AmberCrt90;
    saved.terminal_effect = Some(Selection { effect, speed: 7 });
    Store::open(profile.path()).unwrap().save(saved).unwrap();
    let mut window = ConnectionWindow::open(profile.path()).unwrap();
    let _ = window.update(Message::Duplicate);
    assert_eq!(window.editing_terminal_effect(), Some(effect));
    for value in ["", "99", "0", "100", "∅"] {
        let _ = window.update(Message::Edit(Edit::TerminalEffectSpeed(value.to_owned())));
    }
    let draft = window.draft.as_ref().unwrap().connection().unwrap();
    assert_eq!(draft.id, 0);
    assert_eq!(draft.terminal_effect, Some(Selection { effect, speed: 99 }));
    let _ = window.update(Message::Edit(Edit::TerminalEffect(None)));
    let draft = window.draft.as_ref().unwrap().connection().unwrap();
    assert_eq!(draft.terminal_effect, None);
    let _ = window.update(Message::CancelEdit);
    assert_eq!(
        window.selected().unwrap().terminal_effect,
        Some(Selection { effect, speed: 7 })
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
