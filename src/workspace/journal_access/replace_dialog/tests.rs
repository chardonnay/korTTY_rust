use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::advanced::{layout, widget::Tree};
use iced::{Font, Pixels, Renderer};

use crate::session_journal::replace::Counts;
use crate::session_journal::{self, Capture, RecordKind};

use super::super::{Navigation, Viewer, entry_editor};
use super::*;

const XML: &str = r#"<session-journal formatVersion="1" id="replace-route">
<meta><title>Journal</title></meta>
<entries><entry><id>row</id><kind>USER_NOTE</kind><title>First needle</title>
<text>Second needle</text></entry></entries></session-journal>"#;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-replace-route-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("journal.xml"), XML).unwrap();
        Self(root)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn open(workspace: &mut Workspace, path: &Path) -> (window::Id, window::Id) {
    let document = Arc::new(session_journal::read(path).unwrap());
    let target = ViewerSource::catalog(&document.summary).unwrap();
    let (owner, _) = workspace.open_window(false);
    let _ = workspace.open_journal_viewer(owner, target, "Journal".into());
    let id = workspace
        .journals
        .iter()
        .find_map(|(&id, surface)| {
            (workspace.journal_owner(id, surface) == Some(owner)).then_some(id)
        })
        .unwrap();
    // Test double for Iced delivery of a real on-disk snapshot.
    send(workspace, id, Message::DocumentLoaded(Ok(document)));
    (owner, id)
}

fn viewer(workspace: &Workspace, id: window::Id) -> &Viewer {
    let Mode::Viewer(viewer) = &workspace.journals[&id].mode else {
        panic!("expected journal viewer");
    };
    viewer
}

fn send(workspace: &mut Workspace, id: window::Id, message: Message) {
    let _ = workspace.update(Desktop::SessionJournal(id, message));
}

fn dialog_event(workspace: &mut Workspace, id: window::Id, event: Event) {
    send(workspace, id, Message::Replace(event));
}

fn form(workspace: &Workspace, id: window::Id) -> &Form {
    let Phase::Form(form) = &workspace.journal_replacements.dialogs[&id].phase else {
        panic!("expected replacement form");
    };
    form
}

fn start_count(workspace: &mut Workspace, id: window::Id, live: bool) -> (u64, Arc<Preview>) {
    dialog_event(workspace, id, Event::Count);
    let form = form(workspace, id);
    let target = &workspace.journal_replacements.dialogs[&id].target;
    let rule = Rule::compile(
        &form.search,
        &form.replacement,
        form.regex,
        form.ignore_case,
    )
    .unwrap();
    let preview = replace::preview(target, rule, form.include_log, live, || false).unwrap();
    (form.counting.as_ref().unwrap().revision, Arc::new(preview))
}

fn wait_done(done: &async_channel::Receiver<()>) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !done.is_closed() {
        assert!(Instant::now() < deadline, "journal worker did not finish");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn journal_replace_routes_require_a_current_count_and_refresh_other_views() {
    for hosted in [false, true] {
        let fixture = Fixture::new();
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        let (owner, source) = open(&mut workspace, &fixture.0);
        let (_, second) = open(&mut workspace, &fixture.0);
        send(
            &mut workspace,
            source,
            Message::EntryEditor(entry_editor::Event::Toggle),
        );
        send(
            &mut workspace,
            source,
            Message::EntryEditor(entry_editor::Event::Replace),
        );
        let dialog = workspace.journal_edit_for_owner(source).unwrap();
        assert_eq!(workspace.journal_edit_for_owner(owner), Some(dialog));
        assert_eq!(workspace.journal_title(dialog).as_deref(), Some(TITLE));
        assert!(form(&workspace, dialog).search.is_empty());
        assert!(form(&workspace, dialog).include_log);
        assert_eq!(form(&workspace, dialog).replacement.as_str(), "***");
        let input = Change::Search(Zeroizing::new("needle".into()));
        assert!(!format!("{input:?}").contains("needle"));
        dialog_event(&mut workspace, dialog, Event::Change(input));
        dialog_event(&mut workspace, dialog, Event::Apply);
        assert_eq!(
            fs::read_to_string(fixture.0.join("journal.xml")).unwrap(),
            XML
        );

        let (revision, preview) = start_count(&mut workspace, dialog, false);
        dialog_event(
            &mut workspace,
            dialog,
            Event::Change(Change::IncludeLog(false)),
        );
        // Test double for delayed delivery of an actual earlier count.
        dialog_event(
            &mut workspace,
            dialog,
            Event::Counted(revision, Ok(preview)),
        );
        assert!(form(&workspace, dialog).applicable().is_none());
        dialog_event(&mut workspace, dialog, Event::Dismiss);
        dialog_event(&mut workspace, dialog, Event::Apply);
        assert_eq!(
            fs::read_to_string(fixture.0.join("journal.xml")).unwrap(),
            XML
        );

        send(
            &mut workspace,
            source,
            Message::ViewerNavigation(Navigation::OpenFind),
        );
        send(
            &mut workspace,
            source,
            Message::ViewerNavigation(Navigation::Query("needle".into())),
        );
        dialog_event(&mut workspace, source, Event::FromSearch);
        let dialog = workspace.journal_edit_for_owner(source).unwrap();
        assert_eq!(form(&workspace, dialog).search.as_str(), "needle");
        {
            let mut element = workspace.journal_view(dialog).unwrap();
            let mut tree = Tree::new(element.as_widget());
            let bounds = element
                .as_widget_mut()
                .layout(
                    &mut tree,
                    &Renderer::new(Font::DEFAULT, Pixels(13.0)),
                    &layout::Limits::new(Size::ZERO, Size::new(530.0, 460.0)),
                )
                .bounds();
            assert!(bounds.width > 0.0 && bounds.height > 0.0);
        }
        let (revision, preview) = start_count(&mut workspace, dialog, false);
        let counts = preview.counts();
        assert_eq!(
            counts,
            Counts {
                fields: 2,
                records: 0
            }
        );
        // Test double for Iced delivery of the real replacement preview.
        dialog_event(
            &mut workspace,
            dialog,
            Event::Counted(revision, Ok(preview)),
        );
        workspace.clipboard.policy.session_journal_allowed = false;
        dialog_event(&mut workspace, dialog, Event::Apply);
        assert!(workspace.pending_shutdown.is_empty());
        workspace.clipboard.policy.session_journal_allowed = true;
        dialog_event(&mut workspace, dialog, Event::Apply);
        assert!(viewer(&workspace, source).entry_flow.saving());
        assert!(viewer(&workspace, second).entry_flow.saving());
        dialog_event(&mut workspace, dialog, Event::Dismiss);
        assert_eq!(workspace.journal_edit_for_owner(owner), Some(dialog));
        wait_done(workspace.pending_shutdown.last().unwrap());
        let document = Arc::new(session_journal::read(&fixture.0).unwrap());
        assert_eq!(document.timeline[0].title, "First ***");
        assert_eq!(document.timeline[0].text, "Second ***");
        // Test double for Iced delivery after the real disk worker finished.
        dialog_event(
            &mut workspace,
            dialog,
            Event::Applied(Ok(Arc::new(Applied {
                outcome: Outcome {
                    counts,
                    published: 1,
                    ..Outcome::default()
                },
                document: Some(document),
            }))),
        );
        for id in [source, second] {
            let viewer = viewer(&workspace, id);
            assert!(!viewer.entry_flow.saving());
            assert!(viewer.navigation.replace_query().is_none());
            assert_eq!(
                viewer.document.as_ref().unwrap().timeline[0].text,
                "Second ***"
            );
        }
    }
}

#[test]
fn journal_replace_previews_live_capture_but_never_applies_while_recording() {
    let fixture = Fixture::new();
    let capture = Arc::new(Capture::start(fixture.0.join("journals"), Default::default()).unwrap());
    capture
        .sink()
        .try_write(RecordKind::Output, Zeroizing::new("needle".into()), false)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while capture.published().0 == 0 {
        assert!(
            Instant::now() < deadline,
            "capture did not publish its output"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let mut workspace = Workspace::default();
    // Test double for the retiring-capture registry, backed by the real capture worker.
    workspace
        .retired_journal_captures
        .push(Arc::clone(&capture));
    let (_, source) = open(&mut workspace, capture.directory());
    send(
        &mut workspace,
        source,
        Message::EntryEditor(entry_editor::Event::Toggle),
    );
    send(
        &mut workspace,
        source,
        Message::EntryEditor(entry_editor::Event::Replace),
    );
    let dialog = workspace.journal_edit_for_owner(source).unwrap();
    dialog_event(
        &mut workspace,
        dialog,
        Event::Change(Change::Search(Zeroizing::new("needle".into()))),
    );
    let (revision, preview) = start_count(&mut workspace, dialog, true);
    assert_eq!(
        preview.counts(),
        Counts {
            fields: 0,
            records: 1
        }
    );
    // Test double for Iced delivery of the real live preview.
    dialog_event(
        &mut workspace,
        dialog,
        Event::Counted(revision, Ok(preview)),
    );
    let blocked = &workspace.journal_replacements.dialogs[&dialog];
    assert!(
        workspace
            .journal_replacement_blocker(dialog, blocked)
            .unwrap()
            .contains("Stop")
    );
    dialog_event(&mut workspace, dialog, Event::Apply);
    assert!(workspace.pending_shutdown.is_empty());
    wait_done(&capture.finish());
    let page = viewer(&workspace, source)
        .source
        .capture_page(None, None, None)
        .unwrap();
    assert!(page.records.iter().any(|record| record.text == "needle"));
}
