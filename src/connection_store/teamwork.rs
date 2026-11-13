use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::Connection;
use super::import::{TeamworkEntry, read_teamwork};
use crate::profile;

mod auth;
mod cache;
mod git;
mod local_entries;
pub(crate) use auth::{AuthSave, DefaultAuth};
pub(crate) use git::GitUrl;
pub(crate) use local_entries::HiddenEntry;
pub(super) use local_entries::{Favorites, HiddenEntries};

pub(crate) const MAX_SOURCES: usize = 16;
const MAX_ACTIVE_SOURCES: usize = 2 * MAX_SOURCES;
pub(crate) const MAX_LOCATION_BYTES: usize = 4096;
pub(crate) const DEFAULT_CHECK_INTERVAL_MINUTES: u16 = 15;
pub(crate) const MAX_CHECK_INTERVAL_MINUTES: u16 = 1440;
const MAX_SETTINGS_BYTES: u64 = 128 * 1024;
static WRITES: Mutex<()> = Mutex::new(());
static SETTINGS_REVISION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    pub(crate) sources: Vec<Source>,
    #[serde(default = "default_check_interval")]
    pub(crate) default_check_interval_minutes: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Source {
    pub(crate) location: String,
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) kind: SourceKind,
    #[serde(default = "default_check_interval")]
    pub(crate) check_interval_minutes: u16,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum SourceKind {
    #[default]
    File,
    Git,
}

impl fmt::Display for SourceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::File => "Shared file",
            Self::Git => "Git repository",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManagedSource {
    pub(crate) name: String,
    pub(crate) source: Source,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum Origin {
    File(PathBuf),
    Git(GitUrl),
}

impl Origin {
    fn from_file(path: PathBuf) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(invalid("Choose an absolute path to a Teamwork file."));
        }
        if path.to_str().is_none_or(|path| {
            path.len() > MAX_LOCATION_BYTES || crate::ssh_dialog::unsafe_text(path)
        }) {
            return Err(invalid("Invalid Teamwork file location."));
        }
        Ok(Self::File(path))
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => path.display().fmt(formatter),
            Self::Git(url) => formatter.write_str(url.as_str()),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ActiveSources(BTreeMap<Origin, u16>);

impl ActiveSources {
    pub(crate) fn sync_period(&self, fallback_minutes: u16) -> Duration {
        let minutes = self.0.values().copied().min().unwrap_or(fallback_minutes);
        Duration::from_secs(u64::from(minutes) * 60)
    }
}

#[derive(Default)]
pub(crate) struct RefreshControl {
    cancelled: AtomicBool,
    git_worker: Mutex<Option<async_channel::Receiver<()>>>,
}

impl RefreshControl {
    pub(crate) fn cancel(&self) -> Option<async_channel::Receiver<()>> {
        self.cancelled.store(true, Ordering::Release);
        self.git_worker.lock().ok()?.take()
    }

    fn check(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Teamwork refresh cancelled.",
            ));
        }
        Ok(())
    }

    fn begin_git(&self) -> io::Result<async_channel::Sender<()>> {
        let mut active = self
            .git_worker
            .lock()
            .map_err(|_| io::Error::other("The Teamwork Git worker stopped."))?;
        self.check()?;
        let (finished, wait) = async_channel::bounded(1);
        *active = Some(wait);
        Ok(finished)
    }
}

const fn default_check_interval() -> u16 {
    DEFAULT_CHECK_INTERVAL_MINUTES
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            default_check_interval_minutes: default_check_interval(),
        }
    }
}

fn path() -> io::Result<PathBuf> {
    Ok(profile::root()?.join("teamwork.json"))
}

impl Settings {
    pub(crate) fn validate(&self) -> io::Result<()> {
        self.origins().map(|_| ())
    }

    pub(crate) fn active_sources(
        &self,
        custom_allowed: bool,
        managed: &[ManagedSource],
    ) -> io::Result<ActiveSources> {
        let mut active = ActiveSources::default();
        if custom_allowed {
            for (source, origin) in self.sources.iter().zip(self.origins()?) {
                if source.enabled {
                    active.0.insert(origin, source.check_interval_minutes);
                }
            }
        }
        for managed in managed {
            let source = &managed.source;
            source.validate()?;
            if source.enabled {
                active
                    .0
                    .insert(source.origin()?, source.check_interval_minutes);
            }
        }
        Ok(active)
    }

    pub(crate) fn load() -> io::Result<Self> {
        let file = match profile::open_preference(&path()?) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.take(MAX_SETTINGS_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(invalid("Teamwork settings exceed the size limit."));
        }
        let settings: Self = serde_json::from_slice(&bytes).map_err(invalid)?;
        settings.validate()?;
        Ok(settings)
    }

    pub(crate) fn save(&self, before: &Self) -> io::Result<()> {
        self.validate()?;
        let _write = write_guard()?;
        let current = Self::load()?;
        if current != *before {
            return Err(io::Error::other(
                "Teamwork settings changed since this dialog opened. Reopen the dialog before saving.",
            ));
        }
        if current == *self {
            return Ok(());
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(invalid)?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(invalid("Teamwork settings exceed the size limit."));
        }
        profile::save_atomic(&path()?, &bytes)?;
        SETTINGS_REVISION.fetch_add(1, Ordering::Release);
        Ok(())
    }

    pub(crate) fn revision() -> u64 {
        SETTINGS_REVISION.load(Ordering::Acquire)
    }

    pub(crate) fn persist_catalog(
        expected: Option<&Self>,
        root: &Path,
        catalog: &Catalog,
    ) -> io::Result<(u64, Option<io::Error>)> {
        let _write = write_guard()?;
        if let Some(expected) = expected
            && Self::load()? != *expected
        {
            return Err(io::Error::other(
                "Teamwork settings changed during synchronization.",
            ));
        }
        Ok((Self::revision(), catalog.save_cache(root).err()))
    }

    fn origins(&self) -> io::Result<Vec<Origin>> {
        if self.sources.len() > MAX_SOURCES {
            return Err(invalid("Add at most 16 Teamwork sources."));
        }
        let valid_interval = 1..=MAX_CHECK_INTERVAL_MINUTES;
        if !valid_interval.contains(&self.default_check_interval_minutes)
            || self
                .sources
                .iter()
                .any(|source| !valid_interval.contains(&source.check_interval_minutes))
        {
            return Err(invalid(
                "Teamwork check intervals must be 1 to 1440 minutes.",
            ));
        }
        let mut unique = BTreeSet::new();
        self.sources
            .iter()
            .map(|source| {
                let origin = source.origin()?;
                if !unique.insert(origin.clone()) {
                    return Err(invalid(
                        "The same Teamwork source is listed more than once.",
                    ));
                }
                Ok(origin)
            })
            .collect()
    }
}

impl Source {
    pub(crate) fn validate(&self) -> io::Result<()> {
        if !(1..=MAX_CHECK_INTERVAL_MINUTES).contains(&self.check_interval_minutes) {
            return Err(invalid(
                "Teamwork check intervals must be 1 to 1440 minutes.",
            ));
        }
        self.origin().map(|_| ())
    }

    pub(crate) fn origin(&self) -> io::Result<Origin> {
        self.kind.parse_origin(&self.location)
    }
}

impl SourceKind {
    fn parse_origin(self, location: &str) -> io::Result<Origin> {
        if self == Self::Git {
            return GitUrl::parse(location).map(Origin::Git);
        }
        let location = location.trim();
        if location.len() > MAX_LOCATION_BYTES || crate::ssh_dialog::unsafe_text(location) {
            return Err(invalid("Invalid Teamwork file location."));
        }
        let path = if location.starts_with("file:") {
            let url = url::Url::parse(location).map_err(invalid)?;
            if url.query().is_some() || url.fragment().is_some() {
                return Err(invalid(
                    "A Teamwork file URL cannot have a query or fragment.",
                ));
            }
            url.to_file_path()
                .map_err(|_| invalid("Choose a local or mounted Teamwork file."))?
        } else {
            PathBuf::from(location)
        };
        Origin::from_file(path)
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Identity {
    source: Arc<Origin>,
    id: Arc<str>,
}

impl Identity {
    pub(crate) fn source(&self) -> &Origin {
        &self.source
    }

    pub(crate) fn external_id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Entry {
    pub(crate) identity: Identity,
    pub(crate) connection: Connection,
    pub(crate) group: String,
    pub(crate) badge: Option<String>,
    pub(crate) default_auth_eligible: bool,
    pub(crate) last_used_at: Option<u64>,
}

impl Entry {
    pub(crate) fn identity(&self) -> &Identity {
        &self.identity
    }

    pub(crate) fn source(&self) -> &Origin {
        self.identity.source()
    }

    fn from_source(source: Arc<Origin>, entry: TeamworkEntry) -> Self {
        Self {
            identity: Identity {
                source,
                id: Arc::from(entry.id),
            },
            connection: entry.connection,
            group: entry.group,
            badge: entry.badge,
            default_auth_eligible: entry.default_auth_eligible,
            last_used_at: entry.last_used_at,
        }
    }
}

pub(crate) fn group_title(group: &str) -> String {
    if group.is_empty() {
        "Teamwork".into()
    } else {
        format!("Teamwork / {}", group.replace('/', " / "))
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Catalog {
    sources: BTreeMap<Arc<Origin>, Snapshot>,
    pub(crate) failures: usize,
    pub(crate) first_failure: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Snapshot {
    entries: Arc<[Entry]>,
    branch: Option<git::Branch>,
}

impl Catalog {
    pub(crate) fn same_entries(&self, other: &Self) -> bool {
        self.sources == other.sources
    }

    pub(crate) fn retain_enabled(&mut self, active: &ActiveSources) {
        self.sources
            .retain(|origin, _| active.0.contains_key(origin.as_ref()));
    }

    #[cfg(test)]
    pub(crate) fn refreshed(&self, active: &ActiveSources) -> Self {
        self.refresh(active, &RefreshControl::default()).unwrap()
    }

    pub(crate) fn refresh(
        &self,
        active: &ActiveSources,
        control: &RefreshControl,
    ) -> io::Result<Self> {
        let mut next = Self::default();
        for origin in active.0.keys() {
            control.check()?;
            let previous = self.sources.get(origin);
            let refreshed = match origin {
                Origin::File(path) => read_teamwork(path).map(|entries| (None, entries)),
                Origin::Git(url) => {
                    let _finished = control.begin_git()?;
                    git::read(
                        url,
                        previous.and_then(|source| source.branch.as_ref()),
                        &control.cancelled,
                    )
                    .map(|(branch, entries)| (Some(branch), entries))
                }
            };
            let origin = Arc::new(origin.clone());
            let snapshot = match refreshed {
                Ok((branch, entries)) => Snapshot {
                    branch,
                    entries: entries
                        .into_iter()
                        .map(|entry| Entry::from_source(Arc::clone(&origin), entry))
                        .collect(),
                },
                Err(error) => {
                    next.failures += 1;
                    next.first_failure.get_or_insert_with(|| error.to_string());
                    let Some(previous) = previous else {
                        continue;
                    };
                    previous.clone()
                }
            };
            next.sources.insert(origin, snapshot);
        }
        control.check()?;
        Ok(next)
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.sources
            .values()
            .flat_map(|source| source.entries.iter())
    }

    pub(crate) fn get(&self, identity: &Identity) -> Option<&Entry> {
        self.sources
            .get(&identity.source)?
            .entries
            .iter()
            .find(|entry| entry.identity == *identity)
    }
}

fn invalid(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn write_guard() -> io::Result<MutexGuard<'static, ()>> {
    WRITES
        .lock()
        .map_err(|_| io::Error::other("The Teamwork profile writer stopped."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teamwork_interval_migrates_legacy_settings_and_uses_shortest_enabled_source() {
        let folder = std::env::temp_dir();
        let mut settings: Settings = serde_json::from_value(serde_json::json!({
            "sources": [
                { "location": folder.join("first.xml"), "enabled": true },
                { "location": folder.join("second.xml"), "enabled": false }
            ],
        }))
        .unwrap();
        let period = |settings: &Settings| {
            settings
                .active_sources(true, &[])
                .unwrap()
                .sync_period(settings.default_check_interval_minutes)
        };
        assert_eq!(settings.sources[0].kind, SourceKind::File);
        assert_eq!(period(&settings), Duration::from_secs(15 * 60));
        settings.default_check_interval_minutes = 30;
        settings.sources[1].check_interval_minutes = 1;
        assert_eq!(period(&settings), Duration::from_secs(15 * 60));
        settings.sources[1].enabled = true;
        assert_eq!(period(&settings), Duration::from_secs(60));
        for source in &mut settings.sources {
            source.enabled = false;
        }
        assert_eq!(period(&settings), Duration::from_secs(30 * 60));
        settings.validate().unwrap();
        settings.sources[0].check_interval_minutes = 0;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn managed_teamwork_origins_take_precedence_and_can_exclude_custom_sources() {
        let temp = std::env::temp_dir();
        let custom = Source {
            location: temp.join("custom.xml").to_str().unwrap().into(),
            kind: SourceKind::File,
            enabled: true,
            check_interval_minutes: 1,
        };
        let shared = Source {
            location: temp.join("shared.xml").to_str().unwrap().into(),
            ..custom.clone()
        };
        let managed = [ManagedSource {
            name: "Managed".into(),
            source: Source {
                check_interval_minutes: 15,
                ..shared.clone()
            },
        }];
        let settings = Settings {
            sources: vec![custom.clone(), shared.clone()],
            ..Settings::default()
        };
        let both = settings.active_sources(true, &managed).unwrap();
        assert_eq!(both.0.len(), 2);
        assert_eq!(both.0[&shared.origin().unwrap()], 15);
        let managed_only = settings.active_sources(false, &managed).unwrap();
        assert_eq!(managed_only.0.len(), 1);
        assert_eq!(managed_only.0[&shared.origin().unwrap()], 15);
        assert!(!managed_only.0.contains_key(&custom.origin().unwrap()));
    }

    #[test]
    fn teamwork_shutdown_waits_for_active_git_and_refuses_new_processes() {
        let control = RefreshControl::default();
        let running = control.begin_git().unwrap();
        let wait = control.cancel().unwrap();
        assert!(!wait.is_closed());
        assert_eq!(
            control.begin_git().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        drop(running);
        assert!(wait.is_closed());
    }
}
