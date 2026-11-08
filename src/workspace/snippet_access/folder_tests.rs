use super::*;
use crate::snippet_window::{Message as SnippetMessage, folders::Message as FolderMessage};

struct Fixture {
    profile: snippets::tests::Profile,
    workspace: Workspace,
    folder: snippets::folders::Id,
    snippet: snippets::SnippetId,
    library: window::Id,
    editor: window::Id,
}

impl Fixture {
    fn new() -> Self {
        let profile = snippets::tests::Profile::new();
        let mut library = snippets::Library::open(profile.path()).unwrap();
        let folder = library.create_folder(None, "Operations").unwrap();
        let snippet = save_in_folder(&mut library, "Deploy");
        let mut workspace = Workspace::default();
        let (first, _) = workspace.open_window(false);
        let (second, _) = workspace.open_window(false);
        let (library, _) = workspace.install_snippet_surface(
            first,
            SnippetWindow::open(profile.path()).unwrap(),
            false,
        );
        let (editor, _) = workspace.install_snippet_surface(
            second,
            SnippetWindow::open(profile.path()).unwrap(),
            true,
        );
        let _ = workspace.update_snippets(editor, SnippetMessage::Open(snippet));
        Self {
            profile,
            workspace,
            folder,
            snippet,
            library,
            editor,
        }
    }

    fn persisted(&self) -> snippets::Library {
        snippets::Library::open(self.profile.path()).unwrap()
    }

    fn folder_action(&mut self, action: FolderMessage) {
        let _ = self
            .workspace
            .update_snippets(self.library, SnippetMessage::Folders(action));
    }

    fn edit_name(&mut self, name: &str) {
        let _ = self
            .workspace
            .update_snippets(self.editor, SnippetMessage::Name(name.into()));
    }

    fn save_editor(&mut self) {
        let _ = self
            .workspace
            .update_snippets(self.editor, SnippetMessage::Save);
    }

    fn start_delete(&mut self) {
        self.folder_action(FolderMessage::Delete(self.folder));
        self.folder_action(FolderMessage::DeleteContents);
    }
}

fn save_in_folder(library: &mut snippets::Library, name: &str) -> snippets::SnippetId {
    library
        .save_with_metadata(
            None,
            snippets::Input {
                name: name.into(),
                language: "bash".into(),
                category: String::new(),
                content: "echo ok".into(),
            },
            Some(snippets::Metadata {
                folder_path: "Operations".into(),
                ..Default::default()
            }),
        )
        .unwrap()
}

#[test]
fn renaming_a_folder_keeps_the_peer_edit_and_rebases_its_saved_path() {
    let mut fixture = Fixture::new();
    fixture.edit_name("Unsaved name");
    fixture.folder_action(FolderMessage::Rename(fixture.folder));
    fixture.folder_action(FolderMessage::Name("Production".into()));
    fixture.folder_action(FolderMessage::Apply);
    assert!(fixture.workspace.snippets[&fixture.editor].window.dirty());
    assert_eq!(
        fixture.persisted().folder(fixture.folder).unwrap().path,
        "Production"
    );
    fixture.save_editor();
    let library = fixture.persisted();
    let saved = library.get(fixture.snippet).unwrap();
    assert_eq!(saved.name(), "Unsaved name");
    assert_eq!(saved.metadata().folder_path, "Production");
    assert!(!fixture.workspace.snippets[&fixture.editor].window.dirty());
}

#[test]
fn recursive_folder_deletion_requires_confirmation_and_protects_dirty_peers() {
    let mut fixture = Fixture::new();
    fixture.edit_name("Unsaved name");
    fixture.start_delete();
    assert!(fixture.persisted().folder(fixture.folder).is_some());
    fixture.folder_action(FolderMessage::DeleteConfirmed);
    assert!(fixture.persisted().get(fixture.snippet).is_some());
    assert!(fixture.workspace.snippets[&fixture.editor].window.dirty());
    fixture.save_editor();
    fixture.start_delete();
    fixture.folder_action(FolderMessage::DeleteConfirmed);
    assert!(fixture.persisted().folder(fixture.folder).is_none());
    assert!(fixture.persisted().get(fixture.snippet).is_none());
    assert!(
        !fixture.workspace.snippets[&fixture.editor]
            .window
            .owns_editor(fixture.snippet)
    );
}

#[test]
fn folder_delete_confirmation_does_not_delete_contents_added_by_a_peer() {
    let mut fixture = Fixture::new();
    fixture.start_delete();
    let mut library = fixture.persisted();
    let later = save_in_folder(&mut library, "Later");
    fixture
        .workspace
        .snippets
        .get_mut(&fixture.library)
        .unwrap()
        .window
        .refresh_shared(library);
    fixture.folder_action(FolderMessage::DeleteConfirmed);
    assert!(fixture.persisted().get(fixture.snippet).is_some());
    assert!(fixture.persisted().get(later).is_some());
}
