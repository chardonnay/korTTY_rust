//! Restore Java skill usage while keeping unmapped Java history independent of live skill IDs.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

use serde::de::{self, MapAccess, Visitor};
use serde_json::value::RawValue;
use zeroize::Zeroizing;

use super::*;
use crate::ai_manager::profiles::java_backup;
use crate::ai_manager::skills::{BUILTINS, Store};

pub(in crate::ai_manager) const DRAFT: &str = "java-skill-statistics.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source<'a> {
    version: u8,
    tokenizer: Option<JavaTokenizer>,
    #[serde(borrow)]
    skills: Option<Records<&'a RawValue>>,
}

type Records<T> = UniqueMap<String, T, MAX_RECORDED_SKILLS>;
type JavaPeriods = UniqueMap<String, JavaBucket, { usize::MAX }>;

struct UniqueMap<K, V, const LIMIT: usize>(BTreeMap<K, V>);

impl<'de, K, V, const LIMIT: usize> Deserialize<'de> for UniqueMap<K, V, LIMIT>
where
    K: Ord + Deserialize<'de>,
    V: Deserialize<'de>,
{
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueKeys<K, V, const LIMIT: usize>(PhantomData<(K, V)>);

        impl<'de, K, V, const LIMIT: usize> Visitor<'de> for UniqueKeys<K, V, LIMIT>
        where
            K: Ord + Deserialize<'de>,
            V: Deserialize<'de>,
        {
            type Value = UniqueMap<K, V, LIMIT>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a skill statistics map with unique keys")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut entries: A) -> Result<Self::Value, A::Error> {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = entries.next_entry()? {
                    if values.len() >= LIMIT || values.insert(key, value).is_some() {
                        return Err(de::Error::custom(
                            "Too many or duplicate skill statistics keys",
                        ));
                    }
                }
                Ok(UniqueMap(values))
            }
        }

        deserializer.deserialize_map(UniqueKeys(PhantomData))
    }
}

pub(super) fn deserialize_records<'de, D, T>(
    deserializer: D,
) -> Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Records::deserialize(deserializer).map(|records| records.0)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Disposition {
    Bound(u64),
    Reserved,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum JavaTokenizer {
    Estimate,
    #[serde(rename = "CL100K_BASE")]
    Cl100k,
    #[serde(rename = "O200K_BASE")]
    O200k,
    #[serde(rename = "P50K_BASE")]
    P50k,
    #[serde(rename = "R50K_BASE")]
    R50k,
}

impl From<JavaTokenizer> for Tokenizer {
    fn from(source: JavaTokenizer) -> Self {
        match source {
            JavaTokenizer::Estimate => Self::Estimate,
            JavaTokenizer::Cl100k => Self::Cl100kBase,
            JavaTokenizer::O200k => Self::O200kBase,
            JavaTokenizer::P50k => Self::P50kBase,
            JavaTokenizer::R50k => Self::R50kBase,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JavaRecord {
    name: Option<String>,
    last_used: Option<String>,
    #[serde(rename = "lastFingerprint")]
    _last_fingerprint: Option<de::IgnoredAny>,
    daily: Option<JavaPeriods>,
    monthly: Option<JavaPeriods>,
    yearly: Option<JavaPeriods>,
    snapshots: Option<Vec<JavaSnapshot>>,
}

#[derive(Deserialize)]
struct JavaSnapshot {
    date: String,
    tokens: u64,
    tokenizer: JavaTokenizer,
    fingerprint: String,
}

#[derive(Deserialize)]
struct JavaBucket {
    automatic: u32,
    pinned: u32,
    tokens: u64,
    surfaces: Option<UniqueMap<JavaSurface, u32, { usize::MAX }>>,
}

#[derive(Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "UPPERCASE")]
enum JavaSurface {
    Chat,
    Agent,
    Workflow,
}

fn buckets(source: Option<JavaPeriods>) -> BTreeMap<String, Bucket> {
    source
        .into_iter()
        .flat_map(|periods| periods.0)
        .map(|(date, bucket)| {
            let surfaces = bucket
                .surfaces
                .into_iter()
                .flat_map(|surfaces| surfaces.0)
                .map(|(name, count)| {
                    let surface = match name {
                        JavaSurface::Chat => Surface::Chat,
                        JavaSurface::Agent => Surface::Agent,
                        JavaSurface::Workflow => Surface::Workflow,
                    };
                    (surface, u64::from(count))
                })
                .collect();
            (
                date,
                Bucket {
                    automatic: u64::from(bucket.automatic),
                    pinned: u64::from(bucket.pinned),
                    tokens: bucket.tokens,
                    surfaces,
                },
            )
        })
        .collect()
}

impl JavaRecord {
    fn convert(self, fallback_name: &str) -> SkillRecord {
        let name = self.name.as_deref().map_or_else(String::new, display_name);
        let java_sizes = self
            .snapshots
            .unwrap_or_default()
            .into_iter()
            .map(|snapshot| JavaSize {
                date: snapshot.date,
                tokens: snapshot.tokens,
                tokenizer: snapshot.tokenizer.into(),
                fingerprint: snapshot.fingerprint,
            })
            .collect();
        SkillRecord {
            name: if name.is_empty() {
                fallback_name.into()
            } else {
                name
            },
            last_used: self.last_used,
            daily: buckets(self.daily),
            monthly: buckets(self.monthly),
            yearly: buckets(self.yearly),
            java_sizes,
            ..SkillRecord::default()
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ai_manager) struct Draft {
    tokenizer: Option<Tokenizer>,
    #[serde(deserialize_with = "deserialize_records")]
    by_builtin: BTreeMap<String, SkillRecord>,
    #[serde(
        default,
        deserialize_with = "deserialize_records",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    java_history: BTreeMap<String, SkillRecord>,
    // Older pending restores did not retain the Java source ID of a built-in.
    #[serde(
        default,
        deserialize_with = "deserialize_records",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    source_ids: BTreeMap<String, String>,
}

pub(in crate::ai_manager) struct Import {
    pub(in crate::ai_manager) bytes: Zeroizing<Vec<u8>>,
    pub(in crate::ai_manager) matched: usize,
    pub(in crate::ai_manager) historical: usize,
    pub(in crate::ai_manager) invalid: usize,
    pub(in crate::ai_manager) settings_omitted: bool,
}

fn known_builtin(key: &str) -> bool {
    BUILTINS
        .iter()
        .any(|skill| skill.builtin.as_deref() == Some(key))
}

fn valid_id(id: &str) -> bool {
    !id.trim().is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|ch| !ch.is_control() && crate::session_journal::is_display_character(ch))
}

fn valid_record(record: &SkillRecord) -> bool {
    record.valid()
        && record.last_size.is_none()
        && record.snapshots.is_empty()
        && record
            .name
            .chars()
            .all(crate::session_journal::is_display_character)
}

fn valid_history(history: &BTreeMap<String, SkillRecord>) -> bool {
    history.len() <= MAX_RECORDED_SKILLS
        && history
            .iter()
            .all(|(id, record)| valid_id(id) && valid_record(record))
}

pub(super) fn valid_sources(ledger: &Ledger) -> bool {
    let mut bound = HashSet::new();
    source_count(ledger) <= MAX_RECORDED_SKILLS
        && valid_history(&ledger.java_history)
        && ledger.java_bindings.iter().all(|(id, disposition)| {
            valid_id(id)
                && !ledger.java_history.contains_key(id)
                && match disposition {
                    Disposition::Bound(native) => {
                        ledger.skills.contains_key(native) && bound.insert(*native)
                    }
                    Disposition::Reserved => true,
                }
        })
}

fn source_count(ledger: &Ledger) -> usize {
    ledger.java_history.len() + ledger.java_bindings.len()
}

fn can_add_source(ledger: &Ledger, id: &str) -> bool {
    source_count(ledger) < MAX_RECORDED_SKILLS
        && !ledger.java_history.contains_key(id)
        && !ledger.java_bindings.contains_key(id)
}

fn source_builtins(settings: &[u8]) -> io::Result<BTreeMap<String, String>> {
    let mut identities = BTreeMap::new();
    let mut uses = BTreeMap::<String, usize>::new();
    java_backup::skill_sources(settings, |id, builtin| {
        let builtin = builtin.filter(|id| known_builtin(id)).map(str::to_owned);
        if let Some(builtin) = &builtin {
            *uses.entry(builtin.clone()).or_default() += 1;
        }
        if valid_id(id) {
            identities
                .entry(id.to_owned())
                .and_modify(|entry| *entry = None)
                .or_insert(builtin);
        }
    })?;
    Ok(identities
        .into_iter()
        .filter_map(|(id, builtin)| {
            let builtin = builtin?;
            (uses[&builtin] == 1).then_some((id, builtin))
        })
        .collect())
}

pub(in crate::ai_manager) fn convert(bytes: &[u8], settings: Option<&[u8]>) -> io::Result<Import> {
    if bytes.len() > MAX_BYTES {
        return Err(invalid(
            "The Java skill statistics exceed their size limit.",
        ));
    }
    let source: Source<'_> =
        serde_json::from_slice(bytes).map_err(|_| invalid("Invalid Java skill statistics"))?;
    if source.version != VERSION {
        return Err(invalid("Unsupported Java skill statistics version"));
    }
    let declared = settings.map(source_builtins);
    let settings_omitted = matches!(declared, Some(Err(_)));
    let builtins = declared.and_then(Result::ok).unwrap_or_default();
    let mut by_builtin = BTreeMap::new();
    let mut java_history = BTreeMap::new();
    let mut source_ids = BTreeMap::new();
    let mut malformed = 0;
    for (id, record) in source.skills.map(|records| records.0).unwrap_or_default() {
        if !valid_id(&id) {
            malformed += 1;
            continue;
        }
        let Ok(record) = serde_json::from_str::<JavaRecord>(record.get()) else {
            malformed += 1;
            continue;
        };
        let builtin = builtins.get(&id);
        let record = record.convert(builtin.unwrap_or(&id));
        if !valid_record(&record) {
            malformed += 1;
        } else if let Some(builtin) = builtin {
            by_builtin.insert(builtin.clone(), record);
            source_ids.insert(builtin.clone(), id);
        } else {
            java_history.insert(id, record);
        }
    }
    let matched = by_builtin.len();
    let historical = java_history.len();
    let draft = Draft {
        tokenizer: source.tokenizer.map(Tokenizer::from),
        by_builtin,
        java_history,
        source_ids,
    };
    let bytes = Zeroizing::new(serde_json::to_vec(&draft).map_err(io::Error::other)?);
    if bytes.len() > MAX_BYTES {
        return Err(invalid(
            "The Java skill statistics exceed their size limit.",
        ));
    }
    Ok(Import {
        bytes,
        matched,
        historical,
        invalid: malformed,
        settings_omitted,
    })
}

pub(in crate::ai_manager) fn decode_draft(bytes: &[u8]) -> io::Result<Draft> {
    if bytes.len() > MAX_BYTES {
        return Err(invalid(
            "The Java skill statistics exceed their size limit.",
        ));
    }
    let draft: Draft = serde_json::from_slice(bytes)
        .map_err(|_| invalid("Invalid pending Java skill statistics"))?;
    let count = draft.by_builtin.len() + draft.java_history.len();
    let mut sources = HashSet::new();
    if count == 0
        || count > MAX_RECORDED_SKILLS
        || draft
            .by_builtin
            .iter()
            .any(|(name, record)| !known_builtin(name) || !valid_record(record))
        || !valid_history(&draft.java_history)
        || draft.source_ids.iter().any(|(builtin, id)| {
            !draft.by_builtin.contains_key(builtin)
                || !valid_id(id)
                || draft.java_history.contains_key(id)
                || !sources.insert(id)
        })
    {
        return Err(invalid("Invalid pending Java skill statistics"));
    }
    Ok(draft)
}

pub(in crate::ai_manager) fn publish(
    root: &Directory,
    draft: Draft,
    store: &Store,
    allowed: impl Fn() -> bool,
) -> io::Result<()> {
    if !allowed() {
        return Ok(());
    }
    let directory = root.private_child(FOLDER, true)?;
    let denied = Cell::new(false);
    let allowed = || {
        let permitted = allowed();
        if !permitted {
            denied.set(true);
        }
        permitted
    };
    let result = transact_in(&directory, allowed, |ledger, existing| {
        merge(ledger, draft, store, existing)
    });
    if denied.get() {
        Ok(())
    } else {
        match result {
            Ok(()) => directory.sync(),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::AlreadyExists
                        | io::ErrorKind::InvalidData
                        | io::ErrorKind::FileTooLarge
                ) =>
            {
                directory.sync()
            }
            Err(error) => Err(error),
        }
    }
}

fn merge(ledger: &mut Ledger, draft: Draft, store: &Store, existing: bool) -> io::Result<bool> {
    let Draft {
        tokenizer,
        by_builtin,
        java_history,
        mut source_ids,
    } = draft;
    let before = source_count(ledger);
    for (builtin, record) in by_builtin {
        let mut matching = store
            .skills
            .iter()
            .filter(|(_, skill)| skill.builtin.as_deref() == Some(builtin.as_str()));
        let native = matching
            .next()
            .filter(|_| matching.next().is_none())
            .map(|(&id, _)| id);
        match (source_ids.remove(&builtin), native) {
            // Pending backups prepared before source IDs were stored can still import a built-in.
            (None, Some(id)) if !existing => {
                ledger.skills.insert(id, record);
            }
            (Some(source), native) if can_add_source(ledger, &source) => match native {
                None => {
                    ledger.java_history.insert(source, record);
                }
                Some(_) if existing => {
                    // An existing native ledger takes precedence. Retain the source identity.
                    ledger.java_bindings.insert(source, Disposition::Reserved);
                }
                Some(id) => {
                    ledger.skills.insert(id, record);
                    ledger.java_bindings.insert(source, Disposition::Bound(id));
                }
            },
            _ => {}
        }
    }
    for (id, record) in java_history {
        if can_add_source(ledger, &id) {
            ledger.java_history.insert(id, record);
        }
    }
    let builtins_published = !existing && !ledger.skills.is_empty();
    if builtins_published {
        ledger.tokenizer = tokenizer;
    }
    ledger.validate()?;
    Ok(source_count(ledger) > before || builtins_published)
}
