use std::fs;
use std::path::PathBuf;

use iced::advanced::{layout, widget::Tree};
use iced::{Font, Pixels, Renderer};

use crate::session_journal;

use super::super::{entry_editor, screenshot_access};
use super::*;

const REFERENCE: &str = "screenshots/capture.png";
const ORIGINAL: &str = "screenshots/capture.orig.png";

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-screenshot-editor-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir_all(root.join("screenshots")).unwrap();
        fs::write(root.join("journal.xml"), format!(
            r#"<session-journal formatVersion="1" id="shot-editor"><meta><title>Images</title></meta>
            <entries><entry><id>shot</id><kind>SCREENSHOT</kind><screenshotFile>{REFERENCE}</screenshotFile>
            </entry></entries></session-journal>"#
        )).unwrap();
        let mut pixels = tiny_skia::Pixmap::new(32, 24).unwrap();
        pixels.fill(tiny_skia::Color::from_rgba8(240, 230, 220, 255));
        fs::write(root.join(REFERENCE), pixels.encode_png().unwrap()).unwrap();
        Self(root)
    }

    fn open(&self, hosted: bool) -> (Workspace, window::Id, window::Id, ViewerSource) {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        let (owner, _) = workspace.open_window(false);
        let document = Arc::new(session_journal::read(&self.0).unwrap());
        let source = ViewerSource::catalog(&document.summary).unwrap();
        let _ = workspace.open_journal_viewer(owner, source.clone(), "Images".into());
        let viewer = *workspace.journals.keys().next().unwrap();
        // Test double for Iced delivery of the verified on-disk document.
        let _ = workspace.update(Desktop::SessionJournal(
            viewer,
            Message::DocumentLoaded(Ok(Arc::clone(&document))),
        ));
        let _ = workspace.update(Desktop::SessionJournal(
            viewer,
            Message::EntryEditor(entry_editor::Event::Toggle),
        ));
        let row = screenshot_access::Row::new(0, &document.timeline[0]).unwrap();
        let _ = workspace.update(Desktop::SessionJournal(
            viewer,
            Message::ScreenshotAction(screenshot_access::Event::DoubleClick(row)),
        ));
        let dialog = workspace.journal_edit_for_owner(owner).unwrap();
        (workspace, owner, dialog, source)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn send(workspace: &mut Workspace, id: window::Id, event: Event) {
    let _ = workspace.update(Desktop::SessionJournal(id, Message::Annotate(event)));
}

fn open_additional_viewer(workspace: &mut Workspace, source: &ViewerSource) -> window::Id {
    let (owner, _) = workspace.open_window(false);
    let _ = workspace.open_journal_viewer(owner, source.clone(), "Images".into());
    workspace
        .journals
        .iter()
        .find_map(|(&id, surface)| {
            (workspace.journal_owner(id, surface) == Some(owner)).then_some(id)
        })
        .unwrap()
}

fn translation_ticket(workspace: &mut Workspace, dialog: window::Id) -> u64 {
    let profile = workspace
        .window_settings
        .journal_ai
        .profile()
        .unwrap()
        .unwrap();
    let Phase::Editing(form) = &mut workspace
        .journal_screenshot_editors
        .dialogs
        .get_mut(&dialog)
        .unwrap()
        .phase
    else {
        panic!("expected screenshot editor form");
    };
    form.fixture_pending_translation(profile)
}

fn note_draft(workspace: &Workspace, dialog: window::Id) -> String {
    let Phase::Editing(form) = &workspace.journal_screenshot_editors.dialogs[&dialog].phase else {
        panic!("expected screenshot editor form");
    };
    form.note_for_save()
}

#[test]
fn journal_screenshot_editor_translation_preserves_draft_and_rejects_late_results() {
    for hosted in [false, true] {
        let fixture = Fixture::new();
        let (mut workspace, owner, dialog, source) = fixture.open(hosted);
        workspace.window_settings.journal_ai.enabled = true;
        workspace.window_settings.journal_ai.endpoint = "127.0.0.1:11434".into();
        workspace.window_settings.journal_ai.model = "fixture-note".into();
        let loaded = Loaded::read(Target::row(source, "shot".into(), REFERENCE.into())).unwrap();
        // Test double for Iced delivery of the real decoded screenshot and bound entry.
        send(&mut workspace, dialog, Event::Loaded(Ok(Arc::new(loaded))));
        send(
            &mut workspace,
            dialog,
            Event::Note(text_editor::Action::Edit(text_editor::Edit::Paste(
                Arc::new("Original note".into()),
            ))),
        );

        let ticket = translation_ticket(&mut workspace, dialog);
        send(
            &mut workspace,
            dialog,
            Event::Note(text_editor::Action::Move(text_editor::Motion::DocumentEnd)),
        );
        // Test doubles deliver model results with real dialog routing and draft state.
        let completed = |ticket| {
            Event::Translate(note_translation::Event::Finished(
                ticket,
                Ok(Arc::new("Translated note".into())),
            ))
        };
        let before = fs::read(fixture.0.join("journal.xml")).unwrap();
        send(&mut workspace, dialog, completed(ticket));
        assert_eq!(note_draft(&workspace, dialog), "Translated note");
        assert_eq!(fs::read(fixture.0.join("journal.xml")).unwrap(), before);
        send(
            &mut workspace,
            dialog,
            Event::Translate(note_translation::Event::Undo),
        );
        assert_eq!(note_draft(&workspace, dialog), "Original note");

        let ticket = translation_ticket(&mut workspace, dialog);
        send(
            &mut workspace,
            dialog,
            Event::Note(text_editor::Action::Edit(text_editor::Edit::Paste(
                Arc::new(" revised".into()),
            ))),
        );
        send(&mut workspace, dialog, completed(ticket));
        assert_eq!(note_draft(&workspace, dialog), "Original note revised");

        let ticket = translation_ticket(&mut workspace, dialog);
        send(
            &mut workspace,
            dialog,
            Event::Label(
                0,
                Annotation {
                    kind: Kind::Text,
                    points: vec![[4.0, 16.0]],
                    ..Annotation::default()
                },
            ),
        );
        let prompt = workspace.journal_edit_for_owner(owner).unwrap();
        assert_ne!(prompt, dialog);
        send(&mut workspace, dialog, completed(ticket));
        send(&mut workspace, prompt, Event::Close);
        send(&mut workspace, dialog, completed(ticket));
        assert_eq!(note_draft(&workspace, dialog), "Original note revised");

        let ticket = translation_ticket(&mut workspace, dialog);
        send(&mut workspace, dialog, Event::Close);
        send(&mut workspace, dialog, completed(ticket));
        assert_eq!(workspace.journal_edit_for_owner(owner), None);
        assert_eq!(fs::read(fixture.0.join("journal.xml")).unwrap(), before);
    }
}

#[test]
fn journal_screenshot_editor_opens_by_row_and_saves_only_after_confirmation() {
    for hosted in [false, true] {
        let fixture = Fixture::new();
        let initial = fs::read(fixture.0.join(REFERENCE)).unwrap();
        let (mut workspace, owner, dialog, source) = fixture.open(hosted);
        let loaded = Arc::new(
            Loaded::read(Target::row(source.clone(), "shot".into(), REFERENCE.into())).unwrap(),
        );
        // Test double for Iced delivery of the real decoded screenshot and bound entry.
        send(
            &mut workspace,
            dialog,
            Event::Loaded(Ok(Arc::clone(&loaded))),
        );
        assert_eq!(workspace.journal_title(dialog).as_deref(), Some(TITLE));
        {
            let mut view = workspace.journal_view(dialog).unwrap();
            let mut tree = Tree::new(view.as_widget());
            let bounds = view
                .as_widget_mut()
                .layout(
                    &mut tree,
                    &Renderer::new(Font::DEFAULT, Pixels(13.0)),
                    &layout::Limits::new(Size::ZERO, DEFAULT_SIZE),
                )
                .bounds();
            assert!(bounds.width > 0.0 && bounds.height > 0.0);
        }
        send(
            &mut workspace,
            dialog,
            Event::Label(
                0,
                Annotation {
                    kind: Kind::Text,
                    points: vec![[4.0, 16.0]],
                    ..Annotation::default()
                },
            ),
        );
        let prompt = workspace.journal_edit_for_owner(owner).unwrap();
        assert_ne!(prompt, dialog);
        assert_eq!(workspace.journal_title(prompt).as_deref(), Some("Text"));
        send(&mut workspace, prompt, Event::LabelText("Cancelled".into()));
        send(&mut workspace, prompt, Event::Close);
        assert_eq!(workspace.journal_edit_for_owner(owner), Some(dialog));
        let mark = Annotation {
            points: vec![[3.0, 3.0], [24.0, 16.0]],
            ..Annotation::default()
        };
        let preview = Preview::render(&loaded.draft, std::slice::from_ref(&mark)).unwrap();
        send(&mut workspace, dialog, Event::Mark(0, mark.clone()));
        // Test double for Iced delivery of the native renderer's actual result.
        send(
            &mut workspace,
            dialog,
            Event::Rendered(1, Ok(Arc::new(preview))),
        );
        let label = Annotation {
            kind: Kind::Text,
            points: vec![[4.0, 20.0]],
            text: "Label".into(),
            ..Annotation::default()
        };
        send(&mut workspace, dialog, Event::Label(1, label.clone()));
        let prompt = workspace.journal_edit_for_owner(owner).unwrap();
        send(&mut workspace, prompt, Event::LabelText(label.text.clone()));
        send(&mut workspace, prompt, Event::ConfirmLabel);
        let preview = Preview::render(&loaded.draft, &[mark, label]).unwrap();
        // Test double for delivery of the preview including the accepted label.
        send(
            &mut workspace,
            dialog,
            Event::Rendered(2, Ok(Arc::new(preview))),
        );
        send(
            &mut workspace,
            dialog,
            Event::Note(text_editor::Action::Edit(text_editor::Edit::Paste(
                Arc::new(" \u{a0}Screenshot note\u{a0} ".into()),
            ))),
        );
        let stale_document = Arc::new(session_journal::read(&fixture.0).unwrap());
        let pending_viewer = open_additional_viewer(&mut workspace, &source);
        let _ = workspace.update(Desktop::SessionJournal(
            pending_viewer,
            Message::DocumentLoaded(Ok(Arc::clone(&stale_document))),
        ));
        let _ = workspace.update(Desktop::SessionJournal(
            pending_viewer,
            Message::SelectEntry(0, "shot".into()),
        ));
        let _ = workspace.update(Desktop::SessionJournal(
            pending_viewer,
            Message::ShowScreenshot(0, "shot".into()),
        ));
        let Mode::Viewer(viewer) = &workspace.journals[&pending_viewer].mode else {
            panic!("expected journal viewer");
        };
        assert!(viewer.loading);
        assert_eq!(viewer.screenshot_request.as_deref(), Some("shot"));
        let stale_revision = viewer.page_revision;
        let stale_image = Arc::new(
            super::super::screenshot_export::Displayed::load(&source, "shot", REFERENCE).unwrap(),
        );
        assert_eq!(fs::read(fixture.0.join(REFERENCE)).unwrap(), initial);
        assert!(!fixture.0.join(ORIGINAL).exists());
        send(&mut workspace, dialog, Event::Save);
        let loading_viewer = open_additional_viewer(&mut workspace, &source);
        let done = workspace.pending_shutdown.last().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done.is_closed() {
            assert!(
                Instant::now() < deadline,
                "screenshot save worker did not finish"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_ne!(fs::read(fixture.0.join(REFERENCE)).unwrap(), initial);
        assert_eq!(fs::read(fixture.0.join(ORIGINAL)).unwrap(), initial);
        let saved = Draft::read(&source, "shot", REFERENCE).unwrap();
        assert_eq!(saved.annotations().len(), 2);
        assert_eq!(saved.annotations()[1].text, "Label");
        assert_eq!(saved.note(), "\u{a0}Screenshot note\u{a0}");

        let document = Arc::new(session_journal::read(&fixture.0).unwrap());
        // Test doubles for delayed Iced task delivery around the real screenshot commit.
        send(
            &mut workspace,
            dialog,
            Event::Saved(Ok(Arc::new(Saved {
                outcome: Outcome::default(),
                document: Some(Arc::clone(&document)),
            }))),
        );
        let _ = workspace.update(Desktop::SessionJournal(
            loading_viewer,
            Message::DocumentLoaded(Ok(stale_document)),
        ));
        let _ = workspace.update(Desktop::SessionJournal(
            pending_viewer,
            Message::ScreenshotLoaded(stale_revision, Ok(stale_image)),
        ));
        for id in [loading_viewer, pending_viewer] {
            let Mode::Viewer(viewer) = &workspace.journals[&id].mode else {
                panic!("expected journal viewer");
            };
            assert!(!viewer.loading);
            assert!(viewer.screenshot.is_none());
            assert!(viewer.screenshot_request.is_none());
            assert!(Arc::ptr_eq(viewer.document.as_ref().unwrap(), &document));
        }
    }
}
