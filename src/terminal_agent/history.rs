//! Private history for prompts explicitly started through the agent composer.

use std::collections::HashSet;
use std::fs::TryLockError;
use std::io::{self, Read as _, Write as _};
use std::path::Path;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::private_files::{Directory, FileStamp, require_single_link};
use crate::snippet_analysis::model::now_millis;

use super::Mode;

pub(crate) const FOLDER: &str = "terminal-agent-history";
const FILE: &str = "prompts.json";
pub(crate) const BACKUP_PATH: &str = "terminal-agent-history/prompts.json";
pub(crate) const MAX_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const DEFAULT_LIMIT: usize = 20;
pub(crate) const MIN_LIMIT: usize = 5;
pub(crate) const MAX_LIMIT: usize = 100;

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Run,
    Ask,
    Plan,
    Imported,
}

impl From<Mode> for Kind {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Run => Self::Run,
            Mode::Ask => Self::Ask,
            Mode::Plan => Self::Plan,
        }
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub(crate) goal: String,
    pub(crate) kind: Kind,
    pub(crate) started_at: u64,
}

impl Entry {
    fn valid(&self) -> bool {
        self.started_at != 0
            && !self.goal.trim().is_empty()
            && self.goal.len() <= 8192
            && self
                .goal
                .chars()
                .all(crate::session_journal::is_display_character)
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    version: u8,
    pub(crate) limit: usize,
    pub(crate) entries: Vec<Entry>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            version: 1,
            limit: DEFAULT_LIMIT,
            entries: Vec::new(),
        }
    }
}

pub(crate) fn list(root: &Path, private: bool) -> io::Result<Snapshot> {
    if private {
        return Ok(Snapshot::default());
    }
    match Directory::open(&root.join(FOLDER), false) {
        Ok(directory) => Ok(read(&directory)?.0),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Snapshot::default()),
        Err(error) => Err(error),
    }
}

pub(crate) fn remember(root: &Path, goal: String, mode: Mode, private: bool) -> io::Result<()> {
    if private {
        return Ok(());
    }
    let entry = Entry {
        goal,
        kind: mode.into(),
        started_at: now_millis(),
    };
    if !entry.valid() {
        return Err(invalid("The prompt is invalid"));
    }
    update(root, |snapshot| {
        snapshot.entries.retain(|saved| saved.goal != entry.goal);
        snapshot.entries.insert(0, entry);
        snapshot.entries.truncate(snapshot.limit);
        Ok(())
    })
}

pub(crate) fn delete(root: &Path, expected: &Entry, private: bool) -> io::Result<()> {
    require_durable(private)?;
    update(root, |snapshot| {
        let entries = &mut snapshot.entries;
        let index = entries
            .iter()
            .position(|entry| entry == expected)
            .ok_or_else(|| invalid("The selected prompt changed or was removed"))?;
        entries.remove(index);
        Ok(())
    })
}

pub(crate) fn clear(root: &Path, expected: &Snapshot, private: bool) -> io::Result<()> {
    require_durable(private)?;
    update(root, |snapshot| {
        if &*snapshot != expected {
            return Err(invalid(
                "The prompt history changed. Review it again before deleting all prompts",
            ));
        }
        snapshot.entries.clear();
        Ok(())
    })
}

pub(crate) fn set_limit(
    root: &Path,
    expected: &Snapshot,
    limit: usize,
    private: bool,
) -> io::Result<()> {
    require_durable(private)?;
    if !(MIN_LIMIT..=MAX_LIMIT).contains(&limit) {
        return Err(invalid("Keep between 5 and 100 prompts"));
    }
    update(root, |snapshot| {
        if &*snapshot != expected {
            return Err(invalid(
                "The prompt history changed. Review it again before changing the limit",
            ));
        }
        snapshot.limit = limit;
        snapshot.entries.truncate(limit);
        Ok(())
    })
}

fn require_durable(private: bool) -> io::Result<()> {
    if private {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Private runs do not change prompt history",
        ))
    } else {
        Ok(())
    }
}

fn update(root: &Path, change: impl FnOnce(&mut Snapshot) -> io::Result<()>) -> io::Result<()> {
    let directory = Directory::open(&root.join(FOLDER), true)?;
    let lock = directory.open_lock(".prompts.lock")?;
    require_single_link(&lock)?;
    lock.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "Another window is changing prompt history",
        ),
        TryLockError::Error(error) => error,
    })?;
    let result = (|| {
        let (mut document, stamp) = read(&directory)?;
        change(&mut document)?;
        let bytes = Zeroizing::new(
            serde_json::to_vec(&document)
                .map_err(|_| invalid("Could not encode prompt history"))?,
        );
        if bytes.len() > MAX_BYTES {
            return Err(invalid("The prompt history exceeds its storage limit"));
        }
        match stamp {
            Some(stamp) => directory.replace_with(
                FILE,
                |file| file.write_all(&bytes),
                || stamp.require_unchanged(&directory.open_private_file(FILE)?),
            ),
            None => directory.publish_copy(FILE, &bytes),
        }
    })();
    // A concurrent fork can retain a copy of the descriptor until exec.
    let _ = lock.unlock();
    result
}

fn read(directory: &Directory) -> io::Result<(Snapshot, Option<FileStamp>)> {
    let file = match directory.open_private_file(FILE) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((Snapshot::default(), None));
        }
        Err(error) => return Err(error),
    };
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Zeroizing::new(Vec::new());
    (&file).take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid("The prompt history exceeds its storage limit"));
    }
    stamp.require_unchanged(&file)?;
    Ok((decode(&bytes)?, Some(stamp)))
}

fn decode(bytes: &[u8]) -> io::Result<Snapshot> {
    if bytes.len() > MAX_BYTES {
        return Err(invalid("The prompt history exceeds its storage limit"));
    }
    let document: Snapshot =
        serde_json::from_slice(bytes).map_err(|_| invalid("Invalid prompt history"))?;
    let mut prompts = HashSet::new();
    if document.version != 1
        || !(MIN_LIMIT..=MAX_LIMIT).contains(&document.limit)
        || document.entries.len() > document.limit
        || document
            .entries
            .iter()
            .any(|entry| !entry.valid() || !prompts.insert(entry.goal.as_str()))
    {
        return Err(invalid("Invalid prompt history entries"));
    }
    Ok(document)
}

pub(crate) fn validate_backup(bytes: &[u8]) -> io::Result<()> {
    decode(bytes).map(|_| ())
}

pub(crate) fn merge_java_backup(
    local: Option<&[u8]>,
    incoming: &[u8],
) -> io::Result<Option<Vec<u8>>> {
    let incoming = decode(incoming)?;
    let Some(local) = local else {
        return Ok(Some(serde_json::to_vec(&incoming)?));
    };
    let mut local = decode(local)?;
    let count = local.entries.len();
    for entry in incoming.entries {
        if local.entries.len() == local.limit {
            break;
        }
        if !local
            .entries
            .iter()
            .any(|current| current.goal == entry.goal)
        {
            local.entries.push(entry);
        }
    }
    if count == local.entries.len() {
        return Ok(None);
    }
    let bytes = serde_json::to_vec(&local)?;
    validate_backup(&bytes)?;
    Ok(Some(bytes))
}

pub(crate) fn read_java_backup(
    wrapper: Option<roxmltree::Node<'_, '_>>,
    limit: Option<usize>,
    omitted: &mut usize,
) -> io::Result<Option<(Vec<u8>, usize)>> {
    let mut snapshot = Snapshot {
        limit: limit.unwrap_or(DEFAULT_LIMIT),
        ..Snapshot::default()
    };
    if let Some(wrapper) = wrapper {
        let policy = crate::clipboard_policy::ClipboardPolicy::load();
        let redactor = crate::workspace::saved_chats::java_xml_redactor(&policy.terminal_logging)?;
        for input in wrapper.children().filter(roxmltree::Node::is_element) {
            let entry = (snapshot.entries.len() < snapshot.limit)
                .then(|| java_entry(input, &redactor))
                .flatten();
            match entry {
                Some(entry)
                    if !snapshot
                        .entries
                        .iter()
                        .any(|current| current.goal == entry.goal) =>
                {
                    snapshot.entries.push(entry);
                }
                _ => *omitted += 1,
            }
        }
    }
    if limit.is_none() && snapshot.entries.is_empty() {
        return Ok(None);
    }
    let bytes = serde_json::to_vec(&snapshot)?;
    validate_backup(&bytes)?;
    Ok(Some((bytes, snapshot.entries.len())))
}

fn java_entry(
    node: roxmltree::Node<'_, '_>,
    redactor: &crate::terminal_logging::Redactor,
) -> Option<Entry> {
    if !node.has_tag_name("input")
        || node.tag_name().namespace().is_some()
        || node.attributes().next().is_some()
    {
        return None;
    }
    let value = |name| {
        let mut nodes = node
            .children()
            .filter(|child| child.has_tag_name(name) && child.tag_name().namespace().is_none());
        let child = nodes.next()?;
        if nodes.next().is_some()
            || child.attributes().next().is_some()
            || !child.children().all(|content| content.is_text())
        {
            return None;
        }
        child.text().map(str::trim)
    };
    let prompt = value("prompt")?;
    if prompt.len() > 8192 || prompt.is_empty() {
        return None;
    }
    let entry = Entry {
        goal: crate::workspace::mask_ai_prompt(redactor, prompt).ok()?,
        kind: Kind::Imported,
        started_at: value("lastUsedEpochMillis")?
            .parse::<i64>()
            .ok()?
            .try_into()
            .ok()?,
    };
    entry.valid().then_some(entry)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::tests::Profile;

    #[test]
    fn agent_prompt_history_keeps_private_runs_ephemeral_and_never_clears_an_unreviewed_entry() {
        let profile = Profile::new();
        let root = &profile.0;
        remember(root, "private fixture".into(), Mode::Plan, true).unwrap();
        assert!(!root.join(FOLDER).exists());
        assert_eq!(list(root, false).unwrap().limit, DEFAULT_LIMIT);
        set_limit(root, &list(root, false).unwrap(), MIN_LIMIT, false).unwrap();
        for index in 0..6 {
            remember(root, format!("fixture {index}"), Mode::Run, false).unwrap();
        }
        remember(root, "fixture 2".into(), Mode::Ask, false).unwrap();
        let before = list(root, false).unwrap();
        assert_eq!(before.entries.len(), MIN_LIMIT);
        assert_eq!(before.entries[0].goal, "fixture 2");
        assert!(before.entries[0].kind == Kind::Ask);
        assert!(list(root, true).unwrap().entries.is_empty());
        remember(root, "newer prompt".into(), Mode::Plan, false).unwrap();
        assert!(clear(root, &before, false).is_err());
        assert!(set_limit(root, &before, DEFAULT_LIMIT, false).is_err());
        let saved = list(root, false).unwrap();
        assert_eq!(saved.entries[0].goal, "newer prompt");
        assert!(delete(root, &saved.entries[0], true).is_err());
        delete(root, &saved.entries[0], false).unwrap();
        clear(root, &list(root, false).unwrap(), false).unwrap();
        assert!(list(root, false).unwrap().entries.is_empty());
    }
}
