use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use super::*;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let folder = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-manager-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&folder).unwrap();
        Self(folder)
    }

    fn seed(&self, name: &str) -> (PathBuf, PathBuf) {
        let root = self.0.join(name);
        let capture = session_journal::Capture::start(
            root.clone(),
            session_journal::Metadata {
                title: name.to_owned(),
                ..Default::default()
            },
        )
        .unwrap();
        let done = capture.finish();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done.is_closed() {
            assert!(Instant::now() < deadline, "journal fixture did not finish");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(capture.status(), session_journal::Status::Complete);
        (root, capture.directory().to_owned())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn manager(workspace: &Workspace, id: window::Id) -> &Manager {
    let Mode::Manager(manager) = &workspace.journals[&id].mode else {
        panic!("expected journal manager")
    };
    manager
}

fn set_root(workspace: &mut Workspace, root: &Path) {
    workspace.window_settings.session_journal.directory = root.to_str().unwrap().to_owned();
}

fn dispatch(workspace: &mut Workspace, id: window::Id, message: Message) {
    let _ = workspace.update(Desktop::SessionJournal(id, message));
}

fn open(workspace: &mut Workspace, owner: window::Id) -> window::Id {
    dispatch(workspace, owner, Message::Open);
    assert_eq!(workspace.journals.len(), 1);
    *workspace.journals.keys().next().unwrap()
}

fn deliver(workspace: &mut Workspace, id: window::Id, request: (u64, PathBuf)) {
    let (revision, root) = request;
    let catalog = session_journal::catalog(&root)
        .map(Arc::new)
        .map_err(|error| error.to_string());
    // Test double for Iced task delivery. The catalog is read from real journals.
    dispatch(
        workspace,
        id,
        Message::CatalogLoaded(revision, root, catalog),
    );
}

fn complete(workspace: &mut Workspace, id: window::Id) {
    let request = manager(workspace, id).reading.clone().unwrap();
    deliver(workspace, id, request);
}

#[test]
fn journal_vision_options_open_settings_for_the_visible_manager_owner() {
    let scratch = Scratch::new();
    for hosted in [false, true] {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        set_root(&mut workspace, &scratch.0);
        let (owner, _) = workspace.open_window(false);
        let manager = open(&mut workspace, owner);
        dispatch(&mut workspace, manager, Message::Options);
        let settings = workspace.settings.for_owner(owner).unwrap();
        let _ = workspace.close_settings(settings);

        if hosted {
            // Test-only terminal tab with no transport, to hide the manager.
            workspace.add_tab(owner, super::super::SessionId(100));
            dispatch(&mut workspace, manager, Message::Options);
            assert!(workspace.settings.for_owner(owner).is_none());
        }
    }
}

#[test]
fn journal_viewer_accepts_only_the_current_visible_link_with_permission() {
    let scratch = Scratch::new();
    let capture =
        session_journal::Capture::start(scratch.0.join("notes"), Default::default()).unwrap();
    for note in [
        "See https://example.test/first",
        "See https://example.test/second",
    ] {
        capture
            .sink()
            .try_write(
                session_journal::RecordKind::Note,
                zeroize::Zeroizing::new(note.to_owned()),
                false,
            )
            .unwrap();
    }
    let done = capture.finish();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done.is_closed() {
        assert!(Instant::now() < deadline, "journal fixture did not finish");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(capture.status(), session_journal::Status::Complete);
    let document = Arc::new(session_journal::read(capture.directory()).unwrap());
    let first = &document.timeline[0];
    let second = &document.timeline[1];
    let target = first
        .link_spans(LinkField::Text, MAX_DETAIL)
        .into_iter()
        .find_map(|part| part.target)
        .unwrap();

    for hosted in [false, true] {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        let (owner, _) = workspace.open_window(false);
        let _ =
            workspace.open_journal_viewer(owner, ViewerSource::capture(&capture), "Notes".into());
        let id = *workspace.journals.keys().next().unwrap();
        // Test double for Iced delivery. The document came from the real writer.
        dispatch(
            &mut workspace,
            id,
            Message::DocumentLoaded(Ok(Arc::clone(&document))),
        );
        dispatch(
            &mut workspace,
            id,
            Message::SelectEntry(0, first.id.clone()),
        );
        let visible = |workspace: &Workspace| {
            workspace.visible_journal_link(id, 0, &first.id, LinkField::Text, target)
        };
        assert!(visible(&workspace));
        assert!(!workspace.visible_journal_link(id, 0, &first.id, LinkField::Note, target));
        workspace.window_settings.session_journal.clickable_links = false;
        assert!(!visible(&workspace));
        workspace.window_settings.session_journal.clickable_links = true;
        dispatch(
            &mut workspace,
            id,
            Message::SelectEntry(1, second.id.clone()),
        );
        assert!(!visible(&workspace));
        dispatch(
            &mut workspace,
            id,
            Message::SelectEntry(0, first.id.clone()),
        );
        assert!(visible(&workspace));
        if hosted {
            // Test-only terminal tab with no transport, to hide the tool tab.
            workspace.add_tab(owner, super::super::SessionId(100));
        } else {
            dispatch(&mut workspace, id, Message::ShowCapture(true));
        }
        assert!(!visible(&workspace));
    }
}

#[test]
fn inherited_manager_resolves_global_root_on_reopen_focus_and_background_refresh() {
    let scratch = Scratch::new();
    let (first, _) = scratch.seed("first");
    let (second, journal) = scratch.seed("second");
    for hosted in [false, true] {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        set_root(&mut workspace, &first);
        let (owner, _) = workspace.open_window(false);
        let id = open(&mut workspace, owner);
        complete(&mut workspace, id);
        assert_eq!(
            manager(&workspace, id).results().next().unwrap().title,
            "first"
        );
        set_root(&mut workspace, &second);
        assert_eq!(open(&mut workspace, owner), id);
        assert_eq!(manager(&workspace, id).root.as_ref(), Some(&second));
        assert!(manager(&workspace, id).results().next().is_none());
        complete(&mut workspace, id);
        let revision = manager(&workspace, id).revision;
        dispatch(
            &mut workspace,
            id,
            Message::Select(revision, journal.clone()),
        );
        let focus = if hosted { owner } else { id };
        let _ = workspace.update(Desktop::Window(focus, window::Event::Focused));
        assert_eq!(manager(&workspace, id).revision, revision);
        assert!(manager(&workspace, id).selected().is_some());

        set_root(&mut workspace, &first);
        let _ = workspace.update(Desktop::Window(focus, window::Event::Focused));
        assert_eq!(manager(&workspace, id).root.as_ref(), Some(&first));
        assert!(manager(&workspace, id).selected().is_none());
        complete(&mut workspace, id);

        if hosted {
            let tab = workspace.windows[&owner].active_tab_id().unwrap();
            // Test-only terminal tab for real SelectTab dispatch, with no transport.
            workspace.add_tab(owner, super::super::SessionId(100));
            set_root(&mut workspace, &second);
            let _ = workspace.refresh_inherited_journal_managers();
            assert_eq!(manager(&workspace, id).root.as_ref(), Some(&second));
            let _ = workspace.update(Desktop::SelectTab(owner, tab));
            assert_eq!(manager(&workspace, id).root.as_ref(), Some(&second));
            complete(&mut workspace, id);
        }
    }
}

#[test]
fn manual_manager_folder_survives_global_changes_and_obeys_policy_override() {
    let scratch = Scratch::new();
    let (first, _) = scratch.seed("manual");
    let (second, _) = scratch.seed("global");
    let mut workspace = Workspace::default();
    assert_eq!(
        workspace.journal_directory().unwrap(),
        crate::profile::root().unwrap().join("journals")
    );
    set_root(&mut workspace, &first);
    let (owner, _) = workspace.open_window(false);
    let id = open(&mut workspace, owner);
    complete(&mut workspace, id);
    dispatch(
        &mut workspace,
        id,
        Message::Folder(first.to_str().unwrap().into()),
    );
    dispatch(&mut workspace, id, Message::Refresh);
    complete(&mut workspace, id);
    assert!(!manager(&workspace, id).inherited);
    set_root(&mut workspace, &second);
    let revision = manager(&workspace, id).revision;
    let _ = workspace.refresh_inherited_journal_managers();
    assert_eq!(manager(&workspace, id).revision, revision);
    assert_eq!(open(&mut workspace, owner), id);
    complete(&mut workspace, id);
    dispatch(&mut workspace, id, Message::Refresh);
    assert_eq!(manager(&workspace, id).root.as_ref(), Some(&first));
    complete(&mut workspace, id);

    let mut journal = toml_edit::Table::new();
    journal["storage-path"] = toml_edit::value(second.to_str().unwrap());
    let mut rule = toml_edit::Table::new();
    rule["session-journal"] = toml_edit::Item::Table(journal);
    workspace
        .clipboard
        .policy
        .terminal_logging
        .read_rule(&rule)
        .unwrap();
    assert_eq!(workspace.journal_directory().unwrap(), second);
    dispatch(&mut workspace, id, Message::Refresh);
    assert_eq!(manager(&workspace, id).locked_root.as_ref(), Some(&second));
    dispatch(
        &mut workspace,
        id,
        Message::Folder("ignored override".into()),
    );
    assert_eq!(manager(&workspace, id).folder, first.to_str().unwrap());
    complete(&mut workspace, id);
    assert_eq!(
        manager(&workspace, id).results().next().unwrap().title,
        "global"
    );

    workspace.clipboard.policy.session_journal_allowed = false;
    assert_eq!(
        workspace.journal_directory().unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    let revision = manager(&workspace, id).revision;
    dispatch(&mut workspace, id, Message::Refresh);
    assert_eq!(manager(&workspace, id).revision, revision);
}

#[test]
fn old_catalogs_and_selections_cannot_reappear_after_a_folder_change() {
    let scratch = Scratch::new();
    let (first, old_journal) = scratch.seed("old");
    let (middle, _) = scratch.seed("middle");
    let (last, new_journal) = scratch.seed("last");
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    set_root(&mut workspace, &first);
    let id = open(&mut workspace, owner);
    complete(&mut workspace, id);
    let revision = manager(&workspace, id).revision;
    dispatch(
        &mut workspace,
        id,
        Message::Select(revision, old_journal.clone()),
    );
    assert!(manager(&workspace, id).selected().is_some());
    set_root(&mut workspace, &middle);
    dispatch(&mut workspace, id, Message::OpenSelected);
    assert_eq!(workspace.journals.len(), 1);
    assert!(manager(&workspace, id).selected().is_none());
    let obsolete = manager(&workspace, id).reading.clone().unwrap();

    set_root(&mut workspace, &last);
    dispatch(&mut workspace, id, Message::Refresh);
    assert_eq!(manager(&workspace, id).reading.as_ref(), Some(&obsolete));
    assert_eq!(manager(&workspace, id).pending.as_ref(), Some(&last));
    deliver(&mut workspace, id, (obsolete.0, first));
    assert_eq!(manager(&workspace, id).reading.as_ref(), Some(&obsolete));
    deliver(&mut workspace, id, obsolete.clone());
    assert_eq!(manager(&workspace, id).reading.as_ref().unwrap().1, last);
    assert!(manager(&workspace, id).results().next().is_none());
    deliver(&mut workspace, id, obsolete);
    complete(&mut workspace, id);
    assert_eq!(
        manager(&workspace, id).results().next().unwrap().directory,
        new_journal
    );
    dispatch(&mut workspace, id, Message::Select(revision, old_journal));
    assert!(manager(&workspace, id).selected().is_none());
}
