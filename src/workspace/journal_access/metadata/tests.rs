use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use super::super::Manager;
use super::*;

struct Harness {
    workspace: Workspace,
    root: PathBuf,
    captures: Vec<Arc<Capture>>,
}

impl Harness {
    fn new(hosted: bool) -> Self {
        let scratch = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-metadata-ui-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&scratch).unwrap();
        let root = scratch.join("journals");
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        workspace.window_settings.session_journal.directory = root.to_str().unwrap().into();
        Self {
            workspace,
            root,
            captures: Vec::new(),
        }
    }

    fn seed(&self, title: &str, description: &str) -> Journal {
        let capture = Capture::start(
            self.root.clone(),
            session_journal::Metadata {
                title: title.into(),
                description: description.into(),
                ..Default::default()
            },
        )
        .unwrap();
        wait(&capture.finish());
        assert_eq!(capture.status(), Status::Complete);
        session_journal::read(capture.directory()).unwrap()
    }

    fn open_manager(&mut self, document: &Journal) -> (window::Id, window::Id) {
        let (owner, _) = self.workspace.open_window(false);
        dispatch(&mut self.workspace, owner, JournalMessage::Open);
        let source = self
            .workspace
            .journals
            .iter()
            .find_map(|(&id, surface)| {
                (self.workspace.journal_owner(id, surface) == Some(owner)
                    && matches!(surface.mode, Mode::Manager(_)))
                .then_some(id)
            })
            .unwrap();
        let (revision, root) = manager(&self.workspace, source).reading.clone().unwrap();
        let catalog = session_journal::catalog(&root).unwrap();
        // Test double for Iced delivery, with a catalog read from real journal files.
        dispatch(
            &mut self.workspace,
            source,
            JournalMessage::CatalogLoaded(revision, root, Ok(Arc::new(catalog))),
        );
        let _ = self.workspace.refresh_journal_live_aliases(source);
        self.select(source, document);
        (owner, source)
    }

    fn open_viewer(&mut self, document: &Journal) -> (window::Id, window::Id) {
        let (owner, source) = self.open_loading_viewer(document);
        let actual = session_journal::read(&document.summary.directory).unwrap();
        // Test double for Iced document delivery. The real reader parsed the saved file.
        dispatch(
            &mut self.workspace,
            source,
            JournalMessage::DocumentLoaded(Ok(Arc::new(actual))),
        );
        (owner, source)
    }

    fn open_loading_viewer(&mut self, document: &Journal) -> (window::Id, window::Id) {
        let (owner, _) = self.workspace.open_window(false);
        let _ = self.workspace.open_journal_viewer(
            owner,
            ViewerSource::catalog(&document.summary).unwrap(),
            document.summary.title.clone(),
        );
        let source = self
            .workspace
            .journals
            .iter()
            .find_map(|(&id, surface)| {
                (self.workspace.journal_owner(id, surface) == Some(owner)
                    && matches!(surface.mode, Mode::Viewer(_)))
                .then_some(id)
            })
            .unwrap();
        (owner, source)
    }

    fn select(&mut self, source: window::Id, document: &Journal) {
        let revision = manager(&self.workspace, source).revision;
        dispatch(
            &mut self.workspace,
            source,
            JournalMessage::Select(revision, document.summary.directory.clone()),
        );
        assert_eq!(
            manager(&self.workspace, source).description_target,
            Some(target(document))
        );
    }

    fn prompt(&mut self, source: window::Id, document: &Journal, viewer: bool) -> window::Id {
        edit(
            &mut self.workspace,
            source,
            if viewer {
                Message::Rename
            } else {
                Message::RenameTarget(target(document))
            },
        );
        self.workspace
            .journal_edits
            .dialogs
            .iter()
            .find_map(|(&id, dialog)| (dialog.source == source).then_some(id))
            .unwrap()
    }

    fn saved_result(&self, source: window::Id) -> (u64, SavedMetadata) {
        let (&ticket, pending) = self
            .workspace
            .journal_edits
            .pending
            .iter()
            .find(|(_, pending)| pending.source == source)
            .unwrap();
        let directory = pending.directory.clone();
        let change = pending.edit.clone();
        let candidates = pending.candidates.clone();
        let bound_source = pending.bound_source.clone();
        let completions = self.workspace.pending_shutdown.clone();
        assert!(!completions.is_empty(), "metadata worker was not tracked");
        for done in &completions {
            wait(done);
        }
        let actual = session_journal::read(&directory).unwrap();
        let source =
            bound_source.unwrap_or_else(|| ViewerSource::catalog(&actual.summary).unwrap());
        (
            ticket,
            SavedMetadata::new(actual, change, source, candidates).unwrap(),
        )
    }

    fn complete_save(&mut self, source: window::Id) -> Journal {
        let (ticket, saved) = self.saved_result(source);
        let actual = (*saved.document).clone();
        // Test double for Iced completion delivery, after the real worker ended.
        // The real saved file and production alias verifier provide the payload.
        edit(
            &mut self.workspace,
            source,
            Message::Saved(ticket, Ok(Arc::new(saved))),
        );
        actual
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        for capture in &self.captures {
            wait(&capture.finish());
        }
        for done in &self.workspace.pending_shutdown {
            let _ = done.recv_blocking();
        }
        let _ = fs::remove_dir_all(self.root.parent().unwrap());
    }
}

fn wait(done: &async_channel::Receiver<()>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done.is_closed() {
        assert!(Instant::now() < deadline, "journal worker did not finish");
        thread::sleep(Duration::from_millis(5));
    }
}

fn dispatch(workspace: &mut Workspace, source: window::Id, message: JournalMessage) {
    let _ = workspace.update(Desktop::SessionJournal(source, message));
}

fn edit(workspace: &mut Workspace, source: window::Id, message: Message) {
    dispatch(workspace, source, JournalMessage::Metadata(message));
}

fn manager(workspace: &Workspace, source: window::Id) -> &Manager {
    let Mode::Manager(manager) = &workspace.journals[&source].mode else {
        panic!("expected journal manager");
    };
    manager
}

fn target(document: &Journal) -> (PathBuf, String) {
    (
        document.summary.directory.clone(),
        document.summary.id.clone(),
    )
}

fn description(workspace: &mut Workspace, source: window::Id, document: &Journal, value: &str) {
    for action in [
        text_editor::Action::SelectAll,
        text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(value.into()))),
    ] {
        edit(
            workspace,
            source,
            Message::Description(target(document), action),
        );
    }
}

#[test]
fn journal_metadata_rename_cancel_preserves_native_and_hosted_journals() {
    for hosted in [false, true] {
        for viewer in [false, true] {
            let mut harness = Harness::new(hosted);
            let document = harness.seed("Original title", "Original description");
            let bytes = fs::read(document.summary.directory.join("journal.xml")).unwrap();
            let (owner, source) = if viewer {
                harness.open_viewer(&document)
            } else {
                harness.open_manager(&document)
            };
            let prompt = harness.prompt(source, &document, viewer);
            assert_eq!(
                harness.workspace.journal_title(prompt).as_deref(),
                Some(TITLE)
            );
            assert!(!harness.workspace.palette_can_open(owner));
            edit(
                &mut harness.workspace,
                prompt,
                Message::Title("Discarded title".into()),
            );
            let _ = harness.workspace.update(Desktop::Quit);
            assert!(!harness.workspace.exiting);
            assert!(!harness.workspace.scheduler_draining);
            assert_eq!(
                harness.workspace.journal_edits.dialogs[&prompt].title,
                "Discarded title"
            );
            assert!(!harness.workspace.palette_can_open(owner));
            edit(&mut harness.workspace, prompt, Message::Cancel);
            assert!(harness.workspace.journal_edits.dialogs.is_empty());
            assert!(harness.workspace.journal_edits.pending.is_empty());
            assert!(harness.workspace.palette_can_open(owner));
            assert_eq!(
                fs::read(document.summary.directory.join("journal.xml")).unwrap(),
                bytes
            );
        }
    }
}

#[test]
fn journal_metadata_rename_save_closes_prompt_and_releases_native_and_hosted_owners() {
    for hosted in [false, true] {
        for viewer in [false, true] {
            let mut harness = Harness::new(hosted);
            let document = harness.seed("Original title", "Preserved description");
            let (owner, source) = if viewer {
                harness.open_viewer(&document)
            } else {
                harness.open_manager(&document)
            };
            let prompt = harness.prompt(source, &document, viewer);
            edit(
                &mut harness.workspace,
                prompt,
                Message::Title("  Saved & renamed  ".into()),
            );
            edit(&mut harness.workspace, prompt, Message::SaveTitle);
            let saved = harness.complete_save(source);
            assert_eq!(saved.summary.title, "Saved & renamed");
            assert_eq!(saved.summary.description, "Preserved description");
            assert!(harness.workspace.journal_edits.dialogs.is_empty());
            assert!(harness.workspace.journal_edits.pending.is_empty());
            assert!(harness.workspace.journal_edit_for_owner(owner).is_none());
            assert!(harness.workspace.palette_can_open(owner));
            match &harness.workspace.journals[&source].mode {
                Mode::Viewer(viewer) => {
                    assert_eq!(
                        viewer.document.as_ref().unwrap().summary.title,
                        saved.summary.title
                    );
                }
                Mode::Manager(state) => {
                    let selected = state.selected().unwrap();
                    assert_eq!(selected.title, saved.summary.title);
                    assert!(selected.deletion_identity.is_none());
                    let revision = state.revision;
                    let root = state.root.clone().unwrap();
                    // Test doubles for the queued refresh and its reader completion.
                    edit(
                        &mut harness.workspace,
                        source,
                        Message::RefreshAfterEdit(revision, root.clone()),
                    );
                    let revision = manager(&harness.workspace, source).revision;
                    let catalog = session_journal::catalog(&root).unwrap();
                    dispatch(
                        &mut harness.workspace,
                        source,
                        JournalMessage::CatalogLoaded(
                            revision,
                            root.clone(),
                            Ok(Arc::new(catalog)),
                        ),
                    );
                    let selected = manager(&harness.workspace, source).selected().unwrap();
                    let opened =
                        Arc::new(crate::private_files::Directory::open(&root, false).unwrap());
                    assert!(
                        session_journal::deletion::Deletion::prepare(
                            opened,
                            &root,
                            &selected.directory,
                            &selected.id,
                            selected.deletion_identity.as_ref().unwrap(),
                        )
                        .is_ok()
                    );
                }
            }
        }
    }
}

#[test]
fn journal_metadata_description_refreshes_pristine_managers_and_preserves_unsaved_drafts() {
    use iced::futures::{StreamExt as _, executor::block_on};

    for hosted in [false, true] {
        for other_has_draft in [false, true] {
            let mut harness = Harness::new(hosted);
            let document = harness.seed("Shared journal", "Original description");
            let (_, first) = harness.open_manager(&document);
            let (_, second) = harness.open_manager(&document);
            if other_has_draft {
                description(
                    &mut harness.workspace,
                    second,
                    &document,
                    "Other unsaved draft",
                );
            }
            description(
                &mut harness.workspace,
                first,
                &document,
                "Saved description",
            );
            assert_eq!(
                manager(&harness.workspace, first).description.text(),
                "Saved description"
            );
            let save = harness.workspace.update(Desktop::SessionJournal(
                first,
                JournalMessage::Metadata(Message::SaveDescription(target(&document))),
            ));
            for done in &harness.workspace.pending_shutdown {
                wait(done);
            }
            let mut actions = iced_runtime::task::into_stream(save).unwrap();
            let (ticket, result) = loop {
                let action = block_on(actions.next()).expect("missing metadata worker result");
                if let iced_runtime::Action::Output(Desktop::SessionJournal(
                    id,
                    JournalMessage::Metadata(Message::Saved(ticket, result)),
                )) = action
                {
                    assert_eq!(id, first);
                    break (ticket, result);
                }
            };
            let saved = result.expect("description worker could not save the journal");
            assert_eq!(saved.document.summary.description, "Saved description");
            let persisted = session_journal::read(&document.summary.directory).unwrap();
            assert_eq!(persisted.summary.description, "Saved description");
            edit(
                &mut harness.workspace,
                first,
                Message::Saved(ticket, Ok(saved)),
            );
            assert_eq!(
                manager(&harness.workspace, first).description.text(),
                "Saved description"
            );
            assert_eq!(
                manager(&harness.workspace, first)
                    .description_status
                    .as_deref(),
                Some("Description saved")
            );
            assert_eq!(
                manager(&harness.workspace, second).description.text(),
                if other_has_draft {
                    "Other unsaved draft"
                } else {
                    "Saved description"
                }
            );
            assert_eq!(
                manager(&harness.workspace, second)
                    .selected()
                    .unwrap()
                    .description,
                "Saved description"
            );
        }
    }
}

#[test]
fn journal_metadata_stale_description_and_save_do_not_follow_a_new_selection() {
    for hosted in [false, true] {
        let mut harness = Harness::new(hosted);
        let first = harness.seed("First journal", "First description");
        let second = harness.seed("Second journal", "Second description");
        let (_, source) = harness.open_manager(&first);
        let first_bytes = fs::read(first.summary.directory.join("journal.xml")).unwrap();
        let second_bytes = fs::read(second.summary.directory.join("journal.xml")).unwrap();
        harness.select(source, &second);
        edit(
            &mut harness.workspace,
            source,
            Message::Description(
                target(&first),
                text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new("Stale text".into()))),
            ),
        );
        edit(
            &mut harness.workspace,
            source,
            Message::SaveDescription(target(&first)),
        );
        edit(
            &mut harness.workspace,
            source,
            Message::RenameTarget(target(&first)),
        );
        assert!(harness.workspace.journal_edits.pending.is_empty());
        assert!(harness.workspace.journal_edits.dialogs.is_empty());
        assert!(harness.workspace.pending_shutdown.is_empty());
        assert_eq!(
            manager(&harness.workspace, source).description.text(),
            "Second description"
        );
        assert_eq!(
            fs::read(first.summary.directory.join("journal.xml")).unwrap(),
            first_bytes
        );
        assert_eq!(
            fs::read(second.summary.directory.join("journal.xml")).unwrap(),
            second_bytes
        );
    }
}

#[test]
fn journal_metadata_rename_policy_rejects_a_queued_title_save() {
    for hosted in [false, true] {
        let mut harness = Harness::new(hosted);
        let document = harness.seed("Original title", "Original description");
        let (owner, source) = harness.open_manager(&document);
        let prompt = harness.prompt(source, &document, false);
        edit(
            &mut harness.workspace,
            prompt,
            Message::Title("Denied title".into()),
        );
        let deny = "[session-journal]\nallow-rename = false"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        harness
            .workspace
            .clipboard
            .policy
            .terminal_logging
            .read_rule(deny.as_table())
            .unwrap();
        edit(&mut harness.workspace, prompt, Message::SaveTitle);
        assert!(!harness.workspace.journal_rename_allowed());
        assert!(harness.workspace.journal_edits.pending.is_empty());
        assert!(harness.workspace.pending_shutdown.is_empty());
        assert!(!harness.workspace.journal_edits.dialogs[&prompt].saving);
        assert_eq!(
            session_journal::read(&document.summary.directory)
                .unwrap()
                .summary
                .title,
            "Original title"
        );
        edit(&mut harness.workspace, prompt, Message::Cancel);
        edit(
            &mut harness.workspace,
            source,
            Message::RenameTarget(target(&document)),
        );
        assert!(harness.workspace.journal_edits.dialogs.is_empty());
        assert!(harness.workspace.palette_can_open(owner));
    }
}

#[test]
fn journal_metadata_delayed_save_preserves_a_newer_viewer_timeline_and_description() {
    for metadata_first in [false, true] {
        let mut harness = Harness::new(false);
        let original = harness.seed("Original title", "Original description");
        let (_, source) = harness.open_manager(&original);
        let prompt = harness.prompt(source, &original, false);
        edit(
            &mut harness.workspace,
            prompt,
            Message::Title("Renamed title".into()),
        );
        edit(&mut harness.workspace, prompt, Message::SaveTitle);
        let (ticket, saved) = harness.saved_result(source);
        assert_eq!(saved.document.summary.title, "Renamed title");
        assert!(saved.document.timeline.is_empty());
        let saved = Arc::new(saved);

        let path = original.summary.directory.join("journal.xml");
        let previous = fs::read_to_string(&path).unwrap();
        // A real on-disk revision arrives after this metadata snapshot was produced.
        let revision = previous
            .replace("Original description", "Newer description")
            .replace(
                "</entries>",
                "<entry><id>later-note</id><kind>USER_NOTE</kind><text>Newer timeline entry</text></entry></entries>",
            );
        assert_ne!(revision, previous);
        fs::write(&path, revision).unwrap();
        let newer = session_journal::read(&original.summary.directory).unwrap();
        assert_eq!(newer.timeline.len(), 1);
        let (_, viewer_id) = harness.open_loading_viewer(&newer);
        if metadata_first {
            // Test double for Iced save completion while the normal reader remains pending.
            edit(
                &mut harness.workspace,
                source,
                Message::Saved(ticket, Ok(Arc::clone(&saved))),
            );
            let Mode::Viewer(viewer) = &harness.workspace.journals[&viewer_id].mode else {
                panic!("expected journal viewer");
            };
            assert!(viewer.document.is_none());
            assert!(viewer.loading);
        }
        // Test double for Iced delivery of the newer document parsed from the actual file.
        dispatch(
            &mut harness.workspace,
            viewer_id,
            JournalMessage::DocumentLoaded(Ok(Arc::new(newer.clone()))),
        );
        dispatch(
            &mut harness.workspace,
            viewer_id,
            JournalMessage::SelectEntry(0, newer.timeline[0].id.clone()),
        );
        if !metadata_first {
            // Test double for Iced save completion after the newer document was displayed.
            edit(
                &mut harness.workspace,
                source,
                Message::Saved(ticket, Ok(saved)),
            );
        }

        let Mode::Viewer(viewer) = &harness.workspace.journals[&viewer_id].mode else {
            panic!("expected journal viewer");
        };
        let document = viewer.document.as_ref().unwrap();
        assert_eq!(document.summary.title, "Renamed title");
        assert_eq!(document.summary.description, "Newer description");
        assert_eq!(document.timeline.len(), 1);
        assert_eq!(document.timeline[0].text, "Newer timeline entry");
        assert_eq!(viewer.selected_entry, Some(0));
        assert!(harness.workspace.journal_edits.dialogs.is_empty());
    }
}

#[test]
fn journal_metadata_bound_viewer_cannot_rename_replaced_directory_with_same_id() {
    let mut harness = Harness::new(false);
    let original = harness.seed("Original title", "Original description");
    let (_, viewer) = harness.open_viewer(&original);
    let prompt = harness.prompt(viewer, &original, true);
    edit(
        &mut harness.workspace,
        prompt,
        Message::Title("Stale rename".into()),
    );

    let directory = &original.summary.directory;
    let before = fs::read(directory.join("journal.xml")).unwrap();
    let moved = harness.root.join("moved");
    fs::rename(directory, &moved).unwrap();
    fs::create_dir(directory).unwrap();
    fs::write(directory.join("journal.xml"), &before).unwrap();
    let replacement = session_journal::read(directory).unwrap();
    assert_eq!(replacement.summary.id, original.summary.id);

    edit(&mut harness.workspace, prompt, Message::SaveTitle);
    assert!(
        harness
            .workspace
            .journal_edits
            .pending
            .values()
            .any(|pending| {
                pending.source == viewer
                    && pending.bound_source.as_ref().is_some_and(|source| {
                        !source.same_journal(&ViewerSource::catalog(&replacement.summary).unwrap())
                    })
            })
    );
    assert!(!harness.workspace.pending_shutdown.is_empty());
    for done in &harness.workspace.pending_shutdown {
        wait(done);
    }
    assert_eq!(fs::read(directory.join("journal.xml")).unwrap(), before);
    assert_eq!(fs::read(moved.join("journal.xml")).unwrap(), before);
    assert!(!directory.join(".kortty-journal.lock").exists());
}

#[test]
fn journal_metadata_delayed_save_cannot_replace_a_viewer_with_another_source() {
    for (loading, same_id) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut harness = Harness::new(true);
        let original = harness.seed("Replacement title", "Replacement description");
        let before = fs::read(original.summary.directory.join("journal.xml")).unwrap();
        let (_, source) = harness.open_manager(&original);
        let prompt = harness.prompt(source, &original, false);
        edit(
            &mut harness.workspace,
            prompt,
            Message::Title("Previous journal renamed".into()),
        );
        edit(&mut harness.workspace, prompt, Message::SaveTitle);
        let (ticket, saved) = harness.saved_result(source);
        if same_id {
            fs::rename(&original.summary.directory, harness.root.join("moved")).unwrap();
            fs::create_dir(&original.summary.directory).unwrap();
            fs::write(original.summary.directory.join("journal.xml"), before).unwrap();
        } else {
            let replacement = harness.seed("Replacement title", "Replacement description");
            fs::copy(
                replacement.summary.directory.join("journal.xml"),
                original.summary.directory.join("journal.xml"),
            )
            .unwrap();
        }
        let replacement = session_journal::read(&original.summary.directory).unwrap();
        assert_eq!(replacement.summary.id == original.summary.id, same_id);
        let (_, viewer_id) = if loading {
            harness.open_loading_viewer(&replacement)
        } else {
            harness.open_viewer(&replacement)
        };
        // Test double for an old Iced completion after a viewer requested another real document.
        edit(
            &mut harness.workspace,
            source,
            Message::Saved(ticket, Ok(Arc::new(saved))),
        );

        if loading {
            let Mode::Viewer(viewer) = &harness.workspace.journals[&viewer_id].mode else {
                panic!("expected journal viewer");
            };
            assert!(viewer.document.is_none());
            assert!(viewer.loading);
            // Test double for the pending Iced load, using the real replacement document.
            dispatch(
                &mut harness.workspace,
                viewer_id,
                JournalMessage::DocumentLoaded(Ok(Arc::new(replacement.clone()))),
            );
        }
        let Mode::Viewer(viewer) = &harness.workspace.journals[&viewer_id].mode else {
            panic!("expected journal viewer");
        };
        let document = viewer.document.as_ref().unwrap();
        assert_eq!(viewer.source.id(), replacement.summary.id);
        assert_eq!(document.summary.id, replacement.summary.id);
        assert_eq!(document.summary.title, "Replacement title");
        assert_eq!(document.summary.description, "Replacement description");
        assert_eq!(viewer.name, "Replacement title");
        assert!(!viewer.loading);
    }
}

#[test]
fn journal_metadata_loading_viewer_defers_both_fields_until_its_newer_reader_completes() {
    let mut harness = Harness::new(true);
    let original = harness.seed("Original title", "Original description");
    let (_, source) = harness.open_manager(&original);
    let (_, viewer_id) = harness.open_loading_viewer(&original);
    let prompt = harness.prompt(source, &original, false);
    edit(
        &mut harness.workspace,
        prompt,
        Message::Title("Saved title".into()),
    );
    edit(&mut harness.workspace, prompt, Message::SaveTitle);
    assert_eq!(harness.complete_save(source).summary.title, "Saved title");
    description(
        &mut harness.workspace,
        source,
        &original,
        "Saved description",
    );
    edit(
        &mut harness.workspace,
        source,
        Message::SaveDescription(target(&original)),
    );
    assert_eq!(
        harness.complete_save(source).summary.description,
        "Saved description"
    );
    let Mode::Viewer(viewer) = &harness.workspace.journals[&viewer_id].mode else {
        panic!("expected journal viewer");
    };
    assert!(viewer.loading);
    assert!(viewer.document.is_none());
    assert_eq!(
        viewer.deferred_title.as_ref(),
        Some(&(original.summary.id.clone(), "Saved title".into()))
    );
    assert_eq!(
        viewer.deferred_description.as_ref(),
        Some(&(original.summary.id.clone(), "Saved description".into()))
    );

    let path = original.summary.directory.join("journal.xml");
    let contents = fs::read_to_string(&path).unwrap();
    let revised = contents.replace(
        "</entries>",
        "<entry><id>newer-note</id><kind>USER_NOTE</kind><text>Newer reader timeline</text></entry></entries>",
    );
    assert_ne!(revised, contents);
    fs::write(&path, revised).unwrap();
    let actual = session_journal::read(&original.summary.directory).unwrap();
    assert_eq!(actual.timeline.len(), 1);
    // Test double for the normal Iced reader completion after both real metadata writes.
    dispatch(
        &mut harness.workspace,
        viewer_id,
        JournalMessage::DocumentLoaded(Ok(Arc::new(actual))),
    );
    let Mode::Viewer(viewer) = &harness.workspace.journals[&viewer_id].mode else {
        panic!("expected journal viewer");
    };
    let document = viewer.document.as_ref().unwrap();
    assert_eq!(document.summary.title, "Saved title");
    assert_eq!(document.summary.description, "Saved description");
    assert_eq!(document.timeline.len(), 1);
    assert_eq!(document.timeline[0].text, "Newer reader timeline");
    assert!(viewer.deferred_title.is_none());
    assert!(viewer.deferred_description.is_none());
    assert!(!viewer.loading);
}

#[test]
fn journal_metadata_propagates_through_verified_aliases_and_excludes_distinct_directories() {
    let mut harness = Harness::new(false);
    let original = harness.seed("Original title", "Original description");
    let (_, source) = harness.open_manager(&original);
    let alias_root = harness.root.parent().unwrap().join("JOURNALS");
    let alias_directory = alias_root.join(original.summary.directory.file_name().unwrap());
    assert_ne!(
        alias_directory.as_os_str(),
        original.summary.directory.as_os_str()
    );
    if !alias_directory.exists() {
        // On case-sensitive filesystems this is a distinct imported copy with the same UUID.
        fs::create_dir_all(&alias_directory).unwrap();
        fs::copy(
            original.summary.directory.join("journal.xml"),
            alias_directory.join("journal.xml"),
        )
        .unwrap();
    }
    let primary =
        crate::private_files::Directory::open(&original.summary.directory, false).unwrap();
    let alternative = crate::private_files::Directory::open(&alias_directory, false).unwrap();
    let same_directory = primary.same_as(&alternative).unwrap();
    let alias = session_journal::read(&alias_directory).unwrap();
    assert_eq!(alias.summary.id, original.summary.id);
    harness.workspace.window_settings.session_journal.directory =
        alias_root.to_str().unwrap().into();
    let (_, other_manager) = harness.open_manager(&alias);
    let (_, other_viewer) = harness.open_viewer(&alias);

    description(
        &mut harness.workspace,
        source,
        &original,
        "Saved description",
    );
    edit(
        &mut harness.workspace,
        source,
        Message::SaveDescription(target(&original)),
    );
    assert_eq!(
        harness.complete_save(source).summary.description,
        "Saved description"
    );
    let expected_description = if same_directory {
        "Saved description"
    } else {
        "Original description"
    };
    assert_eq!(
        manager(&harness.workspace, other_manager)
            .description
            .text(),
        expected_description
    );
    let prompt = harness.prompt(source, &original, false);
    edit(
        &mut harness.workspace,
        prompt,
        Message::Title("Renamed title".into()),
    );
    edit(&mut harness.workspace, prompt, Message::SaveTitle);
    assert_eq!(harness.complete_save(source).summary.title, "Renamed title");
    let expected_title = if same_directory {
        "Renamed title"
    } else {
        "Original title"
    };
    let selected = manager(&harness.workspace, other_manager)
        .selected()
        .unwrap();
    assert_eq!(selected.title, expected_title);
    assert_eq!(selected.description, expected_description);
    assert_eq!(selected.directory, alias_directory);
    let Mode::Viewer(viewer) = &harness.workspace.journals[&other_viewer].mode else {
        panic!("expected journal viewer");
    };
    assert_eq!(viewer.name, expected_title);
    let document = viewer.document.as_ref().unwrap();
    assert_eq!(document.summary.title, expected_title);
    assert_eq!(document.summary.description, expected_description);
    assert_eq!(document.summary.directory, alias_directory);
    let on_disk = session_journal::read(&alias_directory).unwrap();
    assert_eq!(on_disk.summary.title, expected_title);
    assert_eq!(on_disk.summary.description, expected_description);
}

#[test]
fn journal_metadata_live_probe_uses_directory_identity_and_rejects_stale_delivery() {
    let mut harness = Harness::new(false);
    let capture = Arc::new(Capture::start(harness.root.clone(), Default::default()).unwrap());
    harness.captures.push(Arc::clone(&capture));
    let document = session_journal::read(capture.directory()).unwrap();
    let copied_directory = harness.root.join("copied-journal");
    fs::create_dir(&copied_directory).unwrap();
    fs::copy(
        capture.directory().join("journal.xml"),
        copied_directory.join("journal.xml"),
    )
    .unwrap();
    let (_, source) = harness.open_manager(&document);
    let revision = manager(&harness.workspace, source).revision;
    let root = manager(&harness.workspace, source).root.clone().unwrap();
    let Mode::Manager(state) = &mut harness.workspace.journals.get_mut(&source).unwrap().mode
    else {
        panic!("expected journal manager");
    };
    // Test-only state of a manager awaiting its asynchronous recording-directory probe.
    state.live_aliases = None;
    edit(
        &mut harness.workspace,
        source,
        Message::RenameTarget(target(&document)),
    );
    assert!(harness.workspace.journal_edits.dialogs.is_empty());

    let aliases = Arc::new(
        resolve_live_aliases(
            vec![
                (capture.directory().to_owned(), capture.id().into()),
                (copied_directory, capture.id().into()),
            ],
            vec![Arc::clone(&capture)],
        )
        .unwrap(),
    );
    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases[0].directory, capture.directory());
    assert_eq!(aliases[0].id, capture.id());
    for (stale_revision, stale_root) in [
        (revision.wrapping_sub(1), root.clone()),
        (revision, root.join("another-root")),
    ] {
        // Test double for a stale Iced completion, using the actual filesystem probe result.
        edit(
            &mut harness.workspace,
            source,
            Message::LiveAliasesLoaded(stale_revision, stale_root, Ok(Arc::clone(&aliases))),
        );
        assert!(manager(&harness.workspace, source).live_aliases.is_none());
    }
    // Test double for Iced delivery of the production probe's result at its current revision.
    edit(
        &mut harness.workspace,
        source,
        Message::LiveAliasesLoaded(revision, root, Ok(Arc::clone(&aliases))),
    );
    let accepted = manager(&harness.workspace, source)
        .live_aliases
        .as_ref()
        .unwrap();
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0].directory, aliases[0].directory);
    assert_eq!(accepted[0].capture_directory, capture.directory());
}
