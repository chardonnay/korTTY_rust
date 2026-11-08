use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Choice, Entry, Layout, prepare};
use crate::snippets::tests::Profile;

fn scratch() -> (Profile, PathBuf) {
    let directory = Profile::new();
    let root = directory.0.canonicalize().unwrap();
    (directory, root)
}

fn layout(directories: &[&str], paths: &[&str]) -> Layout {
    Layout {
        directories: directories.iter().map(ToString::to_string).collect(),
        entries: paths
            .iter()
            .map(|&path| Entry {
                path: path.into(),
                content: "#!/bin/sh\nprintf 'ok\\n'\n".into(),
                mode: 0o755,
            })
            .collect(),
    }
}

fn no_staging(path: &Path) {
    assert!(fs::read_dir(path).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".kortty-part")
    }));
}

#[test]
fn copies_empty_folders_and_skips_existing_files() {
    let (_scratch, root) = scratch();
    fs::write(root.join("existing"), "keep").unwrap();
    let layout = layout(&["tools", "tools/empty"], &["existing", "tools/run.sh"]);
    let cancelled = AtomicBool::new(false);
    let plan = prepare(root.clone(), &layout, &cancelled).unwrap();
    assert_eq!(plan.conflicts(), ["existing"]);
    let mut progress = Vec::new();
    let result = plan
        .run(&layout, Choice::Skip, &cancelled, &mut |event| {
            progress.push((event.done, event.total, event.path));
        })
        .unwrap();
    assert_eq!(
        (result.written, result.skipped, result.cancelled),
        (1, 1, false)
    );
    assert_eq!(progress.last(), Some(&(2, 2, String::new())));
    assert_eq!(fs::read(root.join("existing")).unwrap(), b"keep");
    let output = root.join("tools/run.sh");
    assert_eq!(
        fs::read_to_string(&output).unwrap(),
        layout.entries[1].content
    );
    assert!(root.join("tools/empty").is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            output.metadata().unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    no_staging(&root.join("tools"));
}

#[test]
fn refuses_new_conflicts_after_review() {
    let (_scratch, root) = scratch();
    let layout = layout(&[], &["run.sh"]);
    let cancelled = AtomicBool::new(false);
    let plan = prepare(root.clone(), &layout, &cancelled).unwrap();
    fs::write(root.join("run.sh"), "new file").unwrap();
    assert!(
        plan.run(&layout, Choice::Copy, &cancelled, &mut |_| {})
            .is_err()
    );
    assert_eq!(fs::read(root.join("run.sh")).unwrap(), b"new file");
    no_staging(&root);
}

#[cfg(unix)]
#[test]
fn overwrites_only_reviewed_regular_files() {
    let (_scratch, root) = scratch();
    let path = root.join("run.sh");
    fs::write(&path, "original").unwrap();
    let layout = layout(&[], &["run.sh"]);
    let cancelled = AtomicBool::new(false);
    let plan = prepare(root.clone(), &layout, &cancelled).unwrap();
    assert!(plan.can_overwrite());
    fs::write(&path, "changed since review").unwrap();
    assert!(
        plan.run(&layout, Choice::Overwrite, &cancelled, &mut |_| {})
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), b"changed since review");

    let plan = prepare(root.clone(), &layout, &cancelled).unwrap();
    let result = plan
        .run(&layout, Choice::Overwrite, &cancelled, &mut |_| {})
        .unwrap();
    assert_eq!(result.written, 1);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        layout.entries[0].content
    );
    no_staging(&root);
}

#[cfg(unix)]
#[test]
fn refuses_link_directories_and_link_overwrite() {
    use std::os::unix::fs::symlink;

    let (_scratch, root) = scratch();
    let (_outside, outside) = scratch();
    let existing = outside.join("existing");
    fs::write(&existing, "untouched").unwrap();
    symlink(&outside, root.join("linked")).unwrap();
    symlink(&existing, root.join("linked-file")).unwrap();
    let cancelled = AtomicBool::new(false);
    assert!(prepare(root.clone(), &layout(&["linked"], &[]), &cancelled).is_err());

    let layout = layout(&[], &["linked-file"]);
    let plan = prepare(root, &layout, &cancelled).unwrap();
    assert_eq!(plan.conflicts(), ["linked-file"]);
    assert!(!plan.can_overwrite());
    assert!(
        plan.run(&layout, Choice::Overwrite, &cancelled, &mut |_| {})
            .is_err()
    );
    assert_eq!(fs::read(&existing).unwrap(), b"untouched");
}

#[test]
fn cancellation_preserves_completed_files_and_stops_future_writes() {
    let (_scratch, root) = scratch();
    let layout = layout(&[], &["first", "second"]);
    let cancelled = AtomicBool::new(false);
    let plan = prepare(root.clone(), &layout, &cancelled).unwrap();
    let result = plan
        .run(&layout, Choice::Copy, &cancelled, &mut |event| {
            if event.path == "second" {
                cancelled.store(true, Ordering::Relaxed);
            }
        })
        .unwrap();
    assert_eq!(
        (result.written, result.skipped, result.cancelled),
        (1, 0, true)
    );
    assert!(root.join("first").is_file());
    assert!(!root.join("second").exists());
    no_staging(&root);
}
