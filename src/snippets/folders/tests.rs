use std::fs;

use super::*;
use crate::snippets::{Input, Metadata, tests::Profile, validate_backup};

fn save(library: &mut Library, name: &str, path: &str) -> SnippetId {
    library
        .save_with_metadata(
            None,
            Input {
                name: name.into(),
                language: "bash".into(),
                category: "ops".into(),
                content: "printf hello\n".into(),
            },
            Some(Metadata {
                folder_path: path.into(),
                file_name: Some("script.sh".into()),
                ..Metadata::default()
            }),
        )
        .unwrap()
}

#[test]
fn legacy_paths_are_read_without_rewriting_and_vacated_paths_keep_their_folders() {
    let profile = Profile::new();
    let path = profile.path();
    let id = SnippetId::new().unwrap();
    let legacy = serde_json::json!({
        "version": 1,
        "snippets": [{
            "id": id,
            "name": "Server",
            "language": "bash",
            "content": "echo hello",
            "metadata": { "folder_path": "Operations\\ ./Servers." }
        }]
    });
    let bytes = serde_json::to_vec_pretty(&legacy).unwrap();
    crate::profile::save_atomic(&path, &bytes).unwrap();
    let mut library = Library::open(path.clone()).unwrap();
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let paths: Vec<_> = library
        .folders()
        .iter()
        .map(|folder| folder.path.as_str())
        .collect();
    assert_eq!(paths, ["Operations", "Operations/Servers"]);
    let snippet = library.get(id).unwrap();
    assert_eq!(snippet.metadata().folder_path, "Operations\\ ./Servers.");
    let server = library.folder_id(snippet).unwrap();
    assert_eq!(library.snippets_in_folder(Some(server), false), [id]);
    assert_eq!(Library::open(path.clone()).unwrap().folders()[1].id, server);

    library.remove(id).unwrap();
    let reopened = Library::open(path.clone()).unwrap();
    assert_eq!(reopened.folder(server).unwrap().path, "Operations/Servers");
    assert!(reopened.snippets_in_folder(Some(server), true).is_empty());
    validate_backup(&fs::read(path).unwrap()).unwrap();
}

#[test]
fn rename_move_and_subtree_delete_preserve_ids_and_reject_stale_or_invalid_mutations() {
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let source = library.create_folder(None, "Ops").unwrap();
    let sibling = library.create_folder(None, "Ops-Legacy").unwrap();
    let child = library.create_folder(Some(source), "Servers").unwrap();
    let empty = library.create_folder(Some(child), "Empty").unwrap();
    let destination = library.create_folder(None, "Destination").unwrap();
    let collision = library.create_folder(Some(destination), "ops").unwrap();
    let snippet = save(&mut library, "Server", "oPs/Servers");
    let untouched = save(&mut library, "Unrelated", "Ops-Legacy");
    let mut stale = library.clone();
    let before = fs::read(profile.path()).unwrap();
    assert!(library.move_folder(source, Some(child)).is_err());
    assert!(library.move_folder(source, Some(destination)).is_err());
    assert!(library.rename_folder(source, "OPS-legacy").is_err());
    assert!(library.create_folder(Some(source), ".").is_err());
    assert!(
        library
            .move_snippets_to_folder(&[snippet, SnippetId::new().unwrap()], None)
            .is_err()
    );
    assert_eq!(fs::read(profile.path()).unwrap(), before);

    assert_eq!(library.snippets_in_folder(Some(source), true), [snippet]);
    library.rename_folder(collision, "Previous").unwrap();
    library.move_folder(source, Some(destination)).unwrap();
    library.rename_folder(source, "oPS").unwrap();
    assert!(stale.remove_folder(source, true).is_err());
    let mut library = Library::open(profile.path()).unwrap();
    assert_eq!(
        library.folder(empty).unwrap().path,
        "Destination/oPS/Servers/Empty"
    );
    assert_eq!(
        library.folder(child).unwrap().path,
        "Destination/oPS/Servers"
    );
    let saved = library.get(snippet).unwrap();
    assert_eq!(library.folder_id(saved), Some(child));
    assert_eq!(saved.metadata().file_name.as_deref(), Some("script.sh"));
    assert_eq!(saved.category(), "ops");
    assert_eq!(saved.content(), "printf hello\n");

    assert_eq!(library.remove_folder(source, true).unwrap(), [snippet]);
    assert!(library.folder(child).is_none());
    assert!(library.folder(empty).is_none());
    assert!(library.get(snippet).is_none());
    assert!(library.get(untouched).is_some());
    assert_eq!(
        library.snippets_in_folder(Some(sibling), false),
        [untouched]
    );
}

#[test]
fn keep_contents_promotes_children_and_direct_snippets_with_collision_suffixes() {
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let parent = library.create_folder(None, "Parent").unwrap();
    let removed = library.create_folder(Some(parent), "Group").unwrap();
    let promoted = library.create_folder(Some(removed), "Build").unwrap();
    let nested = library.create_folder(Some(promoted), "Nested").unwrap();
    let conflict = library.create_folder(Some(parent), "BUILD").unwrap();
    let second_conflict = library.create_folder(Some(parent), "build-2").unwrap();
    let direct = save(&mut library, "Direct", "Parent/Group");
    let inner = save(&mut library, "Inner", "Parent/Group/Build/Nested");

    assert!(library.remove_folder(removed, false).unwrap().is_empty());
    let mut library = Library::open(profile.path()).unwrap();
    assert!(library.folder(removed).is_none());
    assert_eq!(library.folder(conflict).unwrap().path, "Parent/BUILD");
    assert_eq!(
        library.folder(second_conflict).unwrap().path,
        "Parent/build-2"
    );
    assert_eq!(library.folder(promoted).unwrap().path, "Parent/Build-3");
    assert_eq!(
        library.folder(nested).unwrap().path,
        "Parent/Build-3/Nested"
    );
    assert_eq!(library.snippets_in_folder(Some(parent), false), [direct]);
    assert_eq!(
        library.snippets_in_folder(Some(parent), true),
        [direct, inner]
    );

    assert!(
        library
            .move_snippets_to_folder(&[inner, direct, inner], None)
            .unwrap()
    );
    assert!(!library.move_snippets_to_folder(&[direct], None).unwrap());
    assert_eq!(library.snippets_in_folder(None, false), [direct, inner]);
    let reopened = Library::open(profile.path()).unwrap();
    assert!(reopened.folder(nested).is_some());
}

#[test]
fn reusing_a_renamed_legacy_path_gets_a_different_stable_id() {
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let original_snippet = save(&mut library, "Original", "Ops");
    let original = library
        .folder_id(library.get(original_snippet).unwrap())
        .unwrap();
    library.rename_folder(original, "Archive").unwrap();
    let replacement_snippet = save(&mut library, "Replacement", "Ops/Servers");
    let replacement = library
        .folders()
        .iter()
        .find(|folder| folder.path == "Ops")
        .unwrap()
        .id;
    assert_ne!(replacement, original);
    assert!(library.remove_folder(original, true).is_ok());

    let library = Library::open(profile.path()).unwrap();
    assert_eq!(library.folder(replacement).unwrap().path, "Ops");
    assert_eq!(
        library.snippets_in_folder(Some(replacement), true),
        [replacement_snippet]
    );
}

#[test]
fn existing_out_of_range_paths_remain_accessible_as_snippets() {
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let path = "d/".repeat(100) + "folder";
    let id = save(&mut library, "Legacy", &path);
    let library = Library::open(profile.path()).unwrap();
    assert!(library.folders().is_empty());
    assert_eq!(library.snippets_in_folder(None, false), [id]);
    assert_eq!(library.get(id).unwrap().metadata().folder_path, path);
}
