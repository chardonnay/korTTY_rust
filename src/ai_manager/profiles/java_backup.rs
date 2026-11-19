//! Java HTTP, HTTPS, CLI and embedded AI profiles retained as drafts for review.

use std::collections::BTreeSet;
use std::io;

use roxmltree::{Document, Node, ParsingOptions};
use serde::{Deserialize, Serialize};

use super::{
    Catalog, ConnectionMode, JavaProfileSource, MAX_PROFILES, ModelMode, Profile, ProfileId,
    Reasoning, Vision, cli_profile,
};
use crate::ai_manager::{
    CodeTextLanguage, JavaAgentShortcut, JavaAiProfileHint, JavaGeneralPreferences,
    JavaJournalEvidence, JavaJournalPreferences, JavaJournalProfiles,
};
use crate::local_ai::cli::{self, catalog::Provider};
use crate::local_ai::web_search::{McpMode, McpSettings};
use crate::local_ai::{self, PromptPreset, RemoteProtocol, Tokenizer, UsageBudget};
use crate::session_journal::summary_plan::EvidenceLimits;
use crate::ssh_dialog::unsafe_text;

mod minimax;

const MAX_XML_BYTES: usize = 4 * 1024 * 1024;
const MAX_XML_NODES: u32 = 65_536;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub(in crate::ai_manager) enum JavaWebHint {
    Tavily,
    LmStudioTavily,
    BrightData,
    Brave { plugin: String },
    Searxng { plugin: String },
    Toolpack { plugin: String },
}

impl JavaWebHint {
    pub(super) fn mcp_settings(&self) -> Option<McpSettings> {
        let (mode, plugin) = match self {
            Self::Tavily => return None,
            Self::LmStudioTavily => (McpMode::Tavily, None),
            Self::BrightData => (McpMode::BrightData, None),
            Self::Brave { plugin } => (McpMode::Brave, Some(plugin)),
            Self::Searxng { plugin } => (McpMode::Searxng, Some(plugin)),
            Self::Toolpack { plugin } => (McpMode::Toolpack, Some(plugin)),
        };
        let mut settings = McpSettings::new(mode);
        if let Some(plugin) = plugin {
            settings.plugin.clone_from(plugin);
        }
        Some(settings)
    }

    pub(super) fn supports(&self, mode: ConnectionMode) -> bool {
        if let Self::Toolpack { plugin } = self {
            // Java saved the plugin ID without the tool allowlist required by Rust.
            return mode == ConnectionMode::LmStudio && McpSettings::valid_plugin_id(plugin);
        }
        self.mcp_settings().map_or_else(
            || {
                matches!(
                    mode,
                    ConnectionMode::External
                        | ConnectionMode::LmStudioCompatible
                        | ConnectionMode::RemoteOpenAi
                        | ConnectionMode::RemoteOpenAiExact
                        | ConnectionMode::MiniMaxNative
                )
            },
            |settings| mode == ConnectionMode::LmStudio && settings.validate().is_ok(),
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "EmbeddedModelRecord", into = "EmbeddedModelRecord")]
pub(in crate::ai_manager) struct JavaEmbeddedModel {
    id: String,
    mlx_reasoning: Reasoning,
}

#[derive(Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
enum EmbeddedModelRecord {
    Id(String),
    MlxReasoning { id: String, reasoning: Reasoning },
}

impl JavaEmbeddedModel {
    pub(in crate::ai_manager) fn as_str(&self) -> &str {
        &self.id
    }

    pub(in crate::ai_manager) fn has_mlx_reasoning(&self) -> bool {
        self.mlx_reasoning != Reasoning::Default
    }

    pub(super) fn mlx_reasoning(&self) -> Option<Reasoning> {
        self.has_mlx_reasoning().then_some(self.mlx_reasoning)
    }
}

impl TryFrom<String> for JavaEmbeddedModel {
    type Error = &'static str;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        let mut bytes = id.bytes();
        if id.len() <= 128
            && bytes
                .next()
                .is_some_and(|byte| byte.is_ascii_alphanumeric())
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            Ok(Self {
                id,
                mlx_reasoning: Reasoning::Default,
            })
        } else {
            Err("Invalid Java embedded model identifier")
        }
    }
}

impl TryFrom<EmbeddedModelRecord> for JavaEmbeddedModel {
    type Error = &'static str;

    fn try_from(record: EmbeddedModelRecord) -> Result<Self, Self::Error> {
        match record {
            EmbeddedModelRecord::Id(id) => Self::try_from(id),
            EmbeddedModelRecord::MlxReasoning { id, reasoning }
                if reasoning != Reasoning::Default =>
            {
                let mut model = Self::try_from(id)?;
                model.mlx_reasoning = reasoning;
                Ok(model)
            }
            _ => Err("Invalid stored Java MLX reasoning"),
        }
    }
}

impl From<JavaEmbeddedModel> for EmbeddedModelRecord {
    fn from(model: JavaEmbeddedModel) -> Self {
        if model.has_mlx_reasoning() {
            Self::MlxReasoning {
                id: model.id,
                reasoning: model.mlx_reasoning,
            }
        } else {
            Self::Id(model.id)
        }
    }
}

#[derive(Default)]
pub(in crate::ai_manager) struct Report {
    pub(in crate::ai_manager) general_preferences: JavaGeneralPreferences,
    pub(in crate::ai_manager) request_timeout_omitted: bool,
    pub(in crate::ai_manager) code_text_language_omitted: bool,
    pub(in crate::ai_manager) roles: crate::ai_manager::java_roles::Hints,
    pub(in crate::ai_manager) roles_omitted: usize,
    pub(in crate::ai_manager) https_imported: usize,
    pub(in crate::ai_manager) loopback_imported: usize,
    pub(in crate::ai_manager) embedded_imported: usize,
    pub(in crate::ai_manager) mlx_reasoning_omitted: usize,
    pub(in crate::ai_manager) cli_imported: usize,
    pub(in crate::ai_manager) omitted: usize,
    pub(in crate::ai_manager) keys_omitted: usize,
    pub(in crate::ai_manager) pricing_omitted: usize,
    pub(in crate::ai_manager) default_reasoning_omitted: usize,
    pub(in crate::ai_manager) web_hints: usize,
    pub(in crate::ai_manager) web_hints_omitted: usize,
    pub(in crate::ai_manager) web_keys_omitted: usize,
    pub(in crate::ai_manager) journal_profiles: JavaJournalProfiles,
    pub(in crate::ai_manager) journal_evidence_omitted: bool,
    pub(in crate::ai_manager) journal_preferences_omitted: bool,
    pub(in crate::ai_manager) memory_tokens: Option<u16>,
    pub(in crate::ai_manager) memory_tokens_omitted: bool,
    pub(in crate::ai_manager) memory_preferences: crate::ai_manager::memory::JavaPreferences,
    pub(in crate::ai_manager) memory_preferences_omitted: usize,
    pub(in crate::ai_manager) skill_preferences: crate::ai_manager::skills::JavaPreferences,
    pub(in crate::ai_manager) skill_preferences_omitted: usize,
    pub(in crate::ai_manager) agent_shortcut: Option<JavaAgentShortcut>,
    pub(in crate::ai_manager) agent_shortcut_omitted: bool,
    pub(in crate::ai_manager) agent_execution_enabled: Option<bool>,
    pub(in crate::ai_manager) agent_execution_omitted: bool,
    pub(in crate::ai_manager) agent_confirm_mutating: Option<bool>,
    pub(in crate::ai_manager) agent_confirmation_omitted: bool,
    pub(in crate::ai_manager) journal_semantic_search: Option<bool>,
    pub(in crate::ai_manager) journal_semantic_search_omitted: bool,
}

fn document(bytes: &[u8]) -> io::Result<Document<'_>> {
    if bytes.len() > MAX_XML_BYTES {
        return Err(invalid("Java global settings exceed their XML size limit"));
    }
    let xml = std::str::from_utf8(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
        .map_err(|_| invalid("Java global settings XML must use UTF-8"))?;
    let document = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XML_NODES,
            entity_resolver: None,
        },
    )
    .map_err(|_| invalid("Invalid Java global settings XML"))?;
    if !named(document.root_element(), "globalSettings") {
        return Err(invalid("Unexpected Java global settings XML root"));
    }
    Ok(document)
}

pub(in crate::ai_manager) fn skill_sources(
    bytes: &[u8],
    mut visit: impl FnMut(&str, Option<&str>),
) -> io::Result<()> {
    let document = document(bytes)?;
    let root = document.root_element();
    let skills = child(root, "aiSkills").map_err(|()| invalid("Duplicate Java AI Skills list"))?;
    for node in skills
        .into_iter()
        .flat_map(|wrapper| wrapper.children())
        .filter(|node| named(*node, "skill"))
    {
        let Some(id) =
            scalar_text(node, "id").map_err(|()| invalid("Ambiguous Java AI Skill identity"))?
        else {
            continue;
        };
        let builtin = scalar_text(node, "builtinId")
            .map_err(|()| invalid("Ambiguous Java built-in AI Skill identity"))?;
        visit(
            id.trim_matches(|ch| ch <= ' '),
            builtin.map(|value| value.trim_matches(|ch| ch <= ' ')),
        );
    }
    Ok(())
}

pub(in crate::ai_manager) fn decode(bytes: &[u8]) -> io::Result<(Catalog, Report)> {
    let document = document(bytes)?;
    let root = document.root_element();
    let mut catalog = Catalog::default();
    let mut report = Report {
        web_keys_omitted: root
            .children()
            .filter(|node| {
                [
                    "encryptedAiTavilyApiKey",
                    "encryptedAiBrightDataApiToken",
                    "encryptedAiBraveSearchApiKey",
                ]
                .iter()
                .any(|field| named(*node, field))
            })
            .count(),
        ..Report::default()
    };
    let wrapper =
        child(root, "aiProfiles").map_err(|()| invalid("Duplicate Java AI profile list"))?;
    let mut profiles = wrapper
        .into_iter()
        .flat_map(|wrapper| wrapper.children())
        .filter(|node| named(*node, "profile"))
        .peekable();
    let has_profiles = profiles.peek().is_some();
    let first_id = profiles
        .peek()
        .and_then(|node| scalar(*node, "id").ok().flatten())
        .and_then(JavaProfileSource::parse_id);
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    for node in profiles {
        if node.children().any(|node| named(node, "encryptedApiKey")) {
            report.keys_omitted += 1;
        }
        let source_id = match scalar(node, "id") {
            Ok(Some(id)) if !id.is_empty() && !ids.insert(id) => {
                return Err(invalid("Duplicate Java AI profile identity"));
            }
            Ok(id) => id.and_then(JavaProfileSource::parse_id),
            Err(()) => {
                report.omitted += 1;
                continue;
            }
        };
        let Ok((mut profile, pricing_omitted, default_reasoning_omitted)) = read_profile(node)
        else {
            report.omitted += 1;
            continue;
        };
        if (profile.is_loopback() || profile.is_managed()) && source_id.is_none()
            || catalog.profiles.len() == MAX_PROFILES
            || !names.insert(profile.name.to_ascii_lowercase())
        {
            report.omitted += 1;
            continue;
        }
        if profile.is_cli() {
            report.cli_imported += 1;
        } else if profile.is_managed() {
            report.embedded_imported += 1;
            if profile
                .java_embedded_model
                .as_ref()
                .is_some_and(JavaEmbeddedModel::has_mlx_reasoning)
            {
                report.mlx_reasoning_omitted += 1;
            }
        } else if profile.remote_protocol().is_some() {
            report.https_imported += 1;
        } else {
            report.loopback_imported += 1;
        }
        match web_hint(root, node, profile.connection_mode) {
            Ok(Some(hint)) if source_id.is_some() && profile.supports_java_web_hint(&hint) => {
                profile.java_web_hint = Some(hint);
                report.web_hints += 1;
            }
            Ok(None) => {}
            _ => report.web_hints_omitted += 1,
        }
        report.pricing_omitted += usize::from(pricing_omitted);
        report.default_reasoning_omitted += usize::from(default_reasoning_omitted);
        profile.java_source = source_id
            .map(|id| JavaProfileSource::for_draft(id, &profile))
            .transpose()?;
        catalog.append_draft(profile)?;
    }
    (report.roles, report.roles_omitted) =
        crate::ai_manager::java_roles::read(&catalog, |name| scalar(root, name));
    let selection = |element| -> io::Result<Option<&str>> {
        Ok(scalar(root, element)
            .map_err(|()| invalid("Invalid Java Journal AI profile selection"))?
            .and_then(JavaProfileSource::parse_id))
    };
    let regular = selection("sessionJournalAiProfileId")?;
    let automation = selection("automationJournalAiProfileId")?;
    let default_id = scalar(root, "defaultAiProfileId")
        .ok()
        .flatten()
        .and_then(JavaProfileSource::parse_id);
    let selected = |id: &&str| !has_profiles || ids.contains(id);
    let regular = regular
        .filter(selected)
        .or_else(|| default_id.filter(selected))
        .or(first_id);
    let automation = automation.filter(selected).or(regular);
    let hint = |id: &str| JavaAiProfileHint {
        id: id.into(),
        source: catalog
            .java_profile_sources()
            .find(|source| source.id() == id)
            .cloned(),
    };
    match scalar(root, "aiMemoryMaxTokens").and_then(|value| {
        value
            .map(|value| value.parse::<i32>().map_err(|_| ()))
            .transpose()
    }) {
        Ok(tokens) => {
            report.memory_tokens = tokens.map(crate::ai_manager::memory::java_token_budget)
        }
        Err(()) => report.memory_tokens_omitted = true,
    }
    for preference in crate::ai_manager::skills::Preference::ALL {
        match scalar(root, preference.java_field()).and_then(|value| {
            value
                .map(|value| match value {
                    "true" | "1" => Ok(true),
                    "false" | "0" => Ok(false),
                    _ => Err(()),
                })
                .transpose()
        }) {
            Ok(Some(value)) => report.skill_preferences.set(preference, value),
            Ok(None) => {}
            Err(()) => report.skill_preferences_omitted += 1,
        }
    }
    match scalar(root, "aiMemoryEnabled").and_then(|value| {
        value
            .map(|value| match value {
                "true" | "1" => Ok(true),
                "false" | "0" => Ok(false),
                _ => Err(()),
            })
            .transpose()
    }) {
        Ok(enabled) => report.memory_preferences.enabled = enabled,
        Err(()) => report.memory_preferences_omitted += 1,
    }
    match scalar(root, "aiMemoryLearningMode").and_then(|value| {
        value
            .map(|value| match value {
                "PROPOSE" => Ok(crate::ai_manager::memory::Learning::Propose),
                "OFF" => Ok(crate::ai_manager::memory::Learning::Off),
                _ => Err(()),
            })
            .transpose()
    }) {
        Ok(learning) => report.memory_preferences.learning = learning,
        Err(()) => report.memory_preferences_omitted += 1,
    }
    match scalar(root, "sessionJournalSemanticSearchEnabled").and_then(|value| {
        value
            .map(|value| match value {
                "true" | "1" => Ok(true),
                "false" | "0" => Ok(false),
                _ => Err(()),
            })
            .transpose()
    }) {
        Ok(enabled) => report.journal_semantic_search = enabled,
        Err(()) => report.journal_semantic_search_omitted = true,
    }
    match scalar(root, "terminalAgentExecutionEnabled").and_then(|value| {
        value
            .map(|value| match value {
                "true" | "1" => Ok(true),
                "false" | "0" => Ok(false),
                _ => Err(()),
            })
            .transpose()
    }) {
        Ok(enabled) => report.agent_execution_enabled = enabled,
        Err(()) => report.agent_execution_omitted = true,
    }
    match scalar(root, "terminalAgentConfirmMutatingCommandSets").and_then(|value| {
        value
            .map(|value| match value {
                "true" | "1" => Ok(true),
                "false" | "0" => Ok(false),
                _ => Err(()),
            })
            .transpose()
    }) {
        Ok(enabled) => report.agent_confirm_mutating = enabled,
        Err(()) => report.agent_confirmation_omitted = true,
    }
    match agent_shortcut(root) {
        Ok(shortcut) => report.agent_shortcut = shortcut,
        Err(()) => report.agent_shortcut_omitted = true,
    }
    match scalar(root, "aiRequestTimeoutMinutes").and_then(|value| {
        value
            .map(|text| text.parse::<u16>().map_err(|_| ()))
            .transpose()
    }) {
        Ok(None) => {}
        Ok(Some(minutes @ 0..=crate::ai_manager::MAX_TIMEOUT_MINUTES)) => {
            report.general_preferences.timeout_minutes = Some(minutes);
        }
        Ok(Some(_)) | Err(()) => report.request_timeout_omitted = true,
    }
    match scalar(root, "aiCodeTextDefaultLanguage").and_then(|value| {
        value
            .map(|code| {
                CodeTextLanguage::ALL
                    .into_iter()
                    .find(|language| {
                        language
                            .explicit_code()
                            .unwrap_or("auto")
                            .eq_ignore_ascii_case(code)
                    })
                    .ok_or(())
            })
            .transpose()
    }) {
        Ok(language) => report.general_preferences.code_text_language = language,
        Err(()) => report.code_text_language_omitted = true,
    }
    report.journal_profiles = JavaJournalProfiles {
        preferences: match journal_preferences(root) {
            Ok(preferences) => preferences,
            Err(()) => {
                report.journal_preferences_omitted = true;
                None
            }
        },
        recording: regular.map(hint),
        automation: automation.map(hint),
        evidence: match journal_evidence(root) {
            Ok(evidence) => evidence,
            Err(()) => {
                report.journal_evidence_omitted = true;
                None
            }
        },
    };
    Ok((catalog, report))
}

fn web_hint(
    root: Node<'_, '_>,
    profile: Node<'_, '_>,
    mode: ConnectionMode,
) -> Result<Option<JavaWebHint>, ()> {
    let plugin = |field| {
        scalar(root, field)?
            .filter(|value| value.len() <= 68)
            .map(str::to_owned)
            .ok_or(())
    };
    let hint = match scalar(profile, "internetAccessMode")? {
        None | Some("DISABLED") => return Ok(None),
        Some("KORTTY_TAVILY_TOOL") => JavaWebHint::Tavily,
        Some("LM_STUDIO_TAVILY_MCP") => JavaWebHint::LmStudioTavily,
        Some("BRIGHT_DATA_WEB_MCP") => JavaWebHint::BrightData,
        Some("BRAVE_SEARCH_MCP") => JavaWebHint::Brave {
            plugin: plugin("aiBraveSearchMcpPluginId")?,
        },
        Some("SEARXNG_MCP") => JavaWebHint::Searxng {
            plugin: plugin("aiSearxngMcpPluginId")?,
        },
        Some("LM_STUDIO_TOOLPACK") => JavaWebHint::Toolpack {
            plugin: plugin("aiLmStudioToolpackMcpPluginId")?,
        },
        _ => return Err(()),
    };
    hint.supports(mode).then_some(Some(hint)).ok_or(())
}

fn agent_shortcut(root: Node<'_, '_>) -> Result<Option<JavaAgentShortcut>, ()> {
    let name = scalar_text(root, "terminalAgentCommandName")?.map(|name| {
        let name = if name.chars().all(java_whitespace) {
            "agent"
        } else {
            trim_java(name)
        };
        name.to_owned()
    });
    if name.as_ref().is_some_and(|name| !name.is_ascii()) {
        return Err(());
    }
    let case_insensitive = scalar(root, "terminalAgentCommandNameCaseInsensitive")?
        .map(|value| match value {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            _ => Err(()),
        })
        .transpose()?;
    if name.is_none() && case_insensitive.is_none() {
        return Ok(None);
    }
    let hint = JavaAgentShortcut {
        name,
        case_insensitive,
    };
    hint.validate().map_err(|_| ())?;
    Ok(Some(hint))
}

fn journal_preferences(root: Node<'_, '_>) -> Result<Option<JavaJournalPreferences>, ()> {
    let interval_minutes = scalar(root, "sessionJournalSummarizeIntervalMinutes")?
        .map(|value| {
            value
                .parse::<i32>()
                .map(|value| value.clamp(1, 240) as u16)
                .map_err(|_| ())
        })
        .transpose()?;
    let automatic_title = scalar(root, "sessionJournalAiTitleEnabled")?
        .map(|value| match value {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            _ => Err(()),
        })
        .transpose()?;
    Ok(
        (interval_minutes.is_some() || automatic_title.is_some()).then_some(
            JavaJournalPreferences {
                interval_minutes,
                automatic_title,
            },
        ),
    )
}

fn journal_evidence(root: Node<'_, '_>) -> Result<Option<JavaJournalEvidence>, ()> {
    let max_lines = scalar(root, "sessionJournalAiMaxLines")?;
    let token_budget = scalar(root, "sessionJournalAiTokenBudget")?;
    let chunking = scalar(root, "sessionJournalAiChunkingEnabled")?;
    if max_lines.is_none() && token_budget.is_none() && chunking.is_none() {
        return Ok(None);
    }
    let parse = |value: Option<&str>, minimum: i32, fallback| -> Result<u32, ()> {
        match value {
            Some(value) => {
                let value = value.parse::<i32>().map_err(|_| ())?;
                u32::try_from(value.max(minimum)).map_err(|_| ())
            }
            None => Ok(fallback),
        }
    };
    let defaults = EvidenceLimits::default();
    let limits = EvidenceLimits {
        max_lines_per_direction: parse(max_lines, 0, defaults.max_lines_per_direction)?,
        context_tokens: parse(token_budget, 1_000, defaults.context_tokens)?,
    };
    let chunking = match chunking {
        None | Some("false" | "0") => false,
        Some("true" | "1") => true,
        _ => return Err(()),
    };
    limits
        .valid()
        .then_some(Some(JavaJournalEvidence { limits, chunking }))
        .ok_or(())
}

impl Catalog {
    fn append_draft(&mut self, mut profile: Profile) -> io::Result<()> {
        let id = self
            .last_id
            .checked_add(1)
            .filter(|id| !self.is_policy_reference(*id))
            .ok_or_else(|| invalid("AI profile identifiers are exhausted"))?;
        profile.id = ProfileId::from(id);
        self.profiles.push(profile);
        self.last_id = id;
        Ok(())
    }

    pub(in crate::ai_manager) fn merge_java_profiles(
        &mut self,
        incoming: Self,
    ) -> io::Result<usize> {
        let mut added = 0;
        for profile in incoming.profiles {
            if self.profiles.len() == MAX_PROFILES
                || self.profiles.iter().any(|local| {
                    local.name.eq_ignore_ascii_case(&profile.name)
                        || profile.java_source.is_some() && local.java_source == profile.java_source
                })
            {
                continue;
            }
            self.append_draft(profile)?;
            added += 1;
        }
        Ok(added)
    }

    pub(in crate::ai_manager) fn restore_missing_toolpack_hints(
        &mut self,
        incoming: &Self,
    ) -> bool {
        let mut changed = false;
        for profile in &incoming.profiles {
            let (Some(source), Some(hint @ JavaWebHint::Toolpack { .. })) =
                (&profile.java_source, &profile.java_web_hint)
            else {
                continue;
            };
            // Fill older pending imports only when their definition still matches the source.
            if let Some(local) = self.profiles.iter_mut().find(|local| {
                local.restore_review
                    && local.java_web_hint.is_none()
                    && local.java_source.as_ref() == Some(source)
                    && JavaProfileSource::for_draft(source.id(), local)
                        .as_ref()
                        .ok()
                        == Some(source)
            }) {
                local.java_web_hint = Some(hint.clone());
                changed = true;
            }
        }
        changed
    }

    pub(in crate::ai_manager) fn changes_existing_java_profiles(&self, incoming: &Self) -> bool {
        self.profiles
            .iter()
            .any(|profile| incoming.profile(profile.id) != Some(profile))
    }

    pub(in crate::ai_manager) fn validate_java_profiles(&self) -> io::Result<()> {
        let mut source_ids = BTreeSet::new();
        if self.default.is_some()
            || self.policy_default.is_some()
            || self.profiles.iter().any(|profile| {
                !profile.is_java_draft()
                    || ((profile.is_loopback() || profile.is_managed())
                        && profile.java_source.is_none())
                    || profile.java_source.as_ref().is_some_and(|source| {
                        !source_ids.insert(source.id())
                            || JavaProfileSource::for_draft(source.id(), profile)
                                .as_ref()
                                .ok()
                                != Some(source)
                    })
            })
        {
            return Err(invalid("Invalid Java AI profile delta"));
        }
        Ok(())
    }

    pub(in crate::ai_manager) fn java_profile_sources(
        &self,
    ) -> impl Iterator<Item = &JavaProfileSource> {
        self.profiles
            .iter()
            .filter_map(|profile| profile.java_source.as_ref())
    }
}

impl Profile {
    fn is_java_draft(&self) -> bool {
        let explicit_or_delegated = local_ai::valid_model(&self.model)
            || self.uses_server_default_model() && self.model.is_empty();
        let default_budget = UsageBudget::default();
        let supported_route = if self.is_cli() {
            self.cli
                .as_ref()
                .is_some_and(cli_profile::Settings::is_java_draft)
        } else if self.connection_mode == ConnectionMode::LoopbackAnthropic {
            self.model_mode == ModelMode::Manual
                && explicit_or_delegated
                && self
                    .loopback_anthropic_endpoint()
                    .is_ok_and(|endpoint| endpoint.destination() == self.endpoint)
        } else if self.connection_mode.uses_lm_studio_inventory() {
            (self.ordinary_lm_studio_auto() && self.model.is_empty() || explicit_or_delegated)
                && self.lm_studio_endpoint().is_ok_and(|endpoint| {
                    persisted_lm_studio_endpoint(&endpoint) == self.endpoint
                        && (self.model_mode != ModelMode::Auto || endpoint.supports_metadata())
                })
        } else if self.is_loopback() {
            explicit_or_delegated
                && self
                    .local_address()
                    .is_ok_and(|address| address.to_string() == self.endpoint)
        } else if self.is_managed() {
            self.java_embedded_model
                .as_ref()
                .is_some_and(|model| !model.has_mlx_reasoning() || self.is_mlx())
                && self.model.is_empty()
        } else if self.connection_mode.is_exact_https() {
            explicit_or_delegated
                && self
                    .destination()
                    .is_ok_and(|destination| destination == self.endpoint)
        } else {
            explicit_or_delegated
                && remote_destination(&self.endpoint).is_ok_and(|(mode, destination)| {
                    mode == self.connection_mode && destination == self.endpoint
                })
        };
        self.restore_review
            && (self.model_mode == ModelMode::Manual
                || self.ordinary_lm_studio_auto()
                || self.uses_server_default_model())
            && !unsafe_text(&self.name)
            && !unsafe_text(&self.model)
            && supported_route
            && self.usage_budget.token_limit.is_none()
            && self.usage_budget.reset_days == default_budget.reset_days
            && self.usage_budget.yellow_percent == default_budget.yellow_percent
            && self.usage_budget.red_percent == default_budget.red_percent
            && self.validate().is_ok()
    }
}

fn read_profile(node: Node<'_, '_>) -> Result<(Profile, bool, bool), ()> {
    let name = scalar(node, "name")?
        .filter(|name| !unsafe_text(name))
        .ok_or(())?;
    let mut profile = Profile {
        name: name.into(),
        restore_review: true,
        prompt_preset: Some(match scalar(node, "promptPreset")? {
            None | Some("AUTO") => PromptPreset::Auto,
            Some("GENERIC") => PromptPreset::Generic,
            Some("LLAMA") => PromptPreset::Llama,
            Some("QWEN") => PromptPreset::Qwen,
            Some("MISTRAL") => PromptPreset::Mistral,
            Some("GEMMA") => PromptPreset::Gemma,
            Some("DEEPSEEK") => PromptPreset::DeepSeek,
            Some("PHI") => PromptPreset::Phi,
            Some("GPT_OSS") => PromptPreset::GptOss,
            _ => return Err(()),
        }),
        tokenizer: match scalar(node, "tokenizerType")? {
            None | Some("ESTIMATE") => Tokenizer::Estimate,
            Some("CL100K_BASE") => Tokenizer::Cl100kBase,
            Some("O200K_BASE") => Tokenizer::O200kBase,
            Some("P50K_BASE") => Tokenizer::P50kBase,
            Some("R50K_BASE") => Tokenizer::R50kBase,
            _ => return Err(()),
        },
        selection_limit: number(node, "maxSelectionChars")?
            .unwrap_or(Profile::default().selection_limit),
        timeout_minutes: number(node, "requestTimeoutMinutes")?,
        ..Profile::default()
    };
    match scalar(node, "connectionMode")? {
        Some("LOCAL_CLI") => read_cli_profile(node, &mut profile)?,
        Some("EMBEDDED_LLAMA_CPP") => {
            read_embedded_profile(node, &mut profile, ConnectionMode::Managed)?
        }
        Some("EMBEDDED_MLX") => {
            read_embedded_profile(node, &mut profile, ConnectionMode::ManagedMlx)?
        }
        None | Some("HTTP_API") => read_http_profile(node, &mut profile)?,
        _ => return Err(()),
    }
    let (budget, pricing_omitted) = read_pricing(node);
    profile.usage_budget = budget;
    let default_reasoning_omitted =
        profile.uses_server_default_model() && profile.reasoning != Reasoning::Default;
    if default_reasoning_omitted {
        profile.reasoning = Reasoning::Default;
    }
    profile
        .is_java_draft()
        .then_some((profile, pricing_omitted, default_reasoning_omitted))
        .ok_or(())
}

fn read_pricing(node: Node<'_, '_>) -> (UsageBudget, bool) {
    let mut budget = UsageBudget::default();
    match scalar(node, "priceCurrency") {
        Ok(None | Some("")) => {}
        Ok(Some(code))
            if code.len() == 3 && code.bytes().all(|byte| byte.is_ascii_alphabetic()) =>
        {
            budget.currency = code.to_ascii_uppercase();
        }
        _ => return (budget, true),
    }
    let price = |field| {
        scalar(node, field).and_then(|value| match value {
            None | Some("") => Ok(None),
            Some(value) => value
                .parse::<f64>()
                .ok()
                .and_then(|units| {
                    let significand = value.split(['e', 'E']).next().unwrap_or_default();
                    if units == 0.0 && significand.bytes().any(|byte| matches!(byte, b'1'..=b'9')) {
                        return None;
                    }
                    UsageBudget::price_micros_per_million(units)
                })
                .map(Some)
                .ok_or(()),
        })
    };
    let input = price("pricePerMillionPromptTokens");
    let output = price("pricePerMillionCompletionTokens");
    budget.input_micros_per_million = input.unwrap_or_default();
    budget.output_micros_per_million = output.unwrap_or_default();
    (budget, input.is_err() || output.is_err())
}

fn read_embedded_profile(
    node: Node<'_, '_>,
    profile: &mut Profile,
    mode: ConnectionMode,
) -> Result<(), ()> {
    let model = scalar(node, "embeddedModelId")?.ok_or(())?;
    let mut model = JavaEmbeddedModel::try_from(model.to_owned()).map_err(|_| ())?;
    profile.connection_mode = mode;
    profile.endpoint.clear();
    profile.model.clear();
    profile.vision = match scalar(node, "visionSupport")? {
        None | Some("AUTO" | "DISABLED" | "ENABLED") => Vision::Disabled,
        _ => return Err(()),
    };
    let reasoning = read_reasoning(node)?;
    if mode == ConnectionMode::Managed {
        profile.reasoning = reasoning;
    } else {
        model.mlx_reasoning = reasoning;
    }
    profile.java_embedded_model = Some(model);
    profile.max_output_tokens = number(node, "maxOutputTokens")?;
    Ok(())
}

fn read_http_profile(node: Node<'_, '_>, profile: &mut Profile) -> Result<(), ()> {
    let endpoint = scalar(node, "apiUrl")?.ok_or(())?;
    let internet = scalar(node, "internetAccessMode")?;
    let model = scalar_text(node, "model")?.unwrap_or_default();
    let model_is_blank = model.chars().all(java_whitespace);
    let inferred_mode = if model_is_blank { "AUTO" } else { "MANUAL" };
    let selection = scalar(node, "modelSelectionMode")?.unwrap_or(inferred_mode);
    let messages = if java_selects_messages(endpoint) {
        Some(java_anthropic_destination(endpoint, selection).ok_or(())?)
    } else {
        None
    };
    match (selection, messages) {
        ("MANUAL" | "AUTO" | "DEFAULT", Some((mode, destination))) => {
            // Java dispatches Messages before selecting the model and requires an explicit model.
            profile.connection_mode = mode;
            profile.endpoint = destination;
            profile.model = model.trim().into();
        }
        ("MANUAL", None) => {
            (profile.connection_mode, profile.endpoint) =
                manual_http_destination(endpoint, internet)?;
            profile.model = model.trim().into();
        }
        ("AUTO", None) => {
            let (mode, endpoint) = java_lm_studio_destination(endpoint, internet)?;
            if !endpoint.supports_metadata() {
                return Err(());
            }
            profile.connection_mode = mode;
            profile.endpoint = persisted_lm_studio_endpoint(&endpoint);
            profile.model_mode = ModelMode::Auto;
            profile.model = if model_is_blank {
                String::new()
            } else {
                trim_java(model).into()
            };
        }
        ("DEFAULT", None) => {
            (profile.connection_mode, profile.endpoint) =
                if let Ok((mode, endpoint)) = java_lm_studio_destination(endpoint, internet) {
                    (mode, persisted_lm_studio_endpoint(&endpoint))
                } else {
                    let (mode, endpoint) = manual_http_destination(endpoint, internet)?;
                    if !matches!(
                        mode,
                        ConnectionMode::RemoteOpenAi | ConnectionMode::RemoteOpenAiExact
                    ) {
                        return Err(());
                    }
                    (mode, endpoint)
                };
            profile.model_mode = ModelMode::ServerDefault;
            profile.model.clear();
        }
        _ => return Err(()),
    }
    profile.vision = match scalar(node, "visionSupport")? {
        None | Some("AUTO") => Vision::Auto,
        Some("ENABLED") => Vision::Enabled,
        Some("DISABLED") => Vision::Disabled,
        _ => return Err(()),
    };
    profile.reasoning = read_reasoning(node)?;
    profile.max_output_tokens = number(node, "maxOutputTokens")?;
    Ok(())
}

fn java_selects_messages(endpoint: &str) -> bool {
    let Some((_, tail)) = endpoint.split_once("://") else {
        return false;
    };
    let route = tail.split(['?', '#']).next().unwrap_or_default();
    let Some(path) = route.find('/').map(|slash| &route[slash..]) else {
        return false;
    };
    let parsed = url::Url::parse(endpoint).ok();
    let path = percent_encoding::percent_decode_str(path).decode_utf8_lossy();
    local_ai::is_messages_path(parsed.as_ref().and_then(url::Url::host_str), &path)
}

fn java_anthropic_destination(endpoint: &str, selection: &str) -> Option<(ConnectionMode, String)> {
    let loopback = || {
        local_ai::LmStudioEndpoint::parse_anthropic(endpoint)
            .ok()
            .map(|endpoint| (ConnectionMode::LoopbackAnthropic, endpoint.destination()))
    };
    let exact = (!unsafe_text(endpoint) && endpoint.is_ascii())
        .then(|| local_ai::remote_destination(endpoint, RemoteProtocol::AnthropicExact).ok())
        .flatten();
    let canonical = || {
        let exact = exact.as_ref()?;
        let route @ (ConnectionMode::Anthropic, _) = remote_destination(endpoint).ok()? else {
            return None;
        };
        (route.1 == *exact).then_some(route)
    };
    // Keep the existing connection mode and fingerprint for earlier imports.
    let legacy = if selection == "MANUAL" {
        canonical().or_else(loopback)
    } else {
        loopback().or_else(canonical)
    };
    legacy.or_else(|| exact.map(|endpoint| (ConnectionMode::AnthropicExact, endpoint)))
}

fn read_cli_profile(node: Node<'_, '_>, profile: &mut Profile) -> Result<(), ()> {
    let provider_id = scalar_text(node, "cliProviderId")?
        .map(trim_java)
        .filter(|id| !id.is_empty())
        .ok_or(())?;
    let provider = serde_json::from_value::<Provider>(serde_json::Value::String(
        provider_id.to_ascii_lowercase(),
    ))
    .ok();
    let path = scalar_text(node, "cliExecutablePath")?
        .map(trim_java)
        .unwrap_or_default();
    let arguments = cli_arguments(scalar_text(node, "cliArgumentsTemplate")?.ok_or(())?)?;
    if let Some(settings) = minimax::draft(provider, &arguments) {
        profile.connection_mode = ConnectionMode::Cli;
        profile.endpoint.clear();
        profile.vision = Vision::Disabled;
        profile.model.clear();
        profile.cli = Some(settings);
        return Ok(());
    }
    let requirements = cli::template_requirements(&arguments).ok_or(())?;
    if requirements.reasoning {
        profile.reasoning = read_reasoning(node)?;
    }
    profile.model = match scalar(node, "modelSelectionMode")? {
        None | Some("MANUAL" | "AUTO") => scalar_text(node, "model")?
            .map(trim_java)
            .unwrap_or_default()
            .into(),
        Some("DEFAULT") => String::new(),
        _ => return Err(()),
    };
    profile.connection_mode = ConnectionMode::Cli;
    profile.endpoint.clear();
    profile.vision = Vision::Disabled;
    profile.cli = Some(cli_profile::Settings::java_draft(path, arguments, provider));
    Ok(())
}

fn read_reasoning(node: Node<'_, '_>) -> Result<Reasoning, ()> {
    match scalar(node, "reasoningEffort")? {
        None | Some("DISABLED") => Ok(Reasoning::Default),
        Some("NONE") => Ok(Reasoning::None),
        Some("MINIMAL") => Ok(Reasoning::Minimal),
        Some("LOW") => Ok(Reasoning::Low),
        Some("MEDIUM") => Ok(Reasoning::Medium),
        Some("HIGH") => Ok(Reasoning::High),
        Some("XHIGH") => Ok(Reasoning::ExtraHigh),
        _ => Err(()),
    }
}

fn cli_arguments(template: &str) -> Result<Vec<String>, ()> {
    let template = trim_java(template);
    if template.len() > 4 * cli::MAX_ARG_BYTES {
        return Err(());
    }
    Ok(if template.contains(['\r', '\n']) {
        template
            .split(['\r', '\n'])
            .map(trim_java)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        inline_arguments(template)?
    })
}

fn trim_java(value: &str) -> &str {
    value.trim_matches(|character| character <= '\u{20}')
}

fn java_whitespace(character: char) -> bool {
    matches!(character, '\t'..='\r' | '\u{1c}'..=' ' | '\u{1680}' | '\u{2000}'..='\u{2006}' |
        '\u{2008}'..='\u{200a}' | '\u{2028}'..='\u{2029}' | '\u{205f}' | '\u{3000}')
}

fn inline_arguments(template: &str) -> Result<Vec<String>, ()> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut chars = template.chars();
    while let Some(character) = chars.next() {
        match character {
            '\\' if quote != Some('\'') => current.push(chars.next().unwrap_or('\\')),
            '\'' | '"' if quote == Some(character) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(character),
            character if quote.is_none() && java_whitespace(character) => {
                if !current.is_empty() {
                    arguments.push(std::mem::take(&mut current));
                }
            }
            character => current.push(character),
        }
    }
    if quote.is_some() {
        return Err(());
    }
    if !current.is_empty() {
        arguments.push(current);
    }
    Ok(arguments)
}

fn remote_destination(endpoint: &str) -> Result<(ConnectionMode, String), ()> {
    if unsafe_text(endpoint) || !endpoint.is_ascii() {
        return Err(());
    }
    let raw_path = endpoint
        .strip_prefix("https://")
        .and_then(|rest| rest.split_once('/'))
        .map(|(_, path)| path)
        .ok_or(())?;
    // URL parsing could silently remove dot segments. Java's saved route must stay exact.
    if raw_path.contains('%') || raw_path.split('/').any(|part| matches!(part, "." | "..")) {
        return Err(());
    }
    let url = url::Url::parse(endpoint).map_err(|_| ())?;
    let path = url.path();
    let (mode, protocol, java_path) = if path.ends_with("/v1/messages") {
        (
            ConnectionMode::Anthropic,
            RemoteProtocol::Anthropic,
            path.to_owned(),
        )
    } else if path.trim_end_matches('/') == local_ai::MINIMAX_NATIVE_PATH {
        (
            ConnectionMode::MiniMaxNative,
            RemoteProtocol::MiniMaxNative,
            local_ai::MINIMAX_NATIVE_PATH.to_owned(),
        )
    } else if path.ends_with("/chat/completions") {
        (
            ConnectionMode::RemoteOpenAi,
            RemoteProtocol::OpenAi,
            path.trim_end_matches('/').to_owned(),
        )
    } else if path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .is_some_and(|segment| segment.eq_ignore_ascii_case("v1"))
    {
        (
            ConnectionMode::RemoteOpenAi,
            RemoteProtocol::OpenAi,
            format!("{}/chat/completions", path.trim_end_matches('/')),
        )
    } else {
        return Err(());
    };
    let native = local_ai::remote_destination(endpoint, protocol).map_err(|_| ())?;
    if native != format!("{}{java_path}", url.origin().ascii_serialization()) {
        return Err(());
    }
    Ok((mode, native))
}

fn manual_http_destination(
    endpoint: &str,
    internet: Option<&str>,
) -> Result<(ConnectionMode, String), ()> {
    let local = java_lm_studio_destination(endpoint, internet);
    let case_variant_base = endpoint
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .is_some_and(|segment| segment != "v1" && segment.eq_ignore_ascii_case("v1"));
    // Leave previously supported HTTPS routes in their original connection mode.
    if let Ok(previous) = remote_destination(endpoint)
        && (!case_variant_base || local.is_err())
    {
        if previous.0 == ConnectionMode::Anthropic || java_uses_lm_studio_mcp(internet) {
            return Err(());
        }
        return Ok(previous);
    }
    let Ok((mut mode, target)) = local else {
        if java_uses_lm_studio_mcp(internet) {
            return Err(());
        }
        return exact_remote_destination(endpoint);
    };
    if mode == ConnectionMode::LmStudioCompatible && target.canonical_http_address().is_some() {
        // Existing Manual imports used the generic local OpenAI-compatible mode.
        mode = ConnectionMode::External;
    }
    Ok((mode, persisted_lm_studio_endpoint(&target)))
}

fn exact_remote_destination(endpoint: &str) -> Result<(ConnectionMode, String), ()> {
    if unsafe_text(endpoint) || !endpoint.is_ascii() {
        return Err(());
    }
    let endpoint =
        local_ai::remote_destination(endpoint, RemoteProtocol::OpenAiExact).map_err(|_| ())?;
    Ok((ConnectionMode::RemoteOpenAiExact, endpoint))
}

fn java_lm_studio_destination(
    endpoint: &str,
    internet: Option<&str>,
) -> Result<(ConnectionMode, local_ai::LmStudioEndpoint), ()> {
    let (scheme, authority_path) = endpoint.split_once("://").ok_or(())?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(());
    }
    let path = authority_path.split_once('/').map_or("", |(_, path)| path);
    let path = path.strip_suffix('/').unwrap_or(path);
    let native_path = path.eq_ignore_ascii_case("api/v1/chat")
        || path
            .get(path.len().saturating_sub("/api/v1/chat".len())..)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case("/api/v1/chat"));
    let mode = match internet {
        None | Some("DISABLED" | "KORTTY_TAVILY_TOOL") => ConnectionMode::LmStudioCompatible,
        internet if native_path && java_uses_lm_studio_mcp(internet) => ConnectionMode::LmStudio,
        _ => return Err(()),
    };
    let target = local_ai::LmStudioEndpoint::parse(endpoint, mode.uses_native_lm_studio())
        .map_err(|_| ())?;
    Ok((mode, target))
}

fn java_uses_lm_studio_mcp(internet: Option<&str>) -> bool {
    matches!(
        internet,
        Some(
            "LM_STUDIO_TAVILY_MCP"
                | "BRIGHT_DATA_WEB_MCP"
                | "BRAVE_SEARCH_MCP"
                | "SEARXNG_MCP"
                | "LM_STUDIO_TOOLPACK"
        )
    )
}

fn persisted_lm_studio_endpoint(endpoint: &local_ai::LmStudioEndpoint) -> String {
    // Preserve the source fingerprint for earlier numeric HTTP imports.
    endpoint
        .canonical_http_address()
        .map_or_else(|| endpoint.destination(), |address| address.to_string())
}

fn named(node: Node<'_, '_>, field: &str) -> bool {
    node.is_element() && node.tag_name().namespace().is_none() && node.has_tag_name(field)
}

fn child<'a, 'input>(node: Node<'a, 'input>, field: &str) -> Result<Option<Node<'a, 'input>>, ()> {
    let mut found = node.children().filter(|child| named(*child, field));
    let first = found.next();
    if found.next().is_some() {
        return Err(());
    }
    Ok(first)
}

fn scalar<'a>(node: Node<'a, '_>, field: &str) -> Result<Option<&'a str>, ()> {
    scalar_text(node, field).map(|value| value.map(str::trim))
}

fn scalar_text<'a>(node: Node<'a, '_>, field: &str) -> Result<Option<&'a str>, ()> {
    let Some(node) = child(node, field)? else {
        return Ok(None);
    };
    let mut text = node
        .children()
        .filter(|child| child.is_element() || child.is_text());
    let value = text.next().map_or(Ok(""), |child| {
        child
            .is_text()
            .then(|| child.text().unwrap_or_default())
            .ok_or(())
    })?;
    if text.next().is_some() {
        return Err(());
    }
    Ok(Some(value))
}

fn number(node: Node<'_, '_>, field: &str) -> Result<Option<u32>, ()> {
    scalar(node, field)?
        .map(|value| value.parse().map_err(|_| ()))
        .transpose()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("fixtures/java-global-settings.xml");

    const EMBEDDED: &[u8] = br#"<globalSettings>
        <sessionJournalAiProfileId>java-gguf</sessionJournalAiProfileId>
        <automationJournalAiProfileId>java-mlx</automationJournalAiProfileId>
        <aiProfiles>
            <profile><id>java-gguf</id><name>Java GGUF</name>
                <connectionMode>EMBEDDED_LLAMA_CPP</connectionMode>
                <embeddedModelId>llama-family.v3_q4</embeddedModelId>
                <model>unrelated-provider-value</model>
                <apiUrl>https://example.invalid/unused</apiUrl>
                <trustedLocalEndpoint>true</trustedLocalEndpoint>
                <encryptedApiKey>fixture-embedded-key</encryptedApiKey>
                <internetAccessMode>KORTTY_TAVILY_TOOL</internetAccessMode>
                <reasoningEffort>HIGH</reasoningEffort>
                <tokenizerType>O200K_BASE</tokenizerType>
                <promptPreset>LLAMA</promptPreset>
                <requestTimeoutMinutes>3</requestTimeoutMinutes>
                <pricePerMillionCompletionTokens>0.000001</pricePerMillionCompletionTokens>
            </profile>
            <profile><id>java-mlx</id><name>Java MLX</name>
                <connectionMode>EMBEDDED_MLX</connectionMode>
                <embeddedModelId>mlx-family-4bit</embeddedModelId>
                <visionSupport>ENABLED</visionSupport>
                <reasoningEffort>XHIGH</reasoningEffort>
            </profile>
            <profile><id>java-path</id><name>Invalid Model ID</name>
                <connectionMode>EMBEDDED_LLAMA_CPP</connectionMode>
                <embeddedModelId>../unverified.gguf</embeddedModelId>
            </profile>
            <profile><name>Missing provenance</name><connectionMode>EMBEDDED_MLX</connectionMode>
                <embeddedModelId>valid-id</embeddedModelId>
            </profile>
        </aiProfiles>
    </globalSettings>"#;

    #[test]
    fn embedded_java_drafts_retain_source_model_and_stage_no_local_runtime_binding() {
        let (catalog, report) = decode(EMBEDDED).unwrap();
        assert_eq!(
            (
                report.embedded_imported,
                report.keys_omitted,
                report.omitted
            ),
            (2, 1, 2)
        );
        let [gguf, mlx] = catalog.profiles.as_slice() else {
            panic!("expected GGUF and MLX drafts")
        };
        for (profile, mode, java_id, hint) in [
            (
                gguf,
                ConnectionMode::Managed,
                "llama-family.v3_q4",
                report.journal_profiles.recording,
            ),
            (
                mlx,
                ConnectionMode::ManagedMlx,
                "mlx-family-4bit",
                report.journal_profiles.automation,
            ),
        ] {
            assert_eq!(profile.connection_mode, mode);
            assert_eq!(
                profile.java_embedded_model.as_ref().unwrap().as_str(),
                java_id
            );
            assert_eq!(
                hint.as_ref().and_then(JavaAiProfileHint::source),
                profile.java_source.as_ref()
            );
            assert!(profile.model.is_empty() && profile.endpoint.is_empty());
            assert!(profile.restore_review && !profile.can_be_default());
            assert!(!profile.allows_local_roles() && profile.transport(1).is_err());
            assert!(profile.credential_env.is_empty() && !profile.has_web_search());
        }
        assert_eq!(gguf.reasoning, Reasoning::High);
        assert_eq!(gguf.prompt_preset, Some(PromptPreset::Llama));
        assert_eq!(gguf.tokenizer, Tokenizer::O200kBase);
        assert_eq!(gguf.timeout_minutes, Some(3));
        assert_eq!(gguf.usage_budget.output_micros_per_million, Some(1));
        assert_eq!(gguf.usage_budget.currency, "EUR");
        assert_eq!(mlx.vision, Vision::Disabled);
        assert_eq!(mlx.reasoning, Reasoning::Default);
        assert_eq!(report.mlx_reasoning_omitted, 1);
        assert_eq!(
            serde_json::to_value(mlx.java_embedded_model.as_ref().unwrap()).unwrap(),
            serde_json::json!({"id":"mlx-family-4bit","reasoning":"extra-high"})
        );
        assert!(catalog.default_profile().is_none());
        catalog.validate_java_profiles().unwrap();
        let portable = serde_json::to_string(&catalog).unwrap();
        assert!(!portable.contains("fixture-embedded-key"));
        assert!(!portable.contains("https://example.invalid"));
    }

    #[test]
    fn embedded_java_source_detects_kind_and_model_forgery_and_rejects_native_bindings() {
        let (catalog, _) = decode(EMBEDDED).unwrap();
        let original = catalog.profiles[0].java_source.as_ref().unwrap();
        let xml = std::str::from_utf8(EMBEDDED).unwrap();
        let changed = xml.replace("llama-family.v3_q4", "different-gguf");
        let (different, _) = decode(changed.as_bytes()).unwrap();
        assert_ne!(different.profiles[0].java_source.as_ref(), Some(original));
        let mlx = catalog.profiles[1].java_source.as_ref().unwrap();
        let changed = xml.replace("XHIGH", "HIGH");
        let (different_reasoning, _) = decode(changed.as_bytes()).unwrap();
        assert_ne!(
            different_reasoning.profiles[1].java_source.as_ref(),
            Some(mlx)
        );

        let mut forged = catalog.clone();
        forged.profiles[0].java_embedded_model = different.profiles[0].java_embedded_model.clone();
        assert!(forged.validate_java_profiles().is_err());
        let mut forged = catalog.clone();
        forged.profiles[1].connection_mode = ConnectionMode::Managed;
        assert!(forged.validate_java_profiles().is_err());
        let mut forged = catalog.clone();
        forged.profiles[0].model = "1".repeat(32);
        assert!(forged.validate_java_profiles().is_err());
        let mut forged = catalog;
        forged.profiles[0].restore_review = false;
        assert!(forged.validate_java_profiles().is_err());

        for unsafe_id in ["", ".", "../model", "a/model", "éclair", "a:b", " b"] {
            assert!(serde_json::from_value::<JavaEmbeddedModel>(unsafe_id.into()).is_err());
        }
        assert!(JavaEmbeddedModel::try_from("a".repeat(128)).is_ok());
        assert!(JavaEmbeddedModel::try_from("a".repeat(129)).is_err());
    }

    #[test]
    fn imports_exact_https_and_cli_drafts_without_approvals_or_secrets() {
        let (catalog, report) = decode(FIXTURE).unwrap();
        assert_eq!(
            (
                report.https_imported,
                report.cli_imported,
                report.omitted,
                report.keys_omitted
            ),
            (2, 1, 2, 1)
        );
        let [openai, anthropic, cli] = catalog.profiles.as_slice() else {
            panic!("expected two HTTPS drafts and one CLI draft")
        };
        assert_eq!(openai.name, "Café HTTPS");
        assert_eq!(
            openai.endpoint,
            "https://example.test/openai/v1/chat/completions"
        );
        assert_eq!(openai.model, "family/model");
        assert_eq!(openai.prompt_preset, Some(PromptPreset::GptOss));
        assert_eq!(openai.reasoning, Reasoning::ExtraHigh);
        assert_eq!(openai.vision, Vision::Enabled);
        assert_eq!(openai.timeout_minutes, Some(0));
        assert_eq!(openai.max_output_tokens, Some(4096));
        assert_eq!(openai.selection_limit, 12000);
        assert_eq!(openai.tokenizer, Tokenizer::O200kBase);
        assert_eq!(
            openai.usage_budget.input_micros_per_million,
            Some(1_234_567)
        );
        assert_eq!(openai.usage_budget.output_micros_per_million, Some(25_000));
        assert_eq!(openai.usage_budget.currency, "CHF");
        assert_eq!(openai.java_web_hint, Some(JavaWebHint::Tavily));
        assert_eq!(report.web_hints, 1);
        assert_eq!(anthropic.connection_mode, ConnectionMode::Anthropic);
        assert_eq!(anthropic.usage_budget.input_micros_per_million, None);
        assert_eq!(
            anthropic.usage_budget.output_micros_per_million,
            Some(4_250_000)
        );
        assert_eq!(anthropic.usage_budget.currency, "EUR");
        assert_eq!(report.pricing_omitted, 1);
        assert_eq!(cli.connection_mode, ConnectionMode::Cli);
        assert_eq!(cli.usage_budget.input_micros_per_million, Some(0));
        assert_eq!(cli.usage_budget.output_micros_per_million, None);
        assert_eq!(cli.model, "cli-fixture-model");
        assert_eq!(cli.timeout_minutes, Some(3));
        assert_eq!(
            serde_json::to_value(cli.cli.as_ref().unwrap()).unwrap(),
            serde_json::json!({
                "path": "/fixture/cli-not-installed",
                "arguments": ["--label", "two words", "--model", "{model}", "{stdinPrompt}"],
                "provider": "gemini-cli",
                "fingerprint": null
            })
        );
        for profile in &catalog.profiles {
            assert!(profile.restore_review);
            assert!(profile.validate_connection().is_err());
            assert!(!profile.can_be_default());
            assert!(!profile.has_web_search());
        }
        assert!(catalog.default_profile().is_none());
        let journal = report.journal_profiles;
        assert_eq!(
            journal
                .recording
                .as_ref()
                .and_then(JavaAiProfileHint::source),
            openai.java_source.as_ref()
        );
        assert_eq!(
            journal
                .automation
                .as_ref()
                .and_then(JavaAiProfileHint::source),
            cli.java_source.as_ref()
        );
        catalog.validate_java_profiles().unwrap();
        let json = serde_json::to_string(&catalog).unwrap();
        assert!(!json.contains("synthetic-source-api-key"));
        assert!(!json.contains("fixture-store"));
        assert!(!json.contains("untrusted-command"));
    }

    #[test]
    fn java_profile_pricing_omits_only_unrepresentable_rates_or_currency() {
        let load = |pricing: &str| {
            let xml = format!(
                "<globalSettings><aiProfiles><profile><id>java-prices</id><name>Prices</name>\
                 <apiUrl>https://example.test/v1</apiUrl><model>fixture</model>\
                 {pricing}</profile></aiProfiles></globalSettings>"
            );
            let (catalog, report) = decode(xml.as_bytes()).unwrap();
            assert_eq!(report.omitted, 0);
            catalog.validate_java_profiles().unwrap();
            (catalog, report.pricing_omitted)
        };
        for invalid in [
            "-1",
            "INF",
            "0.0000001",
            "1000000.000001",
            "1,5",
            "1e-999",
            "-1e-999",
        ] {
            let fields = format!(
                "<pricePerMillionPromptTokens>{invalid}</pricePerMillionPromptTokens>\
                 <pricePerMillionCompletionTokens>1000000</pricePerMillionCompletionTokens>"
            );
            let (catalog, omitted) = load(fields.as_str());
            let budget = &catalog.profiles[0].usage_budget;
            assert_eq!(omitted, 1);
            assert_eq!(budget.input_micros_per_million, None);
            assert_eq!(budget.output_micros_per_million, Some(1_000_000_000_000));
        }
        let (catalog, omitted) = load(
            "<priceCurrency>Fr.</priceCurrency>\
             <pricePerMillionPromptTokens>1.5</pricePerMillionPromptTokens>",
        );
        assert_eq!(omitted, 1);
        assert!(catalog.profiles[0].usage_budget.is_default());

        let (mut catalog, omitted) = load(
            "<priceCurrency/><pricePerMillionPromptTokens/>\
             <pricePerMillionCompletionTokens>0.0</pricePerMillionCompletionTokens>",
        );
        let profile = &mut catalog.profiles[0];
        assert_eq!(omitted, 0);
        assert_eq!(profile.usage_budget.input_micros_per_million, None);
        assert_eq!(profile.usage_budget.output_micros_per_million, Some(0));
        assert_eq!(profile.usage_budget.currency, "EUR");
        profile.usage_budget.token_limit = Some(100);
        profile.java_source = Some(JavaProfileSource::for_draft("java-prices", profile).unwrap());
        assert!(catalog.validate_java_profiles().is_err());
    }

    #[test]
    fn java_web_hints_bind_exclusively_to_supported_transports_and_global_plugin_fields() {
        let xml = |searxng| {
            format!(
                r#"<globalSettings>
                <aiBraveSearchMcpPluginId>mcp/brave_fixture</aiBraveSearchMcpPluginId>
                {searxng}
                <aiSearxngUrl>https://private.example.test/searx</aiSearxngUrl>
                <aiTavilyMcpServerLabel>custom-label-not-imported</aiTavilyMcpServerLabel>
                <aiLmStudioToolpackMcpPluginId>mcp/custom-toolpack</aiLmStudioToolpackMcpPluginId>
                <aiLmStudioToolpackAllowedTools>global-tool-not-imported</aiLmStudioToolpackAllowedTools>
                <encryptedAiTavilyApiKey>test-key-not-imported</encryptedAiTavilyApiKey>
                <encryptedAiBrightDataApiToken>test-token-not-imported</encryptedAiBrightDataApiToken>
                <encryptedAiBraveSearchApiKey>test-brave-key-not-imported</encryptedAiBraveSearchApiKey>
                <aiAgentAlwaysOfferWebTools>true</aiAgentAlwaysOfferWebTools>
                <aiProfiles>
                    <profile><id>tavily</id><name>Tavily</name><model>fixture</model>
                        <apiUrl>http://127.0.0.1:1234/api/v1/chat</apiUrl>
                        <internetAccessMode>LM_STUDIO_TAVILY_MCP</internetAccessMode></profile>
                    <profile><id>bright</id><name>Bright Data</name><model>fixture</model>
                        <apiUrl>http://127.0.0.1:1234/api/v1/chat</apiUrl>
                        <internetAccessMode>BRIGHT_DATA_WEB_MCP</internetAccessMode></profile>
                    <profile><id>brave</id><name>Brave</name><model>fixture</model>
                        <apiUrl>http://127.0.0.1:1234/api/v1/chat</apiUrl>
                        <aiBraveSearchMcpPluginId>mcp/wrong-scope</aiBraveSearchMcpPluginId>
                        <internetAccessMode>BRAVE_SEARCH_MCP</internetAccessMode></profile>
                    <profile><id>searxng</id><name>SearXNG</name><model>fixture</model>
                        <apiUrl>http://127.0.0.1:1234/api/v1/chat</apiUrl>
                        <internetAccessMode>SEARXNG_MCP</internetAccessMode></profile>
                    <profile><id>toolpack</id><name>Toolpack</name><model>fixture</model>
                        <apiUrl>http://127.0.0.1:1234/api/v1/chat</apiUrl>
                        <aiLmStudioToolpackMcpPluginId>mcp/wrong-scope</aiLmStudioToolpackMcpPluginId>
                        <allowedTools>profile-tool-not-imported</allowedTools>
                        <internetAccessMode>LM_STUDIO_TOOLPACK</internetAccessMode></profile>
                    <profile><id>anthropic</id><name>Anthropic</name><model>fixture</model>
                        <apiUrl>https://fixture.test/v1/messages</apiUrl>
                        <internetAccessMode>KORTTY_TAVILY_TOOL</internetAccessMode></profile>
                    <profile><name>Unnamed source</name><model>fixture</model>
                        <apiUrl>https://fixture.test/v1</apiUrl>
                        <internetAccessMode>KORTTY_TAVILY_TOOL</internetAccessMode></profile>
                </aiProfiles></globalSettings>"#
            )
        };
        let searxng = "<aiSearxngMcpPluginId>mcp/searxng_fixture</aiSearxngMcpPluginId>";
        let (catalog, report) = decode(xml(searxng).as_bytes()).unwrap();
        assert_eq!((report.web_hints, report.web_hints_omitted), (5, 2));
        assert_eq!(report.web_keys_omitted, 3);
        let hints = catalog
            .profiles
            .iter()
            .map(|profile| profile.java_web_hint.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            hints,
            [
                Some(JavaWebHint::LmStudioTavily),
                Some(JavaWebHint::BrightData),
                Some(JavaWebHint::Brave {
                    plugin: "mcp/brave_fixture".into()
                }),
                Some(JavaWebHint::Searxng {
                    plugin: "mcp/searxng_fixture".into()
                }),
                Some(JavaWebHint::Toolpack {
                    plugin: "mcp/custom-toolpack".into()
                }),
                None,
                None,
            ]
        );
        assert!(
            catalog
                .profiles
                .iter()
                .all(|profile| !profile.has_web_search())
        );
        let stored = serde_json::to_string(&catalog).unwrap();
        for omitted in ["not-imported", "private.example.test", "wrong-scope"] {
            assert!(!stored.contains(omitted));
        }
        let source = catalog.profiles[3].java_source.as_ref().unwrap();
        let duplicated_searxng = searxng.repeat(2);
        for invalid in [
            "",
            "<aiSearxngMcpPluginId>https://private.example.test</aiSearxngMcpPluginId>",
            &duplicated_searxng,
        ] {
            let (catalog, report) = decode(xml(invalid).as_bytes()).unwrap();
            assert_eq!((report.web_hints, report.web_hints_omitted), (4, 3));
            assert!(catalog.profiles[3].java_web_hint.is_none());
            assert_eq!(catalog.profiles[3].java_source.as_ref(), Some(source));
        }

        let original = catalog.profiles[4].java_source.as_ref();
        let xml = xml(searxng);
        let field =
            "<aiLmStudioToolpackMcpPluginId>mcp/custom-toolpack</aiLmStudioToolpackMcpPluginId>";
        for invalid in [
            String::new(),
            field.replace("mcp/custom-toolpack", "mcp/*"),
            field.replace("mcp/custom-toolpack", "https://private.example.test"),
            field.repeat(2),
        ] {
            let (catalog, report) = decode(xml.replace(field, &invalid).as_bytes()).unwrap();
            assert_eq!((report.web_hints, report.web_hints_omitted), (4, 3));
            assert!(catalog.profiles[4].java_web_hint.is_none());
            assert_eq!(catalog.profiles[4].java_source.as_ref(), original);
        }
    }

    #[test]
    fn selects_journal_roles_from_the_original_java_profile_order() {
        let xml = br#"<globalSettings>
            <sessionJournalAiProfileId>missing-regular</sessionJournalAiProfileId>
            <automationJournalAiProfileId>missing-automation</automationJournalAiProfileId>
            <defaultAiProfileId>supported</defaultAiProfileId>
            <aiProfiles>
                <profile><id>original-first</id><name>Unsupported first</name>
                    <apiUrl>http://127.0.0.1:8080/v1/responses</apiUrl><model>fixture</model></profile>
                <profile><id>supported</id><name>Supported second</name>
                    <apiUrl>https://example.test/v1</apiUrl><model>fixture</model></profile>
            </aiProfiles>
        </globalSettings>"#;
        let (catalog, report) = decode(xml).unwrap();
        assert_eq!(catalog.profiles.len(), 1);
        let regular = report.journal_profiles.recording.unwrap();
        let automation = report.journal_profiles.automation.unwrap();
        assert_eq!(regular.id(), "supported");
        assert_eq!(regular.source(), catalog.profiles[0].java_source.as_ref());
        assert_eq!(automation, regular);

        let xml = std::str::from_utf8(xml).unwrap();
        let absent_default = xml.replace(
            "<defaultAiProfileId>supported",
            "<defaultAiProfileId>missing-default",
        );
        let (_, report) = decode(absent_default.as_bytes()).unwrap();
        let regular = report.journal_profiles.recording.unwrap();
        assert_eq!(regular.id(), "original-first");
        assert!(regular.source().is_none());
        assert!(report.journal_profiles.automation.as_ref() == Some(&regular));

        let explicit_unsupported = xml.replace("missing-regular", "original-first");
        let (_, report) = decode(explicit_unsupported.as_bytes()).unwrap();
        let regular = report.journal_profiles.recording.unwrap();
        assert_eq!(regular.id(), "original-first");
        assert!(regular.source().is_none());
        assert!(report.journal_profiles.automation.as_ref() == Some(&regular));
    }

    #[test]
    fn retains_role_only_java_settings_and_rejects_ambiguous_role_elements() {
        let xml = br#"<globalSettings>
            <sessionJournalAiProfileId> journal-id </sessionJournalAiProfileId>
            <automationJournalAiProfileId>other-id</automationJournalAiProfileId>
        </globalSettings>"#;
        let (_, report) = decode(xml).unwrap();
        let regular = report.journal_profiles.recording.unwrap();
        let automation = report.journal_profiles.automation.unwrap();
        assert_eq!(regular.id(), "journal-id");
        assert_eq!(automation.id(), "other-id");
        assert!(regular.source().is_none() && automation.source().is_none());
        assert!(
            decode(
                br#"<globalSettings><sessionJournalAiProfileId>first</sessionJournalAiProfileId>
            <sessionJournalAiProfileId>second</sessionJournalAiProfileId></globalSettings>"#
            )
            .is_err()
        );
    }

    #[test]
    fn retains_java_journal_evidence_defaults_and_omits_invalid_limits_only() {
        let xml = br#"<globalSettings>
            <sessionJournalAiMaxLines>-4</sessionJournalAiMaxLines>
            <sessionJournalAiChunkingEnabled>1</sessionJournalAiChunkingEnabled>
        </globalSettings>"#;
        let (_, report) = decode(xml).unwrap();
        assert_eq!(
            report.journal_profiles.evidence,
            Some(JavaJournalEvidence {
                limits: EvidenceLimits {
                    max_lines_per_direction: 0,
                    ..EvidenceLimits::default()
                },
                chunking: true,
            })
        );
        let bad_xml = String::from_utf8(FIXTURE.to_vec()).unwrap().replace(
            ">12000</sessionJournalAiTokenBudget>",
            ">10000001</sessionJournalAiTokenBudget>",
        );
        let (catalog, report) = decode(bad_xml.as_bytes()).unwrap();
        assert_eq!(catalog.profiles.len(), 3);
        assert!(report.journal_profiles.evidence.is_none());
        assert!(report.journal_evidence_omitted);
        assert!(report.journal_profiles.automation.is_some());
    }

    #[test]
    fn destination_approval_leaves_default_unset_until_explicitly_chosen() {
        let (mut catalog, _) = decode(FIXTURE).unwrap();
        assert!(catalog.validate().is_ok());
        assert!(!catalog.set_default(1));
        catalog.profiles[0].trusted_remote_endpoint = true;
        catalog.profiles[0].restore_review = false;
        assert!(catalog.validate().is_ok());
        assert!(catalog.default_profile().is_none());
        assert!(catalog.set_default(1));
        assert_eq!(catalog.default_profile().map(|profile| profile.id), Some(1));
    }

    #[test]
    fn existing_labels_and_default_win_and_replay_is_idempotent() {
        let (mut local, _) = decode(FIXTURE).unwrap();
        local.profiles[0].endpoint = "https://existing.test/v1/chat/completions".into();
        local.profiles[0].trusted_remote_endpoint = true;
        local.profiles[0].restore_review = false;
        local.profiles.truncate(1);
        assert!(local.set_default(1));
        let first_available = ProfileId::from(local.last_id + 1);
        let (incoming, _) = decode(FIXTURE).unwrap();
        assert_eq!(local.merge_java_profiles(incoming.clone()).unwrap(), 2);
        assert_eq!(local.merge_java_profiles(incoming).unwrap(), 0);
        assert_eq!(local.profiles.len(), 3);
        assert_eq!(
            local.profiles[0].endpoint,
            "https://existing.test/v1/chat/completions"
        );
        assert_eq!(local.profiles[1].id, first_available);
        assert_eq!(local.profiles[2].id, first_available + 1);
        assert_eq!(local.default_profile().map(|profile| profile.id), Some(1));
        assert!(local.validate().is_ok());
    }

    #[test]
    fn java_profile_provenance_survives_replay_and_distinguishes_changed_drafts() {
        let (incoming, _) = decode(FIXTURE).unwrap();
        let source = incoming.profiles[0].java_source.as_ref().unwrap();
        let xml = std::str::from_utf8(FIXTURE).unwrap();
        let normalized = xml.replace(source.id(), &format!(" {} ", source.id()));
        let (normalized, _) = decode(normalized.as_bytes()).unwrap();
        assert_eq!(normalized.profiles[0].java_source.as_ref(), Some(source));

        let altered = xml.replace("family/model", "family/other-model");
        let (altered, _) = decode(altered.as_bytes()).unwrap();
        let changed = altered.profiles[0].java_source.as_ref().unwrap();
        assert_eq!(changed.id(), source.id());
        assert_ne!(changed, source);

        let mut local = Catalog::default();
        assert_eq!(local.merge_java_profiles(incoming.clone()).unwrap(), 3);
        local.profiles[0].name = "Reviewed and renamed".into();
        local.profiles[0].restore_review = false;
        local.profiles[0].trusted_remote_endpoint = true;
        assert_eq!(local.merge_java_profiles(incoming.clone()).unwrap(), 0);
        let saved = serde_json::to_vec(&local).unwrap();
        let reopened: Catalog = serde_json::from_slice(&saved).unwrap();
        assert_eq!(reopened.profiles[0].java_source.as_ref(), Some(source));
        assert!(reopened.validate().is_ok());

        let mut forged = incoming;
        forged.profiles[0].java_source = Some(changed.clone());
        assert!(forged.validate_java_profiles().is_err());

        for invalid in ["", " untrimmed", "embedded\tcontrol", "bidi\u{202e}"] {
            let mut malformed = serde_json::to_value(changed).unwrap();
            malformed["id"] = invalid.into();
            assert!(serde_json::from_value::<JavaProfileSource>(malformed).is_err());
        }
    }

    #[test]
    fn rejects_urls_whose_path_or_authentication_would_change() {
        for url in [
            "https://example.test/arbitrary",
            "https://name:password@example.test/v1",
            "https://example.test/v1?secret=example",
            "https://example.test/v1/chat/completions#model",
            "https://example.test/v1/../v1/chat/completions",
            "https://example.test/a/%2e%2e/v1/chat/completions",
            "http://127.0.0.1:8080/v1/chat/completions",
        ] {
            assert!(
                remote_destination(url).is_err(),
                "unexpected destination {url}"
            );
        }
    }

    #[test]
    fn java_minimax_native_profile_retains_the_reviewed_destination() {
        let global = "https://api.minimax.io/v1/text/chatcompletion_v2";
        let xml = format!(
            "<globalSettings><aiProfiles><profile><id>java-minimax</id><name>MiniMax</name>\
             <apiUrl>{global}</apiUrl><model>MiniMax-M3</model>\
             <reasoningEffort>NONE</reasoningEffort><trustedLocalEndpoint>true</trustedLocalEndpoint>\
             <encryptedApiKey>fixture-secret</encryptedApiKey>\
             </profile></aiProfiles></globalSettings>"
        );
        for endpoint in [
            global,
            "https://api.minimaxi.com/v1/text/chatcompletion_v2",
            "https://api.minimax.chat/v1/text/chatcompletion_v2",
        ] {
            let xml = xml.replace(global, endpoint);
            let (catalog, report) = decode(xml.as_bytes()).unwrap();
            assert_eq!(report.https_imported, 1);
            assert_eq!(report.keys_omitted, 1);
            let profile = &catalog.profiles[0];
            assert_eq!(profile.connection_mode, ConnectionMode::MiniMaxNative);
            assert_eq!(profile.endpoint, endpoint);
            assert_eq!(profile.destination().unwrap(), endpoint);
            assert_eq!(profile.reasoning, Reasoning::None);
            assert!(profile.restore_review && profile.transport(1).is_err());
            assert!(!profile.trusted_local_endpoint && !profile.trusted_remote_endpoint);
            assert!(profile.credential_env.is_empty());
            catalog.validate_java_profiles().unwrap();
            let persisted = serde_json::to_value(profile).unwrap();
            assert_eq!(persisted["connection_mode"], "minimax-native");
            let reopened: Profile = serde_json::from_value(persisted).unwrap();
            assert_eq!(reopened.destination().unwrap(), endpoint);
            assert!(
                !serde_json::to_string(&catalog)
                    .unwrap()
                    .contains("fixture-secret")
            );
        }
        let xml = xml.replace(
            global,
            "https://proxy.example.test/v1/text/chatcompletion_v2",
        );
        let (_, report) = decode(xml.as_bytes()).unwrap();
        assert_eq!(report.omitted, 1);
    }

    #[test]
    fn loopback_java_profiles_keep_the_exact_http_protocol_and_never_import_authority() {
        let xml = br#"<globalSettings>
            <sessionJournalAiProfileId>native</sessionJournalAiProfileId>
            <aiProfiles>
                <profile><id>openai</id><name>Loopback OpenAI</name>
                    <apiUrl>http://127.0.0.2:11434/v1/</apiUrl><model>fixture:local</model>
                    <trustedLocalEndpoint>true</trustedLocalEndpoint>
                    <internetAccessMode>KORTTY_TAVILY_TOOL</internetAccessMode>
                    <encryptedApiKey>fixture-omitted-key</encryptedApiKey>
                    <pricePerMillionPromptTokens>0.75</pricePerMillionPromptTokens>
                    <priceCurrency>usd</priceCurrency>
                </profile>
                <profile><id>native</id><name>Loopback LM Studio</name>
                    <apiUrl>http://[::1]:1234/api/v1/chat</apiUrl><model>fixture:lm</model>
                    <internetAccessMode>BRAVE_SEARCH_MCP</internetAccessMode>
                </profile>
                <profile><id>native-mismatch</id><name>Native path without native protocol</name>
                    <apiUrl>http://[::1]:1234/api/v1/chat</apiUrl><model>fixture:lm</model>
                </profile>
                <profile><id>automatic</id><name>Automatic model</name>
                    <apiUrl>http://127.0.0.1:1234/v1</apiUrl><model>old-model</model>
                    <modelSelectionMode>AUTO</modelSelectionMode>
                </profile>
                <profile><name>Loopback without Java identity</name>
                    <apiUrl>http://127.0.0.1:11434/v1</apiUrl><model>fixture:local</model>
                </profile>
            </aiProfiles>
        </globalSettings>"#;
        let (catalog, report) = decode(xml).unwrap();
        assert_eq!(
            (
                report.loopback_imported,
                report.https_imported,
                report.omitted,
                report.keys_omitted
            ),
            (3, 0, 2, 1)
        );
        let [openai, native, automatic] = catalog.profiles.as_slice() else {
            panic!("expected three loopback drafts")
        };
        assert_eq!(openai.endpoint, "127.0.0.2:11434");
        assert_eq!(openai.connection_mode, ConnectionMode::External);
        assert_eq!(openai.usage_budget.input_micros_per_million, Some(750_000));
        assert_eq!(openai.usage_budget.currency, "USD");
        assert_eq!(native.endpoint, "[::1]:1234");
        assert_eq!(
            native.destination().unwrap(),
            "http://[::1]:1234/api/v1/chat"
        );
        assert_eq!(native.connection_mode, ConnectionMode::LmStudio);
        assert_eq!(
            automatic.connection_mode,
            ConnectionMode::LmStudioCompatible
        );
        assert_eq!(automatic.model_mode, ModelMode::Auto);
        assert_eq!(automatic.model, "old-model");
        assert_eq!(
            automatic.destination().unwrap(),
            "http://127.0.0.1:1234/v1/chat/completions"
        );
        assert_eq!(
            report
                .journal_profiles
                .recording
                .as_ref()
                .and_then(JavaAiProfileHint::source),
            native.java_source.as_ref()
        );
        assert!(catalog.default_profile().is_none());
        catalog.validate_java_profiles().unwrap();
        for profile in &catalog.profiles {
            assert!(profile.restore_review);
            assert!(!profile.trusted_local_endpoint);
            assert!(!profile.has_web_search());
            assert!(profile.credential_env.is_empty());
            assert!(!profile.allows_local_roles());
            assert!(!profile.can_be_default());
            assert!(profile.transport(1).is_err());
        }
        let mut editor = super::super::Editor::new(catalog);
        editor.update(super::super::Message::TrustLocal(true), true);
        let mut reviewed = editor.validated_catalog().unwrap();
        let approved = reviewed
            .profiles
            .iter()
            .find(|profile| !profile.restore_review)
            .unwrap();
        assert!(approved.trusted_local_endpoint && approved.allows_local_roles());
        assert!(approved.transport(1).is_ok());
        assert_eq!(
            approved.usage_budget.input_micros_per_million,
            Some(750_000)
        );
        assert_eq!(approved.usage_budget.currency, "USD");
        assert!(reviewed.default_profile().is_none());
        reviewed.prepare_backup();
        assert!(
            reviewed
                .profiles
                .iter()
                .all(|profile| profile.restore_review)
        );
        assert!(
            reviewed
                .profiles
                .iter()
                .all(|profile| profile.transport(1).is_err())
        );
        assert!(reviewed.validate_java_profiles().is_ok());
    }

    #[test]
    fn java_lm_studio_auto_drafts_keep_only_the_optional_preference() {
        let xml = br#"<globalSettings><aiProfiles>
            <profile><id>preferred</id><name>Preferred</name>
                <apiUrl>http://127.0.0.1:1234/api/v1/chat</apiUrl>
                <modelSelectionMode>AUTO</modelSelectionMode><model>vendor/model</model>
                <internetAccessMode>BRAVE_SEARCH_MCP</internetAccessMode>
                <trustedLocalEndpoint>true</trustedLocalEndpoint>
                <encryptedApiKey>fixture-omitted-credential</encryptedApiKey></profile>
            <profile><id>empty</id><name>Choose sole loaded</name>
                <apiUrl>http://127.0.0.1:1234/api/v1/chat</apiUrl>
                <modelSelectionMode>AUTO</modelSelectionMode>
                <internetAccessMode>SEARXNG_MCP</internetAccessMode></profile>
            <profile><id>invalid</id><name>Invalid</name>
                <apiUrl>http://127.0.0.1:1234/api/v1/chat</apiUrl>
                <modelSelectionMode>AUTO</modelSelectionMode><model>invalid model</model>
                <internetAccessMode>SEARXNG_MCP</internetAccessMode></profile>
            <profile><id>mismatch</id><name>Native mode on compatible endpoint</name>
                <apiUrl>http://127.0.0.1:1234/v1</apiUrl>
                <modelSelectionMode>AUTO</modelSelectionMode>
                <internetAccessMode>BRAVE_SEARCH_MCP</internetAccessMode></profile>
        </aiProfiles></globalSettings>"#;
        let (catalog, report) = decode(xml).unwrap();
        assert_eq!(report.loopback_imported, 2);
        assert_eq!(report.omitted, 2);
        catalog.validate_java_profiles().unwrap();
        let [preferred, empty] = catalog.profiles.as_slice() else {
            panic!("expected two LM Studio drafts")
        };
        assert_eq!(preferred.model, "vendor/model");
        assert!(empty.model.is_empty());
        for profile in [preferred, empty] {
            assert!(profile.ordinary_lm_studio_auto() && profile.restore_review);
            assert!(!profile.trusted_local_endpoint && !profile.has_web_search());
            assert!(profile.credential_env.is_empty() && profile.transport(1).is_err());
        }
    }

    #[test]
    fn java_lm_studio_compatible_auto_drafts_preserve_the_openai_chat_route() {
        let xml = br#"<globalSettings><aiProfiles>
            <profile><id>base</id><name>Automatic base</name>
                <apiUrl>http://127.0.0.1:1234</apiUrl>
                <modelSelectionMode>AUTO</modelSelectionMode>
                <internetAccessMode>KORTTY_TAVILY_TOOL</internetAccessMode></profile>
            <profile><id>version</id><name>Automatic v1</name>
                <apiUrl>http://[::1]:1234/v1/</apiUrl>
                <modelSelectionMode>AUTO</modelSelectionMode><model>Vendor/Model</model></profile>
            <profile><id>chat</id><name>Automatic chat</name>
                <apiUrl>http://127.0.0.2:1234/v1/chat/completions</apiUrl>
                <modelSelectionMode>AUTO</modelSelectionMode><model></model></profile>
            <profile><id>manual</id><name>Generic manual</name>
                <apiUrl>http://127.0.0.1:1234/v1/chat/completions</apiUrl>
                <model>generic</model></profile>
            <profile><id>localhost</id><name>Localhost</name>
                <apiUrl>http://localhost:1234/v1</apiUrl>
                <model>&#x2003;</model></profile>
            <profile><id>tls</id><name>TLS loopback</name>
                <apiUrl>https://127.0.0.1:1234/v1</apiUrl></profile>
            <profile><id>remote</id><name>Cloud automatic</name>
                <apiUrl>https://api.openai.com/v1</apiUrl></profile>
            <profile><id>explicit-manual</id><name>Empty Manual</name>
                <apiUrl>http://localhost:1234/v1</apiUrl>
                <modelSelectionMode>MANUAL</modelSelectionMode></profile>
            <profile><id>default</id><name>Provider default</name>
                <apiUrl>http://localhost:1234/v1</apiUrl>
                <modelSelectionMode>DEFAULT</modelSelectionMode></profile>
        </aiProfiles></globalSettings>"#;
        let (catalog, report) = decode(xml).unwrap();
        assert_eq!(
            (
                report.loopback_imported,
                report.https_imported,
                report.omitted
            ),
            (6, 1, 2)
        );
        assert_eq!(report.web_hints, 1);
        catalog.validate_java_profiles().unwrap();
        let [base, version, chat, manual, localhost, tls, server_default] =
            catalog.profiles.as_slice()
        else {
            panic!("expected six loopback drafts and one loopback HTTPS draft")
        };
        assert_eq!(server_default.model_mode, ModelMode::ServerDefault);
        assert!(server_default.model.is_empty());
        assert_eq!(
            server_default.destination().unwrap(),
            "http://localhost:1234/v1/chat/completions"
        );
        assert!(server_default.restore_review && server_default.transport(1).is_err());
        assert_eq!(base.java_web_hint, Some(JavaWebHint::Tavily));
        for (profile, preference, address) in [
            (base, "", "127.0.0.1:1234"),
            (version, "Vendor/Model", "[::1]:1234"),
            (chat, "", "127.0.0.2:1234"),
        ] {
            assert_eq!(profile.connection_mode, ConnectionMode::LmStudioCompatible);
            assert_eq!(profile.model_mode, ModelMode::Auto);
            assert_eq!(profile.model, preference);
            assert_eq!(profile.endpoint, address);
            assert_eq!(
                profile.destination().unwrap(),
                format!("http://{address}/v1/chat/completions")
            );
            assert!(profile.restore_review && !profile.trusted_local_endpoint);
            assert!(!profile.has_web_search() && profile.transport(1).is_err());
        }
        assert_eq!(manual.connection_mode, ConnectionMode::External);
        assert_eq!(manual.model_mode, ModelMode::Manual);
        for (profile, expected, uses_https) in [
            (
                localhost,
                "http://localhost:1234/v1/chat/completions",
                false,
            ),
            (tls, "https://127.0.0.1:1234/v1/chat/completions", true),
        ] {
            assert_eq!(profile.connection_mode, ConnectionMode::LmStudioCompatible);
            assert_eq!(profile.model_mode, ModelMode::Auto);
            assert!(profile.model.is_empty());
            assert_eq!(profile.endpoint, expected);
            assert_eq!(profile.destination().unwrap(), expected);
            assert_eq!(profile.remote_protocol().is_some(), uses_https);
            assert!(profile.restore_review && !profile.trusted_local_endpoint);
            assert!(!profile.trusted_remote_endpoint && !profile.reviewed_agent_https);
            assert!(!profile.has_web_search() && profile.transport(1).is_err());
        }
    }

    #[test]
    fn java_server_default_model_imports_supported_routes_as_inactive_drafts() {
        let xml = br#"<globalSettings><aiProfiles>
            <profile><id>native-default</id><name>Native Delegated</name>
                <apiUrl>https://localhost:1234/api/v1/chat</apiUrl>
                <internetAccessMode>LM_STUDIO_TAVILY_MCP</internetAccessMode>
                <modelSelectionMode>DEFAULT</modelSelectionMode><model>ignored</model>
                <reasoningEffort>HIGH</reasoningEffort></profile>
            <profile><id>cloud-default</id><name>Cloud Delegated</name>
                <apiUrl>https://api.minimax.io/v1</apiUrl>
                <modelSelectionMode>DEFAULT</modelSelectionMode></profile>
            <profile><id>wrong-protocol</id><name>Anthropic Messages</name>
                <apiUrl>https://api.anthropic.com/v1/messages</apiUrl>
                <modelSelectionMode>DEFAULT</modelSelectionMode></profile>
            <profile><id>unreviewed-route</id><name>Unmatched Path</name>
                <apiUrl>https://localhost:1234/prefix/api/v1/complete</apiUrl>
                <internetAccessMode>LM_STUDIO_TAVILY_MCP</internetAccessMode>
                <modelSelectionMode>DEFAULT</modelSelectionMode></profile>
        </aiProfiles></globalSettings>"#;
        let (catalog, report) = decode(xml).unwrap();
        assert_eq!((report.https_imported, report.omitted), (2, 2));
        assert_eq!(report.default_reasoning_omitted, 1);
        catalog.validate_java_profiles().unwrap();
        let [native, cloud] = catalog.profiles.as_slice() else {
            panic!("expected two reviewed-route drafts");
        };
        assert_eq!(native.connection_mode, ConnectionMode::LmStudio);
        assert_eq!(
            native.destination().unwrap(),
            "https://localhost:1234/api/v1/chat"
        );
        assert_eq!(native.java_web_hint, Some(JavaWebHint::LmStudioTavily));
        assert_eq!(cloud.connection_mode, ConnectionMode::RemoteOpenAi);
        assert_eq!(
            cloud.destination().unwrap(),
            "https://api.minimax.io/v1/chat/completions"
        );
        for profile in [native, cloud] {
            assert!(profile.uses_server_default_model() && profile.model.is_empty());
            assert_eq!(profile.reasoning, Reasoning::Default);
            assert!(profile.restore_review && !profile.trusted_remote_endpoint);
            assert!(profile.transport(1).is_err());
        }
    }

    #[test]
    fn java_lm_studio_manual_preserves_new_routes_and_legacy_profile_modes() {
        let xml = br#"<globalSettings><aiProfiles>
            <profile><id>localhost</id><name>Manual Localhost</name>
                <apiUrl>http://LOCALHOST:1234/v1</apiUrl>
                <modelSelectionMode>MANUAL</modelSelectionMode><model>Vendor/Model</model></profile>
            <profile><id>native-https</id><name>Manual native HTTPS</name>
                <apiUrl>https://localhost:1234/api/v1/chat</apiUrl>
                <internetAccessMode>LM_STUDIO_TAVILY_MCP</internetAccessMode>
                <model>Vendor/Native</model>
                <encryptedApiKey>fixture-not-imported</encryptedApiKey></profile>
            <profile><id>base-https</id><name>Manual HTTPS base</name>
                <apiUrl>https://[::1]:1234/</apiUrl>
                <modelSelectionMode>MANUAL</modelSelectionMode><model>Vendor/Model</model></profile>
            <profile><id>legacy-https</id><name>Manual HTTPS v1</name>
                <apiUrl>https://localhost:1234/v1</apiUrl><model>Vendor/Model</model></profile>
            <profile><id>legacy-http</id><name>Manual numeric HTTP</name>
                <apiUrl>http://127.0.0.1:1234/v1</apiUrl><model>Vendor/Model</model></profile>
        </aiProfiles></globalSettings>"#;
        let (incoming, report) = decode(xml).unwrap();
        assert_eq!((report.loopback_imported, report.https_imported), (2, 3));
        assert_eq!((report.keys_omitted, report.omitted), (1, 0));
        assert_eq!(incoming.profiles.len(), 5);
        for (profile, mode, endpoint, model) in [
            (
                &incoming.profiles[0],
                ConnectionMode::LmStudioCompatible,
                "http://localhost:1234/v1/chat/completions",
                "Vendor/Model",
            ),
            (
                &incoming.profiles[1],
                ConnectionMode::LmStudio,
                "https://localhost:1234/api/v1/chat",
                "Vendor/Native",
            ),
            (
                &incoming.profiles[2],
                ConnectionMode::LmStudioCompatible,
                "https://[::1]:1234/v1/chat/completions",
                "Vendor/Model",
            ),
            (
                &incoming.profiles[3],
                ConnectionMode::RemoteOpenAi,
                "https://localhost:1234/v1/chat/completions",
                "Vendor/Model",
            ),
            (
                &incoming.profiles[4],
                ConnectionMode::External,
                "127.0.0.1:1234",
                "Vendor/Model",
            ),
        ] {
            assert_eq!(profile.connection_mode, mode);
            assert_eq!(profile.model_mode, ModelMode::Manual);
            assert_eq!(profile.model, model);
            assert_eq!(profile.endpoint, endpoint);
            assert_eq!(profile.destination().unwrap(), endpoint);
            assert_eq!(
                profile.remote_protocol().is_some(),
                endpoint.starts_with("https:")
            );
            assert!(profile.restore_review && profile.transport(1).is_err());
            assert!(!profile.trusted_local_endpoint && !profile.trusted_remote_endpoint);
            assert!(profile.credential_env.is_empty() && !profile.has_web_search());
        }
        incoming.validate_java_profiles().unwrap();
        let mut local = Catalog::default();
        assert_eq!(local.merge_java_profiles(incoming.clone()).unwrap(), 5);
        local.profiles[0].name = "Locally renamed draft".into();
        assert_eq!(local.merge_java_profiles(incoming.clone()).unwrap(), 0);

        let mut editor = super::super::Editor::new(incoming);
        for (id, uses_https) in [(1, false), (2, true), (3, true)] {
            editor.update(super::super::Message::Select(id), true);
            editor.update(
                if uses_https {
                    super::super::Message::TrustRemote(true)
                } else {
                    super::super::Message::TrustLocal(true)
                },
                true,
            );
            let profile = editor.selected().unwrap();
            assert!(!profile.restore_review);
            assert_eq!(profile.transport(1).unwrap().is_remote(), uses_https);
        }
        assert!(
            editor
                .validated_catalog()
                .unwrap()
                .default_profile()
                .is_none()
        );
    }

    #[test]
    fn java_lm_studio_native_auto_preserves_loopback_hosts_and_transport() {
        let xml = br#"<globalSettings><aiProfiles>
            <profile><id>local</id><name>Local LM Studio</name>
                <apiUrl>http://localhost:1234/api/v1/chat</apiUrl>
                <modelSelectionMode>AUTO</modelSelectionMode><model>Vendor/Model</model>
                <internetAccessMode>LM_STUDIO_TAVILY_MCP</internetAccessMode></profile>
            <profile><id>tls</id><name>LM Studio with TLS</name>
                <apiUrl>https://localhost:1234/api/v1/chat</apiUrl>
                <modelSelectionMode>AUTO</modelSelectionMode><model>Vendor/Model</model>
                <internetAccessMode>LM_STUDIO_TAVILY_MCP</internetAccessMode>
                <encryptedApiKey>never-restore-this-key</encryptedApiKey></profile>
            <profile><name>No identity</name>
                <apiUrl>https://127.0.0.1:1234/api/v1/chat</apiUrl>
                <modelSelectionMode>AUTO</modelSelectionMode>
                <internetAccessMode>LM_STUDIO_TAVILY_MCP</internetAccessMode></profile>
        </aiProfiles></globalSettings>"#;
        let (catalog, report) = decode(xml).unwrap();
        assert_eq!(
            (
                report.loopback_imported,
                report.https_imported,
                report.omitted
            ),
            (1, 1, 1)
        );
        assert_eq!(
            (
                report.web_hints,
                report.web_hints_omitted,
                report.keys_omitted
            ),
            (2, 0, 1)
        );
        let [local, tls] = catalog.profiles.as_slice() else {
            panic!("expected two native LM Studio drafts")
        };
        for (profile, expected) in [
            (local, "http://localhost:1234/api/v1/chat"),
            (tls, "https://localhost:1234/api/v1/chat"),
        ] {
            assert_eq!(profile.connection_mode, ConnectionMode::LmStudio);
            assert_eq!(profile.model_mode, ModelMode::Auto);
            assert_eq!(profile.model, "Vendor/Model");
            assert_eq!(profile.endpoint, expected);
            assert_eq!(profile.destination().unwrap(), expected);
            assert!(profile.restore_review);
            assert!(!profile.trusted_local_endpoint && !profile.trusted_remote_endpoint);
            assert!(profile.credential_env.is_empty() && !profile.has_web_search());
            assert!(profile.transport(1).is_err());
        }
        assert_eq!(local.java_web_hint, Some(JavaWebHint::LmStudioTavily));
        assert_eq!(tls.java_web_hint, Some(JavaWebHint::LmStudioTavily));
        catalog.validate_java_profiles().unwrap();
    }

    #[test]
    fn java_exact_chat_paths_keep_their_source_destination_and_enable_only_supported_auto() {
        for (endpoint, selection, internet, expected) in [
            (
                "http://127.0.0.1:1234/V1///",
                "MANUAL",
                "DISABLED",
                Some((
                    ConnectionMode::LmStudioCompatible,
                    "http://127.0.0.1:1234/V1/chat/completions",
                )),
            ),
            (
                "https://127.0.0.1:1234/V1/",
                "MANUAL",
                "DISABLED",
                Some((
                    ConnectionMode::LmStudioCompatible,
                    "https://127.0.0.1:1234/V1/chat/completions",
                )),
            ),
            (
                "https://fixture.test/gateway/V1/",
                "MANUAL",
                "DISABLED",
                Some((
                    ConnectionMode::RemoteOpenAi,
                    "https://fixture.test/gateway/V1/chat/completions",
                )),
            ),
            (
                "http://127.0.0.1:1234/tenant/API/v1/Chat/",
                "DEFAULT",
                "LM_STUDIO_TAVILY_MCP",
                Some((
                    ConnectionMode::LmStudio,
                    "http://127.0.0.1:1234/tenant/API/v1/Chat/",
                )),
            ),
            (
                "http://127.0.0.1:1234/api/v1/chat/",
                "AUTO",
                "LM_STUDIO_TAVILY_MCP",
                Some((
                    ConnectionMode::LmStudio,
                    "http://127.0.0.1:1234/api/v1/chat/",
                )),
            ),
            (
                "http://127.0.0.1:1234/v1/chat/completions/",
                "AUTO",
                "DISABLED",
                Some((
                    ConnectionMode::LmStudioCompatible,
                    "http://127.0.0.1:1234/v1/chat/completions/",
                )),
            ),
            ("http://127.0.0.1:1234/V1/", "AUTO", "DISABLED", None),
            (
                "http://127.0.0.1:1234/tenant/api/v1/chat/",
                "AUTO",
                "LM_STUDIO_TAVILY_MCP",
                None,
            ),
            ("http://127.0.0.1:1234/../V1", "MANUAL", "DISABLED", None),
        ] {
            let xml = format!(
                r#"<globalSettings><aiProfiles><profile>
                <id>exact-route</id><name>Exact route</name><model>fixture</model>
                <apiUrl>{endpoint}</apiUrl><modelSelectionMode>{selection}</modelSelectionMode>
                <internetAccessMode>{internet}</internetAccessMode>
            </profile></aiProfiles></globalSettings>"#
            );
            let (catalog, report) = decode(xml.as_bytes()).unwrap();
            assert_eq!(
                catalog.profiles.len(),
                usize::from(expected.is_some()),
                "{endpoint}"
            );
            assert_eq!(
                report.omitted,
                usize::from(expected.is_none()),
                "{endpoint}"
            );
            if let Some((mode, destination)) = expected {
                let profile = &catalog.profiles[0];
                assert_eq!(profile.connection_mode, mode);
                assert_eq!(profile.endpoint, destination);
                assert_eq!(profile.destination().unwrap(), destination);
                assert!(profile.restore_review && !profile.has_web_search());
                catalog.validate_java_profiles().unwrap();
            }
        }
    }

    #[test]
    fn java_lm_studio_auto_refuses_to_change_authority_or_chat_protocol() {
        for endpoint in [
            "https://host.example/v1",
            "https://localhost.example/v1",
            "https://localhost.:1234/v1",
            "https://localhost:0/v1",
            "https://127.1:1234/v1",
            "https://@localhost:1234/v1",
            "https://localhost:1234/v1?credential=value",
            "https://localhost:1234/v1#fragment",
            "https://localhost:1234/path/../v1",
            "http://localhost:1234/%76%31",
            "https://localhost:1234/api/v1/chat",
        ] {
            assert!(
                java_lm_studio_destination(endpoint, None).is_err(),
                "{endpoint}"
            );
        }
        assert!(
            java_lm_studio_destination("https://localhost:1234/v1", Some("LM_STUDIO_TAVILY_MCP"),)
                .is_err()
        );
    }

    #[test]
    fn java_openai_exact_https_drafts_preserve_path_model_mode_and_existing_route_kind() {
        let xml = br#"<globalSettings><aiProfiles>
            <profile><id>completion-manual</id><name>Exact Manual</name>
                <apiUrl>https://gateway.example/tenant/V1/Chat/Completions/</apiUrl>
                <modelSelectionMode>MANUAL</modelSelectionMode><model>fixture/manual</model>
                <apiKey>backup-credential</apiKey></profile>
            <profile><id>completion-default</id><name>Exact Server Default</name>
                <apiUrl>https://gateway.example/other/v1/chat/completions/</apiUrl>
                <modelSelectionMode>DEFAULT</modelSelectionMode><model>unused-in-Java</model>
            </profile>
            <profile><id>completion-base</id><name>Canonical HTTPS</name>
                <apiUrl>https://gateway.example/old/V1/</apiUrl><model>fixture/canonical</model>
            </profile>
            <profile><id>completion-loopback</id><name>Pinned HTTPS Loopback</name>
                <apiUrl>https://localhost:8443/old/v1/Chat/Completions/</apiUrl>
                <modelSelectionMode>DEFAULT</modelSelectionMode></profile>
            <profile><id>completion-auto</id><name>Unsupported Remote Auto</name>
                <apiUrl>https://gateway.example/tenant/V1/Chat/Completions/</apiUrl>
                <modelSelectionMode>AUTO</modelSelectionMode></profile>
            <profile><id>completion-native-mcp</id><name>Wrong MCP Route</name>
                <apiUrl>https://gateway.example/tenant/V1/Chat/Completions/</apiUrl><model>fixture</model>
                <internetAccessMode>LM_STUDIO_TOOLPACK</internetAccessMode></profile>
            <profile><id>custom-https</id><name>Custom HTTPS Route</name>
                <apiUrl>https://gateway.example/tenant/chat-proxy/Invoke/</apiUrl>
                <modelSelectionMode>DEFAULT</modelSelectionMode></profile>
            <profile><id>custom-loopback</id><name>Custom Loopback Route</name>
                <apiUrl>http://127.0.0.1:1234/chat-proxy/Invoke/</apiUrl>
                <modelSelectionMode>MANUAL</modelSelectionMode><model>fixture/local</model></profile>
        </aiProfiles></globalSettings>"#;
        let (catalog, report) = decode(xml).unwrap();
        assert_eq!(report.https_imported, 5);
        assert_eq!(report.loopback_imported, 1);
        assert_eq!(report.omitted, 2);
        for (profile, mode, destination, model_mode, model) in [
            (
                &catalog.profiles[0],
                ConnectionMode::RemoteOpenAiExact,
                "https://gateway.example/tenant/V1/Chat/Completions/",
                ModelMode::Manual,
                "fixture/manual",
            ),
            (
                &catalog.profiles[1],
                ConnectionMode::RemoteOpenAiExact,
                "https://gateway.example/other/v1/chat/completions/",
                ModelMode::ServerDefault,
                "",
            ),
            (
                &catalog.profiles[2],
                ConnectionMode::RemoteOpenAi,
                "https://gateway.example/old/V1/chat/completions",
                ModelMode::Manual,
                "fixture/canonical",
            ),
            (
                &catalog.profiles[3],
                ConnectionMode::LmStudioCompatible,
                "https://localhost:8443/old/v1/Chat/Completions/",
                ModelMode::ServerDefault,
                "",
            ),
            (
                &catalog.profiles[4],
                ConnectionMode::RemoteOpenAiExact,
                "https://gateway.example/tenant/chat-proxy/Invoke/",
                ModelMode::ServerDefault,
                "",
            ),
            (
                &catalog.profiles[5],
                ConnectionMode::LmStudioCompatible,
                "http://127.0.0.1:1234/chat-proxy/Invoke/",
                ModelMode::Manual,
                "fixture/local",
            ),
        ] {
            assert_eq!(profile.connection_mode, mode);
            assert_eq!(profile.endpoint, destination);
            assert_eq!(profile.destination().unwrap(), destination);
            assert_eq!(
                (profile.model_mode, profile.model.as_str()),
                (model_mode, model)
            );
            assert!(profile.restore_review && profile.credential_env.is_empty());
            assert!(profile.transport(1).is_err());
        }
        assert_eq!(catalog.profiles.len(), 6);
        catalog.validate_java_profiles().unwrap();
        let replayed = decode(xml).unwrap().0;
        assert_eq!(
            catalog.java_profile_sources().collect::<Vec<_>>(),
            replayed.java_profile_sources().collect::<Vec<_>>()
        );
    }

    #[test]
    fn java_loopback_anthropic_imports_the_explicit_model_as_an_inactive_manual_draft() {
        let xml = br#"<globalSettings><aiProfiles>
            <profile><id>messages-manual</id><name>Messages Manual</name>
                <apiUrl>http://localhost:1234/Proxy/V1/Messages</apiUrl>
                <modelSelectionMode>MANUAL</modelSelectionMode><model>exact/chat</model>
                <apiKey>backup-credential</apiKey>
                <internetAccessMode>LM_STUDIO_TOOLPACK</internetAccessMode></profile>
            <profile><id>messages-auto</id><name>Messages Auto</name>
                <apiUrl>http://127.0.0.1:1234/v1/messages</apiUrl>
                <modelSelectionMode>AUTO</modelSelectionMode><model>original/auto</model></profile>
            <profile><id>messages-default</id><name>Messages Default</name>
                <apiUrl>https://[::1]:8443/v1/messages</apiUrl>
                <modelSelectionMode>DEFAULT</modelSelectionMode><model>original/default</model></profile>
            <profile><id>messages-blank</id><name>Messages Without Model</name>
                <apiUrl>http://localhost:1234/v1/messages</apiUrl>
                <modelSelectionMode>DEFAULT</modelSelectionMode></profile>
        </aiProfiles></globalSettings>"#;
        let (catalog, report) = decode(xml).unwrap();
        assert_eq!(report.loopback_imported, 2);
        assert_eq!(report.https_imported, 1);
        assert_eq!(report.omitted, 1);
        let mut profiles = catalog.profiles.iter();
        for (endpoint, model, id) in [
            (
                "http://localhost:1234/Proxy/V1/Messages",
                "exact/chat",
                "messages-manual",
            ),
            (
                "http://127.0.0.1:1234/v1/messages",
                "original/auto",
                "messages-auto",
            ),
            (
                "https://[::1]:8443/v1/messages",
                "original/default",
                "messages-default",
            ),
        ] {
            let profile = profiles.next().unwrap();
            assert_eq!(profile.connection_mode, ConnectionMode::LoopbackAnthropic);
            assert_eq!(profile.model_mode, ModelMode::Manual);
            assert_eq!(profile.model, model);
            assert_eq!(profile.destination().unwrap(), endpoint);
            assert_eq!(profile.java_source.as_ref().unwrap().id(), id);
            assert!(profile.restore_review && !profile.has_web_search());
            assert!(profile.java_web_hint.is_none());
            assert!(profile.credential_env.is_empty() && profile.transport(1).is_err());
        }
        assert!(profiles.next().is_none());
        catalog.validate_java_profiles().unwrap();
        let stored = serde_json::to_vec(&catalog).unwrap();
        let restored: Catalog = serde_json::from_slice(&stored).unwrap();
        restored.validate_java_profiles().unwrap();
        assert_eq!(
            catalog.java_profile_sources().collect::<Vec<_>>(),
            restored.java_profile_sources().collect::<Vec<_>>()
        );
    }

    #[test]
    fn java_https_anthropic_keeps_the_exact_route_and_model_before_mode_selection() {
        let xml = br#"<globalSettings><aiProfiles>
            <profile><id>gateway</id><name>Gateway Messages</name>
                <apiUrl>https://gateway.example/tenant/V1/Messages</apiUrl>
                <modelSelectionMode>DEFAULT</modelSelectionMode><model>fixture/default</model>
                <encryptedApiKey>backup-credential</encryptedApiKey>
                <internetAccessMode>LM_STUDIO_TOOLPACK</internetAccessMode></profile>
            <profile><id>official</id><name>Official Messages</name>
                <apiUrl>https://api.anthropic.com/v1/messages/</apiUrl>
                <modelSelectionMode>AUTO</modelSelectionMode><model>fixture/auto</model></profile>
            <profile><id>canonical</id><name>Canonical Messages</name>
                <apiUrl>https://api.anthropic.com/v1/messages</apiUrl>
                <modelSelectionMode>MANUAL</modelSelectionMode><model>fixture/manual</model></profile>
            <profile><id>no-model</id><name>Missing model</name>
                <apiUrl>https://gateway.example/tenant/V1/Messages</apiUrl>
                <modelSelectionMode>DEFAULT</modelSelectionMode></profile>
            <profile><id>query</id><name>Unsupported query</name>
                <apiUrl>https://gateway.example/tenant/V1/Messages?token=private</apiUrl>
                <model>fixture/private</model></profile>
            <profile><id>ambiguous</id><name>Ambiguous Messages</name>
                <apiUrl>https://api.anthropic.com/tenant//v1/messages/chat/completions</apiUrl>
                <model>fixture/ambiguous</model></profile>
            <profile><id>encoded-host</id><name>Encoded Messages host</name>
                <apiUrl>https://%61pi.anthropic.com/v1/messages</apiUrl>
                <model>fixture/encoded</model></profile>
        </aiProfiles></globalSettings>"#;
        let (catalog, report) = decode(xml).unwrap();
        assert_eq!(
            (report.https_imported, report.omitted, report.keys_omitted),
            (3, 4, 1)
        );
        for (profile, (mode, destination, model)) in catalog.profiles.iter().zip([
            (
                ConnectionMode::AnthropicExact,
                "https://gateway.example/tenant/V1/Messages",
                "fixture/default",
            ),
            (
                ConnectionMode::AnthropicExact,
                "https://api.anthropic.com/v1/messages/",
                "fixture/auto",
            ),
            (
                ConnectionMode::Anthropic,
                "https://api.anthropic.com/v1/messages",
                "fixture/manual",
            ),
        ]) {
            assert_eq!(profile.connection_mode, mode);
            assert_eq!(profile.model_mode, ModelMode::Manual);
            assert_eq!(profile.model, model);
            assert_eq!(profile.destination().unwrap(), destination);
            assert!(profile.restore_review && profile.transport(1).is_err());
            assert!(!profile.has_web_search() && profile.java_web_hint.is_none());
            assert!(profile.credential_env.is_empty());
        }
        catalog.validate_java_profiles().unwrap();
        let mut editor = super::super::Editor::new(catalog.clone());
        editor.update(super::super::Message::Select(1), true);
        editor.update(super::super::Message::TrustRemote(true), true);
        let reviewed = editor.selected().unwrap();
        assert!(reviewed.transport(1).unwrap().is_remote());
        assert!(!reviewed.supports_model_discovery());

        let restored: Catalog =
            serde_json::from_slice(&serde_json::to_vec(&catalog).unwrap()).unwrap();
        restored.validate_java_profiles().unwrap();
        let mut local = Catalog::default();
        assert_eq!(local.merge_java_profiles(catalog).unwrap(), 3);
        assert_eq!(local.merge_java_profiles(restored).unwrap(), 0);
    }

    #[test]
    fn manual_java_http_rejects_ambiguous_or_different_destinations() {
        for endpoint in [
            "http://localhost.example:8080/v1",
            "https://host.example:8080",
            "http://user:password@127.0.0.1:8080/v1",
            "http://127.0.0.1:8080/v1?token=value",
            "http://127.0.0.1:8080/v1#fragment",
            "http://127.0.0.1:8080/api/v1/chat",
            "http://127.0.0.1:8080/v1/messages?token=value",
            "http://127.0.0.1:8080/v1/../v1",
            "http://127.0.0.1:8080/%76%31",
            "http://127.0.0.1:0/v1",
            "http://10.0.0.1:8080/v1",
            "https://localhost:1234/api/v1/chat",
            "https://localhost:1234/api/v1/../chat",
        ] {
            assert!(
                manual_http_destination(endpoint, None).is_err(),
                "{endpoint}"
            );
        }
        assert_eq!(
            manual_http_destination("http://127.0.0.1/v1/chat/completions", None).unwrap(),
            (ConnectionMode::External, "127.0.0.1:80".into())
        );
        assert_eq!(
            manual_http_destination("http://127.0.0.1/v1////", None).unwrap(),
            (ConnectionMode::External, "127.0.0.1:80".into())
        );
        for endpoint in [
            "http://127.0.0.1:1234/v1",
            "https://localhost:1234/v1",
            "https://localhost:1234/api/v1/chat?token=value",
            "https://host.example:1234/api/v1/chat",
        ] {
            assert!(
                manual_http_destination(endpoint, Some("SEARXNG_MCP")).is_err(),
                "{endpoint}"
            );
        }
        assert_eq!(
            manual_http_destination(
                "http://[0:0:0:0:0:0:0:1]:1234/api/v1/chat",
                Some("SEARXNG_MCP")
            )
            .unwrap(),
            (ConnectionMode::LmStudio, "[::1]:1234".into())
        );
        assert_eq!(
            java_anthropic_destination("https://localhost:1234/v1/messages", "MANUAL").unwrap(),
            (
                ConnectionMode::Anthropic,
                "https://localhost:1234/v1/messages".into()
            )
        );
    }

    #[test]
    fn imports_java_line_and_inline_argv_without_shell_expansion() {
        let profile = |template: &str| {
            let xml = format!(
                r#"<globalSettings><aiProfiles><profile>
                    <name>CLI fixture</name>
                    <connectionMode>LOCAL_CLI</connectionMode>
                    <cliProviderId>codex-cli</cliProviderId>
                    <cliArgumentsTemplate><![CDATA[{template}]]></cliArgumentsTemplate>
                </profile></aiProfiles></globalSettings>"#
            );
            decode(xml.as_bytes()).unwrap().0.profiles.pop()
        };

        for (template, expected) in [
            (
                "exec\r\n  --literal=quoted value\r\n   \n {stdinPrompt}\n",
                vec!["exec", "--literal=quoted value", "{stdinPrompt}"],
            ),
            (
                r#"exec "one two" 'literal\path' escaped\ space '&' '{stdinPrompt}'"#,
                vec![
                    "exec",
                    "one two",
                    r"literal\path",
                    "escaped space",
                    "&",
                    "{stdinPrompt}",
                ],
            ),
            (
                r#"tool {systemPromptFile} {userPromptFile} tail\"#,
                vec!["tool", "{systemPromptFile}", "{userPromptFile}", r"tail\"],
            ),
            (
                "exec --system={systemPromptFile} --user={userPromptFile} --effort={reasoning}",
                vec![
                    "exec",
                    "--system={systemPromptFile}",
                    "--user={userPromptFile}",
                    "--effort={reasoning}",
                ],
            ),
            (
                "exec --reasoning {reasoning} {stdinPrompt}",
                vec!["exec", "--reasoning", "{reasoning}", "{stdinPrompt}"],
            ),
        ] {
            let draft = profile(template).unwrap();
            assert!(draft.restore_review);
            let settings = serde_json::to_value(draft.cli.unwrap()).unwrap();
            assert_eq!(settings["path"], "");
            assert_eq!(settings["provider"], "codex-cli");
            assert_eq!(settings["arguments"], serde_json::json!(expected));
        }

        for template in [
            "exec --prompt {prompt}",
            "exec {stdinPrompt} {promptFile}",
            "exec {model} {stdinPrompt}",
            "exec --model={model} {stdinPrompt}",
            "exec {systemPromptFile}",
            "exec --flag-without-a-prompt",
            "exec 'broken {stdinPrompt}",
        ] {
            assert!(profile(template).is_none(), "accepted {template}");
        }
    }

    #[test]
    fn java_cli_import_preserves_model_reasoning_and_private_file_templates_as_drafts() {
        let xml = br#"<globalSettings><aiProfiles><profile>
                <id>cli-fixture</id>
                <name>CLI fixture</name>
                <connectionMode>LOCAL_CLI</connectionMode>
                <cliProviderId>codex-cli</cliProviderId>
                <modelSelectionMode>AUTO</modelSelectionMode>
                <model>fixture/model</model>
                <reasoningEffort>XHIGH</reasoningEffort>
                <cliArgumentsTemplate>exec '--model={model}' --effort={reasoning} --input={promptFile}</cliArgumentsTemplate>
            </profile></aiProfiles></globalSettings>"#;
        let (catalog, report) = decode(xml).unwrap();
        assert_eq!(report.cli_imported, 1);
        let draft = &catalog.profiles[0];
        assert_eq!(draft.reasoning, Reasoning::ExtraHigh);
        assert_eq!(draft.model, "fixture/model");
        assert!(draft.restore_review);
        assert!(draft.validate_connection().is_err());
        assert_eq!(
            serde_json::to_value(draft.cli.as_ref().unwrap()).unwrap()["arguments"],
            serde_json::json!([
                "exec",
                "--model={model}",
                "--effort={reasoning}",
                "--input={promptFile}"
            ])
        );
    }
}
