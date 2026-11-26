//! Typed AI settings and memory validation for portable profile backups.

use std::cell::Cell;
use std::io::{self, Write as _};

use zeroize::Zeroizing;

use super::memory::{Scope, store};
use super::profiles::java_backup;
use super::skills::{statistics, usage};
use super::{Configuration, FILE_NAME, JavaProfileSource, MAX_BYTES, java_knowledge, java_models};
use crate::clipboard_policy::ClipboardPolicy;
use crate::private_files::{Directory, FileLock, FileStamp, require_single_link};

pub(crate) const SETTINGS: &str = FILE_NAME;
pub(crate) const STATISTICS: &str = statistics::BACKUP_PATH;
pub(crate) const JAVA_STATISTICS: &str = statistics::java::DRAFT;
pub(crate) const MAX_STATISTICS_BYTES: usize = statistics::MAX_BYTES;
pub(crate) const MEMORY: &str = "memory";
pub(crate) const MEMORY_TOP_LEVEL: [&str; 2] = ["MEMORY.md", "archive.md"];
pub(crate) const MEMORY_DIRECTORIES: [&str; 2] = ["connections", "history"];

pub(crate) fn lock_for_restore(root: &Directory) -> io::Result<FileLock> {
    Configuration::lock_mutation(root)
}

pub(crate) struct JavaStatistics {
    pub(crate) bytes: Zeroizing<Vec<u8>>,
    pub(crate) matched: usize,
    pub(crate) historical: usize,
    pub(crate) invalid: usize,
    pub(crate) settings_omitted: bool,
}

pub(crate) fn portable_java_statistics(
    bytes: &[u8],
    settings: Option<&[u8]>,
) -> io::Result<JavaStatistics> {
    let imported = statistics::java::convert(bytes, settings)?;
    Ok(JavaStatistics {
        bytes: imported.bytes,
        matched: imported.matched,
        historical: imported.historical,
        invalid: imported.invalid,
        settings_omitted: imported.settings_omitted,
    })
}

pub(crate) fn validate_java_statistics(bytes: &[u8]) -> io::Result<()> {
    statistics::java::decode_draft(bytes).map(drop)
}

pub(crate) fn publish_java_statistics(
    root: &Directory,
    bytes: &[u8],
    allowed: impl Fn() -> bool,
) -> io::Result<()> {
    if !allowed() {
        return Ok(());
    }
    let configuration = match root.open_file(SETTINGS) {
        Ok(file) => {
            require_single_link(&file)?;
            Configuration::read_preference(file)?
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Configuration::default(),
        Err(error) => return Err(error),
    };
    let draft = statistics::java::decode_draft(bytes)?;
    statistics::java::publish(root, draft, &configuration.skills, allowed)
}

/// Publishes a prepared Java catalogue while the settings mutation lock is held.
pub(crate) fn publish_java_profiles(root: &Directory, contents: &[u8]) -> io::Result<()> {
    publish_java_profiles_with_policy(root, contents, ClipboardPolicy::load)
}

fn publish_java_profiles_with_policy(
    root: &Directory,
    contents: &[u8],
    policy: impl Fn() -> ClipboardPolicy,
) -> io::Result<()> {
    let current_policy = policy();
    if !current_policy.ai_profile_creation_allowed() {
        return Ok(());
    }
    let candidate = Configuration::decode(contents)?;
    let (stamp, edits_profiles, adds_local_ai_hints, changes_web_offer) = match root
        .open_file(SETTINGS)
    {
        Ok(file) => {
            require_single_link(&file)?;
            let stamp = FileStamp::read(&file)?;
            let current = Configuration::read_preference(file)?;
            (
                Some(stamp),
                current
                    .profiles
                    .changes_existing_java_profiles(&candidate.profiles),
                current.java_model_hints != candidate.java_model_hints
                    || current.java_knowledge_hints != candidate.java_knowledge_hints,
                current.general.agent_always_offer_web_tools
                    != candidate.general.agent_always_offer_web_tools
                    || current.general.agent_web_offer_revision
                        != candidate.general.agent_web_offer_revision,
            )
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (
            None,
            false,
            !candidate.java_model_hints.is_empty() || !candidate.java_knowledge_hints.is_empty(),
            candidate.general.agent_always_offer_web_tools.is_some()
                || candidate.general.agent_web_offer_revision.is_some(),
        ),
        Err(error) => return Err(error),
    };
    if edits_profiles && !current_policy.ai_profile_edit_allowed()
        || adds_local_ai_hints && !java_ai_hints_allowed(&current_policy)
        || changes_web_offer && !current_policy.ai_web_search_allowed
    {
        return Ok(());
    }
    let policy_denied = Cell::new(false);
    let check_policy = || {
        let policy = policy();
        if policy.ai_profile_creation_allowed()
            && (!edits_profiles || policy.ai_profile_edit_allowed())
            && (!adds_local_ai_hints || java_ai_hints_allowed(&policy))
            && (!changes_web_offer || policy.ai_web_search_allowed)
        {
            Ok(())
        } else {
            policy_denied.set(true);
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Managed policy prevents this Java AI profile import",
            ))
        }
    };
    let write = |file: &mut std::fs::File| file.write_all(contents);
    let result = match stamp {
        Some(stamp) => root.replace_with(SETTINGS, write, || {
            let file = root.open_file(SETTINGS)?;
            require_single_link(&file)?;
            stamp.require_unchanged(&file)?;
            check_policy()
        }),
        None => root.publish_checked(SETTINGS, write, check_policy),
    };
    if policy_denied.get() {
        return Ok(());
    }
    result
}

pub(crate) fn java_ai_hints_allowed(policy: &ClipboardPolicy) -> bool {
    policy.ai_allowed && policy.ai_profile_creation_allowed()
}

pub(crate) struct JavaKnowledgeReport {
    pub(crate) bytes: Option<Vec<u8>>,
    pub(crate) stores: usize,
    pub(crate) sources: usize,
    pub(crate) stores_omitted: usize,
    pub(crate) sources_omitted: usize,
    pub(crate) destinations_omitted: usize,
}

pub(crate) fn portable_java_knowledge_hints(
    settings: Option<&[u8]>,
    bytes: &[u8],
) -> io::Result<JavaKnowledgeReport> {
    let imported = java_knowledge::import(bytes)?;
    let mut settings = settings
        .map(Configuration::decode)
        .transpose()?
        .unwrap_or_default();
    let added = settings.java_knowledge_hints.merge(imported.hints);
    Ok(JavaKnowledgeReport {
        bytes: (added != 0)
            .then(|| encode_java_profiles(&settings))
            .transpose()?,
        stores: added,
        sources: imported.sources,
        stores_omitted: imported.stores_omitted + imported.stores - added,
        sources_omitted: imported.sources_omitted,
        destinations_omitted: imported.destinations_omitted,
    })
}

pub(crate) struct JavaModelReport {
    pub(crate) bytes: Option<Vec<u8>>,
    pub(crate) imported: usize,
    pub(crate) invalid: usize,
    pub(crate) dates_omitted: usize,
}

pub(crate) fn portable_java_model_hints(
    settings: Option<&[u8]>,
    kind: java_models::Kind,
    bytes: &[u8],
) -> io::Result<JavaModelReport> {
    let imported = java_models::import(kind, bytes)?;
    let mut settings = settings
        .map(Configuration::decode)
        .transpose()?
        .unwrap_or_default();
    let merge = settings.java_model_hints.merge(imported.hints);
    Ok(JavaModelReport {
        bytes: (merge.added != 0)
            .then(|| encode_java_profiles(&settings))
            .transpose()?,
        imported: merge.added,
        invalid: imported.invalid + merge.full,
        dates_omitted: imported.dates_omitted,
    })
}

pub(crate) struct JavaProfileReport {
    pub(crate) general_preferences: bool,
    pub(crate) request_timeout_omitted: bool,
    pub(crate) code_text_language_omitted: bool,
    pub(crate) agent_triage_omitted: bool,
    pub(crate) agent_always_offer_web_tools_omitted: bool,
    pub(crate) agent_run_presentation_omitted: bool,
    pub(crate) https_imported: usize,
    pub(crate) loopback_imported: usize,
    pub(crate) embedded_imported: usize,
    pub(crate) mlx_reasoning_omitted: usize,
    pub(crate) cli_imported: usize,
    pub(crate) omitted: usize,
    pub(crate) keys_omitted: usize,
    pub(crate) pricing_omitted: usize,
    pub(crate) default_reasoning_omitted: usize,
    pub(crate) web_hints: usize,
    pub(crate) web_hints_omitted: usize,
    pub(crate) journal_profile_hints: usize,
    pub(crate) role_hints: usize,
    pub(crate) role_hints_omitted: usize,
    pub(crate) journal_evidence_hint: bool,
    pub(crate) journal_evidence_omitted: bool,
    pub(crate) journal_preferences_hint: bool,
    pub(crate) journal_preferences_omitted: bool,
    pub(crate) memory_tokens_hint: bool,
    pub(crate) memory_tokens_omitted: bool,
    pub(crate) memory_preferences_hint: bool,
    pub(crate) memory_preferences_omitted: usize,
    pub(crate) skill_preferences_hint: bool,
    pub(crate) skill_preferences_omitted: usize,
    pub(crate) skill_provider_hints: usize,
    pub(crate) skill_providers_omitted: usize,
    pub(crate) skill_provider_secrets_omitted: usize,
    pub(crate) agent_shortcut_hint: bool,
    pub(crate) agent_shortcut_omitted: bool,
    pub(crate) agent_execution_hint: bool,
    pub(crate) agent_execution_omitted: bool,
    pub(crate) agent_confirmation_hint: bool,
    pub(crate) agent_confirmation_omitted: bool,
    pub(crate) journal_semantic_search_hint: bool,
    pub(crate) journal_semantic_search_omitted: bool,
    sources: Vec<JavaProfileSource>,
}

impl JavaProfileReport {
    pub(crate) fn java_profile_sources(&self) -> impl Iterator<Item = &JavaProfileSource> {
        self.sources.iter()
    }
}

pub(crate) fn portable_java_profiles(bytes: &[u8]) -> io::Result<(Vec<u8>, JavaProfileReport)> {
    let (profiles, report) = java_backup::decode(bytes)?;
    let settings = Configuration {
        profiles,
        java_general_preferences: report.general_preferences,
        java_journal_profiles: report.journal_profiles,
        java_roles: report.roles,
        java_memory_tokens: report.memory_tokens,
        java_memory_preferences: report.memory_preferences,
        java_skill_preferences: report.skill_preferences,
        java_skill_providers: report.skill_providers,
        java_agent_shortcut: report.agent_shortcut,
        java_agent_execution_enabled: report.agent_execution_enabled,
        java_agent_confirm_mutating: report.agent_confirm_mutating,
        java_journal_semantic_search: report.journal_semantic_search,
        ..Configuration::default()
    };
    let encoded = encode_java_profiles(&settings)?;
    Ok((
        encoded,
        JavaProfileReport {
            general_preferences: !settings.java_general_preferences.is_empty(),
            request_timeout_omitted: report.request_timeout_omitted,
            code_text_language_omitted: report.code_text_language_omitted,
            agent_triage_omitted: report.agent_triage_omitted,
            agent_always_offer_web_tools_omitted: report.agent_always_offer_web_tools_omitted,
            agent_run_presentation_omitted: report.agent_run_presentation_omitted,
            https_imported: report.https_imported,
            loopback_imported: report.loopback_imported,
            embedded_imported: report.embedded_imported,
            mlx_reasoning_omitted: report.mlx_reasoning_omitted,
            cli_imported: report.cli_imported,
            omitted: report.omitted,
            keys_omitted: report.keys_omitted,
            pricing_omitted: report.pricing_omitted,
            default_reasoning_omitted: report.default_reasoning_omitted,
            web_hints: report.web_hints,
            web_hints_omitted: report.web_hints_omitted,
            journal_profile_hints: settings.java_journal_profiles.hints().count(),
            role_hints: settings.java_roles.iter().count(),
            role_hints_omitted: report.roles_omitted,
            journal_evidence_hint: settings.java_journal_profiles.evidence.is_some(),
            journal_evidence_omitted: report.journal_evidence_omitted,
            journal_preferences_hint: settings.java_journal_profiles.preferences.is_some(),
            journal_preferences_omitted: report.journal_preferences_omitted,
            memory_tokens_hint: settings.java_memory_tokens.is_some(),
            memory_tokens_omitted: report.memory_tokens_omitted,
            memory_preferences_hint: !settings.java_memory_preferences.is_empty(),
            memory_preferences_omitted: report.memory_preferences_omitted,
            skill_preferences_hint: !settings.java_skill_preferences.is_empty(),
            skill_preferences_omitted: report.skill_preferences_omitted,
            skill_provider_hints: settings.java_skill_providers.len(),
            skill_providers_omitted: report.skill_providers_omitted,
            skill_provider_secrets_omitted: report.skill_provider_secrets_omitted,
            agent_shortcut_hint: settings.java_agent_shortcut.is_some(),
            agent_shortcut_omitted: report.agent_shortcut_omitted,
            agent_execution_hint: settings.java_agent_execution_enabled.is_some(),
            agent_execution_omitted: report.agent_execution_omitted,
            agent_confirmation_hint: settings.java_agent_confirm_mutating.is_some(),
            agent_confirmation_omitted: report.agent_confirmation_omitted,
            journal_semantic_search_hint: settings.java_journal_semantic_search.is_some(),
            journal_semantic_search_omitted: report.journal_semantic_search_omitted,
            sources: settings.profiles.java_profile_sources().cloned().collect(),
        },
    ))
}

pub(crate) fn merge_java_profiles(
    local: Option<&[u8]>,
    incoming: &[u8],
    policy: &ClipboardPolicy,
) -> io::Result<Option<Vec<u8>>> {
    let incoming = Configuration::decode(incoming)?;
    let empty = Configuration::default();
    if incoming.general != empty.general
        || incoming.local != empty.local
        || incoming.skills != empty.skills
        || incoming.memory != empty.memory
    {
        return Err(super::invalid("Invalid Java AI profile settings"));
    }
    incoming.profiles.validate_java_profiles()?;
    incoming.java_roles.validate_incoming(&incoming.profiles)?;
    for hint in incoming.java_journal_profiles.hints() {
        let source = incoming
            .profiles
            .java_profile_sources()
            .find(|source| source.id() == hint.id());
        if hint.source() != source {
            return Err(super::invalid(
                "Java Journal profile hint does not match its imported draft",
            ));
        }
    }
    if !policy.ai_profile_creation_allowed() {
        return Ok(None);
    }
    let saved = local;
    let mut local = saved
        .map(Configuration::decode)
        .transpose()?
        .unwrap_or_default();
    let mut general_preferences = incoming.java_general_preferences;
    if !policy.ai_web_search_allowed {
        general_preferences.agent_always_offer_web_tools = None;
    }
    let general_added = merge_java_general(&mut local, saved, general_preferences)?;
    let toolpack_hints_added = policy.ai_profile_edit_allowed()
        && local
            .profiles
            .restore_missing_toolpack_hints(&incoming.profiles);
    let added = local.profiles.merge_java_profiles(incoming.profiles)?;
    let confirmation_added = local.java_agent_confirm_mutating.is_none()
        && incoming.java_agent_confirm_mutating.is_some();
    if confirmation_added {
        local.java_agent_confirm_mutating = incoming.java_agent_confirm_mutating;
    }
    let execution_added = local.java_agent_execution_enabled.is_none()
        && incoming.java_agent_execution_enabled.is_some();
    if execution_added {
        local.java_agent_execution_enabled = incoming.java_agent_execution_enabled;
    }
    let skill_preferences_added = local
        .java_skill_preferences
        .merge(incoming.java_skill_preferences);
    let skill_provider_hints_added = local
        .java_skill_providers
        .merge(incoming.java_skill_providers);
    let memory_preferences_added = local
        .java_memory_preferences
        .merge(incoming.java_memory_preferences);
    let semantic_search_added = local.java_journal_semantic_search.is_none()
        && incoming.java_journal_semantic_search.is_some();
    if semantic_search_added {
        local.java_journal_semantic_search = incoming.java_journal_semantic_search;
    }
    let agent_shortcut_added = if let Some(incoming) = incoming.java_agent_shortcut {
        match local.java_agent_shortcut.as_mut() {
            Some(current) => current.merge(incoming),
            None => {
                local.java_agent_shortcut = Some(incoming);
                true
            }
        }
    } else {
        false
    };
    let memory_tokens_added =
        local.java_memory_tokens.is_none() && incoming.java_memory_tokens.is_some();
    if memory_tokens_added {
        local.java_memory_tokens = incoming.java_memory_tokens;
    }
    let role_hints_added = local.java_roles.merge(incoming.java_roles);
    let model_hints_added = java_ai_hints_allowed(policy)
        && local
            .java_model_hints
            .merge(incoming.java_model_hints)
            .added
            != 0;
    let knowledge_hints_added = java_ai_hints_allowed(policy)
        && local
            .java_knowledge_hints
            .merge(incoming.java_knowledge_hints)
            != 0;
    let journal = &mut local.java_journal_profiles;
    let incoming_journal = incoming.java_journal_profiles;
    let mut hints_added = journal.evidence.is_none() && incoming_journal.evidence.is_some();
    if hints_added {
        journal.evidence = incoming_journal.evidence;
    }
    if let Some(incoming) = incoming_journal.preferences {
        if let Some(current) = journal.preferences.as_mut() {
            if current.interval_minutes.is_none() && incoming.interval_minutes.is_some() {
                current.interval_minutes = incoming.interval_minutes;
                hints_added = true;
            }
            if current.automatic_title.is_none() && incoming.automatic_title.is_some() {
                current.automatic_title = incoming.automatic_title;
                hints_added = true;
            }
        } else {
            journal.preferences = Some(incoming);
            hints_added = true;
        }
    }
    for (current, incoming) in [
        (&mut journal.recording, incoming_journal.recording),
        (&mut journal.automation, incoming_journal.automation),
    ] {
        if current.is_none() && incoming.is_some() {
            *current = incoming;
            hints_added = true;
        }
    }
    if added == 0
        && !general_added
        && !confirmation_added
        && !execution_added
        && !memory_preferences_added
        && !skill_preferences_added
        && !skill_provider_hints_added
        && !semantic_search_added
        && !agent_shortcut_added
        && !memory_tokens_added
        && !hints_added
        && !toolpack_hints_added
        && !role_hints_added
        && !model_hints_added
        && !knowledge_hints_added
    {
        return Ok(None);
    }
    encode_java_profiles(&local).map(Some)
}

fn merge_java_general(
    config: &mut Configuration,
    saved: Option<&[u8]>,
    incoming: super::JavaGeneralPreferences,
) -> io::Result<bool> {
    if incoming.is_empty() {
        return Ok(false);
    }
    let saved: Option<serde_json::Value> = saved.map(serde_json::from_slice).transpose()?;
    let general = saved.as_ref().and_then(|saved| saved.get("general"));
    let unset = |key| general.is_none_or(|general| general.get(key).is_none());
    let mut changed = false;
    if let Some(minutes) = incoming.timeout_minutes
        && unset("timeout_minutes")
    {
        config.general.timeout_minutes = minutes;
        changed = true;
    }
    if let Some(language) = incoming.code_text_language
        && unset("code_text_language")
    {
        config.general.code_text_language = language;
        changed = true;
    }
    if let Some(mode) = incoming.agent_triage
        && unset("agent_triage")
    {
        config.general.agent_triage = mode;
        changed = true;
    }
    if let Some(always) = incoming.agent_always_offer_web_tools
        && config.general.agent_always_offer_web_tools.is_none()
    {
        config.general.agent_web_offer_revision = config
            .general
            .agent_web_offer_revision_after(Some(always))?;
        config.general.agent_always_offer_web_tools = Some(always);
        changed = true;
    }
    if let Some(target) = incoming.agent_run_presentation
        && unset("agent_run_presentation")
    {
        config.general.agent_run_presentation = target;
        changed = true;
    }
    Ok(changed)
}

fn encode_java_profiles(settings: &Configuration) -> io::Result<Vec<u8>> {
    settings.validate()?;
    let bytes = serde_json::to_vec(settings).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(super::invalid(
            "AI profile settings exceed their size limit",
        ));
    }
    Ok(bytes)
}

pub(crate) fn flush_statistics() {
    usage::flush();
}

pub(crate) fn file_limit(path: &str) -> Option<usize> {
    match path {
        SETTINGS => Some(MAX_BYTES as usize),
        STATISTICS => Some(statistics::MAX_BYTES),
        "memory/MEMORY.md" => Some(store::MAX_BYTES),
        "memory/archive.md" => Some(store::MAX_ARCHIVE),
        _ => {
            let (directory, name) = path.strip_prefix("memory/")?.split_once('/')?;
            match directory {
                "connections" if connection_id(name.strip_suffix(".md")?).is_some() => {
                    Some(store::MAX_BYTES)
                }
                "history" if history_name(name) => Some(store::MAX_BYTES),
                _ => None,
            }
        }
    }
}

pub(crate) fn memory_connection(path: &str) -> Option<u64> {
    let name = path.strip_prefix("memory/")?;
    if let Some(name) = name.strip_prefix("connections/") {
        return connection_id(name.strip_suffix(".md")?);
    }
    connection_id(name.strip_prefix("history/connection-")?.split_once('-')?.0)
}

fn history_name(name: &str) -> bool {
    let scope = if name.starts_with("global-") {
        Scope::global()
    } else {
        let Some(id) = name
            .strip_prefix("connection-")
            .and_then(|tail| tail.split_once('-'))
            .and_then(|(id, _)| connection_id(id))
        else {
            return false;
        };
        Scope::connection(id, String::new())
    };
    store::history_name(name, &scope) && name.len() <= 64
}

fn connection_id(text: &str) -> Option<u64> {
    if text.is_empty() || text.starts_with('0') || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

pub(crate) fn validate(path: &str, bytes: &[u8]) -> io::Result<()> {
    match path {
        SETTINGS => Configuration::decode(bytes).map(drop),
        STATISTICS => statistics::decode(bytes).map(drop),
        _ => std::str::from_utf8(bytes)
            .map(drop)
            .map_err(io::Error::other),
    }
}

pub(crate) fn portable_settings(bytes: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut settings = Configuration::decode(bytes)?;
    settings.profiles.prepare_backup();
    settings.local.prepare_backup();
    settings.validate()?;
    serde_json::to_vec(&settings)
        .map(Zeroizing::new)
        .map_err(io::Error::other)
}

pub(crate) fn matching_skills(current: Option<&[u8]>, incoming: Option<&[u8]>) -> bool {
    let decode = |bytes: Option<&[u8]>| {
        bytes.map_or_else(|| Ok(Configuration::default()), Configuration::decode)
    };
    match (decode(current), decode(incoming)) {
        (Ok(current), Ok(incoming)) => current.skills.same_definitions(&incoming.skills),
        _ => false,
    }
}

pub(crate) fn safe_memory(path: &str, bytes: &[u8]) -> bool {
    file_limit(path).is_some_and(|limit| {
        std::str::from_utf8(bytes).is_ok_and(|text| store::validate(text, "", limit).is_ok())
    })
}

#[cfg(test)]
mod java_profile_tests {
    #[test]
    fn java_agent_presentation_fills_only_an_unset_preference() {
        use crate::terminal_agent::RunPresentation;

        let policy = ClipboardPolicy::default();
        let (incoming, report) = portable_java_profiles(
            br#"<globalSettings><terminalAgentExecutionTarget>CHAT_WINDOW</terminalAgentExecutionTarget></globalSettings>"#,
        )
        .unwrap();
        assert!(report.general_preferences && !report.agent_run_presentation_omitted);
        let merged = merge_java_profiles(None, &incoming, &policy)
            .unwrap()
            .unwrap();
        assert_eq!(
            Configuration::decode(&merged)
                .unwrap()
                .agent_run_presentation(),
            RunPresentation::ChatTab
        );
        let explicit = br#"{"general":{"agent_run_presentation":"terminal_window"}}"#;
        assert!(
            merge_java_profiles(Some(explicit), &incoming, &policy)
                .unwrap()
                .is_none()
        );

        let (_, report) = portable_java_profiles(
            br#"<globalSettings><terminalAgentExecutionTarget>UNKNOWN</terminalAgentExecutionTarget></globalSettings>"#,
        )
        .unwrap();
        assert!(report.agent_run_presentation_omitted && !report.general_preferences);
    }

    #[test]
    fn java_agent_triage_fills_only_an_unset_preference() {
        use crate::terminal_agent::planning::TriageMode;

        let policy = ClipboardPolicy::default();
        let (incoming, report) = portable_java_profiles(
            br#"<globalSettings><aiAgentTriageMode>ALWAYS</aiAgentTriageMode></globalSettings>"#,
        )
        .unwrap();
        assert!(report.general_preferences && !report.agent_triage_omitted);
        let merged = merge_java_profiles(None, &incoming, &policy)
            .unwrap()
            .unwrap();
        assert_eq!(
            Configuration::decode(&merged)
                .unwrap()
                .agent_triage(&policy),
            TriageMode::Always
        );
        let explicit = br#"{"general":{"agent_triage":"auto"}}"#;
        assert!(
            merge_java_profiles(Some(explicit), &incoming, &policy)
                .unwrap()
                .is_none()
        );

        let (_, report) = portable_java_profiles(
            br#"<globalSettings><aiAgentTriageMode>OTHER</aiAgentTriageMode></globalSettings>"#,
        )
        .unwrap();
        assert!(report.agent_triage_omitted && !report.general_preferences);
    }

    #[test]
    fn java_agent_web_offer_fills_only_unset_preferences_and_respects_policy() {
        let policy = ClipboardPolicy::default();
        let (incoming, report) = portable_java_profiles(
            br#"<globalSettings>
            <aiAgentAlwaysOfferWebTools>true</aiAgentAlwaysOfferWebTools>
            </globalSettings>"#,
        )
        .unwrap();
        assert!(report.general_preferences && !report.agent_always_offer_web_tools_omitted);
        let merged = merge_java_profiles(None, &incoming, &policy)
            .unwrap()
            .unwrap();
        let saved = Configuration::decode(&merged).unwrap();
        assert!(saved.agent_always_offer_web_tools());
        assert!(saved.general.agent_web_offer_revision.is_some());
        assert!(
            merge_java_profiles(Some(&merged), &incoming, &policy)
                .unwrap()
                .is_none()
        );

        let explicit_off = br#"{"general":{"agent_always_offer_web_tools":false}}"#;
        assert!(
            merge_java_profiles(Some(explicit_off), &incoming, &policy)
                .unwrap()
                .is_none()
        );
        let blocked = crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n[[rule]]\n[rule.ai-profiles]\nallow-internet = false\n",
        )
        .unwrap();
        assert!(
            merge_java_profiles(None, &incoming, &blocked)
                .unwrap()
                .is_none()
        );

        let (off, _) = portable_java_profiles(
            br#"<globalSettings>
            <aiAgentAlwaysOfferWebTools>false</aiAgentAlwaysOfferWebTools>
            </globalSettings>"#,
        )
        .unwrap();
        let explicitly_off = merge_java_profiles(None, &off, &policy).unwrap().unwrap();
        assert_eq!(
            Configuration::decode(&explicitly_off)
                .unwrap()
                .general
                .agent_always_offer_web_tools,
            Some(false)
        );
        assert!(
            merge_java_profiles(Some(&explicitly_off), &incoming, &policy)
                .unwrap()
                .is_none()
        );

        let (_, bad) = portable_java_profiles(
            br#"<globalSettings>
            <aiAgentAlwaysOfferWebTools>maybe</aiAgentAlwaysOfferWebTools>
            </globalSettings>"#,
        )
        .unwrap();
        assert!(bad.agent_always_offer_web_tools_omitted && !bad.general_preferences);
    }

    #[test]
    fn java_general_ai_preferences_fill_only_unset_native_fields() {
        use crate::ai_manager::CodeTextLanguage;

        let xml = br#"<globalSettings><aiRequestTimeoutMinutes>23</aiRequestTimeoutMinutes>
            <aiCodeTextDefaultLanguage>DE</aiCodeTextDefaultLanguage></globalSettings>"#;
        let (incoming, report) = portable_java_profiles(xml).unwrap();
        assert!(report.general_preferences);
        assert!(!report.request_timeout_omitted);
        assert!(!report.code_text_language_omitted);
        let policy = ClipboardPolicy::default();
        let merged = merge_java_profiles(None, &incoming, &policy)
            .unwrap()
            .unwrap();
        let settings = Configuration::decode(&merged).unwrap();
        assert_eq!(settings.general.timeout_minutes, 23);
        assert_eq!(
            settings.general.code_text_language,
            CodeTextLanguage::German
        );
        assert!(settings.java_general_preferences.is_empty());
        assert!(
            merge_java_profiles(Some(&merged), &incoming, &policy)
                .unwrap()
                .is_none()
        );

        let local = br#"{"general":{"timeout_minutes":1,"features_enabled":false}}"#;
        let merged = merge_java_profiles(Some(local), &incoming, &policy)
            .unwrap()
            .unwrap();
        let settings = Configuration::decode(&merged).unwrap();
        assert_eq!(settings.general.timeout_minutes, 1);
        assert_eq!(
            settings.general.code_text_language,
            CodeTextLanguage::German
        );
        assert!(!settings.general.features_enabled);

        let local = br#"{"general":{"code_text_language":"auto"}}"#;
        let merged = merge_java_profiles(Some(local), &incoming, &policy)
            .unwrap()
            .unwrap();
        let settings = Configuration::decode(&merged).unwrap();
        assert_eq!(settings.general.timeout_minutes, 23);
        assert_eq!(settings.general.code_text_language, CodeTextLanguage::Auto);

        let (unlimited, report) = portable_java_profiles(br#"<globalSettings>
            <aiRequestTimeoutMinutes>0</aiRequestTimeoutMinutes><aiCodeTextDefaultLanguage>cs</aiCodeTextDefaultLanguage>
            </globalSettings>"#).unwrap();
        assert!(report.general_preferences);
        assert!(!report.request_timeout_omitted);
        assert!(report.code_text_language_omitted);
        let merged = merge_java_profiles(None, &unlimited, &policy)
            .unwrap()
            .unwrap();
        assert_eq!(
            Configuration::decode(&merged)
                .unwrap()
                .general
                .timeout_minutes,
            0
        );
        let (_, report) = portable_java_profiles(br#"<globalSettings><aiRequestTimeoutMinutes>1441</aiRequestTimeoutMinutes></globalSettings>"#).unwrap();
        assert!(report.request_timeout_omitted);
    }

    #[test]
    fn java_skill_provider_metadata_remains_inactive_and_omits_secrets() {
        let (bytes, report) = portable_java_profiles(
            br#"
            <globalSettings><aiSkillProviders>
                <provider>
                    <id>java-http</id><name>Private catalogue</name><type>HTTP</type>
                    <baseUrl>https://catalogue.invalid/skills</baseUrl><auth>BASIC</auth>
                    <username>reader</username><encryptedSecret>ciphertext-fixture</encryptedSecret>
                    <enabled>true</enabled>
                </provider>
                <provider>
                    <id>invalid-url</id><type>HTTP</type>
                    <baseUrl>https://fixture:private@catalogue.invalid/skills</baseUrl>
                </provider>
            </aiSkillProviders></globalSettings>
        "#,
        )
        .unwrap();
        assert_eq!(report.skill_provider_hints, 1);
        assert_eq!(report.skill_providers_omitted, 1);
        assert_eq!(report.skill_provider_secrets_omitted, 1);
        let serialized = std::str::from_utf8(&bytes).unwrap();
        assert!(!serialized.contains("ciphertext-fixture"));
        assert!(!serialized.contains("fixture:private"));
        let restored = Configuration::decode(&bytes).unwrap();
        assert_eq!(restored.skills, super::super::skills::Store::default());
        assert_eq!(
            restored
                .java_skill_providers
                .get("java-http")
                .unwrap()
                .username,
            "reader"
        );
        let policy = ClipboardPolicy::default();
        let merged = merge_java_profiles(None, &bytes, &policy).unwrap().unwrap();
        assert_eq!(
            Configuration::decode(&merged).unwrap().java_skill_providers,
            restored.java_skill_providers
        );
        assert!(
            merge_java_profiles(Some(&merged), &bytes, &policy)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn java_skill_preferences_are_inactive_additive_and_report_invalid_siblings() {
        use crate::ai_manager::skills::Preference;
        let (bytes, report) = portable_java_profiles(br#"<globalSettings><aiSkillsEnabled>false</aiSkillsEnabled><aiSkillAutoDetectionEnabled>invalid</aiSkillAutoDetectionEnabled><aiSkillAutoCreate>0</aiSkillAutoCreate><aiSkillStatisticsEnabled>false</aiSkillStatisticsEnabled></globalSettings>"#).unwrap();
        assert!(report.skill_preferences_hint);
        assert_eq!(report.skill_preferences_omitted, 1);
        let config = Configuration::decode(&bytes).unwrap();
        assert_eq!(config.skills, super::super::skills::Store::default());
        assert_eq!(
            config.java_skill_preferences.get(Preference::Enabled),
            Some(false)
        );
        assert_eq!(
            config.java_skill_preferences.get(Preference::AutoDetect),
            None
        );
        let (later, _) = portable_java_profiles(br#"<globalSettings><aiSkillsEnabled>true</aiSkillsEnabled><aiSkillAutoDetectionEnabled>false</aiSkillAutoDetectionEnabled></globalSettings>"#).unwrap();
        let merged = merge_java_profiles(Some(&bytes), &later, &ClipboardPolicy::default())
            .unwrap()
            .unwrap();
        let restored = Configuration::decode(&merged).unwrap();
        assert_eq!(
            restored.java_skill_preferences.get(Preference::Enabled),
            Some(false)
        );
        assert_eq!(
            restored.java_skill_preferences.get(Preference::AutoDetect),
            Some(false)
        );
        assert_eq!(restored.skills, config.skills);
        assert!(
            merge_java_profiles(Some(&merged), &later, &ClipboardPolicy::default())
                .unwrap()
                .is_none()
        );
        let portable = serde_json::to_vec(&restored).unwrap();
        assert_eq!(
            Configuration::decode(&portable)
                .unwrap()
                .java_skill_preferences,
            restored.java_skill_preferences
        );
        let (_, duplicates) = portable_java_profiles(br#"<globalSettings><aiSkillsEnabled>true</aiSkillsEnabled><aiSkillsEnabled>false</aiSkillsEnabled><aiSkillStatisticsEnabled>1</aiSkillStatisticsEnabled></globalSettings>"#).unwrap();
        assert_eq!(duplicates.skill_preferences_omitted, 1);
        assert!(duplicates.skill_preferences_hint);
        let denied = ClipboardPolicy {
            ai_profile_creation_allowed: false,
            ..ClipboardPolicy::default()
        };
        assert!(
            merge_java_profiles(None, &later, &denied)
                .unwrap()
                .is_none()
        );
    }

    use std::path::PathBuf;

    use super::*;

    const FIXTURE: &[u8] = include_bytes!("profiles/fixtures/java-global-settings.xml");
    const TOOLPACK: &str = r#"<globalSettings>
        <aiLmStudioToolpackMcpPluginId>mcp/fixture-toolpack</aiLmStudioToolpackMcpPluginId>
        <aiProfiles><profile><id>original-toolpack</id><name>LM Studio Toolpack</name>
            <model>local/model</model><apiUrl>https://localhost:1234/api/v1/chat</apiUrl>
            <internetAccessMode>LM_STUDIO_TOOLPACK</internetAccessMode>
        </profile></aiProfiles></globalSettings>"#;

    #[test]
    fn java_profile_delta_checks_current_creation_policy_and_native_settings() {
        let (incoming, report) = portable_java_profiles(FIXTURE).unwrap();
        assert_eq!(report.https_imported, 2);
        assert_eq!(report.cli_imported, 1);
        assert_eq!(report.journal_profile_hints, 2);
        let sources = report.java_profile_sources().collect::<Vec<_>>();
        assert_eq!(sources.len(), 3);
        let denied = ClipboardPolicy {
            ai_profile_creation_allowed: false,
            ..ClipboardPolicy::default()
        };
        assert!(
            merge_java_profiles(None, &incoming, &denied)
                .unwrap()
                .is_none()
        );

        let mut local = Configuration::default();
        local.general.timeout_minutes = 19;
        let local = serde_json::to_vec(&local).unwrap();
        let allowed = ClipboardPolicy::default();
        let merged = merge_java_profiles(Some(&local), &incoming, &allowed)
            .unwrap()
            .unwrap();
        let merged_config = Configuration::decode(&merged).unwrap();
        assert_eq!(merged_config.general.timeout_minutes, 19);
        assert!(merged_config.profiles.default_profile().is_none());
        assert_eq!(
            merged_config.resolve_imported_journal_summary_profile(false),
            Some(1)
        );
        assert_eq!(
            merged_config.resolve_imported_journal_summary_profile(true),
            Some(3)
        );
        assert!(merged_config.journal_summary_default_id().is_none());
        for (index, source) in sources.into_iter().enumerate() {
            let reference = super::super::ProfileRef::Numeric(index as u64 + 1);
            assert_eq!(merged_config.java_profile_source(&reference), Some(source));
        }
        assert!(
            merge_java_profiles(Some(&merged), &incoming, &allowed)
                .unwrap()
                .is_none()
        );
        assert!(merge_java_profiles(None, &merged, &allowed).is_err());
    }

    #[test]
    fn imported_journal_hints_stick_to_the_reviewed_definition_and_do_not_overwrite_local_hints() {
        let allowed = ClipboardPolicy::default();
        let (incoming, _) = portable_java_profiles(FIXTURE).unwrap();
        let original = merge_java_profiles(None, &incoming, &allowed)
            .unwrap()
            .unwrap();
        let config = Configuration::decode(&original).unwrap();
        let recording = config
            .imported_journal_summary_profile(false)
            .unwrap()
            .clone();
        let automation = config
            .imported_journal_summary_profile(true)
            .unwrap()
            .clone();
        let xml = std::str::from_utf8(FIXTURE).unwrap();
        let replaced = xml.replace("family/model", "family/different-model");
        let (different, _) = portable_java_profiles(replaced.as_bytes()).unwrap();

        let mut local: serde_json::Value = serde_json::from_slice(&different).unwrap();
        local
            .as_object_mut()
            .unwrap()
            .remove("java_journal_profiles");
        let local = serde_json::to_vec(&local).unwrap();
        let merged = merge_java_profiles(Some(&local), &incoming, &allowed)
            .unwrap()
            .unwrap();
        let merged_config = Configuration::decode(&merged).unwrap();
        assert_eq!(
            merged_config.imported_journal_summary_profile(false),
            Some(&recording)
        );
        assert_eq!(
            merged_config.resolve_imported_journal_summary_profile(false),
            None
        );
        assert_eq!(
            merged_config.imported_journal_summary_profile(true),
            Some(&automation)
        );
        assert_eq!(
            merged_config.resolve_imported_journal_summary_profile(true),
            Some(3)
        );
        assert!(
            merge_java_profiles(Some(&merged), &different, &allowed)
                .unwrap()
                .is_none()
        );

        let portable = portable_settings(&merged).unwrap();
        let reopened = Configuration::decode(&portable).unwrap();
        assert_eq!(
            reopened.imported_journal_summary_profile(false),
            Some(&recording)
        );
        assert_eq!(
            reopened.resolve_imported_journal_summary_profile(false),
            None
        );
        assert_eq!(
            reopened.imported_journal_summary_profile(true),
            Some(&automation)
        );
        assert_eq!(
            reopened.resolve_imported_journal_summary_profile(true),
            Some(3)
        );
    }

    #[test]
    fn java_journal_delta_rejects_forged_source_associations() {
        let (incoming, _) = portable_java_profiles(FIXTURE).unwrap();
        let mut incoming: serde_json::Value = serde_json::from_slice(&incoming).unwrap();
        let allowed = ClipboardPolicy::default();
        for source in [
            serde_json::json!(null),
            incoming["java_journal_profiles"]["automation"]["source"].clone(),
            serde_json::json!({
                "id": incoming["java_journal_profiles"]["recording"]["id"],
                "fingerprint": vec![81_u8; 32]
            }),
        ] {
            incoming["java_journal_profiles"]["recording"]["source"] = source;
            assert!(
                merge_java_profiles(None, &serde_json::to_vec(&incoming).unwrap(), &allowed)
                    .is_err()
            );
        }
    }

    #[test]
    fn java_command_confirmation_import_preserves_native_preferences_until_adoption() {
        let import = |value| {
            portable_java_profiles(format!("<globalSettings><terminalAgentConfirmMutatingCommandSets>{value}</terminalAgentConfirmMutatingCommandSets></globalSettings>").as_bytes()).unwrap()
        };
        let (incoming, report) = import("true");
        assert!(report.agent_confirmation_hint);
        let policy = ClipboardPolicy::default();
        let merged = merge_java_profiles(None, &incoming, &policy)
            .unwrap()
            .unwrap();
        let config = Configuration::decode(&portable_settings(&merged).unwrap()).unwrap();
        assert!(!config.agent_requires_command_confirmation());
        assert_eq!(config.java_agent_confirm_mutating, Some(true));
        let (disabled, _) = import("false");
        assert!(
            merge_java_profiles(Some(&merged), &disabled, &policy)
                .unwrap()
                .is_none()
        );
        let (bad, report) = import("invalid");
        assert!(report.agent_confirmation_omitted);
        assert!(
            Configuration::decode(&bad)
                .unwrap()
                .java_agent_confirm_mutating
                .is_none()
        );
    }

    #[test]
    fn java_agent_execution_import_does_not_activate_or_override_native_settings() {
        let import = |value| {
            portable_java_profiles(format!("<globalSettings><terminalAgentExecutionEnabled>{value}</terminalAgentExecutionEnabled></globalSettings>").as_bytes()).unwrap()
        };
        let (incoming, report) = import("0");
        assert!(report.agent_execution_hint);
        let policy = ClipboardPolicy::default();
        let merged = merge_java_profiles(None, &incoming, &policy)
            .unwrap()
            .unwrap();
        let config = Configuration::decode(&portable_settings(&merged).unwrap()).unwrap();
        assert_eq!(config.java_agent_execution_enabled, Some(false));
        assert!(config.agent_execution_enabled());
        let (enabled, _) = import("true");
        assert!(
            merge_java_profiles(Some(&merged), &enabled, &policy)
                .unwrap()
                .is_none()
        );
        let native: Configuration = serde_json::from_value(
            serde_json::json!({"general":{"agent_execution_enabled":false}}),
        )
        .unwrap();
        let merged = merge_java_profiles(
            Some(&serde_json::to_vec(&native).unwrap()),
            &enabled,
            &policy,
        )
        .unwrap()
        .unwrap();
        assert!(
            !Configuration::decode(&merged)
                .unwrap()
                .agent_execution_enabled()
        );
        let (bad, report) = import("invalid");
        assert!(report.agent_execution_omitted);
        assert!(
            Configuration::decode(&bad)
                .unwrap()
                .java_agent_execution_enabled
                .is_none()
        );
    }

    #[test]
    fn java_memory_activation_and_learning_fill_only_missing_hints_and_report_individual_errors() {
        use super::super::memory::{JavaPreferences, Learning};
        let import = |fields: &str| {
            portable_java_profiles(format!("<globalSettings>{fields}</globalSettings>").as_bytes())
                .unwrap()
        };
        let (enabled, report) = import("<aiMemoryEnabled>0</aiMemoryEnabled>");
        assert!(report.memory_preferences_hint);
        let mut native = Configuration::default();
        native.memory.max_tokens = 700;
        let policy = ClipboardPolicy::default();
        let merged = merge_java_profiles(
            Some(&serde_json::to_vec(&native).unwrap()),
            &enabled,
            &policy,
        )
        .unwrap()
        .unwrap();
        let (learning, _) = import(
            "<aiMemoryEnabled>true</aiMemoryEnabled><aiMemoryLearningMode>OFF</aiMemoryLearningMode>",
        );
        let merged = merge_java_profiles(Some(&merged), &learning, &policy)
            .unwrap()
            .unwrap();
        assert!(
            merge_java_profiles(Some(&merged), &learning, &policy)
                .unwrap()
                .is_none()
        );
        let config = Configuration::decode(&portable_settings(&merged).unwrap()).unwrap();
        assert_eq!(config.memory, native.memory);
        assert_eq!(
            config.java_memory_preferences,
            JavaPreferences {
                enabled: Some(false),
                learning: Some(Learning::Off)
            }
        );
        let (invalid, report) = import(
            "<aiMemoryEnabled>true</aiMemoryEnabled><aiMemoryLearningMode>INVALID</aiMemoryLearningMode>",
        );
        assert_eq!(report.memory_preferences_omitted, 1);
        assert_eq!(
            Configuration::decode(&invalid)
                .unwrap()
                .java_memory_preferences,
            JavaPreferences {
                enabled: Some(true),
                learning: None
            }
        );
        let (invalid, report) = import(
            "<aiMemoryEnabled>invalid</aiMemoryEnabled><aiMemoryLearningMode>PROPOSE</aiMemoryLearningMode>",
        );
        assert_eq!(report.memory_preferences_omitted, 1);
        assert_eq!(
            Configuration::decode(&invalid)
                .unwrap()
                .java_memory_preferences,
            JavaPreferences {
                enabled: None,
                learning: Some(Learning::Propose)
            }
        );
        assert!(Configuration::default().java_memory_preferences.is_empty());
    }

    #[test]
    fn java_semantic_search_preference_remains_an_inactive_additive_hint() {
        let import = |value| {
            portable_java_profiles(format!("<globalSettings><sessionJournalSemanticSearchEnabled>{value}</sessionJournalSemanticSearchEnabled></globalSettings>").as_bytes()).unwrap()
        };
        let (incoming, report) = import("1");
        assert!(report.journal_semantic_search_hint);
        let policy = ClipboardPolicy::default();
        let merged = merge_java_profiles(None, &incoming, &policy)
            .unwrap()
            .unwrap();
        let config = Configuration::decode(&portable_settings(&merged).unwrap()).unwrap();
        assert_eq!(config.java_journal_semantic_search, Some(true));
        assert!(!config.journal_semantic_search());
        let (disabled, _) = import("false");
        assert!(
            merge_java_profiles(Some(&merged), &disabled, &policy)
                .unwrap()
                .is_none()
        );
        let mut native = Configuration::default();
        native.general.journal_semantic_search = true;
        let merged = merge_java_profiles(
            Some(&serde_json::to_vec(&native).unwrap()),
            &disabled,
            &policy,
        )
        .unwrap()
        .unwrap();
        let config = Configuration::decode(&merged).unwrap();
        assert!(config.journal_semantic_search());
        assert_eq!(config.java_journal_semantic_search, Some(false));
        let (bad, report) = import("invalid");
        assert!(report.journal_semantic_search_omitted);
        assert!(
            Configuration::decode(&bad)
                .unwrap()
                .java_journal_semantic_search
                .is_none()
        );
    }

    #[test]
    fn java_agent_commands_merge_independent_hints_without_replacing_native_settings() {
        let import = |fields: &str| {
            portable_java_profiles(format!("<globalSettings>{fields}</globalSettings>").as_bytes())
                .unwrap()
        };
        let name = "inspect".repeat(8);
        let (incoming, report) = import(&format!(
            "<terminalAgentCommandName> {name} </terminalAgentCommandName>"
        ));
        assert!(report.agent_shortcut_hint);
        assert!(!report.agent_shortcut_omitted);
        let mut native = Configuration::default();
        native.general.agent_shortcut.name = "native".into();
        native.general.agent_shortcut.enabled = true;
        let allowed = ClipboardPolicy::default();
        let merged = merge_java_profiles(
            Some(&serde_json::to_vec(&native).unwrap()),
            &incoming,
            &allowed,
        )
        .unwrap()
        .unwrap();
        let (second, _) = import(
            "<terminalAgentCommandName>other</terminalAgentCommandName><terminalAgentCommandNameCaseInsensitive>1</terminalAgentCommandNameCaseInsensitive>",
        );
        let merged = merge_java_profiles(Some(&merged), &second, &allowed)
            .unwrap()
            .unwrap();
        assert!(
            merge_java_profiles(Some(&merged), &second, &allowed)
                .unwrap()
                .is_none()
        );
        let config = Configuration::decode(&portable_settings(&merged).unwrap()).unwrap();
        assert_eq!(config.general.agent_shortcut, native.general.agent_shortcut);
        assert_eq!(
            config.java_agent_shortcut.unwrap(),
            super::super::JavaAgentShortcut {
                name: Some(name),
                case_insensitive: Some(true)
            }
        );
        let (blank, _) = import("<terminalAgentCommandName> </terminalAgentCommandName>");
        assert_eq!(
            Configuration::decode(&blank)
                .unwrap()
                .java_agent_shortcut
                .unwrap()
                .name
                .as_deref(),
            Some("agent")
        );
        for fields in [
            "<terminalAgentCommandName>two commands</terminalAgentCommandName>",
            "<terminalAgentCommandNameCaseInsensitive>invalid</terminalAgentCommandNameCaseInsensitive>",
        ] {
            let (bad, report) = import(fields);
            assert!(report.agent_shortcut_omitted);
            assert!(
                Configuration::decode(&bad)
                    .unwrap()
                    .java_agent_shortcut
                    .is_none()
            );
        }
    }

    #[test]
    fn java_memory_budget_roundtrips_as_an_inactive_additive_hint() {
        let import = |value| {
            portable_java_profiles(format!("<globalSettings><aiMemoryMaxTokens>{value}</aiMemoryMaxTokens></globalSettings>").as_bytes()).unwrap()
        };
        let (incoming, report) = import("9000");
        assert!(report.memory_tokens_hint);
        assert!(!report.memory_tokens_omitted);
        let mut native = Configuration::default();
        native.memory.enabled = false;
        native.memory.max_tokens = 700;
        let policy = ClipboardPolicy::default();
        let encoded = serde_json::to_vec(&native).unwrap();
        let merged = merge_java_profiles(Some(&encoded), &incoming, &policy)
            .unwrap()
            .unwrap();
        let portable = portable_settings(&merged).unwrap();
        let current = Configuration::decode(&portable).unwrap();
        assert_eq!(current.java_memory_tokens, Some(4000));
        assert_eq!(current.memory, native.memory);
        let (low, _) = import("-1");
        assert_eq!(
            Configuration::decode(&low).unwrap().java_memory_tokens,
            Some(500)
        );
        assert!(
            merge_java_profiles(Some(&merged), &low, &policy)
                .unwrap()
                .is_none()
        );
        let (bad, report) = import("invalid");
        assert!(report.memory_tokens_omitted);
        assert!(
            Configuration::decode(&bad)
                .unwrap()
                .java_memory_tokens
                .is_none()
        );
        let mut invalid = current;
        invalid.java_memory_tokens = Some(499);
        assert!(Configuration::decode(&serde_json::to_vec(&invalid).unwrap()).is_err());
    }

    #[test]
    fn imported_journal_preferences_merge_missing_fields_without_overwriting_or_granting() {
        let policy = ClipboardPolicy::default();
        let import = |fields: &str| {
            portable_java_profiles(format!("<globalSettings>{fields}</globalSettings>").as_bytes())
                .unwrap()
        };
        let (initial, report) = import(
            "<sessionJournalSummarizeIntervalMinutes>999</sessionJournalSummarizeIntervalMinutes>",
        );
        assert!(report.journal_preferences_hint);
        let initial = merge_java_profiles(None, &initial, &policy)
            .unwrap()
            .unwrap();
        let (incoming, _) = import(
            "<sessionJournalSummarizeIntervalMinutes>-3</sessionJournalSummarizeIntervalMinutes><sessionJournalAiTitleEnabled>true</sessionJournalAiTitleEnabled>",
        );
        let merged = merge_java_profiles(Some(&initial), &incoming, &policy)
            .unwrap()
            .unwrap();
        assert!(
            merge_java_profiles(Some(&merged), &incoming, &policy)
                .unwrap()
                .is_none()
        );
        let portable = portable_settings(&merged).unwrap();
        let config = Configuration::decode(&portable).unwrap();
        assert_eq!(
            config.imported_journal_preferences(),
            Some((Some(240), Some(true)))
        );
        assert!(config.profiles.default_profile().is_none());
        assert_eq!(
            Configuration::decode(&incoming)
                .unwrap()
                .imported_journal_preferences(),
            Some((Some(1), Some(true)))
        );
        let (invalid, report) = import(
            "<sessionJournalAiTitleEnabled>invalid</sessionJournalAiTitleEnabled><sessionJournalAiMaxLines>17</sessionJournalAiMaxLines>",
        );
        assert!(report.journal_preferences_omitted);
        let invalid = Configuration::decode(&invalid).unwrap();
        assert!(invalid.imported_journal_preferences().is_none());
        assert!(invalid.imported_journal_evidence_limits().is_some());
        assert!(
            Configuration::default()
                .imported_journal_preferences()
                .is_none()
        );
    }

    #[test]
    fn imported_journal_evidence_stays_an_optional_hint_on_additive_replay_and_backup() {
        use crate::session_journal::summary_plan::EvidenceLimits;

        let limits = EvidenceLimits {
            max_lines_per_direction: 0,
            context_tokens: 12_000,
        };
        let allowed = ClipboardPolicy::default();
        let (incoming, report) = portable_java_profiles(FIXTURE).unwrap();
        assert!(report.journal_evidence_hint);
        assert!(!report.journal_evidence_omitted);
        let merged = merge_java_profiles(None, &incoming, &allowed)
            .unwrap()
            .unwrap();
        let changed = std::str::from_utf8(FIXTURE).unwrap().replace(
            ">12000</sessionJournalAiTokenBudget>",
            ">3000</sessionJournalAiTokenBudget>",
        );
        let (incoming, _) = portable_java_profiles(changed.as_bytes()).unwrap();
        assert!(
            merge_java_profiles(Some(&merged), &incoming, &allowed)
                .unwrap()
                .is_none()
        );
        let portable = portable_settings(&merged).unwrap();
        assert_eq!(
            Configuration::decode(&portable)
                .unwrap()
                .imported_journal_evidence_limits(),
            Some((limits, true))
        );
        assert_eq!(
            Configuration::default().imported_journal_evidence_limits(),
            None
        );
    }

    #[test]
    fn reimport_fills_the_toolpack_hint_only_for_an_unchanged_pending_draft() {
        let allowed = ClipboardPolicy::default();
        let (incoming, _) = portable_java_profiles(TOOLPACK.as_bytes()).unwrap();
        let old_xml = TOOLPACK.replace("mcp/fixture-toolpack", "");
        let (old, _) = portable_java_profiles(old_xml.as_bytes()).unwrap();
        let current = Configuration::decode(&old).unwrap();
        let source = current.profiles.profile(1).unwrap().java_source.as_ref();

        let denied = ClipboardPolicy {
            ai_profile_edit_allowed: false,
            ..ClipboardPolicy::default()
        };
        assert!(
            merge_java_profiles(Some(&old), &incoming, &denied)
                .unwrap()
                .is_none()
        );
        let merged = merge_java_profiles(Some(&old), &incoming, &allowed)
            .unwrap()
            .unwrap();
        let current = Configuration::decode(&merged).unwrap();
        let profile = current.profiles.profile(1).unwrap();
        assert_eq!(profile.java_source.as_ref(), source);
        assert!(profile.restore_review && !profile.has_web_search());
        assert_eq!(
            profile.java_web_hint,
            Some(java_backup::JavaWebHint::Toolpack {
                plugin: "mcp/fixture-toolpack".into(),
            })
        );
        assert!(
            merge_java_profiles(Some(&merged), &incoming, &allowed)
                .unwrap()
                .is_none()
        );
        let changed = TOOLPACK.replace("mcp/fixture-toolpack", "mcp/other-toolpack");
        let (changed, _) = portable_java_profiles(changed.as_bytes()).unwrap();
        assert!(
            merge_java_profiles(Some(&merged), &changed, &allowed)
                .unwrap()
                .is_none()
        );

        let mut edited: serde_json::Value = serde_json::from_slice(&old).unwrap();
        edited["profiles"]["profiles"][0]["endpoint"] = "https://localhost:4321/api/v1/chat".into();
        let edited = serde_json::to_vec(&edited).unwrap();
        assert!(
            merge_java_profiles(Some(&edited), &incoming, &allowed)
                .unwrap()
                .is_none()
        );

        let mut reviewed: serde_json::Value = serde_json::from_slice(&old).unwrap();
        reviewed["profiles"]["profiles"][0]["restore_review"] = false.into();
        reviewed["profiles"]["profiles"][0]["trusted_remote_endpoint"] = true.into();
        let reviewed = serde_json::to_vec(&reviewed).unwrap();
        assert!(
            merge_java_profiles(Some(&reviewed), &incoming, &allowed)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn policy_is_rechecked_before_java_ai_settings_are_published() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-java-ai-profile-{}",
            crate::private_files::nonce().unwrap()
        )));
        let root = Directory::open(&scratch.0, true).unwrap();
        let _lock = lock_for_restore(&root).unwrap();
        let baseline = serde_json::to_vec(&Configuration::default()).unwrap();
        root.publish_copy(SETTINGS, &baseline).unwrap();
        let (incoming, _) = portable_java_profiles(FIXTURE).unwrap();
        let candidate =
            merge_java_profiles(Some(&baseline), &incoming, &ClipboardPolicy::default())
                .unwrap()
                .unwrap();

        let checks = Cell::new(0);
        publish_java_profiles_with_policy(&root, &candidate, || {
            let before = checks.get();
            checks.set(before + 1);
            ClipboardPolicy {
                ai_profile_creation_allowed: before == 0,
                ..ClipboardPolicy::default()
            }
        })
        .unwrap();
        assert_eq!(checks.get(), 2);
        assert_eq!(std::fs::read(scratch.0.join(SETTINGS)).unwrap(), baseline);

        publish_java_profiles_with_policy(&root, &candidate, ClipboardPolicy::default).unwrap();
        assert_eq!(std::fs::read(scratch.0.join(SETTINGS)).unwrap(), candidate);

        let old_xml = TOOLPACK.replace("mcp/fixture-toolpack", "");
        let (old, _) = portable_java_profiles(old_xml.as_bytes()).unwrap();
        root.replace_file(SETTINGS, &old).unwrap();
        let (incoming, _) = portable_java_profiles(TOOLPACK.as_bytes()).unwrap();
        let candidate = merge_java_profiles(Some(&old), &incoming, &ClipboardPolicy::default())
            .unwrap()
            .unwrap();
        checks.set(0);
        publish_java_profiles_with_policy(&root, &candidate, || {
            let before = checks.get();
            checks.set(before + 1);
            ClipboardPolicy {
                ai_profile_edit_allowed: before == 0,
                ..ClipboardPolicy::default()
            }
        })
        .unwrap();
        assert_eq!(checks.get(), 2);
        assert_eq!(std::fs::read(scratch.0.join(SETTINGS)).unwrap(), old);

        let hints = portable_java_model_hints(
            None,
            java_models::Kind::Mlx,
            br#"{"schemaVersion":1,"models":[{"id":"test","displayName":"Test model"}]}"#,
        )
        .unwrap()
        .bytes
        .unwrap();
        let denied = ClipboardPolicy {
            ai_allowed: false,
            ..ClipboardPolicy::default()
        };
        assert!(
            merge_java_profiles(Some(&old), &hints, &denied)
                .unwrap()
                .is_none()
        );
        let candidate = merge_java_profiles(Some(&old), &hints, &ClipboardPolicy::default())
            .unwrap()
            .unwrap();
        checks.set(0);
        publish_java_profiles_with_policy(&root, &candidate, || {
            let before = checks.get();
            checks.set(before + 1);
            ClipboardPolicy {
                ai_allowed: before == 0,
                ..ClipboardPolicy::default()
            }
        })
        .unwrap();
        assert_eq!(checks.get(), 2);
        assert_eq!(std::fs::read(scratch.0.join(SETTINGS)).unwrap(), old);
    }
}
