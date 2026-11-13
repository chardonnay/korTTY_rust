//! Private per-profile accounting for automatic rule proposals.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};
use std::fs::{File, TryLockError};
#[cfg(not(windows))]
use std::io::Write as _;
use std::io::{self, Read as _};
use std::marker::PhantomData;
use std::path::Path;
use std::time::{Duration, Instant};

use jiff::{SignedDuration, Span, Timestamp, civil::Date, tz::TimeZone};
use pgp::crypto::hash::HashAlgorithm;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use zeroize::Zeroizing;

use crate::private_files::{Directory, FileStamp, require_single_link};

const FILE: &str = "learning.json";
const LOCK: &str = ".learning.lock";
const VERSION: u16 = 1;
const MAX_BYTES: usize = 256 * 1024;
const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_DETECTIONS: u8 = 10;
const MAX_CARDS: u8 = 3;
const MAX_REJECTED: usize = 1000;
const MAX_SNOOZED: usize = 128;
const KEEP_DAYS: i64 = 7;
const MAX_DAYS: usize = 8;
const SNOOZE: SignedDuration = SignedDuration::from_hours(7 * 24);
const LOCK_WAIT: Duration = Duration::from_secs(1);
const LOCK_RETRY: Duration = Duration::from_millis(10);

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(transparent)]
pub(super) struct Fingerprint(String);

impl fmt::Debug for Fingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Fingerprint")
            .finish_non_exhaustive()
    }
}

impl Fingerprint {
    fn valid(&self) -> bool {
        self.0.len() == 64
            && self
                .0
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }
}

/// Java LearningState collapses ASCII regular-expression whitespace before String.strip.
pub(super) fn fingerprint(text: &str) -> io::Result<Fingerprint> {
    if text.len() > MAX_INPUT_BYTES {
        return Err(invalid(
            "The automatic rule fingerprint input is too large.",
        ));
    }
    let collapsed = Zeroizing::new(
        text.split([' ', '\t', '\n', '\r', '\u{000b}', '\u{000c}'])
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
    );
    let normalized = Zeroizing::new(collapsed.trim_matches(java_whitespace).to_lowercase());
    let digest = HashAlgorithm::Sha256
        .digest(normalized.as_bytes())
        .map_err(|_| invalid("Could not hash the automatic rule fingerprint."))?;
    let mut hex = String::with_capacity(64);
    for byte in digest {
        write!(hex, "{byte:02x}").expect("writing into a String succeeds");
    }
    Ok(Fingerprint(hex))
}

fn java_whitespace(character: char) -> bool {
    matches!(character,
        '\u{0009}'..='\u{000d}' | '\u{001c}'..='\u{0020}' | '\u{1680}'
        | '\u{2000}'..='\u{2006}' | '\u{2008}'..='\u{200a}' | '\u{2028}' | '\u{2029}'
        | '\u{205f}' | '\u{3000}')
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Feedback {
    Snooze,
    Reject,
}

pub(super) fn reserve_detection(
    profile_root: &Path,
    message: &Fingerprint,
    active: impl FnOnce() -> bool,
) -> io::Result<bool> {
    reserve_detection_at(profile_root, message, Moment::now, active)
}

pub(super) fn claim_card(
    profile_root: &Path,
    message: &Fingerprint,
    rule: &Fingerprint,
    active: impl FnOnce() -> bool,
) -> io::Result<bool> {
    claim_card_at(profile_root, message, rule, Moment::now, active)
}

pub(super) fn record_feedback(
    profile_root: &Path,
    message: &Fingerprint,
    rule: &Fingerprint,
    feedback: Feedback,
) -> io::Result<()> {
    record_feedback_at(profile_root, message, rule, feedback, Moment::now)
}

#[derive(Clone, Copy)]
struct Moment {
    now: Timestamp,
    today: Date,
}

impl Moment {
    fn now() -> Self {
        let now = Timestamp::now();
        Self {
            now,
            today: now.to_zoned(TimeZone::system()).date(),
        }
    }
}

fn reserve_detection_at(
    profile_root: &Path,
    message: &Fingerprint,
    time: impl FnOnce() -> Moment,
    active: impl FnOnce() -> bool,
) -> io::Result<bool> {
    transaction(profile_root, time, |data, at| {
        if !active() || data.message_blocked(message) {
            return Ok(false);
        }
        let day = data.days.entry(at.today.to_string()).or_default();
        if day.detections >= MAX_DETECTIONS || day.cards >= MAX_CARDS {
            return Ok(false);
        }
        day.detections += 1;
        Ok(true)
    })
}

fn claim_card_at(
    profile_root: &Path,
    message: &Fingerprint,
    rule: &Fingerprint,
    time: impl FnOnce() -> Moment,
    active: impl FnOnce() -> bool,
) -> io::Result<bool> {
    transaction(profile_root, time, |data, at| {
        if !active() || data.message_blocked(message) || data.rule_blocked(rule) {
            return Ok(false);
        }
        let day = data.days.entry(at.today.to_string()).or_default();
        if day.cards >= MAX_CARDS {
            return Ok(false);
        }
        day.cards += 1;
        Ok(true)
    })
}

fn record_feedback_at(
    profile_root: &Path,
    message: &Fingerprint,
    rule: &Fingerprint,
    feedback: Feedback,
    time: impl FnOnce() -> Moment,
) -> io::Result<()> {
    transaction(profile_root, time, |data, at| {
        match feedback {
            Feedback::Reject => {
                remember_rejection(&mut data.rejected, rule);
                remember_rejection(&mut data.rejected_messages, message);
                data.snoozed.remove(rule);
                data.snoozed_messages.remove(message);
            }
            Feedback::Snooze => {
                let until = at
                    .now
                    .checked_add(SNOOZE)
                    .map_err(|_| invalid("Could not set the automatic rule snooze."))?
                    .to_string();
                if !data.rejected.contains(rule) {
                    data.snoozed.insert(rule.clone(), until.clone());
                }
                if !data.rejected_messages.contains(message) {
                    data.snoozed_messages.insert(message.clone(), until);
                }
            }
        }
        Ok(true)
    })?;
    Ok(())
}

fn remember_rejection(rejected: &mut Vec<Fingerprint>, item: &Fingerprint) {
    rejected.retain(|previous| previous != item);
    if rejected.len() == MAX_REJECTED {
        rejected.remove(0);
    }
    rejected.push(item.clone());
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Data {
    version: u16,
    rejected: Vec<Fingerprint>,
    #[serde(deserialize_with = "unique_map")]
    snoozed: BTreeMap<Fingerprint, String>,
    rejected_messages: Vec<Fingerprint>,
    #[serde(deserialize_with = "unique_map")]
    snoozed_messages: BTreeMap<Fingerprint, String>,
    #[serde(deserialize_with = "unique_map")]
    days: BTreeMap<String, Day>,
}

impl Default for Data {
    fn default() -> Self {
        Self {
            version: VERSION,
            rejected: Vec::new(),
            snoozed: BTreeMap::new(),
            rejected_messages: Vec::new(),
            snoozed_messages: BTreeMap::new(),
            days: BTreeMap::new(),
        }
    }
}

impl Data {
    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION
            || !valid_rejected(&self.rejected)
            || !valid_rejected(&self.rejected_messages)
            || !valid_snoozed(&self.snoozed)
            || !valid_snoozed(&self.snoozed_messages)
            || self.days.len() > MAX_DAYS
            || self.days.iter().any(|(key, day)| {
                key.len() != 10
                    || !key
                        .parse::<Date>()
                        .is_ok_and(|date| date.to_string() == *key)
                    || day.detections > MAX_DETECTIONS
                    || day.cards > MAX_CARDS
            })
        {
            return Err(invalid("The automatic rule ledger has an invalid format."));
        }
        Ok(())
    }

    fn prune(&mut self, at: Moment) {
        let oldest = at
            .today
            .saturating_sub(Span::new().days(KEEP_DAYS))
            .to_string();
        self.days.retain(|day, _| day >= &oldest);
        let active = |_: &Fingerprint, until: &mut String| {
            until.parse::<Timestamp>().is_ok_and(|until| at.now < until)
        };
        self.snoozed.retain(active);
        self.snoozed_messages.retain(active);
    }

    fn message_blocked(&self, message: &Fingerprint) -> bool {
        self.rejected_messages.contains(message) || self.snoozed_messages.contains_key(message)
    }

    fn rule_blocked(&self, rule: &Fingerprint) -> bool {
        self.rejected.contains(rule) || self.snoozed.contains_key(rule)
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Day {
    detections: u8,
    cards: u8,
}

fn valid_rejected(items: &[Fingerprint]) -> bool {
    let mut seen = BTreeSet::new();
    items.len() <= MAX_REJECTED && items.iter().all(|item| item.valid() && seen.insert(item))
}

fn valid_snoozed(items: &BTreeMap<Fingerprint, String>) -> bool {
    items.len() <= MAX_SNOOZED
        && items.iter().all(|(item, until)| {
            item.valid()
                && until.len() <= 40
                && until.ends_with('Z')
                && until.parse::<Timestamp>().is_ok()
        })
}

fn unique_map<'de, D, K, V>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
where
    D: Deserializer<'de>,
    K: Ord + Deserialize<'de>,
    V: Deserialize<'de>,
{
    struct UniqueMap<K, V>(PhantomData<(K, V)>);

    impl<'de, K: Ord + Deserialize<'de>, V: Deserialize<'de>> Visitor<'de> for UniqueMap<K, V> {
        type Value = BTreeMap<K, V>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a ledger map with unique keys")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
            let mut map = BTreeMap::new();
            while let Some((key, value)) = access.next_entry()? {
                if map.insert(key, value).is_some() {
                    return Err(de::Error::custom("duplicate ledger key"));
                }
            }
            Ok(map)
        }
    }

    deserializer.deserialize_map(UniqueMap(PhantomData))
}

struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn acquire(directory: &Directory) -> io::Result<Lock> {
    let file = directory.open_lock(LOCK)?;
    require_single_link(&file)?;
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Lock(file)),
            Err(TryLockError::WouldBlock) if start.elapsed() < LOCK_WAIT => {
                std::thread::sleep(LOCK_RETRY);
            }
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "The automatic rule ledger is busy.",
                ));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
}

#[derive(Eq, PartialEq)]
struct Raw {
    bytes: Vec<u8>,
    stamp: FileStamp,
}

fn read(directory: &Directory) -> io::Result<Option<Raw>> {
    let file = match directory.open_private_file(FILE) {
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
        return Err(invalid("The automatic rule ledger exceeds its size limit."));
    }
    Ok(Some(Raw { bytes, stamp }))
}

fn transaction(
    profile_root: &Path,
    time: impl FnOnce() -> Moment,
    change: impl FnOnce(&mut Data, Moment) -> io::Result<bool>,
) -> io::Result<bool> {
    let directory = Directory::open(&profile_root.join("memory"), true)?;
    let _lock = acquire(&directory)?;
    let at = time();
    let previous = read(&directory)?;
    let mut data = match &previous {
        Some(raw) => serde_json::from_slice(&raw.bytes)
            .map_err(|_| invalid("The automatic rule ledger has an invalid format."))?,
        None => Data::default(),
    };
    data.validate()?;
    data.prune(at);
    if !change(&mut data, at)? {
        return Ok(false);
    }
    data.validate()?;
    let bytes = serde_json::to_vec(&data)
        .map_err(|_| invalid("Could not encode the automatic rule ledger."))?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid("The automatic rule ledger exceeds its size limit."));
    }
    let check = || {
        if read(&directory)? == previous {
            Ok(())
        } else {
            Err(invalid(
                "The automatic rule ledger changed during the update.",
            ))
        }
    };
    #[cfg(windows)]
    directory.publish_bytes_with_retry(FILE, &bytes, previous.as_ref().map(|_| check))?;
    #[cfg(not(windows))]
    if previous.is_some() {
        directory.replace_with(FILE, |file| file.write_all(&bytes), check)?;
    } else {
        directory.publish_copy(FILE, &bytes)?;
    }
    directory.sync()?;
    Ok(true)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::tests::Profile;

    fn utc(value: &str) -> Moment {
        let now = value.parse::<Timestamp>().unwrap();
        Moment {
            now,
            today: now.to_zoned(TimeZone::UTC).date(),
        }
    }

    fn stored(root: &Path) -> io::Result<Data> {
        let directory = Directory::open(&root.join("memory"), false)?;
        let bytes = read(&directory)?.expect("written ledger").bytes;
        let data: Data = serde_json::from_slice(&bytes)?;
        data.validate()?;
        Ok(data)
    }

    #[test]
    fn automatic_rule_ledger_fingerprints_match_java_normalization() -> io::Result<()> {
        assert_eq!(
            fingerprint(" \tABC\u{000b}\r\n")?.0,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            fingerprint("\u{2003}USE\n\t  staging\u{3000}")?,
            fingerprint("use staging")?
        );
        assert_ne!(
            fingerprint("use\u{2003}staging")?,
            fingerprint("use staging")?
        );
        assert_ne!(fingerprint("use staging!")?, fingerprint("use staging")?);
        assert!(fingerprint(&" ".repeat(MAX_INPUT_BYTES + 1)).is_err());
        Ok(())
    }

    #[test]
    fn automatic_rule_ledger_caps_both_counters_and_retains_only_recent_days() -> io::Result<()> {
        let profile = Profile::new();
        let root = profile.0.canonicalize()?;
        let message = fingerprint("Always use staging.")?;
        let rule = fingerprint("Use the staging deployment.")?;
        let first = utc("2026-11-01T23:59:58Z");
        for _ in 0..MAX_DETECTIONS {
            assert!(reserve_detection_at(&root, &message, || first, || true)?);
        }
        assert!(!reserve_detection_at(&root, &message, || first, || true)?);
        let next = utc("2026-11-02T00:00:00Z");
        for _ in 0..MAX_CARDS {
            assert!(claim_card_at(&root, &message, &rule, || next, || true)?);
        }
        assert!(!claim_card_at(&root, &message, &rule, || next, || true)?);
        assert!(!reserve_detection_at(&root, &message, || next, || true)?);
        let later = utc("2026-11-09T00:00:00Z");
        assert!(reserve_detection_at(&root, &message, || later, || true)?);
        let state = stored(&root)?;
        assert!(!state.days.contains_key("2026-11-01"));
        assert_eq!(state.days["2026-11-02"].cards, MAX_CARDS);
        assert_eq!(state.days["2026-11-09"].detections, 1);
        Ok(())
    }

    #[test]
    fn automatic_rule_ledger_snoozes_and_rejects_message_and_rule_independently() -> io::Result<()>
    {
        let profile = Profile::new();
        let root = profile.0.canonicalize()?;
        let message = fingerprint("Use staging whenever possible.")?;
        let rule = fingerprint("Prefer the staging service.")?;
        let other = fingerprint("Use the staging region for this project.")?;
        let first = utc("2026-11-01T13:12:00Z");
        record_feedback_at(&root, &message, &rule, Feedback::Snooze, || first)?;
        let before = utc("2026-11-08T13:11:59Z");
        assert!(!reserve_detection_at(&root, &message, || before, || true)?);
        assert!(!claim_card_at(&root, &other, &rule, || before, || true)?);
        assert!(!claim_card_at(&root, &message, &other, || before, || true)?);
        assert!(reserve_detection_at(&root, &rule, || before, || true)?);
        let after = utc("2026-11-08T13:12:00Z");
        assert!(reserve_detection_at(&root, &message, || after, || true)?);
        assert!(claim_card_at(&root, &other, &rule, || after, || true)?);

        record_feedback_at(&root, &message, &rule, Feedback::Snooze, || after)?;
        record_feedback_at(&root, &message, &rule, Feedback::Reject, || after)?;
        let state = stored(&root)?;
        assert!(!state.snoozed.contains_key(&rule));
        assert!(!state.snoozed_messages.contains_key(&message));
        let later = utc("2027-11-01T13:12:00Z");
        assert!(!reserve_detection_at(&root, &message, || later, || true)?);
        assert!(!claim_card_at(&root, &other, &rule, || later, || true)?);
        assert!(reserve_detection_at(&root, &rule, || later, || true)?);
        Ok(())
    }

    #[test]
    fn automatic_rule_ledger_keeps_the_newest_thousand_permanent_rejections() -> io::Result<()> {
        let profile = Profile::new();
        let root = profile.0.canonicalize()?;
        let at = utc("2026-11-01T13:12:00Z");
        transaction(
            &root,
            || at,
            |data, _| {
                for index in 0..MAX_REJECTED {
                    data.rejected.push(Fingerprint(format!("{index:064x}")));
                    let message_index = MAX_REJECTED + index;
                    data.rejected_messages
                        .push(Fingerprint(format!("{message_index:064x}")));
                }
                Ok(true)
            },
        )?;
        let message = fingerprint("This message stays rejected.")?;
        let rule = fingerprint("This rule stays rejected.")?;
        record_feedback_at(&root, &message, &rule, Feedback::Reject, || at)?;
        let state = stored(&root)?;
        assert_eq!(state.rejected.len(), MAX_REJECTED);
        assert_eq!(state.rejected_messages.len(), MAX_REJECTED);
        assert_eq!(
            state.rejected.first(),
            Some(&Fingerprint(format!("{:064x}", 1)))
        );
        assert_eq!(
            state.rejected_messages.first(),
            Some(&Fingerprint(format!("{:064x}", MAX_REJECTED + 1)))
        );
        assert_eq!(state.rejected.last(), Some(&rule));
        assert_eq!(state.rejected_messages.last(), Some(&message));
        Ok(())
    }

    #[test]
    fn automatic_rule_ledger_concurrent_workers_cannot_both_take_the_last_slot() -> io::Result<()> {
        use std::sync::Barrier;

        let profile = Profile::new();
        let root = profile.0.canonicalize()?;
        let message = fingerprint("Always use staging.")?;
        let at = utc("2026-11-01T13:12:00Z");
        for _ in 1..MAX_DETECTIONS {
            assert!(reserve_detection_at(&root, &message, || at, || true)?);
        }
        let ready = Barrier::new(3);
        let attempt = || {
            ready.wait();
            reserve_detection_at(&root, &message, || at, || true)
        };
        let [first, second] = std::thread::scope(|scope| {
            let first = scope.spawn(attempt);
            let second = scope.spawn(attempt);
            ready.wait();
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert_ne!(first?, second?);
        assert_eq!(stored(&root)?.days["2026-11-01"].detections, MAX_DETECTIONS);
        Ok(())
    }

    #[test]
    fn automatic_rule_ledger_refuses_corrupt_or_oversized_existing_file_without_replacing_it()
    -> io::Result<()> {
        let profile = Profile::new();
        let root = profile.0.canonicalize()?;
        let directory = Directory::open(&root.join("memory"), true)?;
        let message = fingerprint("Always use staging.")?;
        let rule = fingerprint("Use the staging deployment.")?;
        let at = utc("2026-11-01T13:12:00Z");
        let duplicate_day = br#"{"version":1,"rejected":[],"snoozed":{},"rejectedMessages":[],"snoozedMessages":{},"days":{"2026-11-01":{"detections":10,"cards":0},"2026-11-01":{"detections":0,"cards":0}}}"#;
        directory.publish_copy(FILE, duplicate_day)?;
        assert!(reserve_detection_at(&root, &message, || at, || true).is_err());
        assert!(claim_card_at(&root, &message, &rule, || at, || true).is_err());
        assert!(record_feedback_at(&root, &message, &rule, Feedback::Reject, || at).is_err());
        assert_eq!(read(&directory)?.unwrap().bytes, duplicate_day);

        let unsupported = br#"{"version":99,"rejected":[],"snoozed":{},"rejectedMessages":[],"snoozedMessages":{},"days":{}}"#;
        directory.replace_file(FILE, unsupported)?;
        assert!(reserve_detection_at(&root, &message, || at, || true).is_err());
        assert_eq!(read(&directory)?.unwrap().bytes, unsupported);

        directory.replace_file(FILE, &vec![b' '; MAX_BYTES + 1])?;
        assert!(reserve_detection_at(&root, &message, || at, || true).is_err());
        assert_eq!(
            directory.open_private_file(FILE)?.metadata()?.len(),
            MAX_BYTES as u64 + 1
        );
        Ok(())
    }
}
