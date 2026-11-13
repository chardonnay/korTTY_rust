use std::fs;
use std::time::{Duration, Instant};

use super::*;
use crate::session_journal::{Capture, Metadata, Status};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("kortty-journal-delete-{}", nonce().unwrap()));
        fs::create_dir(&base).unwrap();
        Self(base)
    }

    fn root(&self) -> PathBuf {
        self.0.join("journals")
    }

    fn journal(&self, name: impl AsRef<Path>, id: &str, metadata: &str) -> PathBuf {
        let directory = self.root().join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(DOCUMENT), xml(id, metadata)).unwrap();
        directory
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn xml(id: &str, metadata: &str) -> String {
    format!(
        "<session-journal id=\"{id}\" formatVersion=\"1\"><meta>{metadata}</meta><entries/></session-journal>"
    )
}

fn identity(root: &Path, directory: &Path) -> (Arc<Directory>, DeletionIdentity) {
    let root = Arc::new(Directory::open(root, false).unwrap());
    let source = LocalSource::open(&directory.join(DOCUMENT)).unwrap();
    let identity = DeletionIdentity::capture(&root, &source).unwrap();
    (root, identity)
}

fn prepare(root: &Path, directory: &Path, id: &str) -> Deletion {
    let (opened, identity) = identity(root, directory);
    Deletion::prepare(opened, root, directory, id, &identity).unwrap()
}

#[test]
fn journal_deletion_removes_pinned_journal_and_nested_data() {
    let fixture = Fixture::new();
    let directory = fixture.journal("pinned journal", "pinned", "<pinned>true</pinned>");
    let neighbor = fixture.journal("neighbor", "neighbor", "<title>Keep</title>");
    fs::create_dir_all(directory.join("screenshots/older")).unwrap();
    fs::write(directory.join("screenshots/older/shot.png"), b"old shot").unwrap();
    fs::write(directory.join("session-log.json"), b"capture").unwrap();
    let plan = prepare(&fixture.root(), &directory, "pinned");
    assert!(!directory.join(LOCK).exists());
    plan.delete().unwrap();
    assert!(!directory.exists());
    assert_eq!(
        fs::read_to_string(neighbor.join(DOCUMENT)).unwrap(),
        xml("neighbor", "<title>Keep</title>")
    );
    assert_eq!(fs::read_dir(fixture.root()).unwrap().count(), 1);
}

#[test]
fn journal_deletion_refuses_wrong_ids_and_non_direct_children() {
    let fixture = Fixture::new();
    let direct = fixture.journal("direct", "same", "");
    let nested = fixture.journal("group/nested", "same", "");
    let outside = fixture.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join(DOCUMENT), xml("same", "")).unwrap();
    let root = fixture.root();
    let (opened, snapshot) = identity(&root, &direct);
    assert!(
        Deletion::prepare(Arc::clone(&opened), &root, &direct, "different", &snapshot).is_err()
    );
    for directory in [&root, &nested, &outside] {
        let (_, snapshot) = if directory == &root {
            (Arc::clone(&opened), snapshot.clone())
        } else {
            identity(&root, directory)
        };
        assert!(
            Deletion::prepare(Arc::clone(&opened), &root, directory, "same", &snapshot).is_err()
        );
        assert!(directory.is_dir());
    }
    assert!(!direct.join(LOCK).exists());
}

#[test]
fn journal_deletion_rejects_same_id_directory_and_document_replacements_before_prepare() {
    let fixture = Fixture::new();
    let root = fixture.root();
    let directory = fixture.journal("journal", "same", "<title>Original</title>");
    let (opened, snapshot) = identity(&root, &directory);
    let original = fixture.0.join("moved-journal");
    fs::rename(&directory, &original).unwrap();
    fixture.journal("journal", "same", "<title>Replacement</title>");
    assert!(Deletion::prepare(Arc::clone(&opened), &root, &directory, "same", &snapshot).is_err());
    assert!(!directory.join(LOCK).exists());
    assert!(original.join(DOCUMENT).is_file());
    let (_, snapshot) = identity(&root, &directory);
    fs::rename(directory.join(DOCUMENT), directory.join("before.xml")).unwrap();
    fs::write(
        directory.join(DOCUMENT),
        xml("same", "<title>Replacement</title>"),
    )
    .unwrap();
    assert!(Deletion::prepare(opened, &root, &directory, "same", &snapshot).is_err());
    assert!(directory.join("before.xml").is_file());
    assert!(!directory.join(LOCK).exists());
}

#[test]
fn journal_deletion_rejects_root_and_document_changes_after_confirmation_preparation() {
    let fixture = Fixture::new();
    let root = fixture.root();
    let directory = fixture.journal("journal", "same", "");
    let plan = prepare(&root, &directory, "same");
    fs::rename(directory.join(DOCUMENT), directory.join("previous.xml")).unwrap();
    fs::write(directory.join(DOCUMENT), xml("same", "")).unwrap();
    assert!(plan.delete().is_err());
    assert!(!directory.join(LOCK).exists());

    let plan = prepare(&root, &directory, "same");
    let original = fixture.0.join("moved-root");
    fs::rename(&root, &original).unwrap();
    fixture.journal("journal", "same", "");
    assert!(plan.delete().is_err());
    assert!(original.join("journal/previous.xml").is_file());
    assert!(directory.join(DOCUMENT).is_file());
    assert!(!directory.join(LOCK).exists());
}

#[test]
fn journal_deletion_excludes_a_real_writer_and_requires_its_final_document() {
    let fixture = Fixture::new();
    let root = fixture.root();
    let capture = Capture::start(root.clone(), Metadata::default()).unwrap();
    let plan = prepare(&root, capture.directory(), capture.id());
    assert_eq!(plan.delete().unwrap_err().kind(), io::ErrorKind::WouldBlock);
    assert_eq!(capture.status(), Status::Recording);
    assert!(capture.directory().join(DOCUMENT).is_file());
    let done = capture.finish();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done.is_closed() {
        assert!(Instant::now() < deadline, "journal writer did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(capture.status(), Status::Complete);
    assert!(plan.delete().is_err());
    prepare(&root, capture.directory(), capture.id())
        .delete()
        .unwrap();
    assert!(!capture.directory().exists());
}

#[test]
fn journal_deletion_refuses_a_hardlinked_document() {
    let fixture = Fixture::new();
    let root = fixture.root();
    let directory = fixture.journal("journal", "linked", "");
    let other = fixture.0.join("other.xml");
    fs::hard_link(directory.join(DOCUMENT), &other).unwrap();
    let (opened, snapshot) = identity(&root, &directory);
    assert!(Deletion::prepare(opened, &root, &directory, "linked", &snapshot).is_err());
    assert_eq!(fs::read_to_string(other).unwrap(), xml("linked", ""));
    assert!(!directory.join(LOCK).exists());
}

#[test]
fn journal_deletion_bounds_the_tree_before_removing_entries() {
    let fixture = Fixture::new();
    let root = fixture.root();
    let directory = fixture.journal("deep", "deep", "");
    let deep = (0..=MAX_DEPTH).fold(directory.clone(), |path, _| path.join("n"));
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("keep"), b"deep data").unwrap();
    let plan = prepare(&root, &directory, "deep");
    assert!(inventory(&plan.current_directory().unwrap(), MAX_DEPTH, &mut 1).is_err());
    let error = plan.delete().unwrap_err();
    assert!(error.to_string().contains("nesting"), "{error}");
    assert!(deep.join("keep").is_file(), "{error}");
    assert_eq!(fs::read(deep.join("keep")).unwrap(), b"deep data");
    assert!(directory.join(DOCUMENT).is_file());
    assert_eq!(fs::read_dir(root).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn journal_deletion_unlinks_nested_symlinks_without_following_them() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let root = fixture.root();
    let directory = fixture.journal("journal", "links", "");
    let outside = fixture.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"outside data").unwrap();
    symlink(&outside, directory.join("folder-link")).unwrap();
    symlink(outside.join("keep"), directory.join("file-link")).unwrap();
    symlink(outside.join("missing"), directory.join("dangling-link")).unwrap();
    let (opened, snapshot) = identity(&root, &directory);
    let alias = root.join("journal-alias");
    symlink(&directory, &alias).unwrap();
    assert!(Deletion::prepare(opened, &root, &alias, "links", &snapshot).is_err());
    prepare(&root, &directory, "links").delete().unwrap();
    assert!(!directory.exists());
    assert!(fs::symlink_metadata(alias).unwrap().is_symlink());
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"outside data");
}

#[cfg(unix)]
#[test]
fn journal_deletion_restores_a_replacement_claimed_after_the_initial_check() {
    let fixture = Fixture::new();
    let root = fixture.root();
    let directory = fixture.journal("journal", "same", "<title>Original</title>");
    let plan = prepare(&root, &directory, "same");
    let lock = lock_metadata_in(&directory, plan.current_directory().unwrap()).unwrap();
    let stage = Stage::new(&plan.root, &root).unwrap();
    let old = fixture.0.join("original");
    fs::rename(&directory, &old).unwrap();
    fixture.journal("journal", "same", "<title>Replacement</title>");
    let lock = plan.claim(&stage, lock).unwrap();
    assert!(plan.delete_claimed(&stage, lock).is_err());
    drop(stage);
    assert_eq!(
        fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
        xml("same", "<title>Replacement</title>")
    );
    assert_eq!(
        fs::read_to_string(old.join(DOCUMENT)).unwrap(),
        xml("same", "<title>Original</title>")
    );
    assert!(!directory.join(LOCK).exists());
    assert_eq!(fs::read_dir(root).unwrap().count(), 1);
}

#[test]
fn journal_deletion_preserves_a_failed_restore_without_overwriting_a_new_entry() {
    let fixture = Fixture::new();
    let root = fixture.root();
    let directory = fixture.journal("journal", "same", "<title>Original</title>");
    let plan = prepare(&root, &directory, "same");
    let lock = lock_metadata_in(&directory, plan.current_directory().unwrap()).unwrap();
    let stage = Stage::new(&plan.root, &root).unwrap();
    let lock = plan.claim(&stage, lock).unwrap();
    let recovery = stage.path.join(&plan.name);
    let changed = xml("same", "<title>Staged changed document</title>");
    fs::write(recovery.join(DOCUMENT), &changed).unwrap();
    fixture.journal("journal", "new", "<title>New entry</title>");
    let error = plan.delete_claimed(&stage, lock).unwrap_err().to_string();
    assert!(error.contains(recovery.to_str().unwrap()));
    drop(stage);
    assert_eq!(
        fs::read_to_string(recovery.join(DOCUMENT)).unwrap(),
        changed
    );
    assert_eq!(
        fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
        xml("new", "<title>New entry</title>")
    );
}

#[test]
fn journal_deletion_refuses_a_nested_directory_replaced_after_inventory() {
    let fixture = Fixture::new();
    let root = fixture.root();
    let directory = fixture.journal("journal", "same", "");
    let nested = directory.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("original"), b"original").unwrap();
    let plan = prepare(&root, &directory, "same");
    let opened = plan.current_directory().unwrap();
    let mut entries = inventory(&opened, MAX_DEPTH, &mut 3).unwrap();
    entries.sort_by_key(|entry| entry.name == OsStr::new(DOCUMENT));
    let original = fixture.0.join("detached");
    fs::rename(&nested, &original).unwrap();
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("foreign"), b"foreign").unwrap();
    assert!(purge(&opened, &entries).is_err());
    assert_eq!(fs::read(nested.join("foreign")).unwrap(), b"foreign");
    assert_eq!(fs::read(original.join("original")).unwrap(), b"original");
    assert!(directory.join(DOCUMENT).is_file());
}

#[cfg(windows)]
#[test]
fn journal_deletion_windows_claim_reacquires_the_staged_writer_lock() {
    let fixture = Fixture::new();
    let root = fixture.root();
    let directory = fixture.journal("journal", "same", "");
    let plan = prepare(&root, &directory, "same");
    let original_lock = lock_metadata_in(&directory, plan.current_directory().unwrap()).unwrap();
    let stage = Stage::new(&plan.root, &root).unwrap();
    let staged_lock = plan.claim(&stage, original_lock).unwrap();
    let staged_path = stage.path.join(&plan.name);
    assert!(!directory.exists());
    let concurrent = lock_metadata_in(
        &staged_path,
        plan.require_directory(&stage.directory).unwrap(),
    );
    assert_eq!(concurrent.err().unwrap().kind(), io::ErrorKind::WouldBlock);
    plan.delete_claimed(&stage, staged_lock).unwrap();
    drop(stage);
    assert!(!staged_path.exists());
    assert_eq!(fs::read_dir(root).unwrap().count(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn journal_deletion_refuses_a_real_filesystem_boundary() {
    // This is a read-only probe of a standard existing mount. No mount or deletion is attempted.
    let root = Directory::open(Path::new("/"), false).unwrap();
    let Ok(proc) = Directory::open(Path::new("/proc"), false) else {
        return;
    };
    if root.identity().unwrap().0 != proc.identity().unwrap().0 {
        assert!(root.deletion_child(OsStr::new("proc")).is_err());
        assert!(root.deletion_entry(OsStr::new("proc")).is_err());
    }
}
