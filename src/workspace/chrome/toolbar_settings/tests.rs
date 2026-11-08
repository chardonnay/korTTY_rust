use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let directory = std::env::temp_dir().join(format!(
                "kortty-toolbar-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match fs::create_dir(&directory) {
                Ok(()) => return Ok(Self(directory)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn portable_defaults_keep_the_reference_order_and_two_visible_rows() {
    let layout = Layout::default();
    assert_eq!(layout.icon_size(), 24);
    assert!(layout.actions_visible());
    assert!(layout.visible(Group::Connections));
    assert!(layout.items(Group::Connections).is_empty());
    assert_eq!(
        &layout.items(Group::Terminal)[..2],
        [
            Item::Action(Action::QuickConnect),
            Item::Action(Action::LocalShell)
        ]
    );
    assert_eq!(
        &layout.items(Group::Tools)[..4],
        [
            Item::Action(Action::ManageConnections),
            Item::Action(Action::Sftp),
            Item::Action(Action::Snippets),
            Item::Action(Action::SessionJournals),
        ]
    );
    assert_eq!(
        layout.items(Group::Tools).last(),
        Some(&Item::Action(Action::Recordings))
    );
    assert_eq!(
        layout.items(Group::Settings).first(),
        Some(&Item::Action(Action::FontSettings))
    );
}

#[test]
fn old_windows_shell_slots_are_upgraded_in_place_on_windows() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let path = scratch.0.join("toolbar.json");
    let mut layout = Layout::default();
    layout.tools.items = vec![
        Item::Reference(Reference::CommandPrompt),
        Item::Separator,
        Item::Reference(Reference::PowerShell),
    ];
    save_to(&path, layout.clone())?;

    let loaded = load_from(&path)?;
    if cfg!(windows) {
        assert_eq!(
            &loaded.items(Group::Terminal)[2..],
            [
                Item::Action(Action::PowerShell),
                Item::Action(Action::CommandPrompt)
            ]
        );
        assert_eq!(
            loaded.items(Group::Tools),
            [
                Item::Action(Action::CommandPrompt),
                Item::Separator,
                Item::Action(Action::PowerShell)
            ]
        );
    } else {
        assert_eq!(loaded, layout);
    }
    Ok(())
}

#[test]
fn saved_layout_round_trips_and_invalid_data_cannot_replace_it() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let path = scratch.0.join("toolbar.json");
    assert_eq!(
        load_from(&path).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );

    let mut layout = Layout {
        icon_size: 41,
        ..Layout::default()
    };
    layout.set_row(Row::Actions, false);
    layout.set_row(Row::Group(Group::Tools), false);
    assert!(
        layout
            .add(Group::Settings, Item::Action(Action::AppearanceSettings))
            .is_some()
    );
    assert!(
        layout
            .add(Group::Connections, Item::Connection(42))
            .is_some()
    );
    assert!(
        layout
            .add(Group::Connections, Item::Folder("team/test".into()))
            .is_some()
    );
    save_to(&path, layout.clone())?;
    assert_eq!(load_from(&path)?, layout);

    let mut invalid = layout.clone();
    invalid.icon_size = 0;
    assert_eq!(
        save_to(&path, invalid).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(load_from(&path)?, layout);
    fs::write(&path, vec![b' '; MAX_PROFILE_BYTES as usize + 1])?;
    assert_eq!(
        load_from(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    Ok(())
}

#[test]
fn promotes_available_tools_in_place_in_older_toolbar_layouts() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let path = scratch.0.join("toolbar.json");
    let mut layout = Layout::default();
    layout.tools.items = vec![
        Item::Separator,
        Item::Reference(Reference::Logfile),
        Item::Reference(Reference::Journal),
        Item::Reference(Reference::Ai),
    ];
    layout.settings.items = vec![
        Item::Reference(Reference::Colors),
        Item::Reference(Reference::Keyboard),
        Item::Reference(Reference::Logging),
        Item::Action(Action::FontSettings),
    ];
    save_to(&path, layout)?;
    let upgraded = load_from(&path)?;
    assert_eq!(
        upgraded.tools.items,
        [
            Item::Separator,
            Item::Action(Action::LogfileManager),
            Item::Action(Action::SessionJournals),
            Item::Reference(Reference::Ai),
        ]
    );
    assert_eq!(
        upgraded.settings.items,
        [
            Item::Action(Action::ColorSettings),
            Item::Action(Action::KeyboardSettings),
            Item::Action(Action::LoggingSettings),
            Item::Action(Action::FontSettings),
        ]
    );
    Ok(())
}

#[test]
fn settings_icon_size_save_preserves_toolbar_drafts_and_committed_items() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let path = scratch.0.join("toolbar.json");
    let mut toolbar = Toolbar::default();
    let customizer = window::Id::unique();
    let owner = window::Id::unique();
    let settings = window::Id::unique();
    let _ = toolbar.update(Message::Open(customizer, Catalog::Actions));
    let _ = toolbar.update(Message::Edit(
        customizer,
        Edit::Add(Group::Tools, Item::Separator),
    ));
    let _ = toolbar.save_icon_size(owner, settings, 40);
    let _ = toolbar.update(Message::Finish(customizer));
    let pending = toolbar.pending.as_ref().unwrap();
    assert_eq!(pending.layout.icon_size(), 40);
    assert!(
        !pending
            .layout
            .items(Group::Tools)
            .contains(&Item::Separator)
    );
    let first = pending.layout.clone();
    let _ = toolbar.update(Message::Saved(
        save_to(&path, first).map_err(|error| error.to_string()),
    ));
    assert_eq!(toolbar.layout(customizer).icon_size(), 40);
    assert!(
        toolbar
            .layout(customizer)
            .items(Group::Tools)
            .contains(&Item::Separator)
    );
    assert_eq!(load_from(&path)?.icon_size(), 40);

    let _ = toolbar.update(Message::Edit(customizer, Edit::IconSize(56)));
    let _ = toolbar.save_icon_size(owner, settings, 48);
    let second = toolbar.pending.as_ref().unwrap().layout.clone();
    let _ = toolbar.update(Message::Saved(
        save_to(&path, second).map_err(|error| error.to_string()),
    ));
    assert_eq!(toolbar.committed_icon_size(), 48);
    assert_eq!(toolbar.layout(customizer).icon_size(), 56);
    let _ = toolbar.update(Message::Finish(customizer));
    let final_layout = toolbar.pending.as_ref().unwrap().layout.clone();
    let _ = toolbar.update(Message::Saved(
        save_to(&path, final_layout).map_err(|error| error.to_string()),
    ));
    let saved = load_from(&path)?;
    assert_eq!(saved.icon_size(), 56);
    assert!(saved.items(Group::Tools).contains(&Item::Separator));
    Ok(())
}

#[test]
fn renamed_folder_pins_survive_an_in_flight_toolbar_save() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let path = scratch.0.join("toolbar.json");
    let mut toolbar = Toolbar::default();
    let owner = window::Id::unique();
    let other = window::Id::unique();
    let pins = |folder: &str| {
        [
            Item::Folder(folder.into()),
            Item::Folder(format!("{folder}/Nested")),
            Item::Folder("Teamwork".into()),
            Item::Connection(42),
        ]
    };
    toolbar.layout.connections.items = pins("Team").to_vec();
    let _ = toolbar.update(Message::Open(owner, Catalog::Connections));
    let _ = toolbar.update(Message::Edit(owner, Edit::IconSize(33)));
    let _ = toolbar.update(Message::Edit(
        owner,
        Edit::PickCatalog(Group::Connections, Item::Folder("Team/Nested".into())),
    ));
    let _ = toolbar.update(Message::Finish(owner));
    let first_write = toolbar.pending.as_ref().unwrap().layout.clone();

    let rename = FolderRename {
        old: "Team".into(),
        new: "Servers".into(),
    };
    let _ = toolbar.rename_folder(other, &rename);
    assert_eq!(
        toolbar.layout(other).items(Group::Connections),
        pins("Servers")
    );
    let draft = toolbar.draft(owner).unwrap();
    assert_eq!(draft.layout.items(Group::Connections), pins("Servers"));
    assert!(matches!(
        draft.drag.as_ref(),
        Some(Drag::Catalog(Group::Connections, Item::Folder(path))) if path == "Servers/Nested"
    ));
    assert!(toolbar.pending.as_ref().unwrap().needs_rewrite);

    // Deliver the real writer's result in order while using the test-only scratch destination.
    let _ = toolbar.update(Message::Saved(
        save_to(&path, first_write).map_err(|error| error.to_string()),
    ));
    assert!(toolbar.draft(owner).is_none());
    assert_eq!(load_from(&path)?.items(Group::Connections), pins("Team"));
    let second_write = toolbar.pending.as_ref().unwrap().layout.clone();
    let _ = toolbar.update(Message::Saved(
        save_to(&path, second_write).map_err(|error| error.to_string()),
    ));
    let saved = load_from(&path)?;
    assert_eq!(saved.items(Group::Connections), pins("Servers"));
    assert_eq!(saved.icon_size(), 33);
    assert!(!toolbar.pending());
    assert!(!toolbar.unsaved_folder_paths);
    Ok(())
}

#[test]
fn failed_folder_pin_save_keeps_working_pins_and_retries_when_finishing() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let path = scratch.0.join("toolbar.json");
    let mut toolbar = Toolbar::default();
    let owner = window::Id::unique();
    toolbar.layout.connections.items = vec![Item::Folder("Team".into())];
    save_to(&path, toolbar.layout.clone())?;

    let _ = toolbar.rename_folder(
        owner,
        &FolderRename {
            old: "Team".into(),
            new: "Servers".into(),
        },
    );
    let write = toolbar.pending.as_ref().unwrap().layout.clone();
    // Renaming a file over the scratch directory gives the real writer an I/O error.
    let blocked = scratch.0.join("blocked");
    fs::create_dir(&blocked)?;
    let error = save_to(&blocked, write).unwrap_err();
    let _ = toolbar.update(Message::Saved(Err(error.to_string())));
    assert!(!toolbar.pending());
    assert!(toolbar.unsaved_folder_paths);
    assert!(toolbar.problem().unwrap().contains("Folder renamed"));
    assert_eq!(
        toolbar.layout(owner).items(Group::Connections),
        [Item::Folder("Servers".into())]
    );
    assert_eq!(
        load_from(&path)?.items(Group::Connections),
        [Item::Folder("Team".into())]
    );

    let _ = toolbar.update(Message::Open(owner, Catalog::Connections));
    let _ = toolbar.update(Message::Finish(owner));
    let retry = toolbar.pending.as_ref().unwrap().layout.clone();
    let _ = toolbar.update(Message::Saved(
        save_to(&path, retry).map_err(|error| error.to_string()),
    ));
    assert_eq!(load_from(&path)?, toolbar.layout);
    assert!(toolbar.problem().is_none());
    assert!(!toolbar.unsaved_folder_paths);
    Ok(())
}

#[test]
fn drafts_are_window_local_and_reset_only_restores_items() {
    let mut toolbar = Toolbar::default();
    let owner = window::Id::unique();
    let other = window::Id::unique();
    let _ = toolbar.update(Message::Open(owner, Catalog::Actions));
    assert!(!toolbar.can_edit(other));
    let _ = toolbar.update(Message::Edit(owner, Edit::IconSize(999)));
    let _ = toolbar.update(Message::Visible(owner, Row::Group(Group::Terminal), false));
    let _ = toolbar.update(Message::Edit(
        owner,
        Edit::Add(Group::Tools, Item::Separator),
    ));
    assert_eq!(toolbar.layout(owner).icon_size(), MAX_ICON);
    assert_eq!(toolbar.layout(other).icon_size(), DEFAULT_ICON);
    assert!(toolbar.layout(other).visible(Group::Terminal));

    let _ = toolbar.update(Message::Edit(owner, Edit::Reset));
    assert_eq!(toolbar.layout(owner).icon_size(), MAX_ICON);
    assert!(!toolbar.layout(owner).visible(Group::Terminal));
    assert_eq!(
        toolbar.layout(owner).items(Group::Tools),
        Layout::default().items(Group::Tools)
    );

    let _ = toolbar.update(Message::Cancel(owner));
    assert_eq!(toolbar.layout(owner), &Layout::default());
    assert!(toolbar.can_edit(other));
}

#[test]
fn drag_places_existing_and_catalog_items_and_cancels_stale_sources() {
    let owner = window::Id::unique();
    let mut draft = Draft {
        owner,
        layout: Layout::default(),
        catalog: Catalog::Actions,
        search: String::new(),
        search_input: iced::widget::Id::unique(),
        catalog_scroll: iced::widget::Id::unique(),
        selected: None,
        item_context: Context::default(),
        drag: None,
    };
    let source = Location {
        group: Group::Terminal,
        index: 0,
    };
    draft.edit(Edit::Pick(source));
    assert!(draft.can_drop(Group::Tools));
    assert!(!draft.can_drop(Group::Connections));
    draft.edit(Edit::Drop(Group::Tools, Some(1)));
    assert_eq!(
        draft.layout.items(Group::Tools)[1],
        Item::Action(Action::QuickConnect)
    );

    let catalog = Item::Connection(42);
    draft.edit(Edit::PickCatalog(Group::Connections, catalog.clone()));
    draft.edit(Edit::Drop(Group::Tools, None));
    assert!(draft.layout.items(Group::Connections).is_empty());
    draft.edit(Edit::PickCatalog(Group::Connections, catalog.clone()));
    draft.edit(Edit::CancelDrag);
    draft.edit(Edit::Drop(Group::Connections, None));
    assert!(draft.layout.items(Group::Connections).is_empty());

    draft.edit(Edit::PickCatalog(Group::Connections, catalog.clone()));
    draft.edit(Edit::ReleaseCatalog(Group::Connections, catalog));
    draft.edit(Edit::PickCatalog(Group::Connections, Item::Connection(13)));
    draft.edit(Edit::Drop(Group::Connections, Some(0)));
    assert_eq!(
        draft.layout.items(Group::Connections),
        [Item::Connection(13), Item::Connection(42)]
    );
}
