//! Field-scoped, restart-safe publication of a stopped journal's summary pass.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::io::{self, Read as _};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::Timestamp;
use roxmltree::{Document, Node};

use crate::local_ai::{JournalCost, JournalProfileIdentity};
use crate::private_files::Directory;

use super::deletion::DeletionIdentity;
use super::entry_edit::{
    append_children, apply_patches, entries, lock_source, patch_scalars, require_java_fields,
};
use super::language;
use super::metadata::{MetadataLock, escape, lock_existing_metadata_in};
use super::summary_ai::{Card, ObservedUsage, State, Summary};
use super::summary_plan::{End, Window};
use super::viewer::Source;
use super::{
    DOCUMENT, Journal, JournalProfileSelection, boolean, child, document_id, field, invalid,
    journal, number, parse_document,
};

const GENERATION: &str = "nativeSummaryGeneration";
const GENERATION_LANGUAGE: &str = "nativeSummaryLanguage";
const CURSOR: &str = "nativeSummaryCursor";
const FINISHED: &str = "nativeSummaryFinished";
const TITLE_PENDING: &str = "nativeSummaryTitlePending";
const AUTOMATION_PROBE_FAILURE: &str = "nativeAutomationAiUnavailable";
const RAW: &str = "nativeSummaryRawFallback";
const LAST_AI: &str = "lastSummarizedSeq";
pub(super) const MANUAL_TITLE: &str = "titleManuallySet";
const MAX_CARDS: usize = 512;
const AI_CARD: &str = "AI_SUMMARY";
const WRAP_UP: &str = "SESSION_SUMMARY";
const PROMPT_TOKENS: &str = "aiPromptTokens";
const COMPLETION_TOKENS: &str = "aiCompletionTokens";
const TOTAL_TOKENS: &str = "aiTotalTokens";
const CALL_COUNT: &str = "aiCallCount";
const USAGE_PARTIAL: &str = "aiUsagePartial";
const PROFILE_LOCAL: &str = "aiProfileLocal";
const PROFILE_REF: &str = "nativeAiProfileRef";
const JAVA_PROFILE_ID: &str = "aiProfileId";
pub(super) const PROFILE_NAME: &str = "aiProfileName";
const COST: &str = "aiCost";
const COST_CURRENCY: &str = "aiCostCurrency";
const COST_PARTIAL: &str = "aiCostPartial";
// Native writers put the end time and source kind near the beginning of the document. If a
// capture's metadata exceeds the prefix, the ordinary bounded XML parser remains authoritative.
const RECOVERY_PREFIX_BYTES: u64 = 8 * 1024;

pub(crate) type UsageEdit = (DeletionIdentity, DeletionIdentity);
type ProfileFields = [(&'static str, Option<String>); 3];

pub(super) fn profile_value<'a>(meta: Node<'a, '_>, name: &str) -> Option<Cow<'a, str>> {
    require_java_fields(meta, &[name]).ok()?;
    field(meta, name).ok().flatten()
}

pub(super) fn profile_selection(meta: Node<'_, '_>) -> Option<JournalProfileSelection> {
    let java = profile_value(meta, JAVA_PROFILE_ID)
        .as_deref()
        .and_then(crate::ai_manager::JavaProfileSource::parse_id)
        .map(str::to_owned);
    let native = profile_value(meta, PROFILE_REF)
        .filter(|value| value.len() <= 1024)
        .and_then(|value| {
            serde_json::from_str::<kortty_jobs::ProfileRef>(&value)
                .ok()
                .filter(kortty_jobs::ProfileRef::is_nonzero)
        });
    match (native, java) {
        (Some(native), Some(java)) => Some(JournalProfileSelection::Migrated(native, java)),
        (Some(native), None) => Some(JournalProfileSelection::Saved(native)),
        (None, Some(java)) => Some(JournalProfileSelection::Java(java)),
        (None, None) => None,
    }
}

#[derive(Clone, Default)]
pub(super) struct TokenUsage {
    pub(super) prompt_tokens: u64,
    pub(super) completion_tokens: u64,
    pub(super) total_tokens: u64,
    pub(super) call_count: u64,
    pub(super) partial: bool,
    pub(super) cost_partial: bool,
    local: Option<bool>,
    pub(super) cost: Option<f64>,
    currency: Option<[u8; 3]>,
    has_currency_field: bool,
    cost_changed: bool,
    currency_changed: bool,
    profile: Option<Arc<JournalProfileIdentity>>,
    had_profile: bool,
}

impl TokenUsage {
    pub(super) fn read(meta: Node<'_, '_>) -> io::Result<Self> {
        let prompt_tokens = number(meta, PROMPT_TOKENS)?;
        let completion_tokens = number(meta, COMPLETION_TOKENS)?;
        let call_count = number(meta, CALL_COUNT)?.unwrap_or_default();
        let cost = number::<f64>(meta, COST)?;
        if cost.is_some_and(|amount| !amount.is_finite()) {
            return Err(invalid("Invalid session journal AI cost"));
        }
        let currency_field = field(meta, COST_CURRENCY)?;
        let currency_text = currency_field.as_deref().map(str::trim).unwrap_or_default();
        let currency = currency_text
            .as_bytes()
            .try_into()
            .ok()
            .filter(|bytes: &[u8; 3]| bytes.iter().all(u8::is_ascii_alphabetic))
            .map(|bytes| bytes.map(|byte| byte.to_ascii_uppercase()));
        Ok(Self {
            prompt_tokens: prompt_tokens.unwrap_or_default(),
            completion_tokens: completion_tokens.unwrap_or_default(),
            total_tokens: number(meta, TOTAL_TOKENS)?.unwrap_or_default(),
            call_count,
            partial: boolean(meta, USAGE_PARTIAL)?
                || call_count > 0 && (prompt_tokens.is_none() || completion_tokens.is_none()),
            cost_partial: boolean(meta, COST_PARTIAL)?,
            local: child(meta, PROFILE_LOCAL)
                .map(|_| boolean(meta, PROFILE_LOCAL))
                .transpose()?,
            cost,
            currency,
            has_currency_field: !currency_text.is_empty(),
            cost_changed: false,
            currency_changed: false,
            profile: None,
            had_profile: profile_value(meta, PROFILE_REF).is_some()
                || profile_value(meta, JAVA_PROFILE_ID).is_some(),
        })
    }

    pub(super) fn observe(&mut self, observed: ObservedUsage) {
        fn add(value: &mut u64, delta: Option<u64>, limit: u64) -> bool {
            let Some(next) = delta.and_then(|delta| value.checked_add(delta)) else {
                return false;
            };
            if next > limit {
                return false;
            }
            *value = next;
            true
        }

        // Keep all persisted fields representable by the Java journal reader. A rejected value
        // leaves the last exact subtotal in place and makes the observation unusable for estimates.
        let limit = i64::MAX as u64;
        self.partial |= !add(&mut self.prompt_tokens, observed.input_tokens, limit);
        self.partial |= !add(&mut self.completion_tokens, observed.output_tokens, limit);
        self.partial |= !add(&mut self.total_tokens, observed.total_tokens, limit);
        self.partial |= !add(&mut self.call_count, Some(observed.calls), i32::MAX as u64);
        self.partial |= observed.partial;
        if observed.profile.is_some() {
            self.profile = observed.profile;
            self.had_profile = true;
            self.local = Some(observed.local);
        } else if !self.had_profile {
            self.local = Some(observed.local);
        }
        if let Some(cost) = observed.cost {
            self.observe_cost(cost);
        } else if !observed.local {
            self.cost_partial = true;
        }
    }

    fn observe_stopped(
        &mut self,
        meta: Node<'_, '_>,
        mut observed: ObservedUsage,
    ) -> io::Result<Option<ProfileFields>> {
        if observed.profile.is_some() {
            let names = [PROFILE_REF, JAVA_PROFILE_ID, PROFILE_NAME];
            if require_java_fields(meta, &names).is_err()
                || names.iter().any(|name| field(meta, name).is_err())
            {
                observed.profile = None;
                self.had_profile = true;
            }
        }
        self.observe(observed);
        self.profile_fields()
    }

    fn observe_cost(&mut self, observation: JournalCost) {
        self.cost_partial |= observation.partial;
        if observation.partial && observation.numerator == 0 {
            return;
        }
        let previous = self.cost.unwrap_or_default();
        let can_add = match self.currency {
            Some(currency) => currency == observation.currency,
            None => previous == 0.0 && !self.has_currency_field,
        };
        let addition = observation.amount();
        let total = previous + addition;
        if !can_add || previous < 0.0 || !total.is_finite() || (addition > 0.0 && total == previous)
        {
            self.cost_partial = true;
            return;
        }
        self.cost = Some(total);
        self.cost_changed = true;
        if self.currency.is_none() {
            self.currency = Some(observation.currency);
            self.currency_changed = true;
            self.has_currency_field = true;
        }
    }

    pub(super) fn fields(&self) -> impl Iterator<Item = (&'static str, String)> {
        let cost = self.cost.filter(|_| self.cost_changed);
        let currency = self.currency.filter(|_| self.currency_changed);
        [
            (PROMPT_TOKENS, self.prompt_tokens.to_string()),
            (COMPLETION_TOKENS, self.completion_tokens.to_string()),
            (TOTAL_TOKENS, self.total_tokens.to_string()),
            (CALL_COUNT, self.call_count.to_string()),
            (USAGE_PARTIAL, self.partial.to_string()),
        ]
        .into_iter()
        .chain(self.local.map(|value| (PROFILE_LOCAL, value.to_string())))
        .chain(cost.map(|amount| (COST, amount.to_string())))
        .chain(
            currency
                .and_then(|code| String::from_utf8(code.to_vec()).ok())
                .map(|value| (COST_CURRENCY, value)),
        )
        .chain(self.cost_partial.then(|| (COST_PARTIAL, "true".into())))
    }

    pub(super) fn profile_fields(&self) -> io::Result<Option<ProfileFields>> {
        self.profile
            .as_ref()
            .map(|profile| {
                Ok([
                    (
                        PROFILE_REF,
                        Some(serde_json::to_string(&profile.reference)?),
                    ),
                    (JAVA_PROFILE_ID, profile.java_id.clone()),
                    (PROFILE_NAME, Some(profile.name.clone())),
                ])
            })
            .transpose()
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn journal_usage_ignores_malformed_or_namespaced_profile_fields() {
        let xml = r#"<meta xmlns:p="urn:extension">
            <p:nativeAiProfileRef>5</p:nativeAiProfileRef>
            <aiProfileId>old-one</aiProfileId><aiProfileId>old-two</aiProfileId>
            <aiProfileName><p:value>legacy</p:value></aiProfileName>
        </meta>"#;
        let document = Document::parse(xml).unwrap();
        let meta = document.root_element();
        assert!(profile_selection(meta).is_none());
        assert!(profile_value(meta, PROFILE_NAME).is_none());
        let mut usage = TokenUsage::read(meta).unwrap();
        let observed = ObservedUsage {
            profile: Some(Arc::new(JournalProfileIdentity {
                reference: kortty_jobs::ProfileRef::Numeric(1),
                name: "Selected".into(),
                java_id: None,
            })),
            ..observation(*b"USD")
        };
        assert!(usage.observe_stopped(meta, observed).unwrap().is_none());
        assert_eq!(usage.total_tokens, 12);
        assert_eq!(usage.cost, Some(0.25));
    }

    fn observation(currency: [u8; 3]) -> ObservedUsage {
        ObservedUsage {
            input_tokens: Some(10),
            output_tokens: Some(2),
            total_tokens: Some(12),
            calls: 1,
            partial: false,
            local: false,
            profile: None,
            cost: Some(JournalCost {
                numerator: 250_000_000_000,
                currency,
                partial: false,
            }),
        }
    }

    #[test]
    fn journal_cost_keeps_imported_currency_and_marks_mixed_usage_partial() {
        let imported =
            "<metadata><aiCost>0.50</aiCost><aiCostCurrency>eur</aiCostCurrency></metadata>";
        let document = Document::parse(imported).unwrap();
        let mut usage = TokenUsage::read(document.root_element()).unwrap();
        usage.observe(observation(*b"EUR"));
        usage.observe(observation(*b"USD"));

        let fields = usage.fields().collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(fields[COST], "0.75");
        assert!(!fields.contains_key(COST_CURRENCY));
        assert_eq!(fields[COST_PARTIAL], "true");
        assert_eq!(fields[USAGE_PARTIAL], "false");
        assert_eq!(fields[TOTAL_TOKENS], "24");
    }

    #[test]
    fn journal_cost_preserves_an_imported_amount_without_a_currency() {
        let document = Document::parse("<metadata><aiCost>0.50</aiCost></metadata>").unwrap();
        let mut usage = TokenUsage::read(document.root_element()).unwrap();
        usage.observe(observation(*b"EUR"));

        let fields = usage.fields().collect::<std::collections::BTreeMap<_, _>>();
        assert!(!fields.contains_key(COST));
        assert!(!fields.contains_key(COST_CURRENCY));
        assert_eq!(fields[COST_PARTIAL], "true");
    }

    #[test]
    fn journal_cost_does_not_set_a_currency_without_a_known_subtotal() {
        let mut usage = TokenUsage::default();
        let unknown = ObservedUsage {
            cost: Some(JournalCost {
                numerator: 0,
                currency: *b"EUR",
                partial: true,
            }),
            ..observation(*b"EUR")
        };
        usage.observe(unknown);
        let fields = usage.fields().collect::<std::collections::BTreeMap<_, _>>();
        assert!(!fields.contains_key(COST_CURRENCY));
        usage.observe(observation(*b"USD"));
        let fields = usage.fields().collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(fields[COST_CURRENCY], "USD");
        assert_eq!(fields[COST], "0.25");
        assert_eq!(fields[COST_PARTIAL], "true");
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Progress {
    pub(crate) generation: Option<String>,
    pub(crate) language: Option<String>,
    pub(crate) cursor: u64,
    pub(crate) last_ai: u64,
    pub(crate) finished: bool,
    pub(crate) raw: bool,
    pub(super) title_pending: bool,
}

impl Progress {
    pub(crate) fn read(source: &Source) -> io::Result<Self> {
        source.with_document(|_, document| progress(meta(document)?))
    }

    pub(crate) fn read_live(source: &Source) -> io::Result<(Self, String)> {
        let read = || {
            source.with_document(|_, document| {
                let meta = meta(document)?;
                Ok((progress(meta)?, app_language(meta)))
            })
        };
        match read() {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => read(),
            result => result,
        }
    }
}

pub(super) fn needs_summary(meta: Node<'_, '_>) -> bool {
    progress(meta).is_ok_and(|progress| {
        progress.raw
            || !progress.title_pending
                && (progress.last_ai == 0 || progress.generation.is_some() && !progress.finished)
    })
}

pub(crate) fn is_waiting_for_title(source: &Source, generation: &str) -> io::Result<bool> {
    let checkpoint = Progress::read(source)?;
    Ok(checkpoint.title_pending && checkpoint.generation.as_deref() == Some(generation))
}

pub(crate) fn automation_probe_failed(source: &Source, generation: &str) -> io::Result<bool> {
    source.with_document(|_, document| {
        Ok(field(meta(document)?, AUTOMATION_PROBE_FAILURE)?.as_deref() == Some(generation))
    })
}

/// Record an unsuccessful connection test independently of the raw closing pass so that an
/// interrupted pass can resume without pinging the provider or adding another system entry.
pub(crate) fn record_automation_probe_failure(
    source: &Source,
    generation: &str,
    body: &str,
    cancelled: &AtomicBool,
    authorize: impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    require_active(cancelled)?;
    authorize()?;
    let lock = lock_source(source)?;
    let (stamp, xml) = lock.read_document()?;
    let document = parse_document(&xml)?;
    let meta = meta(&document)?;
    let source_generation = source.id().replace('-', "");
    if !valid_generation(generation)
        || generation != source_generation
        || document_id(&document)? != source.id()
        || !is_native_source(source.directory(), source.id())
        || !matches!(field(meta, "sourceKind")?.as_deref(), Some("JOB" | "SWARM"))
        || number::<Timestamp>(meta, "endedAt")?.is_none()
    {
        return Err(invalid("The closed automation journal changed"));
    }
    let checkpoint = progress(meta)?;
    if checkpoint.finished && checkpoint.generation.as_deref() == Some(generation)
        || field(meta, AUTOMATION_PROBE_FAILURE)?.as_deref() == Some(generation)
    {
        return Ok(());
    }
    let wrapper = child(document.root_element(), "entries")
        .ok_or_else(|| invalid("Missing session journal entries"))?;
    require_java_fields(wrapper, &[])?;
    let mut notice = String::new();
    new_entry(
        &mut notice,
        "SYSTEM",
        Timestamp::now(),
        "AI not reachable",
        body,
    )?;
    element(&mut notice, "state", "RAW");
    notice.push_str("</entry>");
    let mut patches = Vec::new();
    patch_scalars(
        &xml,
        meta,
        [(AUTOMATION_PROBE_FAILURE, Some(generation))],
        &mut patches,
    )?;
    append_children(&xml, wrapper, notice, &mut patches)?;
    let updated = apply_patches(&xml, patches)?;
    if updated.len() as u64 > super::MAX_DOCUMENT_BYTES {
        return Err(invalid("Automation journal metadata exceeds its limit"));
    }
    parse_document(&updated)?;
    lock.publish_document(&stamp, Some(&updated), |opened| {
        require_active(cancelled)?;
        authorize()?;
        source.require_location(source.directory(), opened)
    })?;
    lock.directory().sync()
}

pub(super) fn is_native_source(directory: &Path, id: &str) -> bool {
    native_id(id)
        && directory
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .and_then(|name| name.strip_prefix("journal-"))
            == Some(id)
}

fn native_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub(super) enum Update<'a> {
    /// Start a reviewed new pass after the selected AI profile passed preflight. Existing cards
    /// remain visible until the new pass has evidence for their ranges.
    Start {
        generation: &'a str,
        replay_from: Option<u64>,
    },
    Window {
        window: &'a Window,
        card: Option<&'a Card>,
        usage: Option<ObservedUsage>,
    },
    Finish {
        closing: Option<&'a Summary>,
        at: Timestamp,
        title_pending: bool,
        usage: Option<ObservedUsage>,
    },
    Title {
        generated: Option<(&'a TitleCandidate, &'a str)>,
        allowed: &'a dyn Fn() -> bool,
        usage: Option<ObservedUsage>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct TitleCandidate {
    original: Option<String>,
    pub(super) connection: String,
}

impl TitleCandidate {
    pub(super) fn read(source: &Source) -> io::Result<Option<Self>> {
        source.with_document(|_, document| Self::from_meta(meta(document)?))
    }

    fn from_meta(meta: Node<'_, '_>) -> io::Result<Option<Self>> {
        require_java_fields(meta, &["title", "connectionName", MANUAL_TITLE])?;
        if boolean(meta, MANUAL_TITLE)? {
            return Ok(None);
        }
        let original = field(meta, "title")?.map(Cow::into_owned);
        let connection = field(meta, "connectionName")?
            .unwrap_or_default()
            .into_owned();
        let default = original.as_deref().is_none_or(|title| {
            title.trim().is_empty()
                || title == connection
                || (!connection.is_empty()
                    && title
                        .strip_prefix(&connection)
                        .and_then(|rest| rest.strip_prefix(" — "))
                        .is_some_and(|suffix| {
                            suffix.len() == 16
                                && jiff::civil::DateTime::strptime("%Y-%m-%d %H:%M", suffix).is_ok()
                        }))
        });
        Ok(default.then_some(Self {
            original,
            connection,
        }))
    }
}

/// Applies an optimistic progress transition under the existing writer/delete lock. The selected
/// physical directory and document ID are both rechecked before publishing; raw logs are read-only.
pub(super) fn apply(
    source: &Source,
    expected: &Progress,
    update: Update<'_>,
    cancelled: &AtomicBool,
    authorize: Option<&super::summary_ai::Authorization>,
) -> io::Result<(Progress, Journal)> {
    apply_guarded(source, expected, update, cancelled, || {
        super::summary_ai::authorize_publication(authorize)
    })
}

pub(super) fn apply_guarded(
    source: &Source,
    expected: &Progress,
    update: Update<'_>,
    cancelled: &AtomicBool,
    authorize: impl Fn() -> io::Result<()>,
) -> io::Result<(Progress, Journal)> {
    require_active(cancelled)?;
    let lock = lock_source(source)?;
    let (stamp, xml) = lock.read_document()?;
    let document = parse_document(&xml)?;
    if document_id(&document)? != source.id() {
        return Err(invalid("The selected journal changed during summarization"));
    }
    let title_guard = match &update {
        Update::Title {
            generated: Some(_),
            allowed,
            ..
        } if allowed() => Some(*allowed),
        _ => None,
    };
    let update = match update {
        Update::Title { allowed, usage, .. } if title_guard.is_none() => Update::Title {
            generated: None,
            allowed,
            usage,
        },
        update => update,
    };
    let (next, output) = patch(&xml, &document, expected, update)?;
    let updated = parse_document(&output)?;
    let snapshot = journal(source.directory(), &updated)?;
    lock.publish_document(&stamp, Some(&output), |opened| {
        authorize()?;
        require_active(cancelled)?;
        if title_guard.is_some_and(|allowed| !allowed()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Automatic journal titles are disabled",
            ));
        }
        source.require_location(source.directory(), opened)
    })?;
    lock.directory().sync()?;
    Ok((next, snapshot))
}

impl Source {
    pub(crate) fn record_ai_usage(&self, observed: ObservedUsage) -> io::Result<UsageEdit> {
        self.record_ai_usage_checked(observed, || Ok(()))
    }

    pub(super) fn record_ai_usage_checked(
        &self,
        observed: ObservedUsage,
        verify: impl Fn() -> io::Result<()>,
    ) -> io::Result<UsageEdit> {
        let lock = lock_source(self)?;
        let (stamp, xml) = lock.read_document()?;
        let document = parse_document(&xml)?;
        if document_id(&document)? != self.id() {
            return Err(invalid("The selected session journal identity changed"));
        }
        let meta = meta(&document)?;
        if field(meta, "endedAt")?.is_none_or(|ended| ended.trim().is_empty()) {
            return Err(invalid(
                "A recording journal must save usage through its writer",
            ));
        }
        let mut usage = TokenUsage::read(meta)?;
        let profile_fields = usage.observe_stopped(meta, observed)?;
        let fields = usage.fields().collect::<Vec<_>>();
        let mut patches = Vec::new();
        patch_scalars(
            &xml,
            meta,
            fields
                .iter()
                .map(|(name, value)| (*name, Some(value.as_str())))
                .chain(
                    profile_fields
                        .iter()
                        .flatten()
                        .map(|(name, value)| (*name, value.as_deref())),
                ),
            &mut patches,
        )?;
        let updated = apply_patches(&xml, patches)?;
        parse_document(&updated)?;
        lock.publish_document(&stamp, Some(&updated), |opened| {
            verify()?;
            self.require_location(self.directory(), opened)
        })?;
        lock.directory().sync()?;
        let (new_stamp, published) = lock.read_document()?;
        if published != updated {
            return Err(invalid(
                "The saved journal changed during usage publication",
            ));
        }
        Ok((
            self.document_identity(stamp),
            self.document_identity(new_stamp),
        ))
    }
}

/// Recovers interactive captures in the configured root without starting an AI request.
pub(crate) fn recover_interactive(root: &Path, cancelled: &AtomicBool) -> io::Result<usize> {
    require_active(cancelled)?;
    let opened_root = match Directory::open(root, false) {
        Ok(opened) => opened,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let root_identity = opened_root.identity()?;
    let mut recovered = 0;
    // Enumeration supplies names only. Every open and mutation is bound to the original root
    // handle and then to the source identity before publication.
    for entry in std::fs::read_dir(root)? {
        require_active(cancelled)?;
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some((name, id)) = name
            .to_str()
            .and_then(|name| name.strip_prefix("journal-").map(|id| (name, id)))
        else {
            continue;
        };
        if !native_id(id) {
            continue;
        }
        let path = root.join(name);
        let result = (|| {
            let opened = opened_root.private_child(name, false)?;
            if !may_need_interactive_recovery(&opened, id)? {
                return Ok(false);
            }
            let source = Source::from_open_directory(&path, id, &opened)?;
            if !source.belongs_to_root(root_identity) {
                return Err(invalid("The configured session journal root changed"));
            }
            let lock = lock_existing_metadata_in(&path, opened)?;
            finish_abandoned_locked(&source, &lock, cancelled, None, true)
        })();
        if result.is_ok_and(|changed| changed) {
            recovered += 1;
        }
    }
    require_active(cancelled)?;
    Ok(recovered)
}

fn may_need_interactive_recovery(directory: &Directory, id: &str) -> io::Result<bool> {
    let file = directory.open_private_file(DOCUMENT)?;
    if file.metadata()?.permissions().readonly() {
        return Ok(false);
    }
    let mut prefix = Vec::with_capacity(RECOVERY_PREFIX_BYTES as usize);
    file.take(RECOVERY_PREFIX_BYTES).read_to_end(&mut prefix)?;
    let Some(end) = prefix
        .windows(b"</meta>".len())
        .position(|bytes| bytes == b"</meta>")
    else {
        return Ok(true);
    };
    prefix.truncate(end + b"</meta>".len());
    prefix.extend_from_slice(b"</session-journal>");
    let metadata = std::str::from_utf8(&prefix)
        .map_err(|_| invalid("Session journal metadata is not UTF-8"))
        .and_then(parse_document)
        .and_then(|document| {
            Ok(document_id(&document)? == id && interactive_open(meta(&document)?)?)
        });
    // An unusual or ambiguous prefix goes through the regular parser under the writer lock.
    Ok(metadata.unwrap_or(true))
}

fn interactive_open(meta: Node<'_, '_>) -> io::Result<bool> {
    Ok(number::<Timestamp>(meta, "endedAt")?.is_none() && field(meta, "sourceKind")?.is_none())
}

/// Finalizes a native capture left open by a previous process. The writer lock must be free
/// before the document can change.
pub(crate) fn finish_abandoned(source: &Source, cancelled: &AtomicBool) -> io::Result<()> {
    finish_abandoned_authorized(source, cancelled, None)
}

pub(crate) fn finish_abandoned_authorized(
    source: &Source,
    cancelled: &AtomicBool,
    authorize: Option<&super::summary_ai::Authorization>,
) -> io::Result<()> {
    require_active(cancelled)?;
    if !is_native_source(source.directory(), source.id()) {
        return Err(invalid("Expected an app-owned native session journal"));
    }
    let lock = lock_source(source)?;
    finish_abandoned_locked(source, &lock, cancelled, authorize, false).map(|_| ())
}

fn finish_abandoned_locked(
    source: &Source,
    lock: &MetadataLock,
    cancelled: &AtomicBool,
    authorize: Option<&super::summary_ai::Authorization>,
    interactive: bool,
) -> io::Result<bool> {
    require_active(cancelled)?;
    let (stamp, xml) = if interactive {
        let Some(document) = lock.read_writable_private_document()? else {
            return Ok(false);
        };
        document
    } else {
        lock.read_document()?
    };
    let document = parse_document(&xml)?;
    if document_id(&document)? != source.id() {
        return Err(invalid("The abandoned session journal changed"));
    }
    let meta = meta(&document)?;
    let open = if interactive {
        interactive_open(meta)?
    } else {
        number::<Timestamp>(meta, "endedAt")?.is_none()
    };
    if !open {
        return Ok(false);
    }
    let mut ended = number::<Timestamp>(meta, "startedAt")?
        .ok_or_else(|| invalid("Missing journal start time"))?;
    let (latest, commands) = super::command_count::read(source, cancelled)?;
    if let Some(latest) = latest {
        ended = ended.max(latest);
    }
    let ended = ended.to_string();
    let commands = commands.map(|count| count.to_string());
    let mut patches = Vec::new();
    if let Some(commands) = &commands {
        patch_scalars(
            &xml,
            meta,
            [
                ("endedAt", Some(ended.as_str())),
                ("commandCount", Some(commands.as_str())),
            ],
            &mut patches,
        )?;
    } else {
        patch_scalars(
            &xml,
            meta,
            [("endedAt", Some(ended.as_str()))],
            &mut patches,
        )?;
    }
    let updated = apply_patches(&xml, patches)?;
    lock.publish_document(&stamp, Some(&updated), |opened| {
        super::summary_ai::authorize_publication(authorize)?;
        require_active(cancelled)?;
        source.require_location(source.directory(), opened)
    })?;
    lock.directory().sync()?;
    Ok(true)
}

#[cfg(test)]
pub(super) fn refresh_command_count(
    source: &Source,
    cancelled: &AtomicBool,
    authorize: Option<&super::summary_ai::Authorization>,
) -> io::Result<()> {
    refresh_command_count_guarded(source, cancelled, || {
        super::summary_ai::authorize_publication(authorize)
    })
}

pub(super) fn refresh_command_count_guarded(
    source: &Source,
    cancelled: &AtomicBool,
    authorize: impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    require_active(cancelled)?;
    let lock = lock_source(source)?;
    let (stamp, xml) = lock.read_document()?;
    let document = parse_document(&xml)?;
    if document_id(&document)? != source.id() {
        return Err(invalid("The closed session journal changed"));
    }
    let meta = meta(&document)?;
    if number::<Timestamp>(meta, "endedAt")?.is_none() {
        return Err(invalid(
            "Finish the journal before classifying its commands",
        ));
    }
    let (_, Some(count)) = super::command_count::read(source, cancelled)? else {
        return Ok(());
    };
    if number::<u64>(meta, "commandCount")? == Some(count) {
        return Ok(());
    }
    let count = count.to_string();
    let mut patches = Vec::new();
    patch_scalars(
        &xml,
        meta,
        [("commandCount", Some(count.as_str()))],
        &mut patches,
    )?;
    let updated = apply_patches(&xml, patches)?;
    lock.publish_document(&stamp, Some(&updated), |opened| {
        authorize()?;
        require_active(cancelled)?;
        source.require_location(source.directory(), opened)
    })?;
    lock.directory().sync()
}

fn require_active(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Journal summary cancelled",
        ))
    } else {
        Ok(())
    }
}

fn meta<'a, 'input>(document: &'a Document<'input>) -> io::Result<Node<'a, 'input>> {
    child(document.root_element(), "meta")
        .ok_or_else(|| invalid("Missing session journal metadata"))
}

fn app_language(meta: Node<'_, '_>) -> String {
    language::read(meta, language::APP_LANGUAGE)
        .map(Cow::into_owned)
        .unwrap_or_else(|| language::DEFAULT.into())
}

fn progress(meta: Node<'_, '_>) -> io::Result<Progress> {
    let generation = field(meta, GENERATION)?.map(|value| value.into_owned());
    if generation
        .as_deref()
        .is_some_and(|id| !valid_generation(id))
    {
        return Err(invalid("Invalid session journal summary generation"));
    }
    let last_ai = number(meta, LAST_AI)?.unwrap_or_default();
    let cursor = number(meta, CURSOR)?.unwrap_or(last_ai);
    if last_ai > cursor {
        return Err(invalid("Invalid session journal summary checkpoint"));
    }
    let language = generation
        .as_ref()
        .and_then(|_| language::read(meta, GENERATION_LANGUAGE))
        .map(Cow::into_owned);
    let finished = boolean(meta, FINISHED)?;
    let title_pending = boolean(meta, TITLE_PENDING)?;
    if title_pending && (finished || generation.is_none()) {
        return Err(invalid("Invalid journal title checkpoint"));
    }
    Ok(Progress {
        generation,
        language,
        cursor,
        last_ai,
        finished,
        raw: boolean(meta, RAW)?,
        title_pending,
    })
}

fn patch(
    xml: &str,
    document: &Document<'_>,
    expected: &Progress,
    update: Update<'_>,
) -> io::Result<(Progress, String)> {
    let meta = meta(document)?;
    if number::<Timestamp>(meta, "endedAt")?.is_none() || progress(meta)? != *expected {
        return Err(invalid("The journal is recording or its summary changed"));
    }
    let wrapper = child(document.root_element(), "entries")
        .ok_or_else(|| invalid("Missing session journal entries"))?;
    require_java_fields(wrapper, &[])?;
    let mut patches = Vec::new();
    let mut next = expected.clone();
    let mut added = String::new();
    let mut metadata = Vec::new();
    let observed = match update {
        Update::Start {
            generation,
            replay_from,
        } => {
            if !valid_generation(generation) {
                return Err(invalid("Invalid session journal summary generation"));
            }
            if next.generation.as_deref() != Some(generation) || next.language.is_none() {
                next.language = Some(app_language(meta));
            }
            next.generation = Some(generation.to_owned());
            next.finished = false;
            next.title_pending = false;
            if let Some(checkpoint) = replay_from {
                if checkpoint > next.last_ai {
                    return Err(invalid("Invalid journal summary replay checkpoint"));
                }
                next.cursor = checkpoint;
                next.last_ai = checkpoint;
                next.raw = false;
            }
            None
        }
        Update::Window {
            window,
            card,
            usage,
        } => {
            let last = window
                .checkpoint()
                .ok_or_else(|| invalid("A closed journal cannot publish a running preview"))?;
            if next.generation.is_none()
                || next.finished
                || next.title_pending
                || window.first <= next.cursor
                || last < window.first
                || window.skipped != card.is_none()
            {
                return Err(invalid("Invalid session journal summary range"));
            }
            patch_window(xml, document, window, card, &mut patches, &mut added)?;
            next.cursor = last;
            next.raw |= card.is_some_and(|card| card.state == State::Raw);
            if !next.raw {
                next.last_ai = last;
            }
            usage
        }
        Update::Finish {
            closing,
            at,
            title_pending,
            usage,
        } => {
            if next.generation.is_none()
                || next.finished
                || next.title_pending
                || closing.is_none() && title_pending
            {
                return Err(invalid("There is no active closing summary pass"));
            }
            if let Some(closing) = closing {
                let mut reused = false;
                for entry in entries(document) {
                    if field(entry, "kind")?.as_deref() != Some(WRAP_UP) {
                        continue;
                    }
                    if !reused {
                        patch_scalars(
                            xml,
                            entry,
                            [
                                ("title", Some(closing.title.as_str())),
                                ("text", Some(closing.text.as_str())),
                            ],
                            &mut patches,
                        )?;
                        reused = true;
                    } else {
                        preserve_or_remove(xml, entry, &mut patches)?;
                    }
                }
                if !reused {
                    new_entry(&mut added, WRAP_UP, at, &closing.title, &closing.text)?;
                    added.push_str("</entry>");
                }
                patch_list(
                    xml,
                    meta,
                    "aiKeywords",
                    "keyword",
                    &closing.keywords,
                    &mut patches,
                )?;
            } else {
                next.raw = true;
            }
            next.title_pending = title_pending;
            next.finished = !title_pending;
            usage
        }
        Update::Title {
            generated, usage, ..
        } => {
            if !next.title_pending {
                return Err(invalid("There is no pending automatic journal title"));
            }
            if let Some((reviewed, title)) = generated
                && TitleCandidate::from_meta(meta)?.as_ref() == Some(reviewed)
                && reviewed.original.as_deref() != Some(title)
            {
                metadata.push(("title", Some(title)));
            }
            next.title_pending = false;
            next.finished = true;
            usage
        }
    };
    let usage_fields = if let Some(observed) = observed {
        let mut usage = TokenUsage::read(meta)?;
        let profile_fields = usage.observe_stopped(meta, observed)?;
        Some((usage.fields().collect::<Vec<_>>(), profile_fields))
    } else {
        None
    };
    if let Some((fields, profiles)) = &usage_fields {
        metadata.extend(
            fields
                .iter()
                .map(|(name, value)| (*name, Some(value.as_str())))
                .chain(
                    profiles
                        .iter()
                        .flatten()
                        .map(|(name, value)| (*name, value.as_deref())),
                ),
        );
    }
    let cursor = next.cursor.to_string();
    let last_ai = next.last_ai.to_string();
    metadata.extend([
        (GENERATION, next.generation.as_deref()),
        (GENERATION_LANGUAGE, next.language.as_deref()),
        (CURSOR, Some(cursor.as_str())),
        (LAST_AI, Some(last_ai.as_str())),
        (FINISHED, Some(if next.finished { "true" } else { "false" })),
        (TITLE_PENDING, next.title_pending.then_some("true")),
        (RAW, Some(if next.raw { "true" } else { "false" })),
    ]);
    patch_scalars(xml, meta, metadata, &mut patches)?;
    append_children(xml, wrapper, added, &mut patches)?;
    Ok((next, apply_patches(xml, patches)?))
}

type Patches = Vec<(Range<usize>, String)>;

fn patch_window(
    xml: &str,
    document: &Document<'_>,
    window: &Window,
    card: Option<&Card>,
    patches: &mut Patches,
    added: &mut String,
) -> io::Result<()> {
    let first = window.first.to_string();
    let last = window.last.to_string();
    let command_state = match window.end {
        End::Complete => None,
        End::Running => Some("RUNNING"),
        End::SessionEnded => Some("ENDED_WITH_SESSION"),
    };
    let generated = card.map(|card| {
        let state = match card.state {
            State::Raw => "RAW",
            State::Summarized => "SUMMARIZED",
        };
        [
            ("title", Some(card.summary.title.as_str())),
            ("text", Some(card.summary.text.as_str())),
            ("state", Some(state)),
            ("runAsUser", card.run_as_user.as_deref()),
            ("commandState", command_state),
            ("logStartSeq", Some(first.as_str())),
            ("logEndSeq", Some(last.as_str())),
        ]
    });
    let mut count = 0;
    let mut reused = false;
    for entry in entries(document) {
        if field(entry, "kind")?.as_deref() != Some(AI_CARD) {
            continue;
        }
        let start = number::<u64>(entry, "logStartSeq")?;
        let end = number::<u64>(entry, "logEndSeq")?;
        let overlaps = start
            .zip(end)
            .is_some_and(|(start, end)| start <= window.last && end >= window.first);
        if !overlaps {
            count += 1;
            continue;
        }
        if !reused
            && start == Some(window.first)
            && let Some((card, generated)) = card.zip(generated)
        {
            patch_scalars(xml, entry, generated, patches)?;
            patch_list(xml, entry, "inputExcerpt", "line", &card.input, patches)?;
            patch_list(xml, entry, "outputExcerpt", "line", &card.output, patches)?;
            reused = true;
        } else {
            preserve_or_remove(xml, entry, patches)?;
        }
    }
    if !reused && let Some((card, generated)) = card.zip(generated) {
        if count >= MAX_CARDS {
            return Err(invalid("The journal reached its summary card limit"));
        }
        new_entry(added, AI_CARD, window.started, "", "")?;
        for (name, text) in generated {
            if let Some(text) = text {
                element(added, name, text);
            }
        }
        list(added, "inputExcerpt", "line", &card.input);
        list(added, "outputExcerpt", "line", &card.output);
        added.push_str("</entry>");
    }
    Ok(())
}

fn preserve_or_remove(xml: &str, entry: Node<'_, '_>, patches: &mut Patches) -> io::Result<()> {
    let annotated = field(entry, "userNote")?.is_some_and(|note| !note.trim().is_empty())
        || matches!(
            field(entry, "markerSource")?.as_deref(),
            Some("MANUAL" | "USER")
        )
        || field(entry, "editedAt")?.is_some();
    if annotated {
        patch_scalars(xml, entry, [("kind", Some("USER_NOTE"))], patches)
    } else {
        patches.push((entry.range(), String::new()));
        Ok(())
    }
}

fn new_entry(
    xml: &mut String,
    kind: &str,
    at: Timestamp,
    title: &str,
    body: &str,
) -> io::Result<()> {
    xml.push_str("<entry>");
    element(xml, "id", &crate::private_files::nonce()?);
    element(xml, "kind", kind);
    element(xml, "createdAt", &at.to_string());
    if !title.is_empty() {
        element(xml, "title", title);
    }
    if !body.is_empty() {
        element(xml, "text", body);
    }
    Ok(())
}

fn patch_list(
    xml: &str,
    parent: Node<'_, '_>,
    wrapper: &'static str,
    item: &'static str,
    values: &[String],
    patches: &mut Patches,
) -> io::Result<()> {
    require_java_fields(parent, &[wrapper])?;
    let mut found = parent.children().filter(|node| node.has_tag_name(wrapper));
    let original = found.next();
    if found.next().is_some() {
        return Err(invalid("Duplicate journal summary list"));
    }
    let mut updated = String::new();
    list(&mut updated, wrapper, item, values);
    if let Some(original) = original {
        patches.push((original.range(), updated));
        Ok(())
    } else {
        append_children(xml, parent, updated, patches)
    }
}

fn list(xml: &mut String, wrapper: &str, item: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    let _ = write!(xml, "<{wrapper}>");
    for value in values {
        element(xml, item, value);
    }
    let _ = write!(xml, "</{wrapper}>");
}

fn element(xml: &mut String, name: &str, value: &str) {
    let _ = write!(xml, "<{name}>{}</{name}>", escape(value));
}

fn valid_generation(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_journal::{CaptureKind, CaptureRecord, summary_plan::Planner};

    #[test]
    fn closing_journal_title_requires_an_unchanged_default_at_publication() {
        let xml = r#"<session-journal formatVersion="1" id="fixture"><meta>
            <title>Shell — 2026-11-14 08:30</title><connectionName>Shell</connectionName>
            <endedAt>2026-11-14T08:40:00Z</endedAt>
            <nativeSummaryGeneration>0123456789abcdef0123456789abcdef</nativeSummaryGeneration>
            <nativeSummaryTitlePending>true</nativeSummaryTitlePending>
            </meta><entries/></session-journal>"#;
        let original = parse_document(xml).unwrap();
        let candidate = TitleCandidate::from_meta(meta(&original).unwrap())
            .unwrap()
            .unwrap();
        for (xml, wanted) in [
            (xml.to_owned(), "Check build"),
            (
                xml.replace("Shell — 2026-11-14 08:30", "A new manual title"),
                "A new manual title",
            ),
            (
                xml.replace(
                    "</meta>",
                    "<titleManuallySet>true</titleManuallySet></meta>",
                ),
                "Shell — 2026-11-14 08:30",
            ),
            (xml.replace("08:30", "08:31"), "Shell — 2026-11-14 08:31"),
        ] {
            let document = parse_document(&xml).unwrap();
            let checkpoint = progress(meta(&document).unwrap()).unwrap();
            let (next, updated) = patch(
                &xml,
                &document,
                &checkpoint,
                Update::Title {
                    generated: Some((&candidate, "Check build")),
                    allowed: &|| true,
                    usage: None,
                },
            )
            .unwrap();
            assert!(next.finished && !next.title_pending);
            let updated = parse_document(&updated).unwrap();
            assert_eq!(
                field(meta(&updated).unwrap(), "title").unwrap().as_deref(),
                Some(wanted)
            );
        }
    }

    #[test]
    fn journal_summary_range_replacement_keeps_the_card_identity_and_manual_fields() {
        let xml = r#"<session-journal formatVersion="1" id="fixture"><meta>
          <title>Handwritten title</title><titleManuallySet>true</titleManuallySet>
          <endedAt>2026-01-01T01:00:00Z</endedAt><nativeSummaryGeneration>0123456789abcdef0123456789abcdef</nativeSummaryGeneration>
          </meta><entries><entry><id>card</id><kind>AI_SUMMARY</kind><createdAt>2026-01-01T00:00:00Z</createdAt>
          <title>Previous</title><text>Old model output</text><logStartSeq>1</logStartSeq><logEndSeq>1</logEndSeq>
          <userNote>Keep this annotation</userNote><marker>ERROR</marker><markerSource>MANUAL</markerSource></entry>
          <entry><id>note</id><kind>USER_NOTE</kind><text>Unrelated note</text></entry></entries></session-journal>"#;
        let document = parse_document(xml).unwrap();
        let expected = progress(meta(&document).unwrap()).unwrap();
        let mut planner = Planner::resume(0);
        let time = Timestamp::from_second(1).unwrap();
        let _ = planner.push(CaptureRecord {
            sequence: 1,
            timestamp: time,
            kind: CaptureKind::Output,
            text: "New output".into(),
            redacted: false,
            partial: false,
            repeat: 1,
        });
        let window = planner.finish_pass(true, true, time).pop().unwrap();
        let card = Card::raw(&window);
        let (next, updated) = patch(
            xml,
            &document,
            &expected,
            Update::Window {
                window: &window,
                card: Some(&card),
                usage: None,
            },
        )
        .unwrap();
        let new = parse_document(&updated).unwrap();
        assert_eq!(next.cursor, 1);
        let rows: Vec<_> = entries(&new).collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(field(rows[0], "id").unwrap().as_deref(), Some("card"));
        assert_eq!(
            field(rows[0], "userNote").unwrap().as_deref(),
            Some("Keep this annotation")
        );
        assert_eq!(
            field(rows[0], "markerSource").unwrap().as_deref(),
            Some("MANUAL")
        );
        assert_eq!(
            field(rows[1], "text").unwrap().as_deref(),
            Some("Unrelated note")
        );

        let closing = Summary {
            title: "Model title".into(),
            text: "New result".into(),
            keywords: vec!["result".into()],
        };
        let (_, final_xml) = patch(
            &updated,
            &new,
            &next,
            Update::Finish {
                closing: Some(&closing),
                at: time,
                title_pending: false,
                usage: None,
            },
        )
        .unwrap();
        let final_document = parse_document(&final_xml).unwrap();
        assert_eq!(
            field(meta(&final_document).unwrap(), "title")
                .unwrap()
                .as_deref(),
            Some("Handwritten title")
        );
        assert_eq!(
            entries(&final_document)
                .filter(|entry| field(*entry, "kind").unwrap().as_deref() == Some(WRAP_UP))
                .count(),
            1
        );
    }
}
