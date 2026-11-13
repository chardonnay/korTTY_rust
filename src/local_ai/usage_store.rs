//! Private per-profile usage periods and idempotent request reservations.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::fs::{File, TryLockError};
#[cfg(not(windows))]
use std::io::Write as _;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{
    Mutex, PoisonError,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use kortty_jobs::{ProfileRef, valid_policy_ai_profile_id};
use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::Usage;
use crate::private_files::{Directory, FileStamp, nonce, require_single_link};

const FOLDER: &str = "ai-usage";
const VERSION: u8 = 1;
const POLICY_VERSION: u8 = 2;
const MAX_BYTES: usize = 128 * 1024;
const MAX_CYCLES: usize = 16;
const MAX_PENDING: usize = 64;
const MAX_DAY: i32 = 4_500_000;
const LOCK_WAIT: Duration = Duration::from_secs(2);
const RESERVATION_LIFETIME: i64 = 25 * 60 * 60;
const MAX_RATE: u64 = 1_000_000_000_000;
const MAX_ISSUES: usize = 256;
static HAS_NOTICE: AtomicBool = AtomicBool::new(false);
static ISSUES: Mutex<VecDeque<Issue>> = Mutex::new(VecDeque::new());

struct Issue {
    root: PathBuf,
    profile: ProfileRef,
    pending: bool,
}

pub(crate) fn take_notice(root: &Path) -> bool {
    if !HAS_NOTICE.load(Ordering::Acquire) {
        return false;
    }
    let mut issues = ISSUES.lock().unwrap_or_else(PoisonError::into_inner);
    let mut notify = false;
    for issue in issues.iter_mut().filter(|issue| issue.root == root) {
        notify |= std::mem::take(&mut issue.pending);
    }
    HAS_NOTICE.store(issues.iter().any(|issue| issue.pending), Ordering::Release);
    notify
}

#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Budget {
    pub(crate) token_limit: Option<u64>,
    pub(crate) reset_days: u16,
    pub(crate) yellow_percent: u8,
    pub(crate) red_percent: u8,
    pub(crate) input_micros_per_million: Option<u64>,
    pub(crate) output_micros_per_million: Option<u64>,
    pub(crate) currency: String,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            token_limit: None,
            reset_days: 30,
            yellow_percent: 75,
            red_percent: 90,
            input_micros_per_million: None,
            output_micros_per_million: None,
            currency: "EUR".into(),
        }
    }
}

impl Budget {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.token_limit == Some(0)
            || !(1..=3660).contains(&self.reset_days)
            || self.yellow_percent > self.red_percent
            || self.red_percent > 100
            || [
                self.input_micros_per_million,
                self.output_micros_per_million,
            ]
            .into_iter()
            .flatten()
            .any(|rate| rate > MAX_RATE)
            || self.currency.len() != 3
            || !self.currency.bytes().all(|byte| byte.is_ascii_uppercase())
        {
            return Err("Enter a valid AI usage period, warning percentages and prices.".into());
        }
        Ok(())
    }

    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub(crate) fn estimated_cost(&self, counts: Counters) -> Option<Cost> {
        let input = self.input_micros_per_million;
        let output = self.output_micros_per_million;
        if input.is_none() && output.is_none() {
            return None;
        }
        let numerator = u128::from(counts.input_tokens) * u128::from(input.unwrap_or(0))
            + u128::from(counts.output_tokens) * u128::from(output.unwrap_or(0));
        let partial = counts.missing_input != 0
            || counts.missing_output != 0
            || counts.interrupted != 0
            || (counts.input_tokens != 0 && input.is_none())
            || (counts.output_tokens != 0 && output.is_none());
        Some(Cost { numerator, partial })
    }

    pub(crate) fn warning(&self, snapshot: Snapshot, additional_output: u32) -> Option<Warning> {
        self.warning_at(snapshot.projected_tokens(0, u64::from(additional_output)))
    }

    fn warning_at(&self, used: u128) -> Option<Warning> {
        let limit = u128::from(self.token_limit?);
        if used * 100 >= limit * u128::from(self.red_percent) {
            Some(Warning::Red)
        } else if used * 100 >= limit * u128::from(self.yellow_percent) {
            Some(Warning::Yellow)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Warning {
    Yellow,
    Red,
}

pub(crate) struct Cost {
    /// One millionth of a millionth of the selected currency.
    pub(crate) numerator: u128,
    pub(crate) partial: bool,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Account {
    root: PathBuf,
    profile: ProfileRef,
    stem: String,
    legacy_stem: Option<String>,
    budget: Budget,
}

impl Account {
    pub(super) fn budget(&self) -> &Budget {
        &self.budget
    }

    pub(crate) fn new(root: PathBuf, profile: u64, budget: Budget) -> Result<Self, String> {
        Self::with_identity(
            root,
            ProfileRef::Numeric(profile),
            profile.to_string(),
            None,
            budget,
        )
    }

    pub(crate) fn policy(
        root: PathBuf,
        id: &str,
        legacy_alias: Option<u64>,
        budget: Budget,
    ) -> Result<Self, String> {
        if !valid_policy_ai_profile_id(id) || legacy_alias.is_some_and(|alias| alias < 1 << 63) {
            return Err("The saved AI profile identity is unavailable.".into());
        }
        let mut hasher = HashAlgorithm::Sha256
            .new_hasher()
            .expect("SHA-256 is supported");
        hasher.update(b"kortty policy profile usage account v1\0");
        hasher.update(id.as_bytes());
        let mut stem = String::from("policy-usage-v1-");
        for byte in hasher.finalize() {
            write!(stem, "{byte:02x}").expect("writing to a string succeeds");
        }
        Self::with_identity(
            root,
            ProfileRef::Policy(id.to_owned()),
            stem,
            legacy_alias.map(|alias| alias.to_string()),
            budget,
        )
    }

    fn with_identity(
        root: PathBuf,
        profile: ProfileRef,
        stem: String,
        legacy_stem: Option<String>,
        budget: Budget,
    ) -> Result<Self, String> {
        if !root.is_absolute() || !profile.is_nonzero() {
            return Err("The saved AI profile identity is unavailable.".into());
        }
        budget.validate()?;
        Ok(Self {
            root,
            profile,
            stem,
            legacy_stem,
            budget,
        })
    }

    pub(crate) fn snapshot(&self) -> io::Result<Snapshot> {
        self.snapshot_at(Clock::now())
    }

    pub(super) fn has_quota(&self) -> bool {
        self.budget.token_limit.is_some()
    }

    pub(super) fn quota_forecast(
        &self,
        estimated_input: u64,
        output_limit: u64,
    ) -> Result<String, String> {
        let snapshot = self
            .snapshot()
            .map_err(|_| "Recorded usage is unavailable for the quota forecast.".to_owned())?;
        let counts = snapshot.counts;
        let reported = snapshot.reported_tokens();
        let limit = self
            .budget
            .token_limit
            .expect("quota forecast needs a quota");
        let projected = snapshot.projected_tokens(estimated_input, output_limit);
        let remaining = u128::from(limit).saturating_sub(projected);
        let warning = match self.budget.warning_at(projected) {
            Some(Warning::Red) => "Second quota warning · ",
            Some(Warning::Yellow) => "First quota warning · ",
            None => "",
        };
        let partial = if snapshot.recording_issue
            || counts.missing_input != 0
            || counts.missing_output != 0
            || counts.interrupted != 0
        {
            " Recorded usage is incomplete."
        } else {
            ""
        };
        Ok(format!(
            "{warning}{reported} of {limit} tokens reported · {} pending output tokens reserved · about {estimated_input} input tokens and up to {output_limit} output tokens · {remaining} tokens projected remaining.{partial}",
            snapshot.reserved_output
        ))
    }

    pub(super) fn note_issue(&self) {
        let mut issues = ISSUES.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(index) = issues
            .iter()
            .position(|issue| issue.profile == self.profile && issue.root == self.root)
        {
            issues.remove(index);
        }
        if issues.len() == MAX_ISSUES {
            issues.pop_front();
        }
        issues.push_back(Issue {
            root: self.root.clone(),
            profile: self.profile.clone(),
            pending: true,
        });
        HAS_NOTICE.store(true, Ordering::Release);
    }

    fn snapshot_at(&self, now: Clock) -> io::Result<Snapshot> {
        let mut ledger = match self.directory(false) {
            Ok(directory) => self.read_snapshot(&directory)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        }
        .unwrap_or_else(|| Ledger::new(now.day));
        ledger.expire(now.seconds)?;
        let mut snapshot = ledger.snapshot(self.budget.reset_days, now.day);
        snapshot.recording_issue = ISSUES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|issue| issue.profile == self.profile && issue.root == self.root);
        Ok(snapshot)
    }

    pub(super) fn reserve(&self, output_limit: u32) -> io::Result<Reservation> {
        self.reserve_at(output_limit, Clock::now())
    }

    fn reserve_at(&self, output_limit: u32, now: Clock) -> io::Result<Reservation> {
        let (mut reservation, sync_failed) = self.transact(now, |ledger| {
            if ledger
                .cycles
                .values()
                .map(|cycle| cycle.pending.len())
                .sum::<usize>()
                >= MAX_PENDING
            {
                return Err(invalid("Too many AI usage reservations are open."));
            }
            let day = ledger.start(self.budget.reset_days, now.day);
            let id = nonce()?;
            ledger.cycles.entry(day).or_default().pending.insert(
                id.clone(),
                Pending {
                    expires: now.seconds.saturating_add(RESERVATION_LIFETIME),
                    output_limit,
                },
            );
            Ok(Reservation {
                day,
                id,
                sync_failed: false,
            })
        })?;
        reservation.sync_failed = sync_failed;
        Ok(reservation)
    }

    pub(super) fn settle(
        &self,
        reservation: &Reservation,
        outcome: Outcome,
        usage: Option<&Usage>,
    ) -> io::Result<bool> {
        self.settle_at(reservation, outcome, usage, Clock::now())
    }

    fn settle_at(
        &self,
        reservation: &Reservation,
        outcome: Outcome,
        usage: Option<&Usage>,
        now: Clock,
    ) -> io::Result<bool> {
        let (_, sync_failed) = self.transact(now, |ledger| {
            if let Some(cycle) = ledger.cycles.get_mut(&reservation.day)
                && cycle.pending.remove(&reservation.id).is_some()
                && outcome != Outcome::Unsent
            {
                cycle.counts.record(outcome, usage)?;
            }
            Ok(())
        })?;
        Ok(sync_failed)
    }

    fn directory(&self, create: bool) -> io::Result<Directory> {
        open_usage_path(|| Directory::open(&self.root, false)?.private_child(FOLDER, create))
    }

    fn read_snapshot(&self, directory: &Directory) -> io::Result<Option<Ledger>> {
        let ProfileRef::Policy(id) = &self.profile else {
            return Ok(read_numeric(directory, &self.stem)?.map(|(ledger, _)| ledger));
        };
        let legacy = self.legacy_stem.as_deref();
        if legacy.is_none() {
            return Ok(read_named(directory, &self.stem, id)?.map(|(ledger, _)| ledger));
        }
        // The two files may be between migration steps. Read a consistent
        // view without publishing a ledger or a migration marker.
        let _locks = self.lock_files(directory)?;
        let files = self.policy_files(directory, id)?;
        Ok(files
            .named
            .map(|(ledger, _)| ledger)
            .or(match files.legacy {
                Some((LegacyRecord::Numeric(ledger), _)) => Some(ledger),
                Some((LegacyRecord::Policy(marker), _)) => marker.ledger,
                None => None,
            }))
    }

    fn lock_files(&self, directory: &Directory) -> io::Result<(Lock, Option<Lock>)> {
        let named = lock(directory, &self.stem)?;
        let legacy = self
            .legacy_stem
            .as_deref()
            .map(|stem| lock(directory, stem))
            .transpose()?;
        Ok((named, legacy))
    }

    fn policy_files(&self, directory: &Directory, id: &str) -> io::Result<PolicyFiles> {
        let named = read_named(directory, &self.stem, id)?;
        let legacy = self
            .legacy_stem
            .as_deref()
            .map(|stem| read_record_with(directory, stem, LegacyRecord::decode))
            .transpose()?
            .flatten();
        if let Some((record, _)) = &legacy {
            match record {
                LegacyRecord::Numeric(ledger) => {
                    ledger.validate()?;
                    if named.is_some() {
                        return Err(invalid("The AI usage account has two independent ledgers."));
                    }
                }
                LegacyRecord::Policy(marker) => {
                    marker.validate(id)?;
                    match (&marker.ledger, &named) {
                        (None, None) => {
                            return Err(invalid("The migrated AI usage ledger is missing."));
                        }
                        (Some(checkpoint), Some((ledger, _))) if checkpoint != ledger => {
                            return Err(invalid("The AI usage migration checkpoint changed."));
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(PolicyFiles { named, legacy })
    }

    fn policy_transaction_target<'a>(
        &'a self,
        directory: &Directory,
        id: &'a str,
        now: Clock,
    ) -> io::Result<(LedgerTarget<'a>, Option<StampedLedger>)> {
        let target = LedgerTarget {
            stem: &self.stem,
            policy: Some(id),
        };
        let Some(legacy_stem) = self.legacy_stem.as_deref() else {
            return Ok((target, read_named(directory, &self.stem, id)?));
        };
        let PolicyFiles { named, legacy } = self.policy_files(directory, id)?;
        let (record, mut legacy_stamp) = match legacy {
            Some((record, stamp)) => (Some(record), Some(stamp)),
            None => (None, None),
        };
        let marker_prepared = matches!(record, Some(LegacyRecord::Policy(_)));
        let checkpoint = match record {
            Some(LegacyRecord::Numeric(mut ledger)) => {
                ledger.expire(now.seconds)?;
                if ledger
                    .cycles
                    .values()
                    .any(|cycle| !cycle.pending.is_empty())
                {
                    // Older processes can settle reservations while the
                    // same numeric balance remains authoritative.
                    return Ok((
                        LedgerTarget {
                            stem: legacy_stem,
                            policy: None,
                        },
                        legacy_stamp.map(|stamp| (ledger, stamp)),
                    ));
                }
                Some(ledger)
            }
            Some(LegacyRecord::Policy(marker)) => {
                if marker.ledger.is_none() {
                    return Ok((target, named));
                }
                marker.ledger
            }
            None => None,
        };
        let named = if let Some(file) = named {
            Some(file)
        } else {
            let ledger = checkpoint.unwrap_or_else(|| Ledger::new(now.day));
            if !marker_prepared {
                // Keep the complete drained balance in the first durable
                // marker so a crash before publishing the named file can
                // recover without recreating an empty balance.
                write_policy_record(
                    directory,
                    legacy_stem,
                    id,
                    Some(&ledger),
                    legacy_stamp.as_ref(),
                )?;
                directory.sync()?;
                legacy_stamp = Some(read_stamp(directory, legacy_stem)?);
            }
            write_policy_record(directory, target.stem, id, Some(&ledger), None)?;
            directory.sync()?;
            Some((ledger, read_stamp(directory, target.stem)?))
        };
        // Once published, the named balance is authoritative. A final
        // marker with no checkpoint also detects later ledger loss.
        write_policy_record(directory, legacy_stem, id, None, legacy_stamp.as_ref())?;
        directory.sync()?;
        Ok((target, named))
    }

    fn transact<T>(
        &self,
        now: Clock,
        update: impl FnOnce(&mut Ledger) -> io::Result<T>,
    ) -> io::Result<(T, bool)> {
        let directory = self.directory(true)?;
        let _locks = self.lock_files(&directory)?;
        let (target, previous) = match &self.profile {
            ProfileRef::Numeric(_) => (
                LedgerTarget {
                    stem: &self.stem,
                    policy: None,
                },
                read_numeric(&directory, &self.stem)?,
            ),
            ProfileRef::Policy(id) => self.policy_transaction_target(&directory, id, now)?,
        };
        let (mut ledger, previous) = previous.map_or_else(
            || (Ledger::new(now.day), None),
            |(ledger, stamp)| (ledger, Some(stamp)),
        );
        ledger.expire(now.seconds)?;
        let value = update(&mut ledger)?;
        ledger.prune(ledger.start(self.budget.reset_days, now.day));
        ledger.validate()?;
        if let Some(id) = target.policy {
            write_policy_record(
                &directory,
                target.stem,
                id,
                Some(&ledger),
                previous.as_ref(),
            )?;
        } else {
            write_record(&directory, target.stem, &ledger, previous.as_ref())?;
        }
        Ok((value, directory.sync().is_err()))
    }
}

type StampedLedger = (Ledger, FileStamp);

struct LedgerTarget<'a> {
    stem: &'a str,
    policy: Option<&'a str>,
}

struct PolicyFiles {
    named: Option<StampedLedger>,
    legacy: Option<(LegacyRecord, FileStamp)>,
}

enum LegacyRecord {
    Numeric(Ledger),
    Policy(PolicyRecord),
}

impl LegacyRecord {
    fn decode(bytes: &[u8]) -> io::Result<Self> {
        #[derive(Deserialize)]
        struct Version {
            version: u8,
        }

        // Decode the selected schema from the original JSON so its numeric
        // period keys retain serde_json's map-key conversions.
        match decode_record::<Version>(bytes)?.version {
            VERSION => decode_record(bytes).map(Self::Numeric),
            POLICY_VERSION => decode_record(bytes).map(Self::Policy),
            _ => Err(invalid("The AI usage ledger has an unsupported version.")),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PolicyRecord<L = Ledger> {
    version: u8,
    profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ledger: Option<L>,
}

impl PolicyRecord {
    fn validate(&self, id: &str) -> io::Result<()> {
        if self.version != POLICY_VERSION || self.profile != id {
            return Err(invalid(
                "The AI usage ledger belongs to a different policy profile.",
            ));
        }
        if let Some(ledger) = &self.ledger {
            ledger.validate()?;
        }
        Ok(())
    }
}

fn filename(stem: &str) -> String {
    format!("{stem}.json")
}

fn read_stamp(directory: &Directory, stem: &str) -> io::Result<FileStamp> {
    let file = directory.open_private_file(&filename(stem))?;
    require_single_link(&file)?;
    FileStamp::read(&file)
}

fn read_record<T: DeserializeOwned>(
    directory: &Directory,
    stem: &str,
) -> io::Result<Option<(T, FileStamp)>> {
    read_record_with(directory, stem, decode_record)
}

fn read_record_with<T>(
    directory: &Directory,
    stem: &str,
    decode: impl FnOnce(&[u8]) -> io::Result<T>,
) -> io::Result<Option<(T, FileStamp)>> {
    let file = match directory.open_private_file(&filename(stem)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Vec::new();
    (&file).take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    stamp.require_unchanged(&file)?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid("The AI usage ledger exceeds its size limit."));
    }
    Ok(Some((decode(&bytes)?, stamp)))
}

fn decode_record<T: DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    serde_json::from_slice(bytes).map_err(|_| invalid("The AI usage ledger has an invalid format."))
}

fn read_numeric(directory: &Directory, stem: &str) -> io::Result<Option<StampedLedger>> {
    let record = read_record::<Ledger>(directory, stem)?;
    if let Some((ledger, _)) = &record {
        ledger.validate()?;
    }
    Ok(record)
}

fn read_named(directory: &Directory, stem: &str, id: &str) -> io::Result<Option<StampedLedger>> {
    let Some((record, stamp)) = read_record::<PolicyRecord>(directory, stem)? else {
        return Ok(None);
    };
    record.validate(id)?;
    let ledger = record
        .ledger
        .ok_or_else(|| invalid("The named AI usage ledger is missing its balance."))?;
    Ok(Some((ledger, stamp)))
}

fn write_policy_record(
    directory: &Directory,
    stem: &str,
    id: &str,
    ledger: Option<&Ledger>,
    previous: Option<&FileStamp>,
) -> io::Result<()> {
    let record = PolicyRecord {
        version: POLICY_VERSION,
        profile: id.to_owned(),
        ledger,
    };
    write_record(directory, stem, &record, previous)
}

fn write_record(
    directory: &Directory,
    stem: &str,
    record: &impl Serialize,
    previous: Option<&FileStamp>,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid("The AI usage ledger exceeds its size limit."));
    }
    let name = filename(stem);
    let check = |previous: &FileStamp| {
        let current = directory.open_private_file(&name)?;
        require_single_link(&current)?;
        previous.require_unchanged(&current)
    };
    #[cfg(windows)]
    {
        directory.publish_bytes_with_retry(
            &name,
            &bytes,
            previous.map(|stamp| move || check(stamp)),
        )
    }
    #[cfg(not(windows))]
    {
        if let Some(previous) = previous {
            directory.replace_with(&name, |file| file.write_all(&bytes), || check(previous))
        } else {
            directory.publish_copy(&name, &bytes)
        }
    }
}

#[derive(Clone, Copy)]
struct Clock {
    seconds: i64,
    day: i32,
}

impl Clock {
    fn now() -> Self {
        let now = jiff::Zoned::now();
        Self {
            seconds: now.timestamp().as_second(),
            day: (now.date() - jiff::civil::date(1970, 1, 1)).get_days(),
        }
    }
}

pub(super) struct Reservation {
    day: i32,
    id: String,
    pub(super) sync_failed: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Outcome {
    Complete,
    Interrupted,
    Unsent,
}

#[derive(Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Counters {
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) requests: u64,
    pub(crate) interrupted: u64,
    pub(crate) missing_input: u64,
    pub(crate) missing_output: u64,
}

impl Counters {
    fn record(&mut self, outcome: Outcome, usage: Option<&Usage>) -> io::Result<()> {
        let input = usage.and_then(|usage| usage.input_tokens);
        let output = usage.and_then(|usage| usage.output_tokens);
        let add = |left: u64, right| {
            left.checked_add(right)
                .ok_or_else(|| invalid("The AI usage counter is full."))
        };
        *self = Self {
            input_tokens: add(self.input_tokens, input.unwrap_or(0))?,
            output_tokens: add(self.output_tokens, output.unwrap_or(0))?,
            requests: add(self.requests, 1)?,
            interrupted: add(self.interrupted, u64::from(outcome == Outcome::Interrupted))?,
            missing_input: add(self.missing_input, u64::from(input.is_none()))?,
            missing_output: add(self.missing_output, u64::from(output.is_none()))?,
        };
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Snapshot {
    pub(crate) counts: Counters,
    pub(crate) start_day: i32,
    pub(crate) next_day: i32,
    pub(crate) in_flight: usize,
    pub(crate) reserved_output: u64,
    pub(crate) recording_issue: bool,
}

impl Snapshot {
    fn reported_tokens(self) -> u128 {
        u128::from(self.counts.input_tokens) + u128::from(self.counts.output_tokens)
    }

    fn projected_tokens(self, estimated_input: u64, output_limit: u64) -> u128 {
        self.reported_tokens()
            + u128::from(self.reserved_output)
            + u128::from(estimated_input)
            + u128::from(output_limit)
    }
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    version: u8,
    anchor: i32,
    cycles: BTreeMap<i32, Cycle>,
}

#[derive(Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Cycle {
    counts: Counters,
    pending: BTreeMap<String, Pending>,
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    expires: i64,
    output_limit: u32,
}

impl Ledger {
    fn new(anchor: i32) -> Self {
        Self {
            version: VERSION,
            anchor,
            cycles: BTreeMap::new(),
        }
    }

    fn validate(&self) -> io::Result<()> {
        let valid_day = |day| (-MAX_DAY..=MAX_DAY).contains(&day);
        if self.version != VERSION
            || !valid_day(self.anchor)
            || self.cycles.len() > MAX_CYCLES
            || self
                .cycles
                .values()
                .map(|cycle| cycle.pending.len())
                .sum::<usize>()
                > MAX_PENDING
            || self.cycles.iter().any(|(&day, cycle)| {
                !valid_day(day)
                    || day < self.anchor
                    || [
                        cycle.counts.interrupted,
                        cycle.counts.missing_input,
                        cycle.counts.missing_output,
                    ]
                    .into_iter()
                    .any(|count| count > cycle.counts.requests)
                    || cycle.pending.keys().any(|id| {
                        id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
            })
        {
            return Err(invalid("The AI usage ledger contains invalid entries."));
        }
        Ok(())
    }

    fn start(&self, period: u16, today: i32) -> i32 {
        let period = i32::from(period);
        self.anchor + (today - self.anchor).max(0) / period * period
    }

    fn snapshot(&self, period: u16, today: i32) -> Snapshot {
        let start_day = self.start(period, today);
        let cycle = self.cycles.get(&start_day);
        Snapshot {
            counts: cycle.map_or(Counters::default(), |cycle| cycle.counts),
            start_day,
            next_day: start_day + i32::from(period),
            in_flight: cycle.map_or(0, |cycle| cycle.pending.len()),
            reserved_output: cycle.map_or(0, |cycle| {
                cycle
                    .pending
                    .values()
                    .map(|pending| u64::from(pending.output_limit))
                    .sum()
            }),
            recording_issue: false,
        }
    }

    fn expire(&mut self, now: i64) -> io::Result<()> {
        for cycle in self.cycles.values_mut() {
            let before = cycle.pending.len();
            cycle.pending.retain(|_, pending| pending.expires > now);
            for _ in cycle.pending.len()..before {
                cycle.counts.record(Outcome::Interrupted, None)?;
            }
        }
        Ok(())
    }

    fn prune(&mut self, current: i32) {
        while self.cycles.len() > MAX_CYCLES {
            let Some(day) = self.cycles.iter().find_map(|(&day, cycle)| {
                (day != current && cycle.pending.is_empty()).then_some(day)
            }) else {
                break;
            };
            self.cycles.remove(&day);
        }
    }
}

struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn lock(directory: &Directory, stem: &str) -> io::Result<Lock> {
    let name = format!(".{stem}.lock");
    let file = open_usage_path(|| directory.open_lock(&name))?;
    require_single_link(&file)?;
    let end = Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Lock(file)),
            Err(TryLockError::WouldBlock) if Instant::now() < end => {
                std::thread::sleep(Duration::from_millis(15))
            }
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "Another process is updating AI usage.",
                ));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
}

fn open_usage_path<T>(mut open: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    // Windows creates new directories with an exclusive creator handle. A
    // competing first-use reader can briefly receive a sharing violation.
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

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY_ALIAS: u64 = (1 << 63) | 123;
    const POLICY_ID: &str = "policy-Team/分析";
    const MIGRATION_DAY: Clock = Clock {
        seconds: 1,
        day: 10,
    };

    fn test_budget() -> Budget {
        Budget {
            token_limit: Some(100),
            reset_days: 2,
            ..Budget::default()
        }
    }

    fn account(root: &crate::snippets::tests::Profile, id: u64) -> Account {
        Account::new(root.0.clone(), id, test_budget()).unwrap()
    }

    fn policy_account(root: &crate::snippets::tests::Profile, alias: Option<u64>) -> Account {
        Account::policy(root.0.clone(), POLICY_ID, alias, test_budget()).unwrap()
    }

    #[test]
    fn named_usage_preserves_old_reservations_before_retiring_the_decimal_ledger() {
        let root = crate::snippets::tests::Profile::new();
        let legacy = account(&root, POLICY_ALIAS);
        let named = policy_account(&root, Some(POLICY_ALIAS));
        let older = legacy.reserve_at(20, MIGRATION_DAY).unwrap();
        assert_eq!(
            named.snapshot_at(MIGRATION_DAY).unwrap().reserved_output,
            20
        );
        assert!(!root.0.join(FOLDER).join(filename(&named.stem)).exists());
        let newer = named.reserve_at(15, MIGRATION_DAY).unwrap();
        assert_eq!(
            legacy.snapshot_at(MIGRATION_DAY).unwrap().reserved_output,
            35
        );

        let usage = Usage {
            input_tokens: Some(11),
            output_tokens: Some(7),
            ..Usage::default()
        };
        legacy
            .settle_at(&older, Outcome::Complete, Some(&usage), MIGRATION_DAY)
            .unwrap();
        named
            .settle_at(&newer, Outcome::Unsent, None, MIGRATION_DAY)
            .unwrap();
        assert!(!root.0.join(FOLDER).join(filename(&named.stem)).exists());

        let after = named.reserve_at(10, MIGRATION_DAY).unwrap();
        let snapshot = named.snapshot_at(MIGRATION_DAY).unwrap();
        assert_eq!(snapshot.reported_tokens(), 18);
        assert_eq!(snapshot.reserved_output, 10);
        assert!(
            legacy
                .reserve_at(1, MIGRATION_DAY)
                .is_err_and(|error| error.kind() == io::ErrorKind::InvalidData)
        );
        assert_eq!(named.stem.len(), "policy-usage-v1-".len() + 64);
        assert!(named.stem.starts_with("policy-usage-v1-"));
        let old_name = &legacy.stem;
        let record: PolicyRecord = read_record(&named.directory(false).unwrap(), old_name)
            .unwrap()
            .unwrap()
            .0;
        assert_eq!(record.profile, POLICY_ID);
        assert!(record.ledger.is_none());
        named
            .settle_at(&after, Outcome::Unsent, None, MIGRATION_DAY)
            .unwrap();
    }

    #[test]
    fn named_usage_recovers_a_prepared_marker_and_detects_a_missing_committed_file() {
        for named_file_published in [false, true] {
            let root = crate::snippets::tests::Profile::new();
            let legacy = account(&root, POLICY_ALIAS);
            let named = policy_account(&root, Some(POLICY_ALIAS));
            let usage = Usage {
                input_tokens: Some(25),
                output_tokens: Some(15),
                ..Usage::default()
            };
            let before = legacy.reserve_at(15, MIGRATION_DAY).unwrap();
            legacy
                .settle_at(&before, Outcome::Complete, Some(&usage), MIGRATION_DAY)
                .unwrap();
            let directory = legacy.directory(false).unwrap();
            let (ledger, stamp) = read_numeric(&directory, &legacy.stem).unwrap().unwrap();
            write_policy_record(
                &directory,
                &legacy.stem,
                POLICY_ID,
                Some(&ledger),
                Some(&stamp),
            )
            .unwrap();
            if named_file_published {
                write_policy_record(&directory, &named.stem, POLICY_ID, Some(&ledger), None)
                    .unwrap();
            }
            assert_eq!(
                named.snapshot_at(MIGRATION_DAY).unwrap().reported_tokens(),
                40
            );
            assert_eq!(
                root.0.join(FOLDER).join(filename(&named.stem)).exists(),
                named_file_published
            );
            named.reserve_at(20, MIGRATION_DAY).unwrap();
            assert_eq!(
                named.snapshot_at(MIGRATION_DAY).unwrap().reported_tokens(),
                40
            );
            std::fs::remove_file(root.0.join(FOLDER).join(filename(&named.stem))).unwrap();
            assert!(named.snapshot_at(MIGRATION_DAY).is_err());
            assert!(named.reserve_at(1, MIGRATION_DAY).is_err());
            assert!(!root.0.join(FOLDER).join(filename(&named.stem)).exists());
        }
    }

    #[test]
    fn named_usage_without_a_legacy_alias_does_not_read_a_high_user_ledger() {
        let root = crate::snippets::tests::Profile::new();
        let legacy = account(&root, POLICY_ALIAS);
        let named = policy_account(&root, None);
        legacy.reserve_at(50, MIGRATION_DAY).unwrap();
        assert_eq!(named.snapshot_at(MIGRATION_DAY).unwrap().reserved_output, 0);
        named.reserve_at(10, MIGRATION_DAY).unwrap();
        assert_eq!(
            legacy.snapshot_at(MIGRATION_DAY).unwrap().reserved_output,
            50
        );
        assert_eq!(
            named.snapshot_at(MIGRATION_DAY).unwrap().reserved_output,
            10
        );

        let ambiguous = policy_account(&root, Some(POLICY_ALIAS));
        assert!(ambiguous.snapshot_at(MIGRATION_DAY).is_err());
        assert!(ambiguous.reserve_at(1, MIGRATION_DAY).is_err());
        assert_eq!(
            legacy.snapshot_at(MIGRATION_DAY).unwrap().reserved_output,
            50
        );
        assert_eq!(
            named.snapshot_at(MIGRATION_DAY).unwrap().reserved_output,
            10
        );
    }

    #[test]
    fn quota_forecast_uses_input_estimate_and_keeps_reservations_read_only() {
        let root = crate::snippets::tests::Profile::new();
        let account = account(&root, 12);
        let recorded = account.reserve(10).unwrap();
        account
            .settle(
                &recorded,
                Outcome::Complete,
                Some(&Usage {
                    input_tokens: Some(12),
                    output_tokens: Some(8),
                    ..Usage::default()
                }),
            )
            .unwrap();
        let pending = account.reserve(15).unwrap();
        let forecast = account.quota_forecast(25, 20).unwrap();
        assert!(forecast.starts_with("First quota warning"));
        assert!(forecast.contains("about 25 input tokens and up to 20 output tokens"));
        assert!(forecast.contains("20 tokens projected remaining"));
        let exhausted = account.quota_forecast(u64::MAX, 20).unwrap();
        assert!(exhausted.starts_with("Second quota warning"));
        assert!(exhausted.contains("0 tokens projected remaining"));
        let snapshot = account.snapshot().unwrap();
        assert_eq!(
            (
                snapshot.counts.requests,
                snapshot.in_flight,
                snapshot.reserved_output
            ),
            (1, 1, 15)
        );
        account.settle(&pending, Outcome::Unsent, None).unwrap();
    }

    #[test]
    fn quota_projection_counts_both_targets_without_creating_a_ledger() {
        let root = crate::snippets::tests::Profile::new();
        let transport =
            crate::local_ai::Profile::new(crate::local_ai::DEFAULT_ENDPOINT, "quota-test")
                .unwrap()
                .with_usage_account(account(&root, 12))
                .with_output_limit(20);
        let input_estimates = [25_u64, 30];
        let forecast = transport
            .quota_projection(
                input_estimates.iter().sum(),
                u64::from(transport.effective_output_limit()) * input_estimates.len() as u64,
            )
            .unwrap();
        assert!(forecast.starts_with("Second quota warning"));
        assert!(forecast.contains("0 pending output tokens reserved"));
        assert!(forecast.contains("about 55 input tokens and up to 40 output tokens"));
        assert!(forecast.contains("5 tokens projected remaining"));
        assert!(!root.0.join(FOLDER).exists());
    }

    #[test]
    fn concurrent_reservations_are_isolated_and_settlement_is_idempotent() {
        let root = crate::snippets::tests::Profile::new();
        let account = account(&root, 7);
        let now = Clock {
            seconds: 1,
            day: 10,
        };
        let start = std::sync::Barrier::new(2);
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                start.wait();
                account.reserve_at(40, now).unwrap()
            });
            start.wait();
            let second = account.reserve_at(40, now).unwrap();
            (first.join().unwrap(), second)
        });
        let pending = account.snapshot_at(now).unwrap();
        assert_eq!(pending.in_flight, 2);
        assert_eq!(pending.reserved_output, 80);
        assert_eq!(account.budget.warning(pending, 0), Some(Warning::Yellow));
        let known = Usage {
            input_tokens: Some(11),
            output_tokens: Some(7),
            ..Usage::default()
        };
        for _ in 0..2 {
            account
                .settle_at(&first, Outcome::Complete, Some(&known), now)
                .unwrap();
        }
        account
            .settle_at(&second, Outcome::Unsent, None, now)
            .unwrap();
        let settled = account.snapshot_at(now).unwrap();
        assert_eq!(settled.in_flight, 0);
        assert_eq!(settled.counts.requests, 1);
        assert_eq!(settled.counts.input_tokens, 11);
        assert_eq!(settled.counts.output_tokens, 7);
        let unrelated = super::Account::new(root.0.clone(), 8, Budget::default()).unwrap();
        assert_eq!(unrelated.snapshot_at(now).unwrap().counts.requests, 0);
    }

    #[test]
    fn rollover_preserves_dispatch_period_and_expiry_is_unknown() {
        let root = crate::snippets::tests::Profile::new();
        let account = account(&root, 3);
        let now = Clock {
            seconds: 1,
            day: 20,
        };
        let first = account.reserve_at(10, now).unwrap();
        let next = Clock {
            seconds: 2,
            day: 22,
        };
        let usage = Usage {
            input_tokens: Some(5),
            ..Usage::default()
        };
        account
            .settle_at(&first, Outcome::Interrupted, Some(&usage), next)
            .unwrap();
        let prior = account.snapshot_at(now).unwrap();
        assert_eq!(prior.start_day, 20);
        assert_eq!(prior.next_day, 22);
        assert_eq!(prior.counts.input_tokens, 5);
        assert_eq!(prior.counts.interrupted, 1);
        assert_eq!(prior.counts.missing_input, 0);
        assert_eq!(prior.counts.missing_output, 1);
        assert_eq!(account.snapshot_at(next).unwrap().counts.requests, 0);
        let _abandoned = account.reserve_at(10, next).unwrap();
        let expired = account
            .snapshot_at(Clock {
                seconds: 2 + RESERVATION_LIFETIME,
                day: 22,
            })
            .unwrap();
        assert_eq!(expired.in_flight, 0);
        assert_eq!(expired.counts.interrupted, 1);
        assert_eq!(expired.counts.missing_input, 1);
        assert_eq!(expired.counts.missing_output, 1);
    }

    #[test]
    fn optional_prices_remain_distinct_from_zero_and_keep_known_partial_cost() {
        let counts = Counters {
            input_tokens: 5,
            output_tokens: 12,
            missing_output: 1,
            ..Counters::default()
        };
        assert!(Budget::default().estimated_cost(counts).is_none());
        let budget = Budget {
            input_micros_per_million: Some(2_500_000),
            output_micros_per_million: Some(0),
            ..Budget::default()
        };
        let cost = budget.estimated_cost(counts).unwrap();
        assert_eq!(cost.numerator, 12_500_000);
        assert!(cost.partial);
    }

    #[cfg(windows)]
    #[test]
    fn settlement_keeps_reported_counts_while_another_reader_holds_the_file() {
        let root = crate::snippets::tests::Profile::new();
        let account = account(&root, 7);
        let now = Clock {
            seconds: 1,
            day: 10,
        };
        let reservation = account.reserve_at(20, now).unwrap();
        let directory = account.directory(false).unwrap();
        let reader = directory
            .open_private_file(&filename(&account.stem))
            .unwrap();
        let usage = Usage {
            input_tokens: Some(3),
            output_tokens: Some(7),
            ..Usage::default()
        };
        account
            .settle_at(&reservation, Outcome::Complete, Some(&usage), now)
            .unwrap();
        drop(reader);
        let snapshot = account.snapshot_at(now).unwrap();
        assert_eq!(snapshot.counts.output_tokens, 7);
        assert_eq!(snapshot.in_flight, 0);
    }

    #[test]
    fn accounting_issue_stays_with_its_root_and_profile_after_notice_delivery() {
        let root = crate::snippets::tests::Profile::new();
        let other = crate::snippets::tests::Profile::new();
        let account = account(&root, 7);
        account.note_issue();
        assert!(!take_notice(&other.0));
        assert!(take_notice(&root.0));
        assert!(!take_notice(&root.0));
        assert!(account.snapshot().unwrap().recording_issue);
        let unrelated = Account::new(root.0.clone(), 8, Budget::default()).unwrap();
        assert!(!unrelated.snapshot().unwrap().recording_issue);
    }
}
