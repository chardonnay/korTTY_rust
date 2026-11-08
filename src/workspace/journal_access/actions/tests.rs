use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use iced::keyboard::Modifiers;
use iced::widget::text_editor;

use super::super::{
    Manager,
    manager_tree::{ContextChoice, ContextEvent, Event},
};
use super::*;
use crate::session_journal::{self, Capture, tree};

struct Fixture {
    workspace: Workspace,
    scratch: PathBuf,
    root: PathBuf,
    captures: Vec<Arc<Capture>>,
}

impl Fixture {
    fn new(hosted: bool) -> Self {
        let scratch = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-action-ui-{}",
            crate::private_files::nonce().unwrap()
        ));
        let root = scratch.join("journals");
        fs::create_dir(&scratch).unwrap();
        crate::private_files::Directory::open(&scratch, false)
            .unwrap()
            .private_child("journals", true)
            .unwrap();
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        workspace.window_settings.session_journal.directory = root.to_str().unwrap().into();
        let fixture = Self {
            workspace,
            scratch,
            root,
            captures: Vec::new(),
        };
        // Private on-disk imports with the pinned Java JAXB metadata fields.
        for (name, title, automation) in [
            ("alpha", "Alpha job", true),
            ("beta", "Beta job", true),
            ("interactive", "Interactive journal", false),
        ] {
            let directory = fixture.path(name);
            fs::create_dir(&directory).unwrap();
            let source = if automation {
                "<sourceKind>JOB</sourceKind><sourceId>nightly</sourceId><sourceName>Nightly</sourceName><runId>nightly-run</runId>"
            } else {
                ""
            };
            fs::write(directory.join("journal.xml"), format!(
                r#"<session-journal formatVersion="1" id="action-{name}"><meta><title>{title}</title><description>Description {name}</description><host>{name}</host><startedAt>2026-11-06T10:00:00Z</startedAt><endedAt>2026-11-06T10:05:00Z</endedAt>{source}</meta><entries/></session-journal>"#
            )).unwrap();
        }
        fixture
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(format!("journal-{name}"))
    }

    fn open(&mut self) -> (window::Id, window::Id) {
        let (owner, _) = self.workspace.open_window(false);
        journal(&mut self.workspace, owner, JournalMessage::Open);
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
        self.complete_catalog(source);
        (owner, source)
    }

    fn complete_catalog(&mut self, source: window::Id) {
        let (revision, root) = manager(&self.workspace, source).reading.clone().unwrap();
        let catalog = session_journal::catalog(&root).unwrap();
        // Test double for Iced delivery, using the actual catalog reader and file identities.
        journal(
            &mut self.workspace,
            source,
            JournalMessage::CatalogLoaded(revision, root, Ok(Arc::new(catalog))),
        );
    }

    fn select(&mut self, source: window::Id, path: PathBuf) {
        let revision = manager(&self.workspace, source).revision;
        journal(
            &mut self.workspace,
            source,
            JournalMessage::Select(revision, path),
        );
    }

    fn selected(&self, source: window::Id) -> Vec<usize> {
        let manager = manager(&self.workspace, source);
        manager
            .tree
            .selected_indices(manager.catalog.as_ref().unwrap())
    }

    fn offer_delete(&mut self, source: window::Id) -> (window::Id, Arc<Prepared>) {
        action(&mut self.workspace, source, Message::DeleteSelected);
        let (&id, dialog) = self
            .workspace
            .journal_actions
            .dialogs
            .iter()
            .find(|(_, dialog)| dialog.source == source)
            .unwrap();
        assert!(matches!(dialog.phase, Phase::Preparing));
        let manager = manager(&self.workspace, source);
        let catalog = manager.catalog.as_ref().unwrap();
        let targets = self
            .selected(source)
            .into_iter()
            .map(|index| Target::from(&catalog.journals[index]))
            .collect();
        // Resolve real directory/document identities with the production preparer.
        (id, Arc::new(prepare_deletion(&dialog.root, targets)))
    }

    fn wait_for_actions(&self) {
        assert!(!self.workspace.pending_shutdown.is_empty());
        let deadline = Instant::now() + Duration::from_secs(10);
        while self
            .workspace
            .pending_shutdown
            .iter()
            .any(|done| !done.is_closed())
        {
            assert!(
                Instant::now() < deadline,
                "journal action worker did not finish"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for capture in &self.captures {
            let _ = capture.finish().recv_blocking();
        }
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

fn journal(workspace: &mut Workspace, source: window::Id, message: JournalMessage) {
    let _ = workspace.update(Desktop::SessionJournal(source, message));
}

fn action(workspace: &mut Workspace, source: window::Id, message: Message) {
    journal(workspace, source, JournalMessage::Actions(message));
}

fn show_review(workspace: &mut Workspace, dialog: window::Id, prepared: Arc<Prepared>) {
    // Test double for Iced delivering the result of real filesystem preparation.
    action(workspace, dialog, Message::Prepared(Ok(prepared)));
    assert!(matches!(
        workspace.journal_actions.dialogs[&dialog].phase,
        Phase::Review(_)
    ));
}

fn group() -> tree::NodeKey {
    tree::NodeKey::Group(tree::GroupKey {
        kind: SourceKind::Job,
        source_id: Some("nightly".into()),
    })
}

fn press(workspace: &mut Workspace, source: window::Id, node: tree::NodeKey, held: Modifiers) {
    let physical = workspace.hosted_tool_owner(source).unwrap_or(source);
    let _ = workspace.update(Desktop::RemoteDock(
        physical,
        super::super::super::remote_dock::Message::Modifiers(held),
    ));
    let generation = manager(workspace, source).tree.generation();
    // Test double for a pointer event emitted by the current Iced tree widget.
    journal(
        workspace,
        source,
        JournalMessage::Tree(Event::Press(generation, node)),
    );
}

#[test]
fn journal_actions_cancel_preparation_or_review_without_deleting() {
    for hosted in [false, true] {
        for cancel_before_review in [false, true] {
            let mut fixture = Fixture::new(hosted);
            let alpha = fixture.path("alpha");
            let original = fs::read(alpha.join("journal.xml")).unwrap();
            let (owner, source) = fixture.open();
            fixture.select(source, alpha.clone());
            let (dialog, prepared) = fixture.offer_delete(source);
            assert_eq!(prepared.ready.len(), 1);
            assert!(prepared.failures.is_empty());
            assert!(!fixture.workspace.palette_can_open(owner));
            action(&mut fixture.workspace, dialog, Message::Confirm);
            assert!(fixture.workspace.pending_shutdown.is_empty());
            if !cancel_before_review {
                show_review(&mut fixture.workspace, dialog, Arc::clone(&prepared));
            }
            action(&mut fixture.workspace, dialog, Message::Dismiss);
            // Test double for an Iced reader completion already queued at cancellation.
            action(
                &mut fixture.workspace,
                dialog,
                Message::Prepared(Ok(prepared)),
            );
            action(&mut fixture.workspace, dialog, Message::Confirm);
            assert!(fixture.workspace.journal_actions.dialogs.is_empty());
            assert!(fixture.workspace.pending_shutdown.is_empty());
            assert!(fixture.workspace.palette_can_open(owner));
            assert_eq!(fs::read(alpha.join("journal.xml")).unwrap(), original);
        }
    }
}

#[test]
fn journal_actions_recheck_current_root_and_policy_after_modal_review() {
    for hosted in [false, true] {
        for change_root in [false, true] {
            let mut fixture = Fixture::new(hosted);
            let alpha = fixture.path("alpha");
            let beta = fixture.path("beta");
            let (owner, source) = fixture.open();
            fixture.select(source, alpha.clone());
            let _ = fixture.workspace.toggle_palette(owner, false);
            action(&mut fixture.workspace, source, Message::DeleteSelected);
            assert!(fixture.workspace.journal_actions.dialogs.is_empty());
            let _ = fixture.workspace.toggle_palette(owner, false);
            let (dialog, prepared) = fixture.offer_delete(source);
            show_review(&mut fixture.workspace, dialog, prepared);
            fixture.select(source, beta.clone());
            action(&mut fixture.workspace, source, Message::DeleteSelected);
            assert_eq!(fixture.workspace.journal_actions.dialogs.len(), 1);
            assert_eq!(
                manager(&fixture.workspace, source)
                    .selected()
                    .unwrap()
                    .directory,
                alpha
            );
            if change_root {
                let alternate = fixture.scratch.join("another-root");
                fs::create_dir(&alternate).unwrap();
                fixture.workspace.window_settings.session_journal.directory =
                    alternate.to_str().unwrap().into();
            } else {
                let deny = "[session-journal]\nallow-delete = false"
                    .parse::<toml_edit::DocumentMut>()
                    .unwrap();
                fixture
                    .workspace
                    .clipboard
                    .policy
                    .terminal_logging
                    .read_rule(deny.as_table())
                    .unwrap();
            }
            action(&mut fixture.workspace, dialog, Message::Confirm);
            let Phase::Report(report) = &fixture.workspace.journal_actions.dialogs[&dialog].phase
            else {
                panic!("confirmation did not reject the changed root or policy");
            };
            assert_eq!(report.completed, 0);
            assert_eq!(report.failures.len(), 1);
            assert!(fixture.workspace.pending_shutdown.is_empty());
            assert!(alpha.join("journal.xml").is_file());
            assert!(beta.join("journal.xml").is_file());
            action(&mut fixture.workspace, dialog, Message::Dismiss);
            assert!(fixture.workspace.journal_edit_for_owner(owner).is_none());
        }
    }
}

#[test]
fn journal_actions_confirm_deduplicated_targets_and_report_partial_preparation_failure() {
    for hosted in [false, true] {
        for fail_one in [false, true] {
            let mut fixture = Fixture::new(hosted);
            let alpha = fixture.path("alpha");
            let beta = fixture.path("beta");
            let alpha_xml = alpha.join("journal.xml");
            let xml = fs::read_to_string(&alpha_xml).unwrap();
            fs::write(
                &alpha_xml,
                xml.replace("</meta>", "<pinned>true</pinned></meta>"),
            )
            .unwrap();
            fs::create_dir(alpha.join("screenshots")).unwrap();
            let png = tiny_skia::Pixmap::new(1, 1).unwrap().encode_png().unwrap();
            fs::write(alpha.join("screenshots").join("shot-000001.png"), png).unwrap();
            fs::write(alpha.join("session-log.json"), b"imported capture fixture").unwrap();
            let (owner, source) = fixture.open();
            fixture.select(source, alpha.clone());
            press(&mut fixture.workspace, source, group(), Modifiers::COMMAND);
            assert_eq!(fixture.selected(source).len(), 2);
            if fail_one {
                let beta_xml = beta.join("journal.xml");
                let xml = fs::read_to_string(&beta_xml).unwrap();
                fs::write(
                    &beta_xml,
                    xml.replace("Beta job", "Changed after catalog load"),
                )
                .unwrap();
            }
            let (dialog, prepared) = fixture.offer_delete(source);
            assert_eq!(prepared.ready.len(), if fail_one { 1 } else { 2 });
            assert_eq!(prepared.failures.len(), usize::from(fail_one));
            let failures = prepared.failures.clone();
            show_review(&mut fixture.workspace, dialog, prepared);
            action(&mut fixture.workspace, dialog, Message::Confirm);
            action(&mut fixture.workspace, dialog, Message::Confirm);
            action(&mut fixture.workspace, dialog, Message::Dismiss);
            assert!(matches!(
                fixture.workspace.journal_actions.dialogs[&dialog].phase,
                Phase::Deleting
            ));
            assert_eq!(fixture.workspace.pending_shutdown.len(), 1);
            fixture.wait_for_actions();
            assert!(!alpha.exists());
            assert_eq!(beta.exists(), fail_one);
            assert!(fixture.path("interactive").exists());
            let completed = usize::from(!alpha.exists()) + usize::from(!beta.exists());
            // Test double for Iced delivery. Count actual removals and keep the real preparation error.
            action(
                &mut fixture.workspace,
                dialog,
                Message::Deleted(Ok(Arc::new(Report {
                    completed,
                    failures,
                }))),
            );
            if fail_one {
                let Phase::Report(report) =
                    &fixture.workspace.journal_actions.dialogs[&dialog].phase
                else {
                    panic!("partial failure did not leave the report open");
                };
                assert_eq!(report.completed, 1);
                assert_eq!(report.failures.len(), 1);
                assert!(report.failures[0].contains("Beta job"));
                action(&mut fixture.workspace, dialog, Message::Dismiss);
            }
            assert!(fixture.workspace.journal_edit_for_owner(owner).is_none());
            fixture.complete_catalog(source);
            assert_eq!(
                manager(&fixture.workspace, source)
                    .catalog
                    .as_ref()
                    .unwrap()
                    .journals
                    .len(),
                if fail_one { 2 } else { 1 }
            );
        }
    }
}

#[test]
fn journal_actions_pin_worker_refreshes_both_managers_without_losing_description_drafts() {
    for hosted in [false, true] {
        let mut fixture = Fixture::new(hosted);
        let alpha = fixture.path("alpha");
        let beta = fixture.path("beta");
        let (_, source) = fixture.open();
        let (_, other) = fixture.open();
        fixture.select(other, alpha.clone());
        for edit in [
            text_editor::Action::SelectAll,
            text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(
                "Unsaved description draft".into(),
            ))),
        ] {
            journal(
                &mut fixture.workspace,
                other,
                JournalMessage::Metadata(metadata::Message::Description(
                    (alpha.clone(), "action-alpha".into()),
                    edit,
                )),
            );
        }
        for (request, pinned, choice) in [
            (1, true, ContextChoice::Pin),
            (2, false, ContextChoice::Unpin),
        ] {
            let generation = manager(&fixture.workspace, source).tree.generation();
            // Test doubles for the group's secondary click and the resulting menu choice.
            for event in [
                ContextEvent::Open(generation, group(), iced::Point::new(30.0, 30.0)),
                ContextEvent::Choose(request, choice),
            ] {
                journal(
                    &mut fixture.workspace,
                    source,
                    JournalMessage::Tree(Event::Context(event)),
                );
            }
            assert_eq!(fixture.selected(source).len(), 2);
            let ticket = fixture.workspace.journal_actions.pinning[&source].ticket;
            fixture.wait_for_actions();
            let completed = [&alpha, &beta]
                .into_iter()
                .filter(|path| session_journal::read(path).unwrap().summary.pinned == pinned)
                .count();
            assert_eq!(completed, 2);
            // Test double for Iced delivery, after the real pin worker saved both fields.
            action(
                &mut fixture.workspace,
                source,
                Message::Pinned(
                    ticket,
                    Ok(Arc::new(Report {
                        completed,
                        failures: Vec::new(),
                    })),
                ),
            );
            fixture.complete_catalog(source);
            fixture.complete_catalog(other);
            assert!(
                !fixture
                    .workspace
                    .journal_actions
                    .pinning
                    .contains_key(&source)
            );
            assert_eq!(
                manager(&fixture.workspace, other).description.text(),
                "Unsaved description draft"
            );
            assert_eq!(
                manager(&fixture.workspace, other)
                    .selected()
                    .unwrap()
                    .pinned,
                pinned
            );
            assert_eq!(
                session_journal::read(&alpha).unwrap().summary.description,
                "Description alpha"
            );
            journal(
                &mut fixture.workspace,
                source,
                JournalMessage::Filter(tree::Filter::Pinned),
            );
            assert_eq!(
                manager(&fixture.workspace, source).results().count(),
                if pinned { 2 } else { 0 }
            );
        }
        assert!(
            !session_journal::read(&fixture.path("interactive"))
                .unwrap()
                .summary
                .pinned
        );
    }
}

#[test]
fn journal_actions_live_capture_blocks_a_mixed_deletion_selection() {
    for hosted in [false, true] {
        let mut fixture = Fixture::new(hosted);
        let capture = Arc::new(Capture::start(fixture.root.clone(), Default::default()).unwrap());
        fixture.captures.push(Arc::clone(&capture));
        let (_, source) = fixture.open();
        let generation = manager(&fixture.workspace, source).tree.generation();
        // Test double for the tree's native select-all event.
        journal(
            &mut fixture.workspace,
            source,
            JournalMessage::Tree(Event::SelectAll(generation)),
        );
        assert_eq!(fixture.selected(source).len(), 4);
        // Test double for a retiring tab's registry entry. The capture and writer are real.
        fixture
            .workspace
            .retired_journal_captures
            .push(Arc::clone(&capture));
        let _ = fixture.workspace.refresh_journal_live_aliases(source);
        let current = manager(&fixture.workspace, source);
        assert!(current.live_aliases.is_none());
        assert!(!fixture.workspace.journal_delete_available(source));
        let revision = current.revision;
        let catalog = current.catalog.as_ref().unwrap();
        let candidates = catalog
            .journals
            .iter()
            .map(|row| (row.directory.clone(), row.id.clone()))
            .collect();
        let aliases =
            metadata::resolve_live_aliases(candidates, vec![Arc::clone(&capture)]).unwrap();
        assert_eq!(aliases.len(), 1);
        // Test double for Iced delivery after the genuine directory-identity probe.
        journal(
            &mut fixture.workspace,
            source,
            JournalMessage::Metadata(metadata::Message::LiveAliasesLoaded(
                revision,
                fixture.root.clone(),
                Ok(Arc::new(aliases)),
            )),
        );
        let current = manager(&fixture.workspace, source);
        let row = current
            .catalog
            .as_ref()
            .unwrap()
            .journals
            .iter()
            .find(|row| row.directory == capture.directory())
            .unwrap();
        assert!(fixture.workspace.journal_metadata_is_live(current, row));
        assert!(!fixture.workspace.journal_delete_available(source));
        action(&mut fixture.workspace, source, Message::DeleteSelected);
        assert!(fixture.workspace.journal_actions.dialogs.is_empty());
        assert!(fixture.workspace.pending_shutdown.is_empty());
        assert!(capture.directory().join("journal.xml").is_file());
        assert!(fixture.path("alpha").join("journal.xml").is_file());
    }
}
