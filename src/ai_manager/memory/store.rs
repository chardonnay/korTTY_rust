use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

use crate::private_files::{Directory, FileStamp, require_single_link};

use super::{Scope, format};

pub(in crate::ai_manager) const MAX_BYTES: usize = 64 * 1024;
pub(in crate::ai_manager) const MAX_ARCHIVE: usize = 4 * 1024 * 1024;
const ARCHIVE: &str = "archive.md";
const MUTATION_LOCK: &str = ".memory.lock";

#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct Snapshot {
    pub(super) text: String,
    stamp: Option<FileStamp>,
}

#[derive(Clone)]
pub(crate) struct ArchiveItem {
    pub(super) range: std::ops::Range<usize>,
    pub(super) entry: format::Entry,
}

pub(super) fn load(path: &Path, scope: &Scope) -> io::Result<Snapshot> {
    let Some(root) = optional(Directory::open(path, false))? else {
        return Ok(Snapshot::default());
    };
    let Some(connection) = optional(connection_dir(&root, scope, false))? else {
        return Ok(Snapshot::default());
    };
    read(
        connection.as_ref().unwrap_or(&root),
        &scope.name(),
        MAX_BYTES,
    )
}

fn connection_dir(root: &Directory, scope: &Scope, create: bool) -> io::Result<Option<Directory>> {
    if scope.id.is_some() {
        root.subdirectory("connections", create).map(Some)
    } else {
        Ok(None)
    }
}

pub(super) fn save(
    path: &Path,
    scope: &Scope,
    base: &Snapshot,
    next: &str,
) -> io::Result<Snapshot> {
    let root = Directory::open(path, true)?;
    let _lock = lock_mutations(&root)?;
    let connection = connection_dir(&root, scope, true)?;
    save_locked(
        &root,
        connection.as_ref().unwrap_or(&root),
        scope,
        base,
        next,
    )
}

fn lock_mutations(root: &Directory) -> io::Result<File> {
    let lock = root.open_lock(MUTATION_LOCK)?;
    require_single_link(&lock)?;
    lock.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "Another window is saving memory. Try again.",
        ),
        std::fs::TryLockError::Error(error) => error,
    })?;
    Ok(lock)
}

fn save_locked(
    root: &Directory,
    dir: &Directory,
    scope: &Scope,
    base: &Snapshot,
    next: &str,
) -> io::Result<Snapshot> {
    validate(next, &base.text, MAX_BYTES)?;
    let name = scope.name();
    verify(dir, &name, base)?;
    if base.text == next {
        return Ok(base.clone());
    }
    if base.stamp.is_some() {
        snapshot(root, scope, &base.text)?;
    }
    publish(dir, &name, base, next)?;
    read(dir, &name, MAX_BYTES)
}

fn snapshot(root: &Directory, scope: &Scope, text: &str) -> io::Result<()> {
    let history = root.subdirectory("history", true)?;
    let date = jiff::Timestamp::now()
        .strftime("%Y%m%d-%H%M%S-%3f")
        .to_string();
    let key = scope.key();
    for counter in 1..=100 {
        let name = if counter == 1 {
            format!("{key}-{date}.md")
        } else {
            format!("{key}-{date}-{counter}.md")
        };
        match history.publish_copy(&name, text.as_bytes()) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            other => return other,
        }
    }
    Err(io::Error::other(
        "Could not allocate a memory history name. Try again.",
    ))
}

pub(super) fn history(path: &Path, scope: &Scope) -> io::Result<Vec<String>> {
    let Some(root) = optional(Directory::open(path, false))? else {
        return Ok(Vec::new());
    };
    let Some(history) = optional(root.subdirectory("history", false))? else {
        return Ok(Vec::new());
    };
    let mut names: Vec<_> = history
        .names(10_000)?
        .into_iter()
        .filter_map(|name| name.into_string().ok())
        .filter(|name| history_name(name, scope))
        .collect();
    names.sort_unstable_by(|a, b| history_order(b, scope).cmp(&history_order(a, scope)));
    names.truncate(100);
    Ok(names)
}

pub(super) fn history_text(path: &Path, scope: &Scope, name: &str) -> io::Result<String> {
    if !history_name(name, scope) {
        return Err(io::Error::other("Invalid history entry"));
    }
    let history = Directory::open(path, false)?.subdirectory("history", false)?;
    let snapshot = read(&history, name, MAX_BYTES)?;
    if snapshot.stamp.is_none() {
        return Err(io::Error::other("The history entry is missing"));
    }
    Ok(snapshot.text)
}

pub(in crate::ai_manager) fn history_name(name: &str, scope: &Scope) -> bool {
    name.strip_prefix(&scope.key())
        .and_then(|tail| tail.strip_prefix('-'))
        .and_then(|tail| tail.strip_suffix(".md"))
        .is_some_and(|stamp| {
            let groups: Vec<_> = stamp.split('-').collect();
            matches!(groups.len(), 3 | 4)
                && groups[0].len() == 8
                && groups[1].len() == 6
                && groups[2].len() == 3
                && groups
                    .iter()
                    .all(|group| !group.is_empty() && group.bytes().all(|c| c.is_ascii_digit()))
        })
}

fn history_order<'a>(name: &'a str, scope: &Scope) -> (&'a str, u32) {
    let stamp = &name[scope.key().len() + 1..name.len() - 3];
    stamp
        .split_at_checked(19)
        .map_or((stamp, 1), |(date, counter)| {
            (date, counter.trim_start_matches('-').parse().unwrap_or(1))
        })
}

pub(super) fn archive(path: &Path, scope: &Scope) -> io::Result<(Snapshot, Vec<ArchiveItem>)> {
    let Some(root) = optional(Directory::open(path, false))? else {
        return Ok((Snapshot::default(), Vec::new()));
    };
    let snapshot = read(&root, ARCHIVE, MAX_ARCHIVE)?;
    let items = archive_items(&snapshot.text, scope);
    Ok((snapshot, items))
}

fn archive_items(text: &str, scope: &Scope) -> Vec<ArchiveItem> {
    let marker = marker(scope);
    let mut found = Vec::new();
    for entry in format::entries(text) {
        let before = &text[..entry.range.start];
        let last = before.lines().next_back().unwrap_or("");
        let marked = last.starts_with("<!-- kortty:archived scope=");
        if last == marker || !marked && scope.id.is_none() {
            let start = if marked {
                before.trim_end_matches(['\r', '\n']).len() - last.len()
            } else {
                entry.range.start
            };
            found.push(ArchiveItem {
                range: start..entry.range.end,
                entry,
            });
        }
    }
    found
}

fn marker(scope: &Scope) -> String {
    let token = scope.id.map_or_else(
        || "global".into(),
        |id| format!("connection:{}", URL_SAFE_NO_PAD.encode(id.to_string())),
    );
    format!("<!-- kortty:archived scope={token} -->")
}

pub(super) fn move_to_archive(
    path: &Path,
    scope: &Scope,
    base: &Snapshot,
    selected: &[format::Entry],
) -> io::Result<Snapshot> {
    let next = without_entries(&base.text, selected);
    move_to_archive_with_text(path, scope, base, selected, &next)
}

pub(super) fn move_to_archive_with_text(
    path: &Path,
    scope: &Scope,
    base: &Snapshot,
    selected: &[format::Entry],
    next: &str,
) -> io::Result<Snapshot> {
    validate(next, &base.text, MAX_BYTES)?;
    let root = Directory::open(path, false)?;
    let _lock = lock_mutations(&root)?;
    let connection = connection_dir(&root, scope, false)?;
    let dir = connection.as_ref().unwrap_or(&root);
    verify(dir, &scope.name(), base)?;
    let old = read(&root, ARCHIVE, MAX_ARCHIVE)?;
    let items = archive_items(&old.text, scope);
    let mut archived: HashSet<&str> = items
        .iter()
        .map(|item| &old.text[item.entry.range.clone()])
        .collect();
    let marker = marker(scope);
    let mut added = String::new();
    for item in selected {
        let fence = &base.text[item.range.clone()];
        // A later archive entry can add a separating LF after a fence that was
        // previously at EOF. Preserve distinct LF and CRLF fences otherwise.
        if !fence.ends_with('\n') && archived.contains(format!("{fence}\n").as_str()) {
            continue;
        }
        if archived.insert(fence) {
            if !added.is_empty() {
                added.push('\n');
            }
            added.push_str(&marker);
            added.push('\n');
            added.push_str(fence);
        }
    }
    if !added.is_empty() {
        let updated = append(&old.text, &added);
        validate(&updated, &old.text, MAX_ARCHIVE)?;
        publish(&root, ARCHIVE, &old, &updated)?;
    }
    // Publish the archive first so an interrupted move cannot lose the entry.
    save_locked(&root, dir, scope, base, next)
}

pub(super) fn without_entries(document: &str, selected: &[format::Entry]) -> String {
    let mut ranges: Vec<_> = selected.iter().map(|item| item.range.clone()).collect();
    ranges.sort_unstable_by_key(|range| range.start);
    let mut remaining = document.to_owned();
    for range in ranges.into_iter().rev() {
        remaining.replace_range(range, "");
    }
    remaining
}

pub(super) fn change_archive(
    path: &Path,
    scope: &Scope,
    base: &Snapshot,
    archive: &Snapshot,
    item: &ArchiveItem,
    restore: bool,
    budget: u16,
) -> io::Result<Snapshot> {
    let root = Directory::open(path, false)?;
    let _lock = lock_mutations(&root)?;
    verify(&root, ARCHIVE, archive)?;
    let mut remaining = archive.text.clone();
    remaining.replace_range(item.range.clone(), "");
    validate(&remaining, &archive.text, MAX_ARCHIVE)?;
    let snapshot = if restore {
        let connection = connection_dir(&root, scope, true)?;
        let dir = connection.as_ref().unwrap_or(&root);
        let fence = &archive.text[item.entry.range.clone()];
        if base.text.contains(fence) {
            // A previous restore may have published the document before archive removal failed.
            verify(dir, &scope.name(), base)?;
            base.clone()
        } else {
            if format::entries(&base.text)
                .iter()
                .any(|entry| entry.id == item.entry.id)
            {
                return Err(io::Error::other(
                    "A different entry already uses this archived entry's ID.",
                ));
            }
            let next = append(&base.text, fence);
            if format::estimate(&next) > usize::from(budget) * 2 {
                return Err(io::Error::other(
                    "The restored memory would exceed twice its token budget",
                ));
            }
            save_locked(&root, dir, scope, base, &next)?
        }
    } else {
        base.clone()
    };
    publish(&root, ARCHIVE, archive, &remaining)?;
    Ok(snapshot)
}

pub(super) fn append(original: &str, block: &str) -> String {
    let separator = if original.is_empty() || original.ends_with("\n\n") {
        ""
    } else if original.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    format!("{original}{separator}{block}")
}

pub(in crate::ai_manager) fn validate(next: &str, previous: &str, limit: usize) -> io::Result<()> {
    if next.len() > limit {
        return Err(io::Error::other(
            "The memory file would exceed its size limit",
        ));
    }
    if let Some(line) = format::secret_line(next, previous) {
        return Err(io::Error::other(format!(
            "Possible credential on line {line}. Remove it before saving."
        )));
    }
    Ok(())
}

pub(super) fn read(dir: &Directory, name: &str, max: usize) -> io::Result<Snapshot> {
    let Some(mut file) = optional(dir.open_private_file(name))? else {
        return Ok(Snapshot::default());
    };
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let mut text = String::new();
    Read::by_ref(&mut file)
        .take(max as u64 + 1)
        .read_to_string(&mut text)?;
    stamp.require_unchanged(&file)?;
    if text.len() > max {
        return Err(io::Error::other("The memory file exceeds its size limit"));
    }
    Ok(Snapshot {
        text,
        stamp: Some(stamp),
    })
}

fn verify(dir: &Directory, name: &str, previous: &Snapshot) -> io::Result<()> {
    let current = read(dir, name, previous.text.len().max(MAX_BYTES))?;
    if current.stamp != previous.stamp || current.text != previous.text {
        return Err(conflict());
    }
    Ok(())
}

pub(super) fn publish(
    dir: &Directory,
    name: &str,
    previous: &Snapshot,
    text: &str,
) -> io::Result<()> {
    if previous.stamp.is_none() {
        dir.publish_copy(name, text.as_bytes())?;
    } else {
        dir.replace_with(
            name,
            |file| file.write_all(text.as_bytes()),
            || verify(dir, name, previous),
        )?;
    }
    dir.sync()
}

pub(super) fn optional<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn conflict() -> io::Error {
    io::Error::other("The memory file changed on disk. Reload it before saving.")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new() -> io::Result<Self> {
            let path = std::env::temp_dir()
                .canonicalize()?
                .join(format!("kortty-memory-{}", crate::private_files::nonce()?));
            Ok(Self(path))
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn stale_memory_cannot_replace_newer_text_and_history_uses_the_selected_scope() -> io::Result<()>
    {
        let temp = TestDirectory::new()?;
        let global = Scope::global();
        let server = Scope::connection(42, "test connection".into());
        let initial = load(&temp.0, &global)?;
        let first = save(&temp.0, &global, &initial, "First note\n")?;
        assert!(save(&temp.0, &global, &initial, "Stale edit\n").is_err());
        let second = save(&temp.0, &global, &first, "Second note\n")?;
        assert_eq!(second.text, "Second note\n");
        assert!(save(&temp.0, &global, &second, "password=secret-value123\n").is_err());
        let versions = history(&temp.0, &global)?;
        assert_eq!(versions.len(), 1);
        assert_eq!(history_text(&temp.0, &global, &versions[0])?, first.text);
        assert!(history_text(&temp.0, &server, &versions[0]).is_err());
        assert!(load(&temp.0, &server)?.text.is_empty());
        Ok(())
    }

    #[test]
    fn managed_entries_archive_and_restore_without_changing_user_text() -> io::Result<()> {
        let temp = TestDirectory::new()?;
        let scope = Scope::connection(7, "test connection".into());
        let owned = format::Entry {
            range: 0..0,
            id: "m1".into(),
            origin: "kortty".into(),
            created: "2026-11-07".into(),
            source: "chat".into(),
            body: String::new(),
        }
        .rewrite("Print a summary before closing the connection.")
        .unwrap();
        let user = "My text\n\n";
        let snapshot = save(
            &temp.0,
            &scope,
            &Snapshot::default(),
            &format!("{user}{owned}"),
        )?;
        let entries = format::entries(&snapshot.text);
        let remaining = move_to_archive(&temp.0, &scope, &snapshot, &entries)?;
        assert_eq!(remaining.text, user);
        assert!(archive(&temp.0, &Scope::global())?.1.is_empty());
        let (archive_version, items) = archive(&temp.0, &scope)?;
        assert_eq!(items.len(), 1);
        let restored = change_archive(
            &temp.0,
            &scope,
            &remaining,
            &archive_version,
            &items[0],
            true,
            1500,
        )?;
        assert_eq!(restored.text, snapshot.text);
        assert!(archive(&temp.0, &scope)?.1.is_empty());
        Ok(())
    }

    #[test]
    fn reviewed_memory_writes_recheck_content_and_share_one_mutation_lock() -> io::Result<()> {
        use std::fs::{FileTimes, OpenOptions};

        let temp = TestDirectory::new()?;
        let global = Scope::global();
        let connection = Scope::connection(7, "test connection".into());
        let first = save(&temp.0, &global, &Snapshot::default(), "Old user note\n")?;
        let root = Directory::open(&temp.0, false)?;
        let lock = lock_mutations(&root)?;
        let error = save(
            &temp.0,
            &connection,
            &Snapshot::default(),
            "Connection note\n",
        )
        .err()
        .expect("the same mutation lock covers every scope");
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        drop(lock);

        let file_path = temp.0.join(global.name());
        let modified = std::fs::metadata(&file_path)?.modified()?;
        let mut file = OpenOptions::new().write(true).open(&file_path)?;
        file.write_all(b"New user note\n")?;
        file.set_times(FileTimes::new().set_modified(modified))?;
        drop(file);
        assert_eq!(std::fs::metadata(&file_path)?.modified()?, modified);

        assert!(save(&temp.0, &global, &first, "").is_err());
        assert_eq!(load(&temp.0, &global)?.text, "New user note\n");
        assert!(history(&temp.0, &global)?.is_empty());
        Ok(())
    }
}
