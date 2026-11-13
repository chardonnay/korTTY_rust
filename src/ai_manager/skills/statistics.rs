//! Private counters for the skills included in successful model requests.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs::TryLockError;
use std::io::{self, Read as _, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

use jiff::{Span, Zoned, civil::Date};
use serde::{Deserialize, Serialize};

use crate::local_ai::Tokenizer;
use crate::private_files::{Directory, FileLock, FileStamp, require_single_link};

mod report;
pub(crate) use report::{Current, Granularity, Hint, Range, Report};

#[cfg(test)]
mod tests;

const FOLDER: &str = "stats";
const FILE: &str = "skill-usage.json";
const LOCK: &str = ".skill-usage.lock";
const VERSION: u8 = 1;
pub(in crate::ai_manager) const MAX_BYTES: usize = 64 * 1024 * 1024;
pub(in crate::ai_manager) const BACKUP_PATH: &str = "stats/skill-usage.json";
const MAX_RECORDED_SKILLS: usize = 4096;
const MAX_TOKENS: u64 = 1_048_576;
const MAX_NAME_BYTES: usize = 256;
const DAILY_RETENTION: i64 = 400;
const MONTHLY_RETENTION: i64 = 60;
const SNAPSHOT_RETENTION: i64 = 90;
const LOCK_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Surface {
    Chat,
    Agent,
    Workflow,
}

impl fmt::Display for Surface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Chat => "Chat",
            Self::Agent => "Agent",
            Self::Workflow => "Workflow",
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Observation {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) tokens: u64,
    pub(crate) tokenizer: Tokenizer,
    pub(crate) fingerprint: [u8; 32],
}

#[derive(Clone, Debug)]
pub(crate) struct Usage {
    pub(crate) skill: Observation,
    pub(crate) pinned: bool,
    /// A partial prompt counts towards usage but cannot replace a complete size snapshot.
    pub(crate) full_size: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Ledger {
    version: u8,
    tokenizer: Option<Tokenizer>,
    skills: BTreeMap<u64, SkillRecord>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            version: VERSION,
            tokenizer: None,
            skills: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SkillRecord {
    name: String,
    last_used: Option<String>,
    last_size: Option<SizeIdentity>,
    daily: BTreeMap<String, Bucket>,
    monthly: BTreeMap<String, Bucket>,
    yearly: BTreeMap<String, Bucket>,
    snapshots: Vec<Snapshot>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Bucket {
    pub(crate) automatic: u64,
    pub(crate) pinned: u64,
    pub(crate) tokens: u64,
    pub(crate) surfaces: BTreeMap<Surface, u64>,
}

impl Bucket {
    fn add(&mut self, other: &Self) {
        self.automatic = self.automatic.saturating_add(other.automatic);
        self.pinned = self.pinned.saturating_add(other.pinned);
        self.tokens = self.tokens.saturating_add(other.tokens);
        for (&surface, &count) in &other.surfaces {
            let stored = self.surfaces.entry(surface).or_default();
            *stored = stored.saturating_add(count);
        }
    }

    fn record(&mut self, surface: Surface, usage: &Usage) {
        let counter = if usage.pinned {
            &mut self.pinned
        } else {
            &mut self.automatic
        };
        *counter = counter.saturating_add(1);
        self.tokens = self.tokens.saturating_add(usage.skill.tokens);
        let counter = self.surfaces.entry(surface).or_default();
        *counter = counter.saturating_add(1);
    }

    fn uses(&self) -> u64 {
        self.automatic.saturating_add(self.pinned)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SizeIdentity {
    fingerprint: [u8; 32],
    tokenizer: Tokenizer,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    date: String,
    tokens: u64,
    identity: SizeIdentity,
}

impl Ledger {
    pub(crate) fn tokenizer(&self) -> Tokenizer {
        self.tokenizer.unwrap_or_default()
    }

    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION || self.skills.len() > MAX_RECORDED_SKILLS {
            return Err(invalid(
                "The skill statistics have an invalid version or too many skills.",
            ));
        }
        for (&id, record) in &self.skills {
            if id == 0
                || record.name.is_empty()
                || record.name.len() > MAX_NAME_BYTES
                || record.name.chars().any(char::is_control)
                || record
                    .last_used
                    .as_deref()
                    .is_some_and(|date| day(date).is_none())
                || record.daily.keys().any(|key| day(key).is_none())
                || record.monthly.keys().any(|key| month(key).is_none())
                || record.yearly.keys().any(|key| year(key).is_none())
                || record
                    .snapshots
                    .iter()
                    .any(|snapshot| snapshot.tokens > MAX_TOKENS || day(&snapshot.date).is_none())
            {
                return Err(invalid("The skill statistics contain an invalid record."));
            }
        }
        Ok(())
    }

    fn record_at(&mut self, surface: Surface, usage: &[Usage], today: Date) -> io::Result<bool> {
        let date = today.to_string();
        let mut seen = HashSet::new();
        for usage in usage.iter().filter(|usage| seen.insert(usage.skill.id)) {
            let record = self.skill(&usage.skill)?;
            record.last_used = Some(
                record
                    .last_used
                    .take()
                    .filter(|last| last > &date)
                    .unwrap_or_else(|| date.clone()),
            );
            record
                .daily
                .entry(date.clone())
                .or_default()
                .record(surface, usage);
            if usage.full_size {
                record.observe(&usage.skill, &date);
            }
            self.tokenizer = Some(usage.skill.tokenizer);
        }
        Ok(!seen.is_empty())
    }

    fn observe_at(&mut self, observations: &[Observation], today: Date) -> io::Result<bool> {
        let date = today.to_string();
        let mut changed = false;
        for observation in observations {
            let name = display_name(&observation.name);
            changed |= self
                .skills
                .get(&observation.id)
                .is_none_or(|record| record.name != name);
            changed |= self.skill(observation)?.observe(observation, &date);
        }
        Ok(changed)
    }

    fn skill(&mut self, observation: &Observation) -> io::Result<&mut SkillRecord> {
        let name = display_name(&observation.name);
        if observation.id == 0 || name.is_empty() || observation.tokens > MAX_TOKENS {
            return Err(invalid("The skill statistics observation is invalid."));
        }
        if !self.skills.contains_key(&observation.id) && self.skills.len() >= MAX_RECORDED_SKILLS {
            return Err(invalid(
                "The skill statistics contain too many historical skills.",
            ));
        }
        let record = self.skills.entry(observation.id).or_default();
        record.name = name;
        Ok(record)
    }

    fn roll_up(&mut self, today: Date) {
        let daily = today
            .saturating_sub(Span::new().days(DAILY_RETENTION))
            .to_string();
        let monthly = month_key(
            today
                .first_of_month()
                .saturating_sub(Span::new().months(MONTHLY_RETENTION)),
        );
        let snapshots = today.saturating_sub(Span::new().days(SNAPSHOT_RETENTION));
        for record in self.skills.values_mut() {
            record.daily.retain(|key, bucket| {
                if key <= &daily {
                    record
                        .monthly
                        .entry(key[..7].to_owned())
                        .or_default()
                        .add(bucket);
                    false
                } else {
                    true
                }
            });
            record.monthly.retain(|key, bucket| {
                if key <= &monthly {
                    record
                        .yearly
                        .entry(key[..4].to_owned())
                        .or_default()
                        .add(bucket);
                    false
                } else {
                    true
                }
            });
            record
                .snapshots
                .sort_by(|left, right| left.date.cmp(&right.date));
            let mut older = BTreeMap::new();
            let mut recent = Vec::new();
            for snapshot in std::mem::take(&mut record.snapshots) {
                if let Some(date) = day(&snapshot.date) {
                    if date <= snapshots {
                        let week = date.iso_week_date();
                        older.insert((week.year(), week.week()), snapshot);
                    } else {
                        recent.push(snapshot);
                    }
                }
            }
            record.snapshots = older.into_values().chain(recent).collect();
        }
    }
}

impl SkillRecord {
    fn observe(&mut self, observation: &Observation, date: &str) -> bool {
        let identity = SizeIdentity {
            fingerprint: observation.fingerprint,
            tokenizer: observation.tokenizer,
        };
        if self.last_size == Some(identity) {
            return false;
        }
        self.last_size = Some(identity);
        if !self
            .snapshots
            .iter()
            .any(|snapshot| snapshot.date == date && snapshot.identity == identity)
        {
            self.snapshots.push(Snapshot {
                date: date.into(),
                tokens: observation.tokens,
                identity,
            });
        }
        true
    }
}

/// Adds one committed model request. Repeated skills within the request count once.
pub(crate) fn record(
    root: &Path,
    surface: Surface,
    usage: &[Usage],
    allowed: impl Fn() -> bool,
) -> io::Result<()> {
    if usage.is_empty() {
        return Ok(());
    }
    transact(root, allowed, |ledger, today| {
        ledger.record_at(surface, usage, today)
    })
}

/// Adds changed complete skill sizes without counting uses.
pub(crate) fn observe(
    root: &Path,
    skills: &[Observation],
    allowed: impl Fn() -> bool,
) -> io::Result<()> {
    if skills.is_empty() {
        return Ok(());
    }
    transact(root, allowed, |ledger, today| {
        ledger.observe_at(skills, today)
    })
}

/// Reads a consistent view of the local ledger without creating any files.
pub(crate) fn read(root: &Path) -> io::Result<Ledger> {
    let Some(directory) = open_directory(root, false)? else {
        return Ok(Ledger::default());
    };
    let mut ledger = load(&directory)?
        .map(|(ledger, _)| ledger)
        .unwrap_or_default();
    ledger.roll_up(Zoned::now().date());
    Ok(ledger)
}

/// An explicit reset also works when the old content cannot be decoded.
pub(crate) fn reset(root: &Path) -> io::Result<()> {
    let Some(directory) = open_directory(root, false)? else {
        return Ok(());
    };
    let _lock = lock(&directory)?;
    let Some(file) = open_ledger(&directory)? else {
        return Ok(());
    };
    let stamp = FileStamp::read(&file)?;
    drop(file);
    directory.remove_unchanged(FILE, &stamp)?;
    directory.sync()
}

fn transact(
    root: &Path,
    allowed: impl Fn() -> bool,
    apply: impl FnOnce(&mut Ledger, Date) -> io::Result<bool>,
) -> io::Result<()> {
    if !allowed() {
        return Ok(());
    }
    let directory = open_directory(root, true)?
        .ok_or_else(|| invalid("The skill statistics directory could not be created."))?;
    let _lock = lock(&directory)?;
    if !allowed() {
        return Ok(());
    }
    let (mut ledger, stamp) = load(&directory)?.map_or_else(
        || (Ledger::default(), None),
        |(ledger, stamp)| (ledger, Some(stamp)),
    );
    let today = Zoned::now().date();
    if !apply(&mut ledger, today)? {
        return Ok(());
    }
    ledger.roll_up(today);
    let bytes = serde_json::to_vec(&ledger).map_err(io::Error::other)?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid("The skill statistics exceed their size limit."));
    }
    let permitted = || {
        if allowed() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Skill statistics recording was disabled.",
            ))
        }
    };
    if let Some(stamp) = &stamp {
        let check = || {
            permitted()?;
            let file = open_ledger(&directory)?.ok_or_else(|| {
                invalid("The skill statistics file changed before it could be saved.")
            })?;
            stamp.require_unchanged(&file)
        };
        #[cfg(windows)]
        {
            directory.publish_bytes_with_retry(FILE, &bytes, Some(check))
        }
        #[cfg(not(windows))]
        {
            directory.replace_with(FILE, |file| file.write_all(&bytes), check)
        }
    } else {
        directory.publish_checked(FILE, |file| file.write_all(&bytes), permitted)
    }
}

fn open_directory(root: &Path, create: bool) -> io::Result<Option<Directory>> {
    match retry_windows(|| Directory::open(root, false)?.private_child(FOLDER, create)) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if !create && error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn open_ledger(directory: &Directory) -> io::Result<Option<std::fs::File>> {
    let file = match retry_windows(|| directory.open_private_file(FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    require_single_link(&file)?;
    Ok(Some(file))
}

fn load(directory: &Directory) -> io::Result<Option<(Ledger, FileStamp)>> {
    let Some(file) = open_ledger(directory)? else {
        return Ok(None);
    };
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Vec::new();
    (&file).take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    stamp.require_unchanged(&file)?;
    decode(&bytes).map(|ledger| Some((ledger, stamp)))
}

pub(in crate::ai_manager) fn decode(bytes: &[u8]) -> io::Result<Ledger> {
    if bytes.len() > MAX_BYTES {
        return Err(invalid("The skill statistics exceed their size limit."));
    }
    let ledger: Ledger = serde_json::from_slice(bytes)
        .map_err(|_| invalid("The skill statistics have an invalid format."))?;
    ledger.validate()?;
    Ok(ledger)
}

fn lock(directory: &Directory) -> io::Result<FileLock> {
    let file = retry_windows(|| directory.open_lock(LOCK))?;
    require_single_link(&file)?;
    let end = Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(FileLock::new(file)),
            Err(TryLockError::WouldBlock) if Instant::now() < end => {
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "Another process is updating the skill statistics.",
                ));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
}

fn retry_windows<T>(mut open: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    // An exclusive creator handle can briefly block the other process on first use.
    #[cfg(windows)]
    for delay in [5, 15, 30, 60, 120] {
        match open() {
            Err(error) if error.raw_os_error() == Some(32) => {
                std::thread::sleep(Duration::from_millis(delay));
            }
            result => return result,
        }
    }
    open()
}

fn display_name(name: &str) -> String {
    let mut safe = String::new();
    for ch in name.trim().chars().filter(|ch| !ch.is_control()) {
        if safe.len() + ch.len_utf8() > MAX_NAME_BYTES {
            break;
        }
        safe.push(ch);
    }
    safe
}

fn month_key(date: Date) -> String {
    format!("{:04}-{:02}", date.year(), date.month())
}

fn day(key: &str) -> Option<Date> {
    if key.len() != 10 {
        return None;
    }
    let date: Date = key.parse().ok()?;
    (date.year() > 0 && date.to_string() == key).then_some(date)
}

fn month(key: &str) -> Option<Date> {
    (key.len() == 7)
        .then(|| day(&format!("{key}-01")))
        .flatten()
}

fn year(key: &str) -> Option<Date> {
    (key.len() == 4)
        .then(|| day(&format!("{key}-01-01")))
        .flatten()
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}
