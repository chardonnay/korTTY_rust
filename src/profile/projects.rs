//! Named workspace layouts in the private desktop profile.
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use super::{
    MAX_BYTES, WorkspaceLayout, invalid, open_preference, root, save_atomic, save_new_atomic,
};

pub(crate) mod java;
mod portable;
pub(crate) use portable::{FileProject, PreparedProject, ProjectFile, ProjectFolder};

const MAX_NAME_BYTES: usize = 64;
const MAX_PROJECTS: usize = 256;
const MAX_DIRECTORY_ENTRIES: usize = 4096;
const SUFFIX: &str = ".json";
pub(crate) const RECENT_LIMIT: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Name(String);

impl Name {
    pub(crate) fn parse(input: &str) -> io::Result<Self> {
        let name = input.trim();
        if name.is_empty()
            || name.len() > MAX_NAME_BYTES
            || !name.chars().all(|character| {
                character.is_alphanumeric() || matches!(character, ' ' | '-' | '_')
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Use a project name of up to 64 bytes with letters, numbers, spaces, hyphens or underscores.",
            ));
        }
        Ok(Self(name.to_owned()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    fn file_name(&self) -> String {
        // A reversible ASCII component preserves names on case-insensitive filesystems.
        let mut file = String::with_capacity(self.0.len() * 2 + SUFFIX.len());
        for byte in self.0.bytes() {
            write!(file, "{byte:02x}").expect("write to String");
        }
        file.push_str(SUFFIX);
        file
    }

    fn from_file_name(file: &str) -> Option<Self> {
        let encoded = file.strip_suffix(SUFFIX)?;
        if encoded.is_empty()
            || encoded.len() > MAX_NAME_BYTES * 2
            || !encoded.len().is_multiple_of(2)
            || !encoded.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return None;
        }
        let decoded = (0..encoded.len())
            .step_by(2)
            .map(|offset| u8::from_str_radix(&encoded[offset..offset + 2], 16))
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        let name = Self::parse(std::str::from_utf8(&decoded).ok()?).ok()?;
        (name.file_name() == file).then_some(name)
    }
}

pub(crate) struct Store {
    directory: PathBuf,
}

struct WriteLock(File);

impl Drop for WriteLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl Store {
    pub(crate) fn open() -> io::Result<Self> {
        Self::open_at(root()?.join("projects"))
    }

    fn open_at(directory: PathBuf) -> io::Result<Self> {
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
            return Err(invalid("project store requires a real directory"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(invalid("project store directory must be owner-only"));
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(invalid(
                    "project store directory must not be a reparse point",
                ));
            }
        }
        Ok(Self { directory })
    }

    pub(crate) fn list(&self) -> io::Result<Vec<Name>> {
        let mut names = Vec::new();
        for (index, entry) in self.directory.read_dir()?.enumerate() {
            if index >= MAX_DIRECTORY_ENTRIES {
                return Err(invalid("project store has too many directory entries"));
            }
            let entry = entry?;
            if entry.file_type()?.is_file()
                && let Some(name) = entry.file_name().to_str().and_then(Name::from_file_name)
            {
                names.push(name);
                if names.len() > MAX_PROJECTS {
                    return Err(invalid("project store exceeds its project count limit"));
                }
            }
        }
        names.sort_unstable();
        Ok(names)
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn recent(&self, remembered: &[Name], cutoff: u64) -> io::Result<Vec<Name>> {
        use std::collections::BTreeSet;
        use std::time::UNIX_EPOCH;

        let mut available: BTreeSet<_> = self.list()?.into_iter().collect();
        let mut recent: Vec<_> = remembered
            .iter()
            .filter_map(|name| available.take(name))
            .take(RECENT_LIMIT)
            .collect();
        if recent.len() == RECENT_LIMIT {
            return Ok(recent);
        }
        let mut changed: Vec<_> = available
            .into_iter()
            .filter_map(|name| {
                let metadata = self.path(&name).symlink_metadata().ok()?;
                if !metadata.is_file() {
                    return None;
                }
                let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
                (modified.as_millis() > u128::from(cutoff)).then_some((modified, name))
            })
            .collect();
        changed.sort_unstable_by(|(a, left), (b, right)| b.cmp(a).then_with(|| left.cmp(right)));
        let remaining = RECENT_LIMIT - recent.len();
        recent.extend(changed.into_iter().take(remaining).map(|(_, name)| name));
        Ok(recent)
    }

    pub(crate) fn load(&self, name: &Name) -> io::Result<WorkspaceLayout> {
        load_file(&self.path(name))
    }

    /// Returns the current project if an explicit replacement is needed.
    pub(crate) fn save_new(
        &self,
        name: &Name,
        layout: &WorkspaceLayout,
    ) -> io::Result<Option<WorkspaceLayout>> {
        let _lock = self.lock()?;
        match self.load(name) {
            Ok(existing) => return Ok(Some(existing)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if self.list()?.len() == MAX_PROJECTS {
            return Err(invalid("project store has reached its project count limit"));
        }
        save_new_atomic(&self.path(name), &encode(layout)?)?;
        Ok(None)
    }

    pub(crate) fn replace(
        &self,
        name: &Name,
        original: &WorkspaceLayout,
        layout: &WorkspaceLayout,
    ) -> io::Result<()> {
        let _lock = self.lock()?;
        if self.load(name)? != *original {
            return Err(io::Error::other("the project changed before replacement"));
        }
        save_atomic(&self.path(name), &encode(layout)?)
    }

    fn path(&self, name: &Name) -> PathBuf {
        self.directory.join(name.file_name())
    }

    fn lock(&self) -> io::Result<WriteLock> {
        let lock = super::session::lock_file(&self.directory.join("projects.lock"))?;
        lock.try_lock().map_err(|error| match error {
            fs::TryLockError::WouldBlock => {
                io::Error::other("another instance is saving a project")
            }
            fs::TryLockError::Error(error) => error,
        })?;
        Ok(WriteLock(lock))
    }
}

fn load_file(path: &Path) -> io::Result<WorkspaceLayout> {
    let file = open_preference(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("project must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    decode(&bytes)
}

pub(crate) fn is_backup_name(name: &str) -> bool {
    Name::from_file_name(name).is_some()
}

pub(crate) fn validate_backup(bytes: &[u8]) -> io::Result<()> {
    decode(bytes).map(|_| ())
}

fn decode(bytes: &[u8]) -> io::Result<WorkspaceLayout> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("project exceeds the layout size limit"));
    }
    let layout: WorkspaceLayout = serde_json::from_slice(bytes).map_err(invalid)?;
    layout.validate()?;
    Ok(layout)
}

fn encode(layout: &WorkspaceLayout) -> io::Result<Vec<u8>> {
    layout.validate()?;
    let bytes = serde_json::to_vec_pretty(layout).map_err(invalid)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("project exceeds the layout size limit"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::WindowLayout;

    pub(super) struct Fixture(pub(super) PathBuf);

    impl Fixture {
        pub(super) fn new() -> Self {
            let mut random = [0; 8];
            getrandom::fill(&mut random).unwrap();
            let path = std::env::temp_dir().join(format!(
                "kortty-projects-{:016x}",
                u64::from_ne_bytes(random)
            ));
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn layout(width: f32) -> WorkspaceLayout {
        WorkspaceLayout::new(vec![WindowLayout {
            tab_titles: Default::default(),
            tab_groups: Default::default(),
            tab_primary_panes: Default::default(),
            tab_terminal_effects: Default::default(),
            width,
            height: 510.0,
            position: None,
            maximized: false,
            dashboard_visible: None,
            active: 0,
            tabs: Vec::new(),
        }])
    }

    #[test]
    fn open_recent_projects_prefers_remembered_names_then_modified_files_after_clear() {
        use std::fs::FileTimes;
        use std::time::{Duration, UNIX_EPOCH};

        let fixture = Fixture::new();
        let store = Store::open_at(fixture.0.clone()).unwrap();
        let older = Name::parse("Older").unwrap();
        let newest = Name::parse("Newest").unwrap();
        let removed = Name::parse("Removed").unwrap();
        for (name, seconds) in [(&older, 10), (&newest, 30), (&removed, 40)] {
            assert!(store.save_new(name, &layout(800.0)).unwrap().is_none());
            fs::OpenOptions::new()
                .write(true)
                .open(store.path(name))
                .unwrap()
                .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(seconds)))
                .unwrap();
        }
        fs::remove_file(store.path(&removed)).unwrap();
        assert_eq!(
            store
                .recent(&[removed, older.clone(), older.clone()], 20_000)
                .unwrap(),
            [older, newest.clone()]
        );
        assert_eq!(store.recent(&[], 20_000).unwrap(), [newest]);
        assert!(store.recent(&[], 30_000).unwrap().is_empty());
        assert_eq!(store.list().unwrap().len(), 2);
    }

    #[test]
    fn named_projects_round_trip_and_replacement_rejects_another_save() {
        let fixture = Fixture::new();
        let first = Store::open_at(fixture.0.clone()).unwrap();
        let second = Store::open_at(fixture.0.clone()).unwrap();
        let mixed = Name::parse("München 3").unwrap();
        let lower = Name::parse("münchen 3").unwrap();
        let original = layout(800.0);
        assert!(first.save_new(&mixed, &original).unwrap().is_none());
        assert!(second.save_new(&lower, &layout(801.0)).unwrap().is_none());
        assert_eq!(first.list().unwrap(), [mixed.clone(), lower.clone()]);
        assert_eq!(first.load(&lower).unwrap(), layout(801.0));

        let existing = first.save_new(&mixed, &layout(850.0)).unwrap().unwrap();
        assert_eq!(existing, original);
        second.replace(&mixed, &original, &layout(860.0)).unwrap();
        assert!(first.replace(&mixed, &existing, &layout(850.0)).is_err());
        assert_eq!(first.load(&mixed).unwrap(), layout(860.0));
        assert!(Name::parse("../connections").is_err());
        assert!(Name::parse("alpha\u{202e}beta").is_err());
    }

    #[test]
    fn invalid_or_busy_projects_cannot_be_overwritten() {
        let fixture = Fixture::new();
        let store = Store::open_at(fixture.0.clone()).unwrap();
        let name = Name::parse("Existing").unwrap();
        let path = store.path(&name);
        fs::write(&path, b"unrecognized contents").unwrap();
        assert!(store.save_new(&name, &layout(900.0)).is_err());
        assert!(
            store
                .replace(&name, &layout(800.0), &layout(900.0))
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"unrecognized contents");

        let lock = store.lock().unwrap();
        let new = Name::parse("New").unwrap();
        assert!(store.save_new(&new, &layout(800.0)).is_err());
        assert!(!store.path(&new).exists());
        let inherited_descriptor = lock.0.try_clone().unwrap();
        drop(lock);
        assert!(store.save_new(&new, &layout(800.0)).unwrap().is_none());
        drop(inherited_descriptor);
    }

    #[cfg(unix)]
    #[test]
    fn project_reads_do_not_follow_symbolic_links() {
        let fixture = Fixture::new();
        let store = Store::open_at(fixture.0.clone()).unwrap();
        let genuine = Name::parse("Original").unwrap();
        let alias = Name::parse("Alias").unwrap();
        let original = layout(820.0);
        store.save_new(&genuine, &original).unwrap();
        std::os::unix::fs::symlink(store.path(&genuine), store.path(&alias)).unwrap();
        assert_eq!(
            store.list().unwrap().as_slice(),
            std::slice::from_ref(&genuine)
        );
        assert!(store.load(&alias).is_err());
        assert!(store.save_new(&alias, &layout(830.0)).is_err());
        assert_eq!(store.load(&genuine).unwrap(), original);
    }
}
