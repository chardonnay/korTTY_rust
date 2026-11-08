use std::fs;
use std::io::Read as _;
use std::time::{Duration, Instant};

use crate::private_files::{self, Directory};
use crate::session_journal;

use super::super::{Manager, manager_tree};
use super::*;

struct Fixture {
    workspace: Workspace,
    scratch: PathBuf,
    source: window::Id,
    destination: PathBuf,
}

impl Fixture {
    fn new(hosted: bool) -> Self {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let name = format!(
            "kortty-journal-export-ui-{}",
            private_files::nonce().unwrap()
        );
        let scratch = temp.join(&name);
        let dir = Directory::open(&temp, false)
            .unwrap()
            .private_child(&name, true)
            .unwrap();
        dir.private_child("journals", true).unwrap();
        dir.private_child("destination", true).unwrap();
        let root = scratch.join("journals");
        for (id, title, note) in [
            ("one", "First", "A retained note"),
            ("two", "Second", "A different note"),
        ] {
            let directory = root.join(format!("journal-{id}"));
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join("journal.xml"), format!(
                r#"<session-journal formatVersion="1" id="{id}"><meta><title>{title}</title></meta><entries><entry><id>note-1</id><kind>USER_NOTE</kind><createdAt>2026-11-06T10:00:00Z</createdAt><userNote>{note}</userNote></entry><entry><id>note-2</id><kind>USER_NOTE</kind><createdAt>2026-11-06T12:00:00Z</createdAt><userNote>A later entry</userNote></entry></entries></session-journal>"#
            )).unwrap();
        }
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        workspace.window_settings.session_journal.directory = root.to_str().unwrap().into();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update(Desktop::SessionJournal(owner, JournalMessage::Open));
        let source = workspace
            .journal_manager_instance(owner)
            .unwrap_or_else(|| *workspace.journals.keys().next().expect("native manager"));
        let destination = scratch.join("destination");
        let mut fixture = Self {
            workspace,
            scratch,
            source,
            destination,
        };
        let (revision, reading_root) = fixture.manager().reading.clone().unwrap();
        assert_eq!(reading_root, root);
        let catalog = session_journal::catalog(&root).unwrap();
        // Test double for Iced delivery, using the actual on-disk catalog.
        fixture.send(
            source,
            JournalMessage::CatalogLoaded(revision, root, Ok(Arc::new(catalog))),
        );
        fixture
    }

    fn manager(&self) -> &Manager {
        let Mode::Manager(manager) = &self.workspace.journals[&self.source].mode else {
            panic!("expected journal manager");
        };
        manager
    }

    fn send(&mut self, id: window::Id, message: JournalMessage) {
        let _ = self.workspace.update(Desktop::SessionJournal(id, message));
    }

    fn export(&mut self, id: window::Id, message: Message) {
        self.send(id, JournalMessage::Export(message));
    }

    fn begin(&mut self) -> window::Id {
        self.select_first();
        self.open_export(self.source, Format::Markdown)
    }

    fn select_first(&mut self) {
        self.send(self.source, JournalMessage::Search("First".into()));
        let generation = self.manager().tree.generation();
        // Test double for a user selecting the single filtered row in Iced.
        self.send(
            self.source,
            JournalMessage::Tree(manager_tree::Event::SelectAll(generation)),
        );
    }

    fn open_export(&mut self, source: window::Id, format: Format) -> window::Id {
        self.export(source, Message::Open(format));
        let id = self.workspace.journal_edit_for_owner(source).unwrap();
        let dialog = &self.workspace.journal_exports.dialogs[&id];
        assert!(matches!(dialog.phase, Phase::Options(_)));
        // Test double for Iced delivery, using a real snapshot of the original selection.
        let preview =
            load_preview(&dialog.root, &dialog.targets, &dialog.preview_cancelled).unwrap();
        assert_eq!(preview.snapshots.len(), 1);
        self.export(id, Message::Previewed(Ok(Arc::new(preview))));
        id
    }

    fn open_viewer(&mut self) -> (window::Id, PathBuf) {
        self.select_first();
        self.send(self.source, JournalMessage::OpenSelected);
        let (source, directory) = self
            .workspace
            .journals
            .iter()
            .find_map(|(&id, surface)| match &surface.mode {
                Mode::Viewer(viewer) => Some((id, viewer.source.directory().to_owned())),
                Mode::Manager(_) => None,
            })
            .unwrap();
        // Test double for Iced delivery, preserving the identity captured by the real reader.
        let document = session_journal::read(&directory).unwrap();
        self.send(
            source,
            JournalMessage::DocumentLoaded(Ok(Arc::new(document))),
        );
        (source, directory)
    }

    fn destination(&mut self, id: window::Id) {
        self.export(id, Message::Options(export_options::Event::Confirm));
        self.export(id, Message::Options(export_options::Event::Cancel));
        let completion = self
            .browser(id)
            .complete_listing_for_test(&self.scratch.join("absent"));
        self.export(id, Message::Browser(completion));
        self.export(
            id,
            Message::Browser(browser::Event::Folder(
                self.destination.to_str().unwrap().into(),
            )),
        );
        self.export(id, Message::Browser(browser::Event::List));
        let completion = self
            .browser(id)
            .complete_listing_for_test(&self.destination);
        self.export(id, Message::Browser(completion));
        self.export(id, Message::Browser(browser::Event::Name("custom".into())));
    }

    fn browser(&self, id: window::Id) -> &browser::Browser {
        let Phase::Destination(save) = &self.workspace.journal_exports.dialogs[&id].phase else {
            panic!("expected destination chooser");
        };
        &save.browser
    }

    fn assert_no_output(&self) {
        assert_eq!(fs::read_dir(&self.destination).unwrap().count(), 0);
    }

    fn wait_for_export(&self) {
        let done = self
            .workspace
            .pending_shutdown
            .last()
            .expect("export worker");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done.is_closed() {
            assert!(Instant::now() < deadline, "export worker did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.scratch);
    }
}

#[test]
fn journal_export_save_publishes_the_original_selection_after_destination_confirmation() {
    for hosted in [false, true] {
        let mut fixture = Fixture::new(hosted);
        let id = fixture.begin();
        fixture.destination(id);
        fixture.assert_no_output();
        fixture.send(fixture.source, JournalMessage::Search("Second".into()));
        fixture.export(id, Message::Browser(browser::Event::Save));
        fixture.wait_for_export();
        let markdown = fs::read_to_string(fixture.destination.join("custom.md")).unwrap();
        assert!(markdown.contains("A retained note"));
        assert!(!markdown.contains("A different note"));
    }
}

#[test]
fn journal_export_viewer_saves_html_bundle_in_native_and_hosted_modes() {
    for hosted in [false, true] {
        let mut fixture = Fixture::new(hosted);
        let (source, directory) = fixture.open_viewer();
        let id = fixture.open_export(source, Format::HtmlBundle);
        fixture.destination(id);
        fixture.assert_no_output();
        fixture.export(id, Message::Browser(browser::Event::Save));
        fixture.wait_for_export();

        let zip = fs::File::open(fixture.destination.join("custom.zip")).unwrap();
        let mut archive = zip::ZipArchive::new(zip).unwrap();
        let mut xml = Vec::new();
        archive
            .by_name("journal.xml")
            .unwrap()
            .read_to_end(&mut xml)
            .unwrap();
        assert_eq!(xml, fs::read(directory.join("journal.xml")).unwrap());
        let mut html = String::new();
        archive
            .by_name("journal.html")
            .unwrap()
            .read_to_string(&mut html)
            .unwrap();
        assert!(html.contains("A retained note"));
        assert!(!html.contains("A different note"));
    }
}

#[test]
fn journal_export_viewer_range_reopens_same_format_with_dated_bounds() {
    let mut fixture = Fixture::new(true);
    let (source, _) = fixture.open_viewer();
    let dialog = fixture.open_export(source, Format::HtmlBundle);
    fixture.export(dialog, Message::Options(export_options::Event::PickRange));
    assert!(
        !fixture
            .workspace
            .journal_exports
            .dialogs
            .contains_key(&dialog)
    );
    let Mode::Viewer(viewer) = &fixture.workspace.journals[&source].mode else {
        panic!("expected a viewer");
    };
    let ticket = viewer.export_range.as_ref().unwrap().ticket;
    fixture.export(
        source,
        Message::Range(ticket, ranges::Event::Select(1, "note-2".into())),
    );
    fixture.export(
        source,
        Message::Range(ticket, ranges::Event::Select(0, "note-1".into())),
    );
    fixture.export(source, Message::Range(ticket, ranges::Event::Add));
    fixture.export(source, Message::Range(ticket, ranges::Event::Apply));
    let id = fixture.workspace.journal_edit_for_owner(source).unwrap();
    let Phase::Options(draft) = &fixture.workspace.journal_exports.dialogs[&id].phase else {
        panic!("expected reopened export options");
    };
    let choice = draft.choice().unwrap();
    assert_eq!(choice.format, Format::HtmlBundle);
    assert_eq!(choice.options.filter.windows.len(), 1);
    let range = &choice.options.filter.windows[0];
    let from: Timestamp = "2026-11-06T10:00:00Z".parse().unwrap();
    let to: Timestamp = "2026-11-06T12:00:00Z".parse().unwrap();
    let from = from.to_zoned(choice.options.filter.zone.clone());
    let to = to.to_zoned(choice.options.filter.zone);
    assert_eq!(range.from_date, Some(from.date()));
    assert_eq!(range.to_date, Some(to.date()));
    assert_eq!(range.from_time, Some(from.time()));
    assert_eq!(range.to_time, Some(to.time()));
    fixture.assert_no_output();
}

#[test]
fn journal_pdf_export_viewer_direct_range_publishes_the_selected_journal() {
    let mut fixture = Fixture::new(false);
    let (source, _) = fixture.open_viewer();
    fixture.export(source, Message::BeginRange);
    let Mode::Viewer(viewer) = &fixture.workspace.journals[&source].mode else {
        panic!("expected a viewer");
    };
    let ticket = viewer.export_range.as_ref().unwrap().ticket;
    fixture.export(
        source,
        Message::Range(ticket, ranges::Event::Select(0, "note-1".into())),
    );
    fixture.export(source, Message::Range(ticket, ranges::Event::Apply));
    let dialog = fixture.workspace.journal_edit_for_owner(source).unwrap();
    fixture.destination(dialog);
    fixture.assert_no_output();
    fixture.export(dialog, Message::Browser(browser::Event::Save));
    fixture.wait_for_export();
    assert!(
        fs::read(fixture.destination.join("custom.pdf"))
            .unwrap()
            .starts_with(b"%PDF-")
    );
}

#[test]
fn journal_export_cancel_and_changed_source_leave_the_destination_empty() {
    let mut fixture = Fixture::new(true);
    let (other, _) = fixture.workspace.open_window(false);
    let id = fixture.begin();
    assert_eq!(fixture.workspace.owner_dialog(other).unwrap().0, id);
    assert!(fixture.workspace.owner_dialog(id).is_none());
    fixture.destination(id);
    assert!(fixture.workspace.owner_dialog(other).is_none());
    fixture.export(id, Message::Browser(browser::Event::Cancel));
    assert!(
        fixture
            .workspace
            .journal_edit_for_owner(fixture.source)
            .is_none()
    );
    fixture.export(id, Message::Browser(browser::Event::Save));
    fixture.assert_no_output();
    assert!(fixture.workspace.pending_shutdown.is_empty());

    let id = fixture.begin();
    fixture.destination(id);
    fixture.workspace.window_settings.session_journal.directory =
        fixture.destination.to_str().unwrap().into();
    fixture.export(id, Message::Browser(browser::Event::Save));
    assert!(fixture.workspace.pending_shutdown.is_empty());
    fixture.assert_no_output();
}

#[test]
fn journal_export_ai_never_sends_before_save_or_after_profile_change() {
    use std::net::TcpListener;

    let mut fixture = Fixture::new(true);
    let endpoint = TcpListener::bind("127.0.0.1:0").unwrap();
    endpoint.set_nonblocking(true).unwrap();
    fixture.workspace.window_settings.journal_ai = topic_ai_settings::Preferences {
        enabled: true,
        endpoint: endpoint.local_addr().unwrap().to_string(),
        model: "fixture:local".into(),
        ..topic_ai_settings::Preferences::default()
    };
    let id = fixture.begin();
    fixture.export(
        id,
        Message::Options(export_options::Event::Topic("diagnostics".into())),
    );
    fixture.export(id, Message::Options(export_options::Event::TopicAi(true)));
    fixture.destination(id);
    assert_eq!(
        endpoint.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    fixture.assert_no_output();

    fixture.workspace.window_settings.journal_ai.model = "fixture:changed".into();
    fixture.export(id, Message::Browser(browser::Event::Save));
    assert!(fixture.workspace.pending_shutdown.is_empty());
    assert_eq!(
        endpoint.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    fixture.assert_no_output();

    fixture.workspace.window_settings.journal_ai.model = "fixture:local".into();
    fixture.workspace.clipboard.policy.ai_allowed = false;
    fixture.export(id, Message::Browser(browser::Event::Save));
    assert!(fixture.workspace.pending_shutdown.is_empty());
    assert_eq!(
        endpoint.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    fixture.assert_no_output();
}
