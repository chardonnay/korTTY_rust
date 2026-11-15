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
    let (stamp, edits_profiles, adds_local_ai_hints) = match root.open_file(SETTINGS) {
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
            )
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (
            None,
            false,
            !candidate.java_model_hints.is_empty() || !candidate.java_knowledge_hints.is_empty(),
        ),
        Err(error) => return Err(error),
    };
    if edits_profiles && !current_policy.ai_profile_edit_allowed()
        || adds_local_ai_hints && !java_ai_hints_allowed(&current_policy)
    {
        return Ok(());
    }
    let policy_denied = Cell::new(false);
    let check_policy = || {
        let policy = policy();
        if policy.ai_profile_creation_allowed()
            && (!edits_profiles || policy.ai_profile_edit_allowed())
            && (!adds_local_ai_hints || java_ai_hints_allowed(&policy))
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
    pub(crate) web_keys_omitted: usize,
    pub(crate) journal_profile_hints: usize,
    pub(crate) role_hints: usize,
    pub(crate) role_hints_omitted: usize,
    pub(crate) journal_evidence_hint: bool,
    pub(crate) journal_evidence_omitted: bool,
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
        java_journal_profiles: report.journal_profiles,
        java_roles: report.roles,
        ..Configuration::default()
    };
    let encoded = encode_java_profiles(&settings)?;
    Ok((
        encoded,
        JavaProfileReport {
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
            web_keys_omitted: report.web_keys_omitted,
            journal_profile_hints: settings.java_journal_profiles.hints().count(),
            role_hints: settings.java_roles.iter().count(),
            role_hints_omitted: report.roles_omitted,
            journal_evidence_hint: settings.java_journal_profiles.evidence.is_some(),
            journal_evidence_omitted: report.journal_evidence_omitted,
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
    let mut local = local
        .map(Configuration::decode)
        .transpose()?
        .unwrap_or_default();
    let toolpack_hints_added = policy.ai_profile_edit_allowed()
        && local
            .profiles
            .restore_missing_toolpack_hints(&incoming.profiles);
    let added = local.profiles.merge_java_profiles(incoming.profiles)?;
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
