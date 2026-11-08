use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::{Entry, Stamp, check_cancel, invalid, read_bounded};
use crate::private_files::{Directory, FileStamp};

const LIBRARY: &str = "terminal-log-badges.json";
pub(super) const MAX_BYTES: u64 = 256 * 1024;
pub(in crate::logfile_window) const MAX_BADGES: usize = 256;
static MUTATIONS: Mutex<()> = Mutex::new(());

pub(super) fn mutation_lock() -> io::Result<MutexGuard<'static, ()>> {
    MUTATIONS
        .lock()
        .map_err(|_| io::Error::other("The logfile badge store is busy"))
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum Style {
    #[default]
    Soft,
    Outline,
    Solid,
    Flag,
    Minimal,
}

impl Style {
    pub(in crate::logfile_window) const ALL: [Self; 5] = [
        Self::Soft,
        Self::Outline,
        Self::Solid,
        Self::Flag,
        Self::Minimal,
    ];
}

impl fmt::Display for Style {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Soft => "Soft",
            Self::Outline => "Outline",
            Self::Solid => "Solid",
            Self::Flag => "Flag",
            Self::Minimal => "Minimal",
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Badge {
    pub(in crate::logfile_window) id: String,
    pub(in crate::logfile_window) name: String,
    pub(in crate::logfile_window) icon: String,
    pub(in crate::logfile_window) color: String,
    pub(in crate::logfile_window) style: Style,
}

impl Badge {
    fn valid(&self) -> bool {
        !self.id.trim().is_empty()
            && self.id.len() <= 400
            && !self.name.trim().is_empty()
            && self.name.chars().count() <= 40
            && !self.name.chars().any(char::is_control)
            && !self.icon.trim().is_empty()
            && self.icon.len() <= 64
            && self.color.len() == 7
            && self.color.starts_with('#')
            && self.color[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    pub(in crate::logfile_window) fn held(&self) -> bool {
        self.id == "hold"
    }

    pub(in crate::logfile_window) fn defaults() -> [Self; 6] {
        [
            ("hold", "Hold", "security", "#c08020"),
            ("review", "Review", "search", "#4488cc"),
            ("incident", "Incident", "warning", "#cc5555"),
            ("important", "Important", "star", "#9966cc"),
            ("done", "Done", "check", "#339977"),
            ("share", "Share", "export", "#3399aa"),
        ]
        .map(|(id, name, icon, color)| Self {
            id: id.into(),
            name: name.into(),
            icon: icon.into(),
            color: color.into(),
            style: Style::Soft,
        })
    }

    pub(super) fn hold() -> Self {
        let [hold, ..] = Self::defaults();
        hold
    }
}

pub(super) fn decode_badges(bytes: Option<Vec<u8>>) -> io::Result<Vec<Badge>> {
    let Some(bytes) = bytes else {
        return Ok(Vec::new());
    };
    let badges: Vec<Badge> = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    validate(&badges)?;
    Ok(badges)
}

fn validate(badges: &[Badge]) -> io::Result<()> {
    if badges.len() > MAX_BADGES || badges.iter().any(|badge| !badge.valid()) {
        return Err(invalid("Unreadable logfile badge metadata"));
    }
    Ok(())
}

fn encode(badges: &[Badge]) -> io::Result<Vec<u8>> {
    validate(badges)?;
    let bytes = serde_json::to_vec(badges).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("The logfile badges exceed the size limit"));
    }
    Ok(bytes)
}

#[derive(Debug)]
pub(in crate::logfile_window) struct Target {
    path: PathBuf,
    filename: String,
    sidecar: String,
    stamp: Stamp,
    active: bool,
}

impl Target {
    pub(in crate::logfile_window) fn from_entry(entry: &Entry) -> Self {
        Self {
            path: entry.path.clone(),
            filename: entry.filename().to_owned(),
            sidecar: entry.badge_name(),
            stamp: entry.stamp.clone(),
            active: entry.live,
        }
    }

    fn folder(&self) -> io::Result<&Path> {
        self.path
            .parent()
            .ok_or_else(|| invalid("The logfile has no parent folder"))
    }

    fn verify(&self, directory: &Directory) -> io::Result<()> {
        let stamp = Stamp::new(&directory.open_file(&self.filename)?)?;
        if stamp == self.stamp
            || (self.active && stamp.same_file(&self.stamp) && stamp.length >= self.stamp.length)
        {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} changed. Refresh the list before applying badges.",
                    self.filename
                ),
            ))
        }
    }
}

#[derive(Debug)]
struct Group {
    targets: Vec<Target>,
    version: Option<FileStamp>,
}

impl Group {
    fn first(&self) -> &Target {
        &self.targets[0]
    }

    fn check(&self, directory: &Directory) -> io::Result<()> {
        require_same_directory(directory, self.first().folder()?)?;
        self.verify_targets(directory)?;
        check_version(directory, &self.first().sidecar, self.version.as_ref())
    }

    fn verify_targets(&self, directory: &Directory) -> io::Result<()> {
        for target in &self.targets {
            target.verify(directory)?;
        }
        Ok(())
    }

    fn publish(&self, directory: &Directory, bytes: &[u8]) -> io::Result<()> {
        publish_change(
            directory,
            self.first().folder()?,
            &self.first().sidecar,
            self.version.as_ref(),
            bytes,
            || self.verify_targets(directory),
        )
    }
}

pub(crate) struct Prepared {
    root: PathBuf,
    // Keep one handle per reviewed folder so its identity cannot be recycled before Apply.
    directories: BTreeMap<PathBuf, Directory>,
    groups: Vec<Group>,
    library_version: Option<FileStamp>,
    pub(in crate::logfile_window) library: Vec<Badge>,
    pub(in crate::logfile_window) first: Vec<Badge>,
    pub(in crate::logfile_window) selected_count: usize,
    pub(in crate::logfile_window) held_count: usize,
}

impl fmt::Debug for Prepared {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Prepared")
            .field("selected_count", &self.selected_count)
            .field("held_count", &self.held_count)
            .finish_non_exhaustive()
    }
}

pub(in crate::logfile_window) fn prepare(
    root: &Path,
    targets: Vec<Target>,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Prepared> {
    let _guard = mutation_lock()?;
    let mut directories = BTreeMap::new();
    let (library_version, library) = match Directory::open(root, false) {
        Ok(directory) => {
            let library = snapshot(&directory, LIBRARY)?;
            directories.insert(root.to_path_buf(), directory);
            library
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (None, Vec::new()),
        Err(error) => return Err(error),
    };
    let selected_count = targets.len();
    let mut held_count = 0;
    let mut first = Vec::new();
    let mut groups = BTreeMap::<_, (Group, bool)>::new();
    for (index, target) in targets.into_iter().enumerate() {
        check_cancel(&cancelled)?;
        let folder = target.folder()?;
        let directory = match directories.entry(folder.to_path_buf()) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Directory::open(folder, false)?)
            }
        };
        target.verify(directory)?;
        let path = folder.join(&target.sidecar);
        match groups.entry(path) {
            std::collections::btree_map::Entry::Occupied(mut existing) => {
                let (group, held) = existing.get_mut();
                held_count += usize::from(*held);
                group.targets.push(target);
            }
            std::collections::btree_map::Entry::Vacant(vacant) => {
                let (version, badges) = snapshot(directory, &target.sidecar)?;
                let held = badges.iter().any(Badge::held);
                held_count += usize::from(held);
                if index == 0 {
                    first = badges;
                }
                vacant.insert((
                    Group {
                        targets: vec![target],
                        version,
                    },
                    held,
                ));
            }
        }
    }
    Ok(Prepared {
        root: root.to_path_buf(),
        directories,
        groups: groups.into_values().map(|(group, _)| group).collect(),
        library_version,
        library,
        first,
        selected_count,
        held_count,
    })
}

pub(in crate::logfile_window) struct Outcome {
    applied: usize,
    total: usize,
    problems: Vec<String>,
    cancelled: bool,
}

impl Outcome {
    pub(in crate::logfile_window) fn notice(self) -> String {
        let mut status = format!(
            "Applied badges to {} of {} logfiles.",
            self.applied, self.total
        );
        if self.cancelled {
            status.push_str(" The remaining changes were cancelled.");
        }
        if !self.problems.is_empty() {
            status.push(' ');
            status.push_str(&self.problems.join(" · "));
        }
        status
    }
}

impl Prepared {
    fn directory(&self, group: &Group) -> io::Result<&Directory> {
        self.directories
            .get(group.first().folder()?)
            .ok_or_else(|| invalid("The logfile badge folder was not reviewed"))
    }

    pub(in crate::logfile_window) fn apply(
        &self,
        root: &Path,
        library: &[Badge],
        selected: &[Badge],
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<Outcome> {
        if root != self.root {
            return Err(invalid(
                "The logfile badge profile changed. Reopen Badges before applying them.",
            ));
        }
        let library_bytes = encode(library)?;
        let bytes = encode(selected)?;
        let _guard = mutation_lock()?;
        let created;
        let directory = match self.directories.get(root) {
            Some(directory) => directory,
            None => {
                created = Directory::open(root, true)?;
                &created
            }
        };
        require_same_directory(directory, root)?;
        check_version(directory, LIBRARY, self.library_version.as_ref())?;

        // Recheck every log before any writes. Each publication also rechecks at its rename.
        for group in &self.groups {
            check_cancel(&cancelled)?;
            group.check(self.directory(group)?)?;
        }
        check_cancel(&cancelled)?;
        publish_change(
            directory,
            root,
            LIBRARY,
            self.library_version.as_ref(),
            &library_bytes,
            || Ok(()),
        )?;
        let mut outcome = Outcome {
            applied: 0,
            total: self.selected_count,
            problems: Vec::new(),
            cancelled: false,
        };
        for group in &self.groups {
            if cancelled() {
                outcome.cancelled = true;
                break;
            }
            match self
                .directory(group)
                .and_then(|directory| group.publish(directory, &bytes))
            {
                Ok(()) => outcome.applied += group.targets.len(),
                Err(error) if outcome.problems.len() < 3 => outcome
                    .problems
                    .push(format!("{}: {error}", group.first().filename)),
                Err(_) => {}
            }
        }
        Ok(outcome)
    }
}

pub(super) fn set_hold(entry: &Entry, held: bool) -> io::Result<()> {
    let _guard = mutation_lock()?;
    let source = entry.require_closed()?;
    let target = Target::from_entry(entry);
    let folder = target.folder()?;
    let directory = Directory::open(folder, false)?;
    target.verify(&directory)?;
    let (version, mut badges) = snapshot(&directory, &target.sidecar)?;
    if held && !badges.iter().any(Badge::held) {
        if badges.len() == MAX_BADGES {
            return Err(invalid("The logfile already has 256 badges"));
        }
        badges.push(Badge::hold());
    } else if !held {
        badges.retain(|badge| !badge.held());
    }
    let bytes = encode(&badges)?;
    publish_change(
        &directory,
        folder,
        &target.sidecar,
        version.as_ref(),
        &bytes,
        || {
            source.verify()?;
            target.verify(&directory)?;
            if entry.is_live_where(SystemTime::now(), |name| {
                source.sibling_exists_or_uncertain(std::ffi::OsStr::new(name))
            }) {
                return Err(invalid("The log became active before Hold could change"));
            }
            Ok(())
        },
    )
}

fn snapshot(directory: &Directory, name: &str) -> io::Result<(Option<FileStamp>, Vec<Badge>)> {
    let file = match directory.open_file(name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((None, Vec::new())),
        Err(error) => return Err(error),
    };
    let stamp = FileStamp::read(&file)?;
    let bytes = read_bounded(&file, MAX_BYTES)?;
    stamp.require_unchanged(&file)?;
    check_version(directory, name, Some(&stamp))?;
    Ok((Some(stamp), decode_badges(Some(bytes))?))
}

fn check_version(
    directory: &Directory,
    name: &str,
    expected: Option<&FileStamp>,
) -> io::Result<()> {
    let actual = match directory.open_file(name) {
        Ok(file) => Some(FileStamp::read(&file)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if actual.as_ref() == expected {
        Ok(())
    } else {
        Err(invalid(
            "Logfile badges changed. Reopen Badges before applying them.",
        ))
    }
}

fn require_same_directory(directory: &Directory, path: &Path) -> io::Result<()> {
    if !directory.same_as(&Directory::open(path, false)?)? {
        return Err(invalid(
            "The logfile badge folder changed before publication",
        ));
    }
    Ok(())
}

fn publish_change(
    directory: &Directory,
    path: &Path,
    name: &str,
    expected: Option<&FileStamp>,
    bytes: &[u8],
    check: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let verify = || {
        require_same_directory(directory, path)?;
        check()?;
        check_version(directory, name, expected)
    };
    let write = |file: &mut std::fs::File| file.write_all(bytes);
    if expected.is_some() {
        directory.replace_with(name, write, verify)
    } else {
        directory.publish_checked(name, write, verify)
    }
}
