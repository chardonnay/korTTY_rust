//! Per-device recovery snapshots. Explicitly saved layouts use the existing layout file.
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    MAX_BYTES, MAX_TAB_TITLE_CHARS, WorkspaceLayout, invalid, normalize_tab_group,
    normalize_tab_title, save_atomic,
};

const SNAPSHOT: &str = "last-session.json";
const RECENTLY_CLOSED: &str = "recently-closed.json";
const MODE: &str = "restore-mode";
const LOCK: &str = "session.lock";
pub(crate) const MAX_CLOSED_ENTRIES: usize = 25;
pub(crate) const MAX_CLOSED_TABS: usize = 100;
const MAX_CLOSED_TITLE_UNITS: usize = 120;
// A full history holds 2,500 tabs, each with up to 1,024 JSON-escaped group
// bytes, 360 title bytes and the saved ID and field names.
const MAX_CLOSED_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RestoreMode {
    #[default]
    Ask,
    Auto,
    Off,
}

impl RestoreMode {
    pub(crate) const ALL: [Self; 3] = [Self::Ask, Self::Auto, Self::Off];

    fn parse(text: &str) -> Self {
        match text.trim().to_ascii_lowercase().as_str() {
            "auto" => Self::Auto,
            "off" => Self::Off,
            _ => Self::Ask,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Auto => "auto",
            Self::Off => "off",
        }
    }
}

impl std::fmt::Display for RestoreMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Ask => "Ask",
            Self::Auto => "Automatic",
            Self::Off => "Off",
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    pub(crate) clean_exit: bool,
    pub(crate) layout: WorkspaceLayout,
}

impl Snapshot {
    fn validate(&self) -> io::Result<()> {
        self.layout.validate()?;
        if self
            .layout
            .windows
            .iter()
            .any(|window| window.tabs.is_empty())
        {
            return Err(invalid("session snapshot contains an empty window"));
        }
        Ok(())
    }
}

/// The persisted close history, newest first. Runtime-only tabs are omitted
/// before this reaches the writer. A saved ID is resolved through the current
/// connection store on reopen.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecentlyClosed {
    pub(crate) entries: Vec<ClosedEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClosedEntry {
    pub(crate) window: bool,
    pub(crate) tabs: Vec<ClosedTab>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClosedTab {
    pub(crate) saved_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) custom_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) group: Option<String>,
}

impl RecentlyClosed {
    fn validate(&self) -> io::Result<()> {
        if self.entries.len() > MAX_CLOSED_ENTRIES {
            return Err(invalid("too many recently closed entries"));
        }
        for entry in &self.entries {
            if entry.tabs.is_empty() || entry.tabs.len() > MAX_CLOSED_TABS {
                return Err(invalid("invalid number of tabs in a recently closed entry"));
            }
            for tab in &entry.tabs {
                if tab.saved_id == 0 {
                    return Err(invalid(
                        "invalid saved connection ID in recently closed history",
                    ));
                }
                if tab.custom_title.as_ref().is_some_and(|title| {
                    title.is_empty()
                        || title.len() > MAX_TAB_TITLE_CHARS * 4
                        || normalize_tab_title(title) != *title
                }) {
                    return Err(invalid("invalid custom title in recently closed history"));
                }
                if tab
                    .group
                    .as_ref()
                    .is_some_and(|group| normalize_tab_group(group).as_ref() != Some(group))
                {
                    return Err(invalid("invalid tab group in recently closed history"));
                }
            }
        }
        Ok(())
    }

    fn titles(&self) -> impl Iterator<Item = &String> {
        self.entries
            .iter()
            .flat_map(|entry| &entry.tabs)
            .filter_map(|tab| tab.custom_title.as_ref())
    }

    fn limit_titles(&mut self) {
        for title in self
            .entries
            .iter_mut()
            .flat_map(|entry| &mut entry.tabs)
            .filter_map(|tab| tab.custom_title.as_mut())
        {
            let mut units = 0;
            if let Some((end, _)) = title.char_indices().find(|&(_, character)| {
                units += character.len_utf16();
                units > MAX_CLOSED_TITLE_UNITS
            }) {
                title.truncate(end);
                *title = normalize_tab_title(title);
            }
        }
    }
}

pub(crate) struct Store {
    directory: PathBuf,
    lock: File,
}

impl Store {
    pub(crate) fn open() -> io::Result<Self> {
        Self::open_at(super::path()?.with_file_name("session"))
    }

    pub(crate) fn open_at(directory: PathBuf) -> io::Result<Self> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(&directory)?;
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.is_dir() {
            return Err(invalid("session store requires a real directory"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(invalid("session store directory must be owner-only"));
            }
        }
        let lock = lock_file(&directory.join(LOCK))?;
        lock.try_lock().map_err(|error| match error {
            fs::TryLockError::WouldBlock => {
                io::Error::other("session store is open in another instance")
            }
            fs::TryLockError::Error(error) => error,
        })?;
        Ok(Self { directory, lock })
    }

    pub(crate) fn previous(&self) -> io::Result<Option<Snapshot>> {
        let Some(bytes) = read_regular(&self.directory.join(SNAPSHOT), MAX_BYTES)? else {
            return Ok(None);
        };
        let snapshot: Snapshot = serde_json::from_slice(&bytes).map_err(invalid)?;
        snapshot.validate()?;
        Ok(Some(snapshot))
    }

    pub(crate) fn recently_closed(&self) -> io::Result<RecentlyClosed> {
        let Some(bytes) = read_regular(&self.directory.join(RECENTLY_CLOSED), MAX_CLOSED_BYTES)?
        else {
            return Ok(RecentlyClosed::default());
        };
        let history: RecentlyClosed = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("invalid recently closed history data"))?;
        history.validate()?;
        if history
            .titles()
            .any(|title| title.encode_utf16().count() > MAX_CLOSED_TITLE_UNITS)
        {
            return Err(invalid(
                "recently closed history title exceeds its size limit",
            ));
        }
        Ok(history)
    }

    pub(crate) fn save_recently_closed(&self, history: &RecentlyClosed) -> io::Result<()> {
        history.validate()?;
        let mut stored = history.clone();
        // Match the Java snapshot limit without splitting a Unicode scalar.
        stored.limit_titles();
        let bytes = serde_json::to_vec(&stored).map_err(invalid)?;
        if bytes.len() as u64 > MAX_CLOSED_BYTES {
            return Err(invalid("recently closed history exceeds its size limit"));
        }
        save_atomic(&self.directory.join(RECENTLY_CLOSED), &bytes)
    }

    pub(crate) fn restore_mode(&self) -> io::Result<RestoreMode> {
        let Some(bytes) = read_regular(&self.directory.join(MODE), 32)? else {
            return Ok(RestoreMode::default());
        };
        Ok(RestoreMode::parse(
            std::str::from_utf8(&bytes).map_err(invalid)?,
        ))
    }

    pub(crate) fn save_mode(&self, mode: RestoreMode) -> io::Result<()> {
        save_atomic(&self.directory.join(MODE), mode.id().as_bytes())
    }

    pub(crate) fn write(&self, snapshot: Option<&Snapshot>) -> io::Result<()> {
        let path = self.directory.join(SNAPSHOT);
        let Some(snapshot) = snapshot else {
            return match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            };
        };
        snapshot.validate()?;
        let bytes = serde_json::to_vec(snapshot).map_err(invalid)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("session snapshot exceeds the profile size limit"));
        }
        save_atomic(&path, &bytes)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // A concurrent fork can retain this description until it reaches exec.
        let _ = self.lock.unlock();
    }
}

fn read_regular(path: &Path, limit: u64) -> io::Result<Option<Vec<u8>>> {
    let file = match regular_read(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("session profile file exceeds its size limit"));
    }
    Ok(Some(bytes))
}

fn regular_read(path: &Path) -> io::Result<File> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let file = {
        use rustix::fs::{Mode, OFlags, open};
        let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
        File::from(open(path, flags, Mode::empty())?)
    };
    #[cfg(windows)]
    let file = kortty_files::open_regular_without_reparse(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("session profile requires a regular file"));
    }
    Ok(file)
}

pub(super) fn lock_file(path: &Path) -> io::Result<File> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let file = {
        use rustix::fs::{Mode, OFlags, open};
        let flags =
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
        File::from(open(path, flags, Mode::RUSR | Mode::WUSR)?)
    };
    #[cfg(windows)]
    let file = {
        if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
            return Err(invalid("profile lock requires a regular file"));
        }
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?
    };
    if !file.metadata()?.is_file() {
        return Err(invalid("profile lock requires a regular file"));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn history() -> RecentlyClosed {
        RecentlyClosed {
            entries: vec![
                ClosedEntry {
                    window: true,
                    tabs: vec![
                        ClosedTab {
                            saved_id: 4,
                            custom_title: Some("Remote work".into()),
                            group: Some("Production".into()),
                        },
                        ClosedTab {
                            saved_id: 5,
                            custom_title: None,
                            group: None,
                        },
                    ],
                },
                ClosedEntry {
                    window: false,
                    tabs: vec![ClosedTab {
                        saved_id: 4,
                        custom_title: None,
                        group: None,
                    }],
                },
            ],
        }
    }

    #[test]
    fn recently_closed_is_independent_of_layout_and_mode_and_uses_the_same_lock() -> io::Result<()>
    {
        let profile = crate::snippets::tests::Profile::new();
        let directory = profile.0.join("session");
        let store = Store::open_at(directory.clone())?;
        assert_eq!(store.recently_closed()?, RecentlyClosed::default());
        let mut history = history();
        let group = "\\".repeat(crate::connection_store::MAX_GROUP_PATH_BYTES);
        history.entries[0].tabs[0].group = Some(group.clone());
        store.save_mode(RestoreMode::Off)?;
        store.save_recently_closed(&history)?;
        assert!(Store::open_at(directory.clone()).is_err());
        let sidecar = directory.join(RECENTLY_CLOSED);
        let raw: serde_json::Value = serde_json::from_slice(&fs::read(&sidecar)?).unwrap();
        assert_eq!(
            raw,
            json!({
                "entries": [
                    {"window": true, "tabs": [
                        {"saved_id": 4, "custom_title": "Remote work", "group": group},
                        {"saved_id": 5}
                    ]},
                    {"window": false, "tabs": [{"saved_id": 4}]}
                ]
            }),
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(fs::metadata(&sidecar)?.permissions().mode() & 0o777, 0o600);
        }
        assert!(store.previous()?.is_none());
        fs::write(directory.join(SNAPSHOT), b"{")?;
        assert!(store.previous().is_err());
        assert_eq!(store.recently_closed()?, history);
        store.write(None)?;
        assert_eq!(store.recently_closed()?, history);
        drop(store);

        let reopened = Store::open_at(directory.clone())?;
        assert_eq!(reopened.restore_mode()?, RestoreMode::Off);
        assert_eq!(reopened.recently_closed()?, history);
        reopened.save_recently_closed(&RecentlyClosed::default())?;
        assert_eq!(fs::read_to_string(sidecar)?, r#"{"entries":[]}"#);
        assert!(reopened.previous()?.is_none());
        Ok(())
    }

    #[test]
    fn recently_closed_rejects_unknown_fields_and_invalid_data_without_echoing_values()
    -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let directory = profile.0.join("session");
        let store = Store::open_at(directory.clone())?;
        let sidecar = directory.join(RECENTLY_CLOSED);
        for document in [
            json!({"entries": [], "layout": {"secret": "sensitive-example"}}),
            json!({"entries": [{"window": false, "tabs": [{"saved_id": 2, "endpoint": "sensitive-example"}]}]}),
            json!({"entries": [{"window": false, "tabs": [{"saved_id": 2, "password": "sensitive-example"}]}]}),
            json!({"entries": [{"window": false, "tabs": [{"saved_id": "sensitive-example"}]}]}),
            json!({"entries": [{"window": false, "tabs": [{"saved_id": 0}]}]}),
            json!({"entries": [{"window": false, "tabs": [{"saved_id": 2, "custom_title": "unsafe\u{202e}title"}]}]}),
            json!({"entries": [{"window": false, "tabs": [{"saved_id": 2, "custom_title": "😀".repeat(61)}]}]}),
            json!({"entries": [{"window": false, "tabs": []}]}),
        ] {
            fs::write(&sidecar, serde_json::to_vec(&document).unwrap())?;
            let error = store.recently_closed().unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(!error.to_string().contains("sensitive-example"));
        }
        fs::write(&sidecar, b"{")?;
        assert_eq!(
            store.recently_closed().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        File::options()
            .write(true)
            .open(sidecar)?
            .set_len(MAX_CLOSED_BYTES + 1)?;
        assert_eq!(
            store.recently_closed().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        Ok(())
    }

    #[test]
    fn recently_closed_clips_titles_at_character_boundaries_and_rejects_excess_items()
    -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let store = Store::open_at(profile.0.join("session"))?;
        let mut history = history();
        let clipped = "😀".repeat(59);
        history.entries[0].tabs[0].custom_title = Some(format!("{clipped} 😀x"));
        store.save_recently_closed(&history)?;
        let saved = store.recently_closed()?;
        assert_eq!(
            saved.entries[0].tabs[0].custom_title.as_deref(),
            Some(clipped.as_str())
        );

        let entry = history.entries[0].clone();
        let mut oversized = RecentlyClosed {
            entries: vec![entry.clone(); MAX_CLOSED_ENTRIES + 1],
        };
        assert!(store.save_recently_closed(&oversized).is_err());
        oversized.entries = vec![ClosedEntry {
            window: false,
            tabs: vec![entry.tabs[0].clone(); MAX_CLOSED_TABS + 1],
        }];
        assert!(store.save_recently_closed(&oversized).is_err());
        assert_eq!(store.recently_closed()?, saved);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recently_closed_never_reads_through_a_symbolic_link() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let directory = profile.0.join("session");
        let store = Store::open_at(directory.clone())?;
        let other = profile.0.join("other.json");
        fs::write(&other, br#"{"entries":[]}"#)?;
        std::os::unix::fs::symlink(&other, directory.join(RECENTLY_CLOSED))?;
        assert!(store.recently_closed().is_err());
        assert_eq!(fs::read(other)?, br#"{"entries":[]}"#);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn session_store_unlocks_even_when_a_child_still_has_the_file_description() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let directory = profile.0.join("session");
        let store = Store::open_at(directory.clone())?;
        // An inherited file description can remain open until the child execs.
        let inherited = store.lock.try_clone()?;
        drop(store);
        let reopened = Store::open_at(directory)?;
        drop(inherited);
        drop(reopened);
        Ok(())
    }
}
