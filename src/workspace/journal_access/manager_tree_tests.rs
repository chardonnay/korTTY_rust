use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use iced::keyboard::{Modifiers, key::Named};

use super::manager_tree::Event;
use super::*;

struct Fixture {
    workspace: Workspace,
    scratch: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new(hosted: bool) -> Self {
        let scratch = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-tree-ui-{}",
            crate::private_files::nonce().unwrap()
        ));
        let root = scratch.join("journals");
        fs::create_dir_all(&root).unwrap();
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        workspace.window_settings.session_journal.directory = root.to_str().unwrap().into();
        let fixture = Self {
            workspace,
            scratch,
            root,
        };
        // Imported on-disk fixtures use the pinned Java JAXB field names.
        fixture.journal("interactive", "Interactive target", "desktop", "13", "");
        for (name, host, status) in [("alpha", "alpha", "SUCCESS"), ("beta", "beta", "BLOCKED")] {
            fixture.journal(name, name, host, "10", &format!(
                "<sourceKind>JOB</sourceKind><sourceId>job-one</sourceId><sourceName>Nightly checks</sourceName><runId>run-one</runId><runStatus>{status}</runStatus><runStartedAt>2026-11-06T10:00:00Z</runStartedAt><logEntryCount>25</logEntryCount>"
            ));
        }
        fixture.journal("solo", "Solo target", "gamma", "11",
            "<sourceKind>JOB</sourceKind><sourceId>job-one</sourceId><sourceName>Nightly checks</sourceName><runId>run-two</runId><runStatus>CANCELLED</runStatus><logEntryCount>12</logEntryCount><aiKeywords><keyword>needle-key</keyword></aiKeywords>");
        fixture.journal("swarm", "Swarm target", "delta", "09",
            "<sourceKind>SWARM</sourceKind><sourceId>swarm-one</sourceId><sourceName>Research swarm</sourceName><runId>swarm-run</runId><runStatus>FAILED</runStatus><pinned>true</pinned><logEntryCount>8</logEntryCount>");
        fixture
    }

    fn journal(&self, name: &str, title: &str, host: &str, hour: &str, extra: &str) {
        let directory = self.path(name);
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("journal.xml"), format!(
            r#"<session-journal formatVersion="1" id="tree-{name}"><meta><title>{title}</title><description>Description {name}</description><connectionName>Connection {name}</connectionName><host>{host}</host><username>operator</username><startedAt>2026-11-06T{hour}:00:00Z</startedAt><endedAt>2026-11-06T15:00:00Z</endedAt>{extra}</meta><entries/></session-journal>"#
        )).unwrap();
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(format!("journal-{name}"))
    }

    fn open(&mut self) -> (window::Id, window::Id) {
        let (owner, _) = self.workspace.open_window(false);
        dispatch(&mut self.workspace, owner, Message::Open);
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
        self.complete(source);
        (owner, source)
    }

    fn complete(&mut self, source: window::Id) {
        let (revision, root) = manager(&self.workspace, source).reading.clone().unwrap();
        let catalog = session_journal::catalog(&root).unwrap();
        // Test double for Iced catalog delivery, read from the real imported journals.
        dispatch(
            &mut self.workspace,
            source,
            Message::CatalogLoaded(revision, root, Ok(Arc::new(catalog))),
        );
        let _ = self.workspace.refresh_journal_live_aliases(source);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for done in &self.workspace.pending_shutdown {
            let _ = done.recv_blocking();
        }
        let _ = fs::remove_dir_all(&self.scratch);
    }
}

fn manager(workspace: &Workspace, source: window::Id) -> &Manager {
    let Mode::Manager(manager) = &workspace.journals[&source].mode else {
        panic!("expected journal manager");
    };
    manager
}

fn dispatch(workspace: &mut Workspace, source: window::Id, message: Message) {
    let _ = workspace.update(Desktop::SessionJournal(source, message));
}

fn generation(workspace: &Workspace, source: window::Id) -> u64 {
    manager(workspace, source).tree.generation()
}

fn key(workspace: &mut Workspace, source: window::Id, key: Named, modifiers: Modifiers) {
    // Test double for an event emitted by the table's focused Iced widget.
    let event = Event::Key(generation(workspace, source), key, modifiers, 320.0);
    dispatch(workspace, source, Message::Tree(event));
}

fn modifiers(workspace: &mut Workspace, source: window::Id, modifiers: Modifiers) {
    let physical = workspace.hosted_tool_owner(source).unwrap_or(source);
    let _ = workspace.update(Desktop::RemoteDock(
        physical,
        super::super::remote_dock::Message::Modifiers(modifiers),
    ));
}

fn press(workspace: &mut Workspace, source: window::Id, node: tree::NodeKey, held: Modifiers) {
    modifiers(workspace, source, held);
    // Test double for the Iced row event, using the current model generation.
    let event = Event::Press(generation(workspace, source), node);
    dispatch(workspace, source, Message::Tree(event));
}

fn toggle(workspace: &mut Workspace, source: window::Id, node: tree::NodeKey) {
    let event = Event::Toggle(generation(workspace, source), node);
    dispatch(workspace, source, Message::Tree(event));
}

fn jobs() -> tree::NodeKey {
    tree::NodeKey::Group(tree::GroupKey {
        kind: tree::SourceKind::Job,
        source_id: Some("job-one".into()),
    })
}

fn run_one() -> tree::NodeKey {
    let tree::NodeKey::Group(group) = jobs() else {
        unreachable!()
    };
    tree::NodeKey::Run(group, tree::RunKey::Id("run-one".into()))
}

#[test]
fn journal_manager_tree_defaults_collapsed_and_retains_keys_after_refresh_and_reorder() {
    for hosted in [false, true] {
        let mut fixture = Fixture::new(hosted);
        let alpha = fixture.path("alpha");
        let (_, source) = fixture.open();
        press(
            &mut fixture.workspace,
            source,
            tree::NodeKey::Journal(alpha.clone()),
            Modifiers::NONE,
        );
        assert!(manager(&fixture.workspace, source).selected().is_none());
        toggle(&mut fixture.workspace, source, jobs());
        press(
            &mut fixture.workspace,
            source,
            tree::NodeKey::Journal(alpha.clone()),
            Modifiers::NONE,
        );
        assert!(manager(&fixture.workspace, source).selected().is_none());
        toggle(&mut fixture.workspace, source, run_one());
        press(
            &mut fixture.workspace,
            source,
            tree::NodeKey::Journal(alpha.clone()),
            Modifiers::NONE,
        );
        assert_eq!(
            manager(&fixture.workspace, source)
                .selected()
                .unwrap()
                .directory,
            alpha
        );
        let stale = Event::Press(
            generation(&fixture.workspace, source),
            tree::NodeKey::Journal(fixture.path("interactive")),
        );
        let path = alpha.join("journal.xml");
        let contents = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            contents.replace("2026-11-06T10:00:00Z", "2026-11-06T14:00:00Z"),
        )
        .unwrap();
        dispatch(&mut fixture.workspace, source, Message::Refresh);
        assert!(manager(&fixture.workspace, source).selected().is_none());
        fixture.complete(source);
        assert_eq!(
            manager(&fixture.workspace, source)
                .selected()
                .unwrap()
                .directory,
            alpha
        );
        dispatch(&mut fixture.workspace, source, Message::Tree(stale));
        assert_eq!(
            manager(&fixture.workspace, source)
                .selected()
                .unwrap()
                .directory,
            alpha
        );
    }
}

#[test]
fn journal_manager_tree_range_and_group_selection_use_deduplicated_exact_one_actions() {
    for hosted in [false, true] {
        let mut fixture = Fixture::new(hosted);
        let alpha = fixture.path("alpha");
        let beta = fixture.path("beta");
        let solo = fixture.path("solo");
        let (owner, source) = fixture.open();
        toggle(&mut fixture.workspace, source, jobs());
        toggle(&mut fixture.workspace, source, run_one());
        press(
            &mut fixture.workspace,
            source,
            tree::NodeKey::Journal(alpha.clone()),
            Modifiers::NONE,
        );
        press(
            &mut fixture.workspace,
            source,
            tree::NodeKey::Journal(beta.clone()),
            Modifiers::SHIFT,
        );
        let current = manager(&fixture.workspace, source);
        let catalog = current.catalog.as_ref().unwrap();
        let selected: Vec<_> = current
            .tree
            .selected_indices(catalog)
            .into_iter()
            .map(|index| catalog.journals[index].directory.clone())
            .collect();
        assert_eq!(selected, [alpha.clone(), beta]);
        press(
            &mut fixture.workspace,
            source,
            run_one(),
            Modifiers::COMMAND,
        );
        press(&mut fixture.workspace, source, jobs(), Modifiers::COMMAND);
        let current = manager(&fixture.workspace, source);
        assert_eq!(
            current
                .tree
                .selected_indices(current.catalog.as_ref().unwrap())
                .len(),
            3
        );
        assert!(current.selected().is_none());
        dispatch(&mut fixture.workspace, source, Message::OpenSelected);
        dispatch(
            &mut fixture.workspace,
            source,
            Message::Metadata(metadata::Message::Rename),
        );
        dispatch(
            &mut fixture.workspace,
            source,
            Message::Metadata(metadata::Message::SaveDescription((
                alpha,
                "tree-alpha".into(),
            ))),
        );
        assert_eq!(fixture.workspace.journals.len(), 1);
        assert!(fixture.workspace.journal_edit_for_owner(owner).is_none());
        assert!(fixture.workspace.pending_shutdown.is_empty());

        dispatch(
            &mut fixture.workspace,
            source,
            Message::Search("needle-key".into()),
        );
        press(&mut fixture.workspace, source, jobs(), Modifiers::NONE);
        assert_eq!(
            manager(&fixture.workspace, source)
                .selected()
                .unwrap()
                .directory,
            solo
        );
        dispatch(
            &mut fixture.workspace,
            source,
            Message::Metadata(metadata::Message::Rename),
        );
        let prompt = fixture.workspace.journal_edit_for_owner(owner).unwrap();
        dispatch(
            &mut fixture.workspace,
            prompt,
            Message::Metadata(metadata::Message::Cancel),
        );
        let target = (solo.clone(), "tree-solo".into());
        for action in [
            text_editor::Action::SelectAll,
            text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(
                "Filtered description".into(),
            ))),
        ] {
            dispatch(
                &mut fixture.workspace,
                source,
                Message::Metadata(metadata::Message::Description(target.clone(), action)),
            );
        }
        dispatch(
            &mut fixture.workspace,
            source,
            Message::Metadata(metadata::Message::SaveDescription(target)),
        );
        assert!(!fixture.workspace.pending_shutdown.is_empty());
        let deadline = Instant::now() + Duration::from_secs(10);
        while fixture
            .workspace
            .pending_shutdown
            .iter()
            .any(|done| !done.is_closed())
        {
            assert!(
                Instant::now() < deadline,
                "description worker did not finish"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            session_journal::read(&solo).unwrap().summary.description,
            "Filtered description"
        );
        assert_eq!(
            session_journal::read(&fixture.path("alpha"))
                .unwrap()
                .summary
                .description,
            "Description alpha"
        );
    }
}

#[test]
fn journal_manager_tree_keyboard_activation_preserves_selection_and_opens_only_after_double_click()
{
    for hosted in [false, true] {
        let mut fixture = Fixture::new(hosted);
        let interactive = fixture.path("interactive");
        let (_, source) = fixture.open();
        press(
            &mut fixture.workspace,
            source,
            tree::NodeKey::Journal(interactive.clone()),
            Modifiers::NONE,
        );
        key(
            &mut fixture.workspace,
            source,
            Named::ArrowDown,
            Modifiers::COMMAND,
        );
        for activation in [Named::Enter, Named::Space, Named::F2] {
            key(&mut fixture.workspace, source, activation, Modifiers::NONE);
        }
        let current = manager(&fixture.workspace, source);
        assert_eq!(
            current
                .tree
                .selected_indices(current.catalog.as_ref().unwrap())
                .len(),
            4
        );
        assert_eq!(fixture.workspace.journals.len(), 1);
        press(
            &mut fixture.workspace,
            source,
            tree::NodeKey::Journal(interactive.clone()),
            Modifiers::NONE,
        );
        press(
            &mut fixture.workspace,
            source,
            tree::NodeKey::Journal(interactive.clone()),
            Modifiers::NONE,
        );
        let event = Event::DoubleClick(
            generation(&fixture.workspace, source),
            tree::NodeKey::Journal(interactive.clone()),
        );
        dispatch(&mut fixture.workspace, source, Message::Tree(event));
        assert_eq!(fixture.workspace.journals.len(), 2);
        assert!(fixture.workspace.journals.values().any(|surface| matches!(&surface.mode, Mode::Viewer(viewer) if viewer.source.directory() == interactive)));
    }
}

#[test]
fn journal_manager_tree_events_obey_hosted_visibility_and_rename_modal_guards() {
    for hosted in [false, true] {
        let mut fixture = Fixture::new(hosted);
        let interactive = fixture.path("interactive");
        let (owner, source) = fixture.open();
        press(
            &mut fixture.workspace,
            source,
            tree::NodeKey::Journal(interactive.clone()),
            Modifiers::NONE,
        );
        if hosted {
            let manager_tab = fixture.workspace.windows[&owner].active_tab_id().unwrap();
            dispatch(&mut fixture.workspace, source, Message::OpenSelected);
            press(&mut fixture.workspace, source, jobs(), Modifiers::NONE);
            dispatch(
                &mut fixture.workspace,
                source,
                Message::Filter(tree::Filter::Pinned),
            );
            assert_eq!(
                manager(&fixture.workspace, source).filter,
                tree::Filter::All
            );
            assert_eq!(
                manager(&fixture.workspace, source)
                    .selected()
                    .unwrap()
                    .directory,
                interactive
            );
            let _ = fixture
                .workspace
                .update(Desktop::SelectTab(owner, manager_tab));
        }
        dispatch(
            &mut fixture.workspace,
            source,
            Message::Metadata(metadata::Message::Rename),
        );
        let prompt = fixture.workspace.journal_edit_for_owner(owner).unwrap();
        press(&mut fixture.workspace, source, jobs(), Modifiers::NONE);
        dispatch(
            &mut fixture.workspace,
            source,
            Message::Filter(tree::Filter::Pinned),
        );
        dispatch(
            &mut fixture.workspace,
            source,
            Message::Search("blocked".into()),
        );
        assert_eq!(
            manager(&fixture.workspace, source).filter,
            tree::Filter::All
        );
        assert!(manager(&fixture.workspace, source).query.is_empty());
        assert_eq!(
            manager(&fixture.workspace, source)
                .selected()
                .unwrap()
                .directory,
            interactive
        );
        let _ = fixture.workspace.refresh_journal_manager(source, false);
        fixture.complete(source);
        assert!(!manager(&fixture.workspace, source).loading);
        assert_eq!(
            fixture.workspace.journal_edit_for_owner(owner),
            Some(prompt)
        );
        dispatch(
            &mut fixture.workspace,
            prompt,
            Message::Metadata(metadata::Message::Cancel),
        );
        assert!(fixture.workspace.journal_edit_for_owner(owner).is_none());
    }
}

#[test]
fn journal_manager_tree_palette_blocks_queued_actions_until_dismissal() {
    for hosted in [false, true] {
        let mut fixture = Fixture::new(hosted);
        let interactive = fixture.path("interactive");
        let (owner, source) = fixture.open();
        let node = tree::NodeKey::Journal(interactive.clone());
        press(
            &mut fixture.workspace,
            source,
            node.clone(),
            Modifiers::NONE,
        );
        press(
            &mut fixture.workspace,
            source,
            node.clone(),
            Modifiers::NONE,
        );
        let queued_double_click = Event::DoubleClick(generation(&fixture.workspace, source), node);
        let _ = fixture.workspace.toggle_palette(owner, false);
        assert!(fixture.workspace.palette.is_open(owner));

        // Test double for Iced actions queued before the real palette was opened.
        dispatch(
            &mut fixture.workspace,
            source,
            Message::Tree(queued_double_click),
        );
        press(&mut fixture.workspace, source, jobs(), Modifiers::NONE);
        dispatch(
            &mut fixture.workspace,
            source,
            Message::Filter(tree::Filter::Pinned),
        );
        dispatch(&mut fixture.workspace, source, Message::OpenSelected);
        assert_eq!(fixture.workspace.journals.len(), 1);
        assert_eq!(
            manager(&fixture.workspace, source).filter,
            tree::Filter::All
        );
        assert_eq!(
            manager(&fixture.workspace, source)
                .selected()
                .unwrap()
                .directory,
            interactive
        );

        let _ = fixture.workspace.toggle_palette(owner, false);
        assert!(!fixture.workspace.palette.is_open(owner));
        dispatch(
            &mut fixture.workspace,
            source,
            Message::Filter(tree::Filter::Pinned),
        );
        assert_eq!(
            manager(&fixture.workspace, source).filter,
            tree::Filter::Pinned
        );
        dispatch(
            &mut fixture.workspace,
            source,
            Message::Filter(tree::Filter::All),
        );
        press(&mut fixture.workspace, source, jobs(), Modifiers::NONE);
        let current = manager(&fixture.workspace, source);
        assert_eq!(
            current
                .tree
                .selected_indices(current.catalog.as_ref().unwrap())
                .len(),
            3
        );
    }
}
