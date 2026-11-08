use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use jiff::civil::{Date, DateTime, Time};
use jiff::{Timestamp, Zoned};
use serde::{Deserialize, Serialize};

use super::document::{self, ExportFormat};
use crate::{profile, sftp_window::LocalSource};

pub(crate) mod badges;
#[cfg(unix)]
mod delete_unix;

use badges::{Badge, decode_badges};

const MAX_FOLDERS: usize = 32;
const MAX_ENTRIES: usize = 4096;
const MAX_INSPECTED: usize = 32_768;
const MAX_PREFERENCES: u64 = 128 * 1024;
const LIVE_GRACE: Duration = Duration::from_secs(5 * 60);
static FOLDER_PREFERENCES: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    Text,
    Xml,
    Json,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "Plain Text",
            Self::Xml => "XML",
            Self::Json => "JSON",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Compression {
    None,
    Gzip,
    Bzip2,
    Zstd,
}

impl Compression {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Gzip => "gzip",
            Self::Bzip2 => "bzip2",
            Self::Zstd => "zstd",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Gzip => ".gz",
            Self::Bzip2 => ".bz2",
            Self::Zstd => ".zst",
        }
    }
}

#[derive(Clone, Debug)]
struct Name {
    filename: String,
    date: Date,
    time: Time,
    slug: String,
    format: Format,
    compression: Compression,
}

impl Name {
    fn parse(filename: &str) -> Option<Self> {
        if filename.len() > 255 || filename.chars().any(char::is_control) {
            return None;
        }
        let (prefix, compression) = [Compression::Gzip, Compression::Bzip2, Compression::Zstd]
            .into_iter()
            .find_map(|value| {
                filename
                    .strip_suffix(value.suffix())
                    .map(|name| (name, value))
            })
            .unwrap_or((filename, Compression::None));
        let (prefix, extension) = prefix.rsplit_once('.')?;
        let format = match extension {
            "log" | "txt" => Format::Text,
            "xml" => Format::Xml,
            "json" => Format::Json,
            _ => return None,
        };
        let prefix = match prefix.rsplit_once(".p") {
            Some((base, number)) if digits(number) => base,
            _ => prefix,
        };
        let (slug, sequence) = prefix.rsplit_once('_')?;
        if !digits(sequence) {
            return None;
        }
        let date: Date = slug.get(..10)?.parse().ok()?;
        let hours = slug.get(11..13)?;
        let minutes = slug.get(14..16)?;
        let seconds = slug.get(17..19)?;
        if ![10, 13, 16, 19]
            .into_iter()
            .all(|at| slug.as_bytes().get(at) == Some(&b'-'))
            || ![hours, minutes, seconds].into_iter().all(digits)
        {
            return None;
        }
        let time = Time::new(
            hours.parse().ok()?,
            minutes.parse().ok()?,
            seconds.parse().ok()?,
            0,
        )
        .unwrap_or(Time::MIN);
        let slug = slug.get(20..)?.to_owned();
        if slug.is_empty() {
            return None;
        }
        Some(Self {
            filename: filename.into(),
            date,
            time,
            slug,
            format,
            compression,
        })
    }

    fn bare(&self) -> &str {
        self.filename
            .strip_suffix(self.compression.suffix())
            .unwrap_or(&self.filename)
    }
}

fn digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    name: Name,
    stamp: Stamp,
    pub live: bool,
    manual_folder: bool,
    badges: Option<Vec<Badge>>,
    connection: String,
    host: String,
}

impl Entry {
    pub fn filename(&self) -> &str {
        &self.name.filename
    }
    pub fn slug(&self) -> &str {
        &self.name.slug
    }
    pub fn connection(&self) -> &str {
        if self.connection.is_empty() {
            self.slug()
        } else {
            &self.connection
        }
    }
    pub fn format(&self) -> Format {
        self.name.format
    }
    pub fn compression(&self) -> Compression {
        self.name.compression
    }
    pub fn started(&self) -> String {
        format!("{} {}", self.name.date, self.name.time)
    }
    pub fn size(&self) -> u64 {
        self.stamp.length
    }
    pub(super) fn started_at(&self) -> DateTime {
        self.name.date.to_datetime(self.name.time)
    }

    pub(super) fn ended_at(&self, now: &Zoned) -> DateTime {
        if !self.live
            && let Some(modified) = self.stamp.modified
            && let Ok(timestamp) = Timestamp::try_from(modified)
        {
            return timestamp.to_zoned(now.time_zone().clone()).datetime();
        }
        now.datetime()
    }

    pub(super) fn folder(&self) -> Option<&Path> {
        self.path.parent()
    }

    pub(super) fn forgettable_folder(&self) -> Option<&Path> {
        self.folder().filter(|_| self.manual_folder)
    }

    pub(super) fn checked_folder(&self) -> io::Result<PathBuf> {
        let _source = self.read()?;
        let folder = self
            .folder()
            .ok_or_else(|| invalid("The logfile has no parent folder"))?;
        #[cfg(windows)]
        {
            use crate::private_files::Directory;

            // Explorer expects a shell path. The catalog uses extended-length
            // filesystem paths, so verify that the conversion names the same folder.
            let shell = url::Url::from_directory_path(folder)
                .and_then(|url| url.to_file_path())
                .map_err(|()| invalid("The folder does not have a Windows shell path"))?;
            if !Directory::open(folder, false)?.same_as(&Directory::open(&shell, false)?)? {
                return Err(invalid(
                    "The Windows shell path points to a different folder",
                ));
            }
            Ok(shell)
        }
        #[cfg(not(windows))]
        Ok(folder.to_path_buf())
    }

    pub fn matches(&self, needle: &str) -> bool {
        self.filename().to_lowercase().contains(needle)
            || self.connection().to_lowercase().contains(needle)
            || self.host.to_lowercase().contains(needle)
            || self.badges().to_lowercase().contains(needle)
            || self
                .path
                .parent()
                .is_some_and(|path| path.to_string_lossy().to_lowercase().contains(needle))
    }

    pub fn deletable(&self) -> String {
        if self.live {
            "Active or recent".into()
        } else {
            match self.hold() {
                Some(false) => "Now".into(),
                Some(true) => "On hold".into(),
                None => "Unavailable".into(),
            }
        }
    }

    pub(super) fn export_path(
        &self,
        format: ExportFormat,
        compression: Compression,
        range: super::range::Range,
    ) -> String {
        let bare = self.name.bare();
        let (stem, extension) = bare.rsplit_once('.').unwrap_or((bare, "log"));
        let extension = match format {
            ExportFormat::Unchanged => extension,
            ExportFormat::Convert(Format::Text) => "log",
            ExportFormat::Convert(Format::Xml) => "xml",
            ExportFormat::Convert(Format::Json) => "json",
        };
        let cut = if range.is_open() {
            String::new()
        } else {
            format!("-cut-{}", range.tag())
        };
        self.path
            .with_file_name(format!(
                "{stem}{cut}-export.{extension}{}",
                compression.suffix()
            ))
            .to_string_lossy()
            .into_owned()
    }

    pub fn refresh(&self) -> io::Result<Self> {
        let source = LocalSource::open(&self.path)?;
        let stamp = Stamp::new(source.file())?;
        if !stamp.same_file(&self.stamp) {
            return Err(invalid(
                "The logfile was replaced. Reopen it from the manager.",
            ));
        }
        let mut next = Self {
            stamp,
            ..self.clone()
        };
        next.live = next.is_live(SystemTime::now());
        next.badges = next.read_badges(&source).ok();
        Ok(next)
    }

    pub fn hold(&self) -> Option<bool> {
        self.badges
            .as_ref()
            .map(|badges| badges.iter().any(Badge::held))
    }

    pub(super) fn badge_labels(&self) -> Option<&[Badge]> {
        self.badges.as_deref()
    }

    pub(super) fn same_log(&self, other: &Self) -> bool {
        self.stamp.same_file(&other.stamp)
    }

    pub fn badges(&self) -> String {
        self.badges.as_ref().map_or_else(
            || "Unreadable badges".into(),
            |badges| {
                badges
                    .iter()
                    .map(|badge| badge.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        )
    }

    pub fn read(&self) -> io::Result<LocalSource> {
        let source = LocalSource::open(&self.path)?;
        let stamp = Stamp::new(source.file())?;
        if stamp == self.stamp
            || (self.live && stamp.same_file(&self.stamp) && stamp.length >= self.size())
        {
            return Ok(source);
        }
        Err(invalid(
            "The terminal log changed. Refresh the list before opening it.",
        ))
    }

    pub fn require_closed(&self) -> io::Result<LocalSource> {
        let source = LocalSource::open(&self.path)?;
        if Stamp::new(source.file())? != self.stamp || self.is_live(SystemTime::now()) {
            return Err(invalid(
                "The log is active or changed. Refresh the list before continuing.",
            ));
        }
        Ok(source)
    }

    fn badge_name(&self) -> String {
        format!(".{}.badges.json", self.name.bare())
    }

    fn read_badges(&self, source: &LocalSource) -> io::Result<Vec<Badge>> {
        let name = self.badge_name();
        decode_badges(read_optional_sibling(
            source,
            OsStr::new(&name),
            badges::MAX_BYTES,
        )?)
    }

    fn is_live(&self, now: SystemTime) -> bool {
        self.is_live_where(now, |name| {
            exists_or_uncertain(&self.path.with_file_name(name))
        })
    }

    fn is_live_where(&self, now: SystemTime, exists: impl Fn(&str) -> bool) -> bool {
        let recent = self
            .stamp
            .modified
            .and_then(|stamp| now.duration_since(stamp).ok())
            .is_none_or(|age| age < LIVE_GRACE);
        if exists(&format!(".{}.lock", self.filename()))
            || (self.compression() != Compression::None
                && exists(&format!(".{}.lock", self.name.bare())))
        {
            return true;
        }
        recent && (self.compression() == Compression::None || exists(self.name.bare()))
    }

    pub fn set_hold(&self, held: bool) -> io::Result<()> {
        badges::set_hold(self, held)
    }

    pub fn delete(&self) -> io::Result<()> {
        let source = self.require_closed()?;
        self.delete_checked(&source)
    }

    fn delete_checked(&self, source: &LocalSource) -> io::Result<()> {
        let _guard = badges::mutation_lock()?;
        self.require_deletable(source)?;
        source.verify()?;
        self.delete_entry(source)?;
        // A surviving sidecar retains protection if this filename returns.
        Ok(())
    }

    fn require_deletable(&self, source: &LocalSource) -> io::Result<()> {
        if self.is_live_where(SystemTime::now(), |name| {
            source.sibling_exists_or_uncertain(OsStr::new(name))
        }) {
            return Err(invalid("The log became active before it could be deleted"));
        }
        if self.read_badges(source)?.iter().any(Badge::held) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The logfile is held",
            ));
        }
        Ok(())
    }

    fn delete_entry(&self, source: &LocalSource) -> io::Result<()> {
        #[cfg(unix)]
        {
            delete_unix::remove(source.parent_file(), &self.path, &self.stamp, || {
                self.require_deletable(source)
            })
        }
        #[cfg(windows)]
        {
            let file = source
                .parent_directory()
                .child_for_removal(std::ffi::OsStr::new(self.filename()))?;
            if Stamp::new(&file)? != self.stamp {
                return Err(invalid("The log changed before it could be deleted"));
            }
            self.require_deletable(source)?;
            kortty_files::tree::remove(file)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Stamp {
    length: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64),
    #[cfg(windows)]
    identity: (u64, [u8; 16]),
}

impl Stamp {
    fn new(file: &File) -> io::Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid("The logfile must be a regular file"));
        }
        #[cfg(windows)]
        if kortty_files::tree::is_reparse_point(&metadata) {
            return Err(invalid("The logfile cannot be a reparse point"));
        }
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            identity: (metadata.dev(), metadata.ino()),
            #[cfg(windows)]
            identity: kortty_files::tree::file_identity(file)?,
        })
    }

    fn same_file(&self, other: &Self) -> bool {
        self.identity == other.identity
    }
}

fn exists_or_uncertain(path: &Path) -> bool {
    !matches!(fs::symlink_metadata(path), Err(error) if error.kind() == io::ErrorKind::NotFound)
}

fn read_optional(path: &Path, limit: u64) -> io::Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(metadata) if !metadata.is_file() || metadata.len() > limit => {
            return Err(invalid(
                "The logfile metadata is not a bounded regular file",
            ));
        }
        Ok(_) => {}
    }
    let source = LocalSource::open(path)?;
    let bytes = read_bounded(source.file(), limit)?;
    source.verify()?;
    Ok(Some(bytes))
}

fn read_optional_sibling(
    source: &LocalSource,
    name: &OsStr,
    limit: u64,
) -> io::Result<Option<Vec<u8>>> {
    let file = match source.open_sibling(name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let expected = Stamp::new(&file)?;
    if expected.length > limit {
        return Err(invalid("The logfile metadata exceeds its size limit"));
    }
    let bytes = read_bounded(&file, limit)?;
    if Stamp::new(&file)? != expected || Stamp::new(&source.open_sibling(name)?)? != expected {
        return Err(invalid("The logfile metadata changed while reading"));
    }
    Ok(Some(bytes))
}

fn read_bounded(file: &File, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("The logfile metadata exceeds its size limit"));
    }
    Ok(bytes)
}

#[derive(Clone, Debug)]
pub struct Report {
    pub entries: Vec<Entry>,
    pub skipped: usize,
    pub bounded: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Preferences {
    folders: BTreeSet<PathBuf>,
}

fn load(path: &Path) -> io::Result<Preferences> {
    let Some(bytes) = read_optional(path, MAX_PREFERENCES)? else {
        return Ok(Preferences::default());
    };
    let prefs: Preferences = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if prefs.folders.len() > MAX_FOLDERS
        || prefs
            .folders
            .iter()
            .any(|folder| check_path(folder).is_err())
    {
        return Err(invalid("Invalid logfile folder settings"));
    }
    Ok(prefs)
}

pub fn add_folder(root: &Path, path: &Path) -> io::Result<()> {
    check_path(path)?;
    let path = path.canonicalize()?;
    if !path.is_dir() {
        return Err(invalid("Select a logfile directory"));
    }
    let settings = root.join("terminal-log-folders.json");
    let _guard = FOLDER_PREFERENCES
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut prefs = load(&settings)?;
    prefs.folders.insert(path);
    if prefs.folders.len() > MAX_FOLDERS {
        return Err(invalid(
            "The logfile manager supports up to 32 additional folders",
        ));
    }
    save_folders(&settings, &prefs)
}

pub(super) fn forget_folder(root: &Path, folder: &Path) -> io::Result<()> {
    check_path(folder)?;
    let default = root.join("terminal-logs");
    if folder == default || default.canonicalize().is_ok_and(|path| path == folder) {
        return Err(invalid("The default logfile folder cannot be forgotten"));
    }
    let settings = root.join("terminal-log-folders.json");
    let _guard = FOLDER_PREFERENCES
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut prefs = load(&settings)?;
    let previous = prefs.folders.len();
    prefs.folders.retain(|path| {
        path != folder
            && !path
                .canonicalize()
                .is_ok_and(|canonical| canonical == folder)
    });
    if previous == prefs.folders.len() {
        return Err(invalid(
            "This logfile folder is no longer in the additional folder list",
        ));
    }
    save_folders(&settings, &prefs)
}

fn save_folders(settings: &Path, prefs: &Preferences) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(prefs).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_PREFERENCES {
        return Err(invalid("The logfile folder settings exceed the size limit"));
    }
    profile::save_atomic(settings, &bytes)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveOwner {
    stamp: Stamp,
    days: u16,
}

const OWNERSHIP_LIMIT: u64 = 4096;

pub(crate) fn archive_owner_name(archive: &str) -> String {
    format!(".{archive}.kortty-owned.json")
}

pub(crate) fn write_archive_owner(mut marker: &File, archive: &File, days: u16) -> io::Result<()> {
    let owner = ArchiveOwner {
        stamp: Stamp::new(archive)?,
        days,
    };
    serde_json::to_writer(&mut marker, &owner).map_err(io::Error::other)?;
    marker.sync_all()
}

pub(crate) fn expire_owned_archive(
    path: &Path,
    source: &LocalSource,
    now: Date,
) -> io::Result<bool> {
    let Some(name) = path
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(Name::parse)
    else {
        return Ok(false);
    };
    if name.compression != Compression::Gzip {
        return Ok(false);
    }
    let marker = archive_owner_name(&name.filename);
    let Some(bytes) = read_optional_sibling(source, OsStr::new(&marker), OWNERSHIP_LIMIT)? else {
        return Ok(false);
    };
    let owner: ArchiveOwner = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let current = Stamp::new(source.file())?;
    if owner.stamp != current || owner.days == 0 || owner.days > 3650 {
        return Ok(false);
    }
    let cutoff = now
        .checked_sub(jiff::Span::new().days(i64::from(owner.days)))
        .map_err(io::Error::other)?;
    if name.date >= cutoff {
        return Ok(false);
    }
    let entry = Entry {
        path: path.into(),
        name,
        stamp: current,
        live: false,
        manual_folder: false,
        badges: None,
        connection: String::new(),
        host: String::new(),
    };
    entry.delete_checked(source)?;
    Ok(true)
}

pub fn scan(root: &Path, cancelled: impl Fn() -> bool) -> io::Result<Report> {
    let mut folders = load(&root.join("terminal-log-folders.json"))?.folders;
    let default = root.join("terminal-logs");
    let default_canonical = default.canonicalize().ok();
    folders.insert(default.clone());
    let now = SystemTime::now();
    let mut report = Report {
        entries: Vec::new(),
        skipped: 0,
        bounded: false,
    };
    let mut inspected = 0;
    let mut seen = BTreeSet::new();
    'folders: for folder in folders {
        let default_origin = folder == default;
        let folder = match folder.canonicalize() {
            Ok(folder) if seen.insert(folder.clone()) => folder,
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => {
                report.skipped += 1;
                continue;
            }
        };
        let manual_folder = !default_origin && default_canonical.as_ref() != Some(&folder);
        let directory = match fs::read_dir(&folder) {
            Ok(directory) => directory,
            Err(_) => {
                report.skipped += 1;
                continue;
            }
        };
        for result in directory {
            check_cancel(&cancelled)?;
            inspected += 1;
            if inspected > MAX_INSPECTED || report.entries.len() == MAX_ENTRIES {
                report.bounded = true;
                break 'folders;
            }
            let Ok(entry) = result else {
                report.skipped += 1;
                continue;
            };
            let filename = entry.file_name();
            let Some(name) = filename.to_str().and_then(Name::parse) else {
                continue;
            };
            let path = entry.path();
            let Ok(source) = LocalSource::open(&path) else {
                report.skipped += 1;
                continue;
            };
            let Ok(stamp) = Stamp::new(source.file()) else {
                report.skipped += 1;
                continue;
            };
            let mut entry = Entry {
                path,
                name,
                stamp,
                live: false,
                manual_folder,
                badges: None,
                connection: String::new(),
                host: String::new(),
            };
            entry.live = entry.is_live(now);
            entry.badges = entry.read_badges(&source).ok();
            if let Ok(file) = source.file().try_clone()
                && let Ok((connection, host)) =
                    document::preamble(file, entry.format(), entry.compression(), &cancelled)
            {
                entry.connection = connection;
                entry.host = host;
            }
            report.entries.push(entry);
        }
    }
    report.entries.sort_unstable_by(|left, right| {
        (right.name.date, right.name.time, &right.path).cmp(&(
            left.name.date,
            left.name.time,
            &left.path,
        ))
    });
    Ok(report)
}

#[cfg(unix)]
pub(crate) fn remove_open_capture(parent: &File, path: &Path, file: &File) -> io::Result<()> {
    delete_unix::remove(parent, path, &Stamp::new(file)?, || Ok(()))
}

pub(super) fn check_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
        || path
            .to_str()
            .is_none_or(|text| text.len() > 4096 || text.chars().any(char::is_control))
    {
        return Err(invalid(
            "Enter an absolute path of up to 4096 bytes without parent segments",
        ));
    }
    Ok(())
}

pub(super) fn check_cancel(cancelled: &impl Fn() -> bool) -> io::Result<()> {
    if cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    Ok(())
}

pub(super) fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
