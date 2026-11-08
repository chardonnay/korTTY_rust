use std::fs;
use std::path::PathBuf;

use crate::session_journal::{self, viewer::Source};

use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn create() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-entry-editor-{}",
            crate::private_files::nonce().unwrap()
        ));
        let fixture = Self(root);
        let directory = fixture.0.join("journal");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("journal.xml"),
            "<session-journal formatVersion=\"1\" id=\"editor\"><meta><title>Test</title></meta>\
             <entries><entry><id>first</id><kind>USER_NOTE</kind><title>Original</title>\
             <text>Summary</text><userNote>Original note</userNote></entry>\
             <entry><id>second</id><kind>USER_NOTE</kind><title>Second</title></entry>\
             </entries></session-journal>",
        )
        .unwrap();
        fixture
    }

    fn draft(&self, id: &str) -> Arc<Draft> {
        let journal = session_journal::read(&self.0.join("journal")).unwrap();
        let source = Source::catalog(&journal.summary).unwrap();
        Arc::new(Draft::read(&source, id).unwrap())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn entry_editor_revert_and_selection_invalidate_old_input() {
    let fixture = Fixture::create();
    let mut editor = State::default();
    editor.toggle();
    editor.select(0, fixture.draft("first"));
    let generation = editor.generation;
    editor.change(generation, Input::Title("Unicode & Ω".into()));
    editor.change(generation, Input::Note(text_editor::Action::SelectAll));
    editor.change(
        generation,
        Input::Note(text_editor::Action::Edit(text_editor::Edit::Paste(
            Arc::new("First line\nZweite Zeile".into()),
        ))),
    );
    assert!(editor.is_dirty());
    let Changes::Fields { fields, .. } = editor.edit().unwrap().into_changes() else {
        panic!("expected field edit");
    };
    assert_eq!(fields.title, "Unicode & Ω");
    assert_eq!(fields.text, "Summary");
    assert_eq!(fields.note, "First line\nZweite Zeile");

    editor.revert();
    assert!(!editor.is_dirty());
    assert!(!editor.matches_generation(generation));
    let reverted = editor.generation;
    editor.select(1, fixture.draft("second"));
    editor.change(reverted, Input::Title("Stale action".into()));
    assert_eq!(editor.selection(), Some((1, "second")));
    assert!(!editor.is_dirty());
    editor.close();
    assert!(!editor.is_open());
    assert_eq!(editor.selection(), None);
}

#[test]
fn entry_editor_split_keeps_basis_points_across_mode_changes() {
    let mut editor = State::default();
    assert_eq!(editor.split(), 6200);
    editor.toggle();
    editor.resize(0.5744);
    editor.resize(f32::NAN);
    editor.close();
    editor.toggle();
    assert_eq!(editor.split(), 5744);
    editor.set_split(u16::MAX);
    assert_eq!(editor.split(), MAX_SPLIT);
}
