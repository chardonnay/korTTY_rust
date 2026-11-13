//! Native AI Manager and its profile-scoped settings.

use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::widget::{
    Id, button, checkbox, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::{Alignment, Element, Length, Task};
use serde::{Deserialize, Serialize};

use crate::connection_window::CredentialChoice;
use crate::terminal_agent::shortcut;
use crate::vault_window::VaultWindow;
use crate::{
    clipboard_policy::{
        AI_PROFILE_CREATION_DISABLED, AI_PROFILE_EDIT_DISABLED, AiMemoryPolicy, ClipboardPolicy,
    },
    local_ai, profile,
};

pub(crate) mod backup;
mod chat_terminal_actions;
mod code_text_language;
mod document_find;
mod local;
mod memory;
mod model_inventory;
pub(crate) mod policy_auto;
#[cfg(test)]
mod policy_auto_tests;
#[cfg(test)]
mod profile_policy_tests;
mod profiles;
#[cfg(test)]
mod provisioned_profile_tests;
mod search;
pub(crate) mod setup;
mod skills;

use profiles::picker::DismissOnEscape as _;

pub(crate) use chat_terminal_actions::ChatTerminalActions;
pub(crate) use code_text_language::CodeTextLanguage;
pub(crate) use kortty_jobs::ProfileRef;
/// Runtime profile identifiers also accommodate provisioned profiles.
pub(crate) type ProfileId = u128;
pub(crate) use local::knowledge_sync;
pub(crate) use local::retrieval::{
    Citation as KnowledgeCitation, KnowledgeAuthority,
    MAX_QUERY_BYTES as MAX_KNOWLEDGE_QUERY_BYTES, Plan as KnowledgePlan,
    Retrieved as RetrievedKnowledge,
};
pub(crate) use memory::Scope;
pub(crate) use memory::checked_reference_text;
pub(crate) use memory::learning::{
    MAX_RULE_CHARACTERS, RuleProposal, SavedRule, normalized_length as rule_normalized_length,
    preview as review_memory_rule,
};
pub(crate) use memory::prompt::MemoryPrompt;
#[cfg(test)]
pub(crate) use profiles::Message as ProfileMessageForTest;
pub(crate) use skills::statistics::Surface as SkillUsageSurface;
pub(crate) use skills::{
    Options as SkillRuleOptions, SavedSkillRule, SkillCandidate, SkillGuidance, SkillRuleProposal,
    SkillRuleTarget, SkillSelection, SkillUsageReceipt,
};

pub(crate) fn shutdown_skill_statistics() {
    skills::usage::shutdown();
}

#[cfg(test)]
pub(crate) fn flush_skill_statistics_for_tests() {
    skills::usage::flush();
}

#[cfg(test)]
pub(crate) fn recorded_skill_uses_for_tests(root: &Path) -> (u64, u64) {
    use skills::statistics::{Granularity, Range, read};

    flush_skill_statistics_for_tests();
    let report = read(root).unwrap().report(
        Range {
            granularity: Granularity::Day,
            periods: 30,
        },
        &[],
    );
    (report.total.automatic, report.total.pinned)
}

pub(crate) const TITLE: &str = "AI Manager";
pub(crate) const FILE_NAME: &str = "ai-manager.json";
const VERSION: u8 = 1;
const MAX_TIMEOUT_MINUTES: u16 = 1440;
const MAX_BYTES: u64 = 4 * 1024 * 1024;
const MUTATION_LOCK: &str = ".ai-manager.lock";
const MAX_NOTICE_HEIGHT: f32 = 32.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RuleSource {
    Manual,
    Chat,
}

impl RuleSource {
    fn name(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Chat => "chat",
        }
    }
}

#[derive(Clone)]
pub(crate) struct RuleOrigin {
    pub(crate) source: RuleSource,
    pub(crate) topic: Option<String>,
}

impl RuleOrigin {
    pub(crate) const MANUAL: Self = Self {
        source: RuleSource::Manual,
        topic: None,
    };
}

#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Configuration {
    version: u8,
    general: General,
    profiles: profiles::Catalog,
    local: local::State,
    skills: skills::Store,
    memory: memory::Settings,
    #[serde(skip)]
    origin: Option<PathBuf>,
    #[serde(skip)]
    model_catalog: Option<Arc<local_ai::model_catalog::Catalog>>,
}

#[derive(Clone, Copy)]
enum ProfileRoute {
    Local,
    ReviewedHttp,
    ReviewedHttps,
    ReviewedCli,
    TerminalChat,
}

pub(crate) struct ReviewedLocalAiProfile<'a> {
    pub(crate) id: ProfileId,
    pub(crate) name: &'a str,
    pub(crate) model: &'a str,
    pub(crate) destination: String,
    pub(crate) policy_model_fingerprint: Option<[u8; 32]>,
}

pub(crate) struct JournalNoteProfile {
    pub(crate) id: ProfileId,
    pub(crate) transport: local_ai::Profile,
    pub(crate) selection_limit: usize,
    pub(crate) destination: String,
}

impl ProfileRoute {
    fn allows(self, profile: &profiles::Profile) -> bool {
        match self {
            Self::Local => profile.allows_local_roles(),
            Self::ReviewedHttp => !profile.is_cli(),
            Self::ReviewedHttps => profile.remote_protocol().is_some(),
            Self::ReviewedCli => profile.is_cli() && !profile.restore_review,
            Self::TerminalChat => true,
        }
    }
}

fn is_policy_model_profile(profile: &profiles::Profile) -> bool {
    profile.is_managed() && crate::clipboard_policy::valid_policy_model_id(&profile.model)
}

fn require_journal_vision(profile: &profiles::Profile, vision: bool) -> Result<(), String> {
    if vision && (profile.vision == profiles::Vision::Disabled || is_policy_model_profile(profile))
    {
        return Err("Vision is disabled for the selected journal AI profile.".into());
    }
    Ok(())
}

impl fmt::Debug for Configuration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AiConfiguration")
            .finish_non_exhaustive()
    }
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            version: VERSION,
            general: General::default(),
            profiles: profiles::Catalog::default(),
            local: local::State::default(),
            skills: skills::Store::default(),
            memory: memory::Settings::default(),
            origin: None,
            model_catalog: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct General {
    features_enabled: bool,
    timeout_minutes: u16,
    code_text_language: CodeTextLanguage,
    chat_terminal_actions: ChatTerminalActions,
    show_agent_setup: bool,
    #[serde(skip_serializing_if = "shortcut::Preferences::is_default")]
    agent_shortcut: shortcut::Preferences,
}

impl Default for General {
    fn default() -> Self {
        Self {
            features_enabled: true,
            timeout_minutes: 1,
            code_text_language: CodeTextLanguage::default(),
            chat_terminal_actions: ChatTerminalActions::default(),
            show_agent_setup: true,
            agent_shortcut: Default::default(),
        }
    }
}

impl Configuration {
    pub(crate) fn load() -> io::Result<Self> {
        Self::load_from(&profile::root()?)
    }

    pub(crate) fn saved_root(&self) -> Option<&Path> {
        self.origin.as_deref()
    }

    pub(crate) fn reload_saved(&self) -> io::Result<Self> {
        Self::load_from(
            self.saved_root()
                .ok_or_else(|| invalid("AI settings have no saved location"))?,
        )
    }

    pub(crate) fn runtime_update_settings(
        &self,
    ) -> crate::local_ai::runtime_package::online::policy::Settings {
        self.local.runtime_update
    }

    pub(crate) fn authorize_runtime_update(
        path: &Path,
        expected: crate::local_ai::runtime_package::online::policy::Settings,
    ) -> io::Result<std::fs::File> {
        use crate::private_files::{Directory, require_single_link};

        ClipboardPolicy::load().require_ai_runtime_downloads()?;
        let root = Directory::open(path, false)?;
        let lock = Self::lock_mutation_file(&root)?;
        let file = root.open_file(FILE_NAME)?;
        require_single_link(&file)?;
        let saved = Self::read_preference(file)?;
        if !expected.mode.checks_channel()
            || !saved.enabled()
            || saved.local.runtime_update != expected
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The saved runtime update policy no longer authorizes this operation.",
            ));
        }
        Ok(lock)
    }

    pub(crate) fn load_from(root: &Path) -> io::Result<Self> {
        let path = root.join(FILE_NAME);
        let mut configuration = match profile::open_preference(&path) {
            Ok(file) => Self::read_preference(file)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error),
        };
        configuration.origin = Some(root.to_path_buf());
        configuration.model_catalog = local_ai::model_catalog::load_cached(root)
            .ok()
            .flatten()
            .map(Arc::new);
        configuration.with_policy_profiles(&ClipboardPolicy::load())
    }

    pub(crate) fn model_catalog(&self) -> Option<&Arc<local_ai::model_catalog::Catalog>> {
        self.model_catalog.as_ref()
    }

    pub(crate) fn install_model_catalog(
        &mut self,
        catalog: Option<Arc<local_ai::model_catalog::Catalog>>,
    ) {
        self.model_catalog = catalog;
    }

    pub(crate) fn require_model_catalog_refresh(root: &Path) -> io::Result<()> {
        let policy = ClipboardPolicy::load();
        if policy.problem.is_some() || !policy.ai_allowed {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Managed policy blocks AI catalog refresh.",
            ));
        }
        policy.require_ai_model_downloads()?;
        let directory = crate::private_files::Directory::open(root, false)?;
        let preference = match directory.open_file(FILE_NAME) {
            Ok(file) => {
                crate::private_files::require_single_link(&file)?;
                Self::read_preference(file)?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error),
        };
        if !preference.enabled() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "AI features are disabled in AI Manager.",
            ));
        }
        Ok(())
    }

    pub(crate) fn with_policy_profiles(mut self, policy: &ClipboardPolicy) -> io::Result<Self> {
        self.profiles.project_policy_profiles(policy)?;
        Ok(self)
    }

    pub(crate) fn policy_profiles_changed(&self, other: &Self) -> bool {
        self.profiles.policy_profiles_changed(&other.profiles)
    }

    pub(crate) fn profile_reference(&self, id: ProfileId) -> Option<ProfileRef> {
        self.profiles.profile_reference(id)
    }

    pub(crate) fn resolve_profile_reference(&self, reference: &ProfileRef) -> Option<ProfileId> {
        self.profiles.resolve_profile_reference(reference)
    }

    pub(crate) fn legacy_policy_alias(&self, id: ProfileId) -> Option<u64> {
        self.profiles.legacy_policy_alias(id)
    }

    pub(crate) fn policy_auto_definitions(
        &self,
    ) -> impl Iterator<Item = &Arc<crate::clipboard_policy::PolicyAiProfile>> {
        self.profiles
            .entries()
            .filter_map(profiles::Profile::policy_auto_candidate)
    }

    pub(crate) fn policy_auto_model_pending(&self, id: ProfileId) -> bool {
        self.profiles
            .profile(id)
            .is_some_and(profiles::Profile::policy_auto_pending)
    }

    fn read_preference(file: std::fs::File) -> io::Result<Self> {
        if !file.metadata()?.is_file() {
            return Err(invalid("AI settings require a regular file"));
        }
        let mut content = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut content)?;
        Self::decode(&content)
    }

    fn decode(content: &[u8]) -> io::Result<Self> {
        if content.len() as u64 > MAX_BYTES {
            return Err(invalid("AI settings exceed the size limit"));
        }
        let mut value: Self = serde_json::from_slice(content).map_err(invalid)?;
        value.skills.migrate_ids();
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION
            || !(1..=MAX_TIMEOUT_MINUTES).contains(&self.general.timeout_minutes)
        {
            return Err(invalid("Invalid AI settings version or request timeout"));
        }
        self.profiles.validate().map_err(invalid)?;
        self.general.agent_shortcut.validate().map_err(invalid)?;
        self.local.validate().map_err(invalid)?;
        self.skills.validate().map_err(invalid)
    }

    fn save_against(&self, path: &Path, baseline: &Self) -> io::Result<()> {
        self.save_with_indexes(path, baseline, None, None)
            .map(|_| ())
    }

    #[cfg(test)]
    pub(crate) fn save_fixture_at(&self, path: &Path, baseline: &Self) -> io::Result<()> {
        self.save_against(path, baseline)
    }

    #[cfg(test)]
    pub(crate) fn load_fixture_from(path: &Path) -> io::Result<Self> {
        Self::load_from(path)
    }

    fn save_with_indexes(
        &self,
        path: &Path,
        baseline: &Self,
        indexes: Option<local::SaveIndexes>,
        client: Option<&local_ai::Client>,
    ) -> io::Result<Option<String>> {
        self.save_with_policy(path, baseline, indexes, client, ClipboardPolicy::load)
    }

    fn save_with_policy(
        &self,
        path: &Path,
        baseline: &Self,
        indexes: Option<local::SaveIndexes>,
        client: Option<&local_ai::Client>,
        load_policy: impl Fn() -> ClipboardPolicy,
    ) -> io::Result<Option<String>> {
        self.save_checked(path, baseline, (indexes, client), load_policy, || Ok(()))
    }

    fn save_checked(
        &self,
        path: &Path,
        baseline: &Self,
        (indexes, client): (Option<local::SaveIndexes>, Option<&local_ai::Client>),
        load_policy: impl Fn() -> ClipboardPolicy,
        check: impl Fn() -> io::Result<()>,
    ) -> io::Result<Option<String>> {
        use crate::private_files::{Directory, FileStamp, require_single_link};
        use std::io::Write as _;

        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(invalid)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("AI settings exceed the size limit"));
        }
        let root = Directory::open(path, true)?;
        let _lock = Self::lock_mutation(&root)?;
        let (mut current, stamp) = match root.open_file(FILE_NAME) {
            Ok(file) => {
                require_single_link(&file)?;
                let stamp = FileStamp::read(&file)?;
                (Self::read_preference(file)?, Some(stamp))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (Self::default(), None),
            Err(error) => return Err(error),
        };
        current.origin.clone_from(&baseline.origin);
        current.model_catalog.clone_from(&baseline.model_catalog);
        if &current != baseline
            || [self.origin.as_deref(), baseline.origin.as_deref()]
                .into_iter()
                .flatten()
                .any(|origin| origin != path)
        {
            return Err(io::Error::other(
                "AI settings changed. Review the current settings before saving.",
            ));
        }
        let new_policy_references: Vec<_> = self
            .local
            .new_profile_bindings(&current.local)
            .filter(|reference| self.profiles.is_policy_profile_reference(reference))
            .collect();
        let require_profile_changes = || -> io::Result<()> {
            if self.profiles == current.profiles && new_policy_references.is_empty() {
                return Ok(());
            }
            let policy = load_policy();
            self.profiles
                .require_policy_changes(&current.profiles, &policy)?;
            for reference in &new_policy_references {
                self.profiles
                    .require_current_profile_reference(reference, &policy)?;
            }
            Ok(())
        };
        require_profile_changes()?;
        check()?;
        let write_settings = |require_current: &dyn Fn() -> io::Result<()>| {
            let authorize = || {
                require_current()?;
                require_profile_changes()?;
                check()
            };
            match stamp {
                Some(stamp) => root.replace_with(
                    FILE_NAME,
                    |file| file.write_all(&bytes),
                    || {
                        let file = root.open_file(FILE_NAME)?;
                        require_single_link(&file)?;
                        stamp.require_unchanged(&file)?;
                        authorize()
                    },
                ),
                None => root.publish_checked(FILE_NAME, |file| file.write_all(&bytes), authorize),
            }
        };
        match indexes {
            Some(indexes) => {
                indexes.commit_checked(write_settings, || root.sync(), client, self.enabled())
            }
            None => write_settings(&|| Ok(())).map(|_| None),
        }
    }

    fn lock_mutation(
        root: &crate::private_files::Directory,
    ) -> io::Result<crate::private_files::FileLock> {
        Self::lock_mutation_file(root).map(crate::private_files::FileLock::new)
    }

    fn lock_mutation_file(root: &crate::private_files::Directory) -> io::Result<std::fs::File> {
        let lock = root.open_lock(MUTATION_LOCK)?;
        crate::private_files::require_single_link(&lock)?;
        lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                "Another operation is saving AI settings or a knowledge index. Try again.",
            ),
            std::fs::TryLockError::Error(error) => error,
        })?;
        Ok(lock)
    }

    fn publish_knowledge_index<T>(
        path: &Path,
        publish: impl FnOnce(&local::State) -> io::Result<T>,
    ) -> io::Result<T> {
        let root = crate::private_files::Directory::open(path, false)?;
        let _lock = Self::lock_mutation(&root)?;
        let file = root.open_file(FILE_NAME)?;
        crate::private_files::require_single_link(&file)?;
        let current = Self::read_preference(file)?;
        if !current.enabled() {
            return Err(io::Error::other("AI is disabled in the saved settings."));
        }
        publish(&current.local)
    }

    fn require_local_settings(path: &Path, baseline: &local::State) -> Result<(), String> {
        let changed = || {
            "The saved knowledge settings changed. Reopen AI Manager before continuing.".to_owned()
        };
        let current = Self::load_from(path).map_err(|_| changed())?;
        if current.enabled() && &current.local == baseline {
            Ok(())
        } else {
            Err(changed())
        }
    }

    fn bound_profile(&self, reference: &ProfileRef) -> Option<&profiles::Profile> {
        self.resolve_profile_reference(reference)
            .and_then(|id| self.profiles.profile(id))
    }

    fn profile_or_default(&self, reference: Option<&ProfileRef>) -> Option<&profiles::Profile> {
        let selected = reference.and_then(|reference| self.bound_profile(reference));
        if matches!(reference, Some(ProfileRef::Policy(_))) {
            return selected;
        }
        selected.or_else(|| self.profiles.default_profile())
    }

    fn profile_for(
        &self,
        role: local::Role,
        allows: impl Fn(&profiles::Profile) -> bool,
    ) -> Option<&profiles::Profile> {
        let reference = self.local.profile(role);
        let selected = self
            .profile_or_default(reference)
            .filter(|profile| allows(profile));
        if matches!(reference, Some(ProfileRef::Policy(_))) {
            return selected;
        }
        selected.or_else(|| self.profiles.entries().find(|profile| allows(profile)))
    }

    fn local_profile_for(&self, role: local::Role) -> Option<&profiles::Profile> {
        self.profile_for(role, profiles::Profile::allows_local_roles)
    }

    fn transport_for(&self, profile: &profiles::Profile) -> Result<local_ai::Profile, String> {
        if let Some(root) = &self.origin {
            let transport = profile.transport_in(
                self.general.timeout_minutes,
                root,
                self.model_catalog.as_deref(),
            )?;
            if profile.is_cli() {
                Ok(transport)
            } else {
                Ok(transport.with_usage_account(self.profiles.usage_account(
                    profile,
                    root,
                    profile.usage_budget.clone(),
                )?))
            }
        } else {
            profile.transport(self.general.timeout_minutes)
        }
    }

    pub(crate) fn drawing_profile(&self, code: bool) -> Option<ManagedProfile> {
        let role = if code {
            local::Role::Coding
        } else {
            local::Role::Text
        };
        let entry = self.local_profile_for(role)?;
        Some(ManagedProfile {
            endpoint: (!entry.is_managed()).then(|| {
                entry
                    .destination()
                    .unwrap_or_else(|_| entry.endpoint.clone())
            }),
            model: entry.model.clone(),
            transport: self.transport_for(entry),
            selection_limit: entry.selection_limit as usize,
        })
    }

    pub(crate) fn journal_profile_name(&self) -> Option<&str> {
        let reference = self.local.profile(local::Role::Journal)?;
        Some(
            &self
                .bound_profile(reference)
                .filter(|profile| profile.allows_local_roles())?
                .name,
        )
    }

    pub(crate) fn journal_note_translation_profile(
        &self,
    ) -> Option<Result<JournalNoteProfile, String>> {
        let reference = self.local.profile(local::Role::Text);
        let profile = match self.profile_or_default(reference) {
            Some(profile) => profile,
            None if reference.is_some() => {
                return Some(Err(
                    "The selected Text and translation profile is unavailable.".into(),
                ));
            }
            None => return None,
        };
        Some((|| {
            let (transport, selection_limit) = if profile.is_cli() {
                self.reviewed_cli_transport(profile.id)?
            } else {
                self.terminal_chat_transport(profile.id)?
            };
            if transport.is_remote() && self.saved_root().is_none() {
                return Err("Save the reviewed HTTPS profile before using it here.".into());
            }
            Ok(JournalNoteProfile {
                id: profile.id,
                transport,
                selection_limit,
                destination: format!(
                    "{} · {} · {}",
                    profile.name,
                    profile.model,
                    profile.destination()?
                ),
            })
        })())
    }

    pub(crate) fn journal_embedding_profile(&self) -> Option<(local_ai::Profile, String)> {
        self.local.journal_embedding(self.saved_root())
    }

    pub(crate) fn journal_summary_default_id(&self) -> Option<ProfileId> {
        let reference = self.local.profile(local::Role::Journal)?;
        self.bound_profile(reference)
            .filter(|profile| profile.allows_local_roles())
            .map(|profile| profile.id)
    }

    pub(crate) fn journal_summary_transport(
        &self,
        id: ProfileId,
        vision: bool,
    ) -> Result<local_ai::Profile, String> {
        if let Some(profile) = self.profiles.profile(id) {
            require_journal_vision(profile, vision)?;
        }
        self.selected_transport(id, ProfileRoute::Local)
            .map(|(profile, _)| profile)
    }

    pub(crate) fn journal_profile(
        &self,
        vision: bool,
    ) -> Option<Result<local_ai::Profile, String>> {
        let reference = self.local.profile(local::Role::Journal)?;
        if self
            .bound_profile(reference)
            .is_some_and(profiles::Profile::is_cli)
        {
            return None;
        }
        Some(
            self.bound_profile(reference)
                .ok_or_else(|| "The selected journal AI profile was removed.".into())
                .and_then(|profile| {
                    if !profile.allows_local_roles() {
                        return Err("Journal analysis requires a local AI profile.".into());
                    }
                    require_journal_vision(profile, vision)?;
                    self.transport_for(profile)
                }),
        )
    }

    #[cfg(test)]
    pub(crate) fn skill_guidance(&self, context: &str) -> String {
        self.skills.prompt_addendum(context, skills::Target::Chat)
    }

    pub(crate) fn chat_skill_selection(
        &self,
        context: &str,
        assigned: &[u64],
    ) -> SkillSelection<'_> {
        self.skills
            .selection(context, skills::Target::Chat, assigned)
    }

    pub(crate) fn agent_skill_selection(
        &self,
        context: &str,
        assigned: &[u64],
    ) -> SkillSelection<'_> {
        self.skills
            .selection(context, skills::Target::Agent, assigned)
    }

    pub(crate) fn reviewed_agent_pinned_skill_guidance(&self, assigned: &[u64]) -> SkillGuidance {
        self.skills
            .reviewed_pinned_guidance(skills::Target::Agent, assigned)
    }

    pub(crate) fn connection_skill_choices(&self) -> Vec<(u64, String)> {
        self.skills.choices()
    }

    pub(crate) fn snippet_skill_choices(&self) -> Vec<(u64, String)> {
        self.skills.snippet_choices()
    }

    pub(crate) fn reviewed_snippet_skill_guidance(&self, selected: &[u64]) -> SkillGuidance {
        self.skills.reviewed_snippet_prompt(selected)
    }

    fn skill_statistics_allowed(&self, policy: &ClipboardPolicy) -> bool {
        self.enabled()
            && policy.problem.is_none()
            && policy.ai_allowed
            && self.skills.statistics_enabled(policy.ai_memory.statistics)
    }

    fn skill_statistics_tokenizer(&self) -> local_ai::Tokenizer {
        self.terminal_chat_profile_id()
            .and_then(|id| self.profiles.profile(id))
            .map_or_else(local_ai::Tokenizer::default, |profile| profile.tokenizer)
    }

    pub(crate) fn skill_usage(
        &self,
        guidance: &SkillGuidance,
        transport: &local_ai::Profile,
        surface: SkillUsageSurface,
        private: bool,
    ) -> Option<SkillUsageReceipt> {
        if private || guidance.text.is_empty() {
            return None;
        }
        let root = self.saved_root()?;
        if !self.skill_statistics_allowed(&ClipboardPolicy::load()) {
            return None;
        }
        SkillUsageReceipt::new(root, &self.skills, guidance, transport.tokenizer(), surface)
    }

    pub(crate) fn suggest_snippet_skills(&self, language: &str, source: &str) -> Vec<u64> {
        self.skills.suggest_snippet_skills(language, source)
    }

    pub(crate) fn skills_changed(&self, other: &Self) -> bool {
        self.skills != other.skills
    }

    pub(crate) fn enabled(&self) -> bool {
        self.general.features_enabled
    }

    pub(crate) fn chat_terminal_actions(&self) -> ChatTerminalActions {
        self.general.chat_terminal_actions
    }

    pub(crate) fn agent_shortcut_aliases(&self) -> Option<shortcut::Aliases> {
        self.general.agent_shortcut.enabled_aliases()
    }

    pub(crate) fn show_agent_setup(&self) -> bool {
        self.general.show_agent_setup
    }

    pub(crate) fn memory_enabled(&self, policy: &AiMemoryPolicy) -> bool {
        self.enabled() && policy.user_files_allowed() && memory::prompt::enabled(&self.memory)
    }

    pub(crate) fn rule_budget(&self, policy: &AiMemoryPolicy) -> Option<u16> {
        (self.memory_enabled(policy)
            && policy.user_memory == crate::clipboard_policy::UserMemoryMode::Editable)
            .then(|| self.memory.budget_with_limit(policy.max_tokens))
    }

    pub(crate) fn automatic_rule_learning(&self, policy: &AiMemoryPolicy) -> bool {
        self.memory.effective_learning(policy) == memory::Learning::Propose
            && (self.rule_budget(policy).is_some() || self.skill_rule_enabled(policy))
    }

    pub(crate) fn skill_rule_enabled(&self, policy: &AiMemoryPolicy) -> bool {
        self.memory_enabled(policy)
            && policy
                .skill_creation
                .unwrap_or_else(|| self.skills.learned_rules_enabled())
    }

    pub(crate) fn skill_rule_targets(
        &self,
        policy: &AiMemoryPolicy,
        input: &str,
        origin: &RuleOrigin,
    ) -> Option<SkillRuleOptions> {
        self.skill_rule_enabled(policy).then(|| {
            self.skills
                .rule_targets(input, origin, policy.skill_creation.unwrap_or(true))
        })
    }

    pub(crate) fn review_skill_rule(
        &self,
        policy: &AiMemoryPolicy,
        target: &SkillRuleTarget,
        input: &str,
        origin: &RuleOrigin,
    ) -> Result<SkillRuleProposal, String> {
        self.require_skill_learning(policy)?;
        self.skills
            .review_rule(target, input, origin, policy.skill_creation.unwrap_or(true))
    }

    pub(crate) fn save_skill_rule(
        &self,
        root: &Path,
        policy: &AiMemoryPolicy,
        proposal: &SkillRuleProposal,
    ) -> Result<(Arc<Self>, SavedSkillRule), String> {
        self.require_skill_learning(policy)?;
        let mut next = self.clone();
        let rule = proposal.apply(&mut next.skills, policy.skill_creation.unwrap_or(true))?;
        next.save_against(root, self)
            .map_err(|error| error.to_string())?;
        skills::usage::observe_sizes(root);
        Ok((Arc::new(next), rule))
    }

    pub(crate) fn undo_skill_rule(
        &self,
        root: &Path,
        policy: &AiMemoryPolicy,
        rule: &SavedSkillRule,
    ) -> Result<Arc<Self>, String> {
        self.require_skill_learning(policy)?;
        let mut next = self.clone();
        rule.undo(&mut next.skills)?;
        next.save_against(root, self)
            .map_err(|error| error.to_string())?;
        skills::usage::observe_sizes(root);
        Ok(Arc::new(next))
    }

    fn require_skill_learning(&self, policy: &AiMemoryPolicy) -> Result<(), String> {
        self.skill_rule_enabled(policy)
            .then_some(())
            .ok_or_else(|| {
                "Learning rules in AI Skills is disabled under the current settings.".into()
            })
    }

    pub(crate) fn organization_memory_enabled(&self, policy: &AiMemoryPolicy) -> bool {
        self.enabled() && policy.feature_allowed && !policy.organization().is_empty()
    }

    pub(crate) fn load_memory(
        &self,
        profile_root: Option<&Path>,
        policy: &AiMemoryPolicy,
        connection: Option<u64>,
        include_external: bool,
        mask: impl FnMut(&str) -> Result<String, String>,
    ) -> Result<Option<MemoryPrompt>, String> {
        if !self.enabled() {
            return Ok(None);
        }
        memory::prompt::load(
            profile_root,
            &self.memory,
            policy,
            connection,
            include_external,
            mask,
        )
    }

    pub(crate) fn code_text_language(&self) -> CodeTextLanguage {
        self.general.code_text_language
    }

    pub(crate) fn code_analysis_profile_id(&self) -> Option<ProfileId> {
        Some(self.local_profile_for(local::Role::Coding)?.id)
    }

    fn agent_profile_binding(&self) -> Option<&ProfileRef> {
        [local::Role::Agent, local::Role::Coding]
            .into_iter()
            .filter_map(|role| self.local.profile(role))
            .find(|reference| {
                matches!(reference, ProfileRef::Policy(_))
                    || self.bound_profile(reference).is_some()
            })
    }

    pub(crate) fn has_named_agent_profile(&self) -> bool {
        matches!(self.agent_profile_binding(), Some(ProfileRef::Policy(_)))
    }

    pub(crate) fn agent_profile_id(&self) -> Option<ProfileId> {
        self.profile_or_default(self.agent_profile_binding())
            .filter(|profile| profile.allows_local_roles())
            .map(|profile| profile.id)
    }

    pub(crate) fn preferred_agent_cli_id(&self) -> Option<ProfileId> {
        self.reviewed_cli_preference(self.profile_or_default(self.agent_profile_binding()))
    }

    pub(crate) fn agent_profiles(&self) -> impl Iterator<Item = (ProfileId, &str, &str, String)> {
        self.available_ai_profiles(ProfileRoute::Local)
    }

    pub(crate) fn reviewed_agent_https_profiles(
        &self,
    ) -> impl Iterator<Item = (ProfileId, &str, &str, String)> {
        self.available_ai_profiles(ProfileRoute::ReviewedHttps)
            .filter(|(id, _, _, _)| {
                self.saved_root().is_some()
                    && self
                        .profiles
                        .profile(*id)
                        .is_some_and(profiles::Profile::allows_reviewed_agent_https)
            })
    }

    pub(crate) fn reviewed_agent_https_transport(
        &self,
        reference: &ProfileRef,
        destination: &str,
    ) -> Result<(local_ai::Profile, usize), String> {
        let changed = || "The reviewed HTTPS agent profile changed. Start a new review.".to_owned();
        let id = self
            .resolve_profile_reference(reference)
            .ok_or_else(changed)?;
        if self.saved_root().is_none()
            || !matches!(reference, ProfileRef::Numeric(_))
            || !self.profiles.profile(id).is_some_and(|profile| {
                profile.allows_reviewed_agent_https()
                    && profile
                        .destination()
                        .is_ok_and(|value| value == destination)
            })
        {
            return Err(changed());
        }
        self.selected_transport(id, ProfileRoute::ReviewedHttps)
    }

    pub(crate) fn reviewed_agent_cli_profiles(
        &self,
    ) -> impl Iterator<Item = (ProfileId, &str, &str, String)> {
        self.available_ai_profiles(ProfileRoute::ReviewedCli)
            .filter(|_| self.saved_root().is_some() && self.enabled())
            .map(|(id, name, model, destination)| {
                (id, name, local_ai::cli::display_model(model), destination)
            })
    }

    pub(crate) fn reviewed_agent_cli_transport(
        &self,
        reference: &ProfileRef,
        destination: &str,
    ) -> Result<(local_ai::Profile, usize), String> {
        let changed = || "The reviewed CLI agent profile changed. Start a new review.".to_owned();
        self.reviewed_cli_destination(reference, destination)
            .ok_or_else(changed)
    }

    fn reviewed_cli_destination(
        &self,
        reference: &ProfileRef,
        destination: &str,
    ) -> Option<(local_ai::Profile, usize)> {
        let id = self.resolve_profile_reference(reference)?;
        if !matches!(reference, ProfileRef::Numeric(_))
            || !self.profiles.profile(id).is_some_and(|profile| {
                profile
                    .destination()
                    .is_ok_and(|value| value == destination)
            })
        {
            return None;
        }
        self.reviewed_cli_transport(id).ok()
    }

    pub(crate) fn reviewed_agent_cli_revision(&self, reference: &ProfileRef) -> Option<[u8; 32]> {
        if !matches!(reference, ProfileRef::Numeric(_)) {
            return None;
        }
        let id = self.resolve_profile_reference(reference)?;
        self.reviewed_cli_revision(id, "kortty reviewed agent CLI profile v1")
    }

    fn reviewed_cli_transport(&self, id: ProfileId) -> Result<(local_ai::Profile, usize), String> {
        self.saved_root()
            .ok_or("Save the reviewed CLI profile before using it here.")?;
        self.selected_transport(id, ProfileRoute::ReviewedCli)
    }

    fn reviewed_cli_revision(&self, id: ProfileId, domain: &str) -> Option<[u8; 32]> {
        self.reviewed_cli_transport(id).ok()?;
        let revision = self.agent_profile_revision(id)?;
        let bytes = serde_json::to_vec(&(domain, revision, self.general.timeout_minutes)).ok()?;
        hash_agent_profile_revision(&bytes)
    }

    fn reviewed_cli_preference(&self, candidate: Option<&profiles::Profile>) -> Option<ProfileId> {
        let profile = candidate?;
        self.reviewed_cli_transport(profile.id).ok()?;
        Some(profile.id)
    }

    pub(crate) fn reviewed_local_ai_profiles(
        &self,
    ) -> impl Iterator<Item = ReviewedLocalAiProfile<'_>> {
        self.available_profile_transports(ProfileRoute::Local)
            .filter_map(|(profile, transport)| {
                Some(ReviewedLocalAiProfile {
                    id: profile.id,
                    name: &profile.name,
                    model: &profile.model,
                    destination: profile.destination().ok()?,
                    policy_model_fingerprint: transport.policy_authority_fingerprint(),
                })
            })
    }

    pub(crate) fn local_ai_profile_matches(
        &self,
        id: ProfileId,
        model: &str,
        destination: &str,
    ) -> bool {
        self.profiles.profile(id).is_some_and(|profile| {
            ProfileRoute::Local.allows(profile)
                && profile.model == model
                && profile
                    .destination()
                    .is_ok_and(|value| value == destination)
        })
    }

    pub(crate) fn agent_transport(
        &self,
        id: ProfileId,
    ) -> Result<(local_ai::Profile, usize), String> {
        self.selected_transport(id, ProfileRoute::Local)
    }

    pub(crate) fn agent_transport_for_reference(
        &self,
        reference: &ProfileRef,
    ) -> Result<(local_ai::Profile, usize), String> {
        let id = self
            .resolve_profile_reference(reference)
            .ok_or("The selected AI profile is unavailable.")?;
        let (transport, limit) = self.agent_transport(id)?;
        if let ProfileRef::Policy(name) = reference
            && transport.policy_profile_id() != Some(name.as_str())
        {
            return Err("The selected policy AI profile changed. Review the request again.".into());
        }
        Ok((transport, limit))
    }

    pub(crate) fn agent_profile_revision(&self, id: ProfileId) -> Option<[u8; 32]> {
        let base = self.agent_profile_revision_with(id, |profile| {
            self.transport_for(profile)
                .ok()?
                .policy_authority_fingerprint()
        })?;
        let profile = self.profiles.profile(id)?;
        bind_prompt_preset_revision(
            base,
            profile.resolved_prompt_preset(self.model_catalog.as_deref(), self.saved_root()),
        )
    }

    pub(crate) fn agent_profile_reference_revision(
        &self,
        reference: &ProfileRef,
    ) -> Option<[u8; 32]> {
        match reference {
            ProfileRef::Numeric(_) => {
                self.agent_profile_revision(self.resolve_profile_reference(reference)?)
            }
            ProfileRef::Policy(name) => {
                let profile = self.bound_profile(reference)?;
                let (transport, _) = self.agent_transport_for_reference(reference).ok()?;
                let authority = transport.policy_authority_fingerprint()?;
                let base = named_agent_profile_revision(name, profile, authority)?;
                bind_prompt_preset_revision(base, transport.prompt_preset())
            }
        }
    }

    fn agent_profile_revision_with(
        &self,
        id: ProfileId,
        policy_fingerprint: impl FnOnce(&profiles::Profile) -> Option<[u8; 32]>,
    ) -> Option<[u8; 32]> {
        // Profile settings include the credential variable name only.
        let profile = self.profiles.profile(id)?;
        let legacy_profile;
        let authority = if let Some(definition) = profile.policy_definition() {
            let authority = policy_fingerprint(profile)?;
            let Some(alias) = self.profiles.legacy_policy_alias(id) else {
                return named_agent_profile_revision(&definition.id, profile, authority);
            };
            legacy_profile = {
                let mut copy = profile.clone();
                copy.id = ProfileId::from(alias);
                copy
            };
            Some((&legacy_profile, authority))
        } else if is_policy_model_profile(profile) {
            Some((profile, policy_fingerprint(profile)?))
        } else {
            None
        };
        let mut bytes =
            serde_json::to_vec(authority.map_or(profile, |(serialized, _)| serialized)).ok()?;
        if let Some((_, fingerprint)) = authority {
            bytes.extend_from_slice(b"\0kortty agent policy model v1\0");
            bytes.extend_from_slice(&fingerprint);
        }
        hash_agent_profile_revision(&bytes)
    }

    pub(crate) fn terminal_chat_profile_id(&self) -> Option<ProfileId> {
        self.profile_for(local::Role::Text, |profile| !profile.is_cli())
            .map(|profile| profile.id)
    }

    pub(crate) fn preferred_terminal_chat_cli_id(&self) -> Option<ProfileId> {
        self.reviewed_cli_preference(self.profile_or_default(self.local.profile(local::Role::Text)))
    }

    pub(crate) fn preferred_manual_profile_id(&self) -> Option<ProfileId> {
        let selected = self.profile_or_default(self.local.profile(local::Role::Text));
        match selected {
            Some(profile) if profile.is_cli() => self.reviewed_cli_preference(Some(profile)),
            Some(profile) => self
                .terminal_chat_transport(profile.id)
                .ok()
                .map(|_| profile.id),
            None => self.terminal_chat_profile_id(),
        }
    }

    pub(crate) fn terminal_chat_policy_selection(&self) -> Option<&ProfileRef> {
        self.local
            .profile(local::Role::Text)
            .filter(|reference| matches!(reference, ProfileRef::Policy(_)))
    }

    pub(crate) fn is_terminal_cli_profile(&self, id: ProfileId) -> bool {
        self.profiles
            .profile(id)
            .is_some_and(profiles::Profile::is_cli)
    }

    pub(crate) fn snippet_analysis_profiles(
        &self,
    ) -> impl Iterator<Item = (ProfileId, &str, &str, String, bool)> {
        self.available_ai_profiles(ProfileRoute::ReviewedHttp).map(
            |(id, name, model, destination)| {
                let remote = self
                    .profiles
                    .profile(id)
                    .is_some_and(|profile| profile.remote_protocol().is_some());
                (id, name, model, destination, remote)
            },
        )
    }

    pub(crate) fn snippet_analysis_cli_profiles(
        &self,
    ) -> impl Iterator<Item = (ProfileId, &str, &str, String)> {
        self.reviewed_agent_cli_profiles()
    }

    pub(crate) fn snippet_analysis_cli_transport(
        &self,
        reference: &ProfileRef,
        destination: &str,
    ) -> Result<(local_ai::Profile, usize), String> {
        self.reviewed_cli_destination(reference, destination)
            .ok_or_else(|| {
                "The reviewed snippet CLI profile changed. Review the request again.".into()
            })
    }

    pub(crate) fn snippet_analysis_cli_revision(&self, reference: &ProfileRef) -> Option<[u8; 32]> {
        let id = self.resolve_profile_reference(reference)?;
        self.reviewed_cli_revision(id, "kortty snippet analysis CLI profile v1")
    }

    pub(crate) fn preferred_snippet_analysis_cli_id(&self) -> Option<ProfileId> {
        self.reviewed_cli_preference(
            self.profile_or_default(self.local.profile(local::Role::Coding)),
        )
    }

    pub(crate) fn preferred_snippet_assistance_profile_id(&self, text: bool) -> Option<ProfileId> {
        let role = if text {
            local::Role::Text
        } else {
            local::Role::Coding
        };
        self.profile_or_default(self.local.profile(role))
            .filter(|profile| {
                if profile.is_cli() {
                    self.reviewed_cli_transport(profile.id).is_ok()
                } else {
                    self.snippet_analysis_transport(profile.id).is_ok()
                }
            })
            .map(|profile| profile.id)
            .or_else(|| {
                if text {
                    self.terminal_chat_profile_id()
                } else {
                    self.code_analysis_profile_id()
                }
            })
    }

    pub(crate) fn terminal_chat_profiles(
        &self,
    ) -> impl Iterator<Item = (ProfileId, &str, &str, String)> {
        self.available_ai_profiles(ProfileRoute::TerminalChat)
    }

    pub(crate) fn journal_question_https_profiles(
        &self,
    ) -> impl Iterator<Item = (ProfileId, &str, &str, String)> {
        self.available_ai_profiles(ProfileRoute::ReviewedHttps)
    }

    pub(crate) fn journal_question_cli_profiles(
        &self,
    ) -> impl Iterator<Item = (ProfileId, &str, &str, String)> {
        self.reviewed_agent_cli_profiles()
    }

    pub(crate) fn journal_question_cli_transport(
        &self,
        id: ProfileId,
    ) -> Result<(local_ai::Profile, usize), String> {
        self.reviewed_cli_transport(id)
    }

    pub(crate) fn journal_question_cli_revision(&self, id: ProfileId) -> Result<[u8; 32], String> {
        self.reviewed_cli_revision(id, "kortty journal question CLI profile v1")
            .ok_or_else(|| "Choose a saved, reviewed CLI profile for journal questions.".into())
    }

    pub(crate) fn preferred_journal_question_cli_id(&self) -> Option<ProfileId> {
        self.reviewed_cli_preference(
            self.profile_or_default(self.local.profile(local::Role::Journal)),
        )
    }

    fn available_ai_profiles(
        &self,
        route: ProfileRoute,
    ) -> impl Iterator<Item = (ProfileId, &str, &str, String)> {
        self.available_profile_transports(route)
            .filter_map(|(entry, _)| {
                Some((
                    entry.id,
                    entry.name.as_str(),
                    entry.model.as_str(),
                    entry.destination().ok()?,
                ))
            })
    }

    fn available_profile_transports(
        &self,
        route: ProfileRoute,
    ) -> impl Iterator<Item = (&profiles::Profile, local_ai::Profile)> {
        self.profiles
            .entries()
            .filter(move |entry| route.allows(entry))
            .filter_map(|entry| Some((entry, self.transport_for(entry).ok()?)))
    }

    pub(crate) fn code_analysis_transport(
        &self,
        id: ProfileId,
    ) -> Result<(local_ai::Profile, usize), String> {
        self.selected_transport(id, ProfileRoute::Local)
    }

    pub(crate) fn snippet_analysis_transport(
        &self,
        id: ProfileId,
    ) -> Result<(local_ai::Profile, usize), String> {
        self.selected_transport(id, ProfileRoute::ReviewedHttp)
    }

    pub(crate) fn journal_question_https_transport(
        &self,
        id: ProfileId,
    ) -> Result<(local_ai::Profile, usize), String> {
        self.selected_transport(id, ProfileRoute::ReviewedHttps)
    }

    pub(crate) fn journal_summary_https_revision(&self, id: ProfileId) -> Result<[u8; 32], String> {
        let unavailable =
            || "Choose a saved, trusted HTTPS profile for journal summaries.".to_owned();
        self.saved_root().ok_or_else(unavailable)?;
        let (profile, _) = self.journal_question_https_transport(id)?;
        profile.require_current_policy_authority()?;
        let revision = self.agent_profile_revision(id).ok_or_else(unavailable)?;
        let bytes = serde_json::to_vec(&(
            "kortty journal summary HTTPS profile v1",
            revision,
            self.general.timeout_minutes,
        ))
        .map_err(|_| unavailable())?;
        hash_agent_profile_revision(&bytes).ok_or_else(unavailable)
    }

    pub(crate) fn journal_summary_cli_profiles(
        &self,
    ) -> impl Iterator<Item = (ProfileId, &str, &str, String)> {
        self.reviewed_agent_cli_profiles()
    }

    pub(crate) fn preferred_journal_summary_cli_id(&self) -> Option<ProfileId> {
        self.reviewed_cli_preference(
            self.profile_or_default(self.local.profile(local::Role::Journal)),
        )
    }

    pub(crate) fn journal_summary_cli_transport(
        &self,
        id: ProfileId,
    ) -> Result<(local_ai::Profile, usize), String> {
        self.reviewed_cli_transport(id)
    }

    pub(crate) fn journal_summary_cli_revision(&self, id: ProfileId) -> Result<[u8; 32], String> {
        self.reviewed_cli_revision(id, "kortty journal summary CLI profile v1")
            .ok_or_else(|| "Choose a saved, reviewed CLI profile for journal summaries.".into())
    }

    pub(crate) fn terminal_chat_transport(
        &self,
        id: ProfileId,
    ) -> Result<(local_ai::Profile, usize), String> {
        self.selected_transport(id, ProfileRoute::TerminalChat)
    }

    pub(crate) fn require_current_terminal_cli(
        &self,
        id: ProfileId,
        expected: &local_ai::Profile,
        selection_limit: usize,
    ) -> Result<(), String> {
        if expected.is_cli()
            && self
                .saved_root()
                .and_then(|root| Self::load_from(root).ok())
                .and_then(|saved| saved.terminal_chat_transport(id).ok())
                .is_some_and(|(current, limit)| current == *expected && limit == selection_limit)
        {
            Ok(())
        } else {
            Err("The reviewed CLI profile changed or was removed. Review the request again.".into())
        }
    }

    pub(crate) fn web_search_profile(
        &self,
        id: ProfileId,
    ) -> Option<(
        local_ai::Profile,
        String,
        Option<local_ai::web_search::McpSettings>,
    )> {
        let entry = self
            .profiles
            .profile(id)
            .filter(|entry| entry.has_web_search() && !entry.is_cli())?;
        entry.validate_connection().ok()?;
        let (profile, _) = self.terminal_chat_transport(id).ok()?;
        Some((profile, entry.destination().ok()?, entry.lm_web.clone()))
    }

    fn selected_transport(
        &self,
        id: ProfileId,
        route: ProfileRoute,
    ) -> Result<(local_ai::Profile, usize), String> {
        if !self.enabled() {
            return Err("AI features are disabled in AI Manager.".into());
        }
        let entry = self
            .profiles
            .profile(id)
            .ok_or("The selected AI profile was removed.")?;
        if !route.allows(entry) {
            return Err(if matches!(route, ProfileRoute::ReviewedHttps) {
                "Choose a saved, trusted HTTPS profile for this request.".into()
            } else if entry.is_cli() {
                "This workflow does not support a configured CLI profile.".into()
            } else {
                "This workflow requires a local AI profile.".into()
            });
        }
        Ok((self.transport_for(entry)?, entry.selection_limit as usize))
    }

    pub(crate) fn coding_knowledge(&self, profile: ProfileId, root: &Path) -> Vec<KnowledgePlan> {
        self.knowledge_for(local::Role::Coding, profile, root)
    }

    pub(crate) fn text_knowledge(&self, profile: ProfileId, root: &Path) -> Vec<KnowledgePlan> {
        self.knowledge_for(local::Role::Text, profile, root)
    }

    pub(crate) fn autonomous_knowledge(
        &self,
        profile: ProfileId,
        root: &Path,
    ) -> Vec<KnowledgePlan> {
        let Some(entry) = self.profiles.profile(profile) else {
            return Vec::new();
        };
        if !self.enabled()
            || self.saved_root().is_some_and(|saved| saved != root)
            || !(entry.allows_local_roles()
                || entry.allows_reviewed_agent_https()
                || entry.is_cli())
            || entry.validate_connection().is_err()
        {
            return Vec::new();
        }
        let Some(reference) = self.profile_reference(profile) else {
            return Vec::new();
        };
        self.local
            .autonomous_knowledge(root)
            .into_iter()
            .map(|plan| plan.with_autonomous_route(reference.clone()))
            .collect()
    }

    fn knowledge_for(
        &self,
        role: local::Role,
        profile: ProfileId,
        root: &Path,
    ) -> Vec<KnowledgePlan> {
        if self.code_analysis_transport(profile).is_err() {
            return Vec::new();
        }
        let Some(reference) = self.profile_reference(profile) else {
            return Vec::new();
        };
        self.local
            .knowledge_for(role, profile, root, &self.profiles)
            .into_iter()
            .map(|plan| plan.with_saved_route(role, reference.clone()))
            .collect()
    }
}

fn named_agent_profile_revision(
    name: &str,
    profile: &profiles::Profile,
    authority: [u8; 32],
) -> Option<[u8; 32]> {
    let mut profile = profile.clone();
    profile.id = 0;
    let bytes =
        serde_json::to_vec(&("kortty named agent profile v1", name, &profile, authority)).ok()?;
    hash_agent_profile_revision(&bytes)
}

fn hash_agent_profile_revision(bytes: &[u8]) -> Option<[u8; 32]> {
    pgp::crypto::hash::HashAlgorithm::Sha256
        .digest(bytes)
        .ok()?
        .as_slice()
        .try_into()
        .ok()
}

fn bind_prompt_preset_revision(base: [u8; 32], preset: local_ai::PromptPreset) -> Option<[u8; 32]> {
    let Some(guidance) = preset.guidance() else {
        return Some(base);
    };
    let bytes = serde_json::to_vec(&("kortty model prompt preset v1", base, guidance)).ok()?;
    hash_agent_profile_revision(&bytes)
}

pub(crate) struct ManagedProfile {
    pub(crate) endpoint: Option<String>,
    pub(crate) model: String,
    pub(crate) transport: Result<local_ai::Profile, String>,
    pub(crate) selection_limit: usize,
}

fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Page {
    #[default]
    General,
    Profiles,
    LocalModels,
    Knowledge,
    Skills,
    Memory,
    LocalPreferences,
}

impl Page {
    const ALL: [Self; 7] = [
        Self::General,
        Self::Profiles,
        Self::LocalModels,
        Self::Knowledge,
        Self::Skills,
        Self::Memory,
        Self::LocalPreferences,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Profiles => "Profiles",
            Self::LocalModels => "Local Models",
            Self::Knowledge => "Knowledge Stores",
            Self::Skills => "AI Skills",
            Self::Memory => "AI Memory",
            Self::LocalPreferences => "Local Preferences",
        }
    }

    fn keywords(self) -> &'static str {
        match self {
            Self::General => "enable features timeout agent internet policies global request",
            Self::Profiles => {
                "provider API URL key connection model GGUF llama reasoning vision limits default"
            }
            Self::LocalModels => {
                "server discovery loopback connection installed model start stop status configure idle remove GGUF Hugging Face"
            }
            Self::Knowledge => "embedding documents files search retrieval index",
            Self::Skills => "prompt instructions rules learned learning markdown import export",
            Self::Memory => "global connection history learned archive restore",
            Self::LocalPreferences => "default profiles text coding agent journal",
        }
    }

    fn step(self, next: bool) -> Option<Self> {
        let position = Self::ALL.iter().position(|&page| page == self)?;
        let target = if next {
            position + 1
        } else {
            position.checked_sub(1)?
        };
        Self::ALL.get(target).copied()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Message {
    Page(Page),
    Search(String),
    SearchStep(isize),
    SearchChoose,
    SearchEscape,
    Reveal(search::Target),
    Find,
    Help,
    Features(bool),
    Timeout(String),
    CodeTextLanguage(CodeTextLanguage),
    ChatTerminalActions(ChatTerminalActions),
    ShowAgentSetup(bool),
    AgentShortcutEnabled(bool),
    AgentShortcutName(String),
    AgentShortcutCaseInsensitive(bool),
    Profiles(profiles::Message),
    Skills(skills::Message),
    Memory(memory::Message),
    Local(local::Message),
    Setup(setup::Message),
    SetupSaved(u64, Result<Arc<Configuration>, String>),
    ManagedModelStatusTick,
    RequestDone(u64, Result<Reply, String>),
    AutoTestResolved(u64, Instant, Result<local_ai::PolicyAutoModel, String>),
    CancelRequest,
    Save,
    Saved(Result<(Arc<Configuration>, Option<String>), String>),
    ProfileDefaultSaved(Result<Arc<Configuration>, String>),
    PolicyProfilesRefreshed(Arc<Configuration>),
    RefreshPolicyAutoModels,
    ManagedDefaultSaved(Result<Option<String>, String>),
    Escape,
    Close,
    CloseAnswer(bool),
}

#[derive(Clone, Debug)]
pub(crate) enum Reply {
    Models(Vec<String>),
    Tested(String, Option<String>),
}

struct Request {
    ticket: u64,
    profile: profiles::Profile,
    test_after_discovery: bool,
    _cancel: local_ai::Cancel,
}

fn save_guided_setup(
    root: &Path,
    baseline: &Configuration,
    commit: setup::Commit,
) -> Result<Arc<Configuration>, String> {
    let mut saved = guided_setup_configuration(baseline, &commit.models, &commit.roles)?;
    let registry = local_ai::managed_models::Registry::new(root.to_owned());
    let mut opened = Vec::with_capacity(commit.models.len());
    for model in &commit.models {
        let entry = registry
            .open(&model.id)
            .map_err(|error| error.to_string())?;
        if entry.model != *model || entry.model.external.is_some() {
            return Err("A tested model changed. Review and test the models again.".into());
        }
        opened.push(entry);
    }
    saved.origin = Some(root.to_owned());
    let _runtime = local_ai::runtime_package::lock_active_package(root, &commit.runtime)
        .map_err(|error| error.to_string())?;
    let check = || {
        let policy = ClipboardPolicy::load();
        if policy.problem.is_some() || !policy.ai_allowed {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Managed policy blocks AI setup.",
            ));
        }
        policy.require_ai_user_model_files()?;
        policy.require_ai_model_downloads()?;
        for entry in &opened {
            entry.revalidate()?;
        }
        Ok(())
    };
    saved
        .save_checked(root, baseline, (None, None), ClipboardPolicy::load, check)
        .map_err(|error| error.to_string())?;
    Ok(Arc::new(saved))
}

fn guided_setup_configuration(
    baseline: &Configuration,
    models: &[local_ai::managed_models::Model],
    roles: &[(setup::Role, String)],
) -> Result<Configuration, String> {
    if !baseline.enabled() || !(1..=3).contains(&roles.len()) || !(1..=3).contains(&models.len()) {
        return Err("Choose and test at least one local AI role.".into());
    }
    let mut saved = baseline.clone();
    let mut selected = Vec::with_capacity(roles.len());
    if models
        .iter()
        .any(|model| !roles.iter().any(|(_, id)| id == &model.id))
    {
        return Err("Each tested model must be used by a selected role.".into());
    }
    for (role, id) in roles {
        if selected.contains(role) {
            return Err("Each local AI role can be assigned once.".into());
        }
        selected.push(*role);
        let mut models = models.iter().filter(|model| &model.id == id);
        let model = models
            .next()
            .filter(|model| model.purpose == role.purpose())
            .ok_or("The tested model is unavailable for this role.")?;
        if models.next().is_some() {
            return Err("The tested model assignment is ambiguous.".into());
        }
        match role {
            setup::Role::Text | setup::Role::Coding => {
                let local_role = if *role == setup::Role::Text {
                    local::Role::Text
                } else {
                    local::Role::Coding
                };
                let reference = saved.profiles.ensure_guided_model_profile(model)?;
                saved.local.set_guided_profile(local_role, reference);
            }
            setup::Role::Embedding => saved.local.set_guided_embedding(model.id.clone()),
        }
    }
    Ok(saved)
}

#[derive(Clone, Copy)]
struct ProfilePolicy {
    create: bool,
    edit: bool,
}

impl ProfilePolicy {
    fn load() -> Self {
        let policy = ClipboardPolicy::load();
        Self {
            create: policy.ai_profile_creation_allowed(),
            edit: policy.ai_profile_edit_allowed(),
        }
    }
}

pub(crate) struct Dialog {
    saved: Arc<Configuration>,
    profile_dir: PathBuf,
    general: General,
    timeout: String,
    profiles: profiles::Editor,
    profile_policy: ProfilePolicy,
    skills: skills::Editor,
    skill_store: skills::Store,
    memory: memory::State,
    local: local::Dialog,
    local_state: local::State,
    setup: Option<setup::Wizard>,
    setup_commit: Option<u64>,
    page: Page,
    search: String,
    search_id: Id,
    search_selection: usize,
    search_scroll: Id,
    search_row: Id,
    revealed: search::Reveal,
    notice: String,
    editable: bool,
    saving: bool,
    skill_rule_writing: bool,
    request: Option<Request>,
    next_request: u64,
    close_token: Option<u64>,
}

impl Dialog {
    pub(crate) fn new(
        saved: Arc<Configuration>,
        profile: PathBuf,
        scopes: Vec<Scope>,
        editable: bool,
        problem: Option<&str>,
    ) -> Self {
        Self {
            profile_dir: profile.clone(),
            general: saved.general.clone(),
            timeout: saved.general.timeout_minutes.to_string(),
            profiles: {
                let mut profiles = profiles::Editor::new(saved.profiles.clone());
                profiles.install_model_catalog(saved.model_catalog.clone());
                profiles
            },
            profile_policy: ProfilePolicy::load(),
            skills: skills::Editor::default().with_statistics_root(profile.clone()),
            skill_store: saved.skills.clone(),
            memory: memory::State::new(
                saved.memory.clone(),
                profile.join("memory"),
                scopes,
                editable,
            ),
            local: {
                let mut local = local::Dialog::new(profile);
                local.install_model_catalog(saved.model_catalog.clone());
                local
            },
            local_state: saved.local.clone(),
            setup: None,
            setup_commit: None,
            saved,
            page: Page::default(),
            search: String::new(),
            search_id: Id::unique(),
            search_selection: 0,
            search_scroll: Id::unique(),
            search_row: Id::unique(),
            revealed: search::Reveal::default(),
            notice: problem.unwrap_or_default().into(),
            editable,
            saving: false,
            skill_rule_writing: false,
            request: None,
            next_request: 0,
            close_token: None,
        }
    }

    pub(crate) fn with_memory_policy(mut self, policy: &AiMemoryPolicy, ai_allowed: bool) -> Self {
        self.memory.set_policy(policy, ai_allowed);
        self.skills
            .set_skill_creation_override(policy.skill_creation);
        self.skills.set_statistics_policy(policy.statistics);
        self
    }

    pub(crate) fn with_web_search_policy(mut self, allowed: bool) -> Self {
        self.profiles.deny_web_search(!allowed);
        self
    }

    pub(crate) fn set_runtime_downloads_allowed(&mut self, allowed: bool) {
        self.local.set_runtime_downloads_allowed(allowed);
    }

    pub(crate) fn set_model_downloads_allowed(&mut self, allowed: bool) {
        self.local.set_model_downloads_allowed(allowed);
    }

    pub(crate) fn install_model_catalog(
        &mut self,
        catalog: Option<Arc<local_ai::model_catalog::Catalog>>,
    ) {
        Arc::make_mut(&mut self.saved).install_model_catalog(catalog.clone());
        self.profiles.install_model_catalog(catalog.clone());
        if let Some(wizard) = &mut self.setup {
            wizard.install_model_catalog(catalog.clone());
        }
        self.local.install_model_catalog(catalog);
    }

    pub(crate) fn can_write_skill_rule(&self) -> bool {
        !self.saving
            && self.setup.is_none()
            && self.close_token.is_none()
            && self.skill_store == self.saved.skills
            && self.skills.can_reload_after_rule()
    }

    pub(crate) fn set_skill_rule_writing(&mut self, writing: bool) {
        self.skill_rule_writing = writing;
    }

    pub(crate) fn install_skill_rule_config(&mut self, saved: &Arc<Configuration>) {
        self.skill_store = saved.skills.clone();
        self.skills.reload_after_rule(&self.skill_store);
        self.saved = Arc::clone(saved);
    }

    pub(crate) fn dirty(&self) -> bool {
        self.general != self.saved.general
            || self.timeout != self.saved.general.timeout_minutes.to_string()
            || self.profiles.catalog() != &self.saved.profiles
            || self.profiles.draft_dirty()
            || self.skill_store != self.saved.skills
            || self.skills.pending_edit()
            || self.memory.is_dirty()
            || self.local_state != self.saved.local
    }

    pub(crate) fn busy(&self) -> bool {
        self.saving
            || self.skills.busy()
            || self.memory.busy()
            || self.local.backend_repair_busy()
            || self.setup.as_ref().is_some_and(setup::Wizard::busy)
    }

    pub(crate) fn automatic_knowledge_idle(&self) -> bool {
        self.editable()
            && !self.dirty()
            && !self.busy()
            && !self.profile_wizard_open()
            && self.setup.is_none()
            && self.request.is_none()
            && self.local.automatic_sync_idle()
    }

    pub(crate) fn set_automatic_knowledge_status(
        &mut self,
        statuses: std::collections::BTreeMap<String, String>,
    ) {
        self.local.set_automatic_status(statuses);
    }

    pub(crate) fn set_runtime_update_status(&mut self, status: Option<String>) {
        self.local.set_runtime_update_status(status);
    }

    pub(crate) fn merge_automatic_knowledge(
        &mut self,
        baseline: &Configuration,
        saved: &Arc<Configuration>,
    ) {
        if self.saved.as_ref() == baseline {
            self.local_state
                .merge_automatic_knowledge(&baseline.local, &saved.local);
            self.local.merge_automatic_knowledge(&self.local_state);
            self.saved = Arc::clone(saved);
        }
    }

    pub(crate) fn prompt_close(&mut self, token: Option<u64>) {
        self.close_token = token;
    }

    pub(crate) fn close_token(&self) -> Option<u64> {
        self.close_token
    }

    pub(crate) fn profile_wizard_open(&self) -> bool {
        self.profiles.wizard_open()
    }

    pub(crate) fn guided_setup_open(&self) -> bool {
        self.setup.is_some()
    }

    pub(crate) fn child_escape_message(&self) -> Option<Message> {
        if self.close_token.is_some() || !self.search.trim().is_empty() {
            return None;
        }
        if self.setup.is_some() {
            return Some(Message::Setup(setup::Message::Cancel));
        }
        match self.page {
            Page::Profiles if self.profile_wizard_open() => Some(Message::Profiles(
                profiles::Message::Wizard(profiles::WizardAction::Cancel),
            )),
            Page::Skills if self.skills.file_prompt_open() => {
                Some(Message::Skills(skills::Message::Cancel))
            }
            Page::LocalModels if self.local.backend_repair_open() => {
                Some(Message::Local(local::Message::CloseBackendRepair))
            }
            Page::LocalModels if self.local.runtime_removal_review_open() => {
                Some(Message::Local(local::Message::CancelRuntimeRemoval))
            }
            _ => None,
        }
    }

    pub(crate) fn child_close_message(&self) -> Option<Message> {
        if self.setup.is_some()
            || self.profile_wizard_open()
            || self.local.backend_repair_open()
            || (self.page == Page::LocalModels && self.local.runtime_removal_review_open())
        {
            self.child_escape_message()
        } else {
            None
        }
    }

    pub(crate) fn escape_message(&self) -> Message {
        if self.close_token.is_some() {
            return Message::Close;
        }
        if !self.search.is_empty() {
            return Message::SearchEscape;
        }
        if let Some(action) = self.child_escape_message() {
            return action;
        }
        match self.page {
            Page::Memory if self.memory.cleanup_open() => Message::Memory(memory::Message::Cancel),
            Page::Skills if self.editable() && self.skills.find_open() => {
                Message::Skills(skills::Message::Find(document_find::Message::Close))
            }
            Page::Memory if self.memory.find_open() => {
                Message::Memory(memory::Message::Find(document_find::Message::Close))
            }
            _ => Message::Close,
        }
    }

    #[cfg(test)]
    pub(crate) fn open_profile_wizard_for_test(&mut self) {
        let _ = self.update(Message::Page(Page::Profiles));
        let _ = self.update(Message::Profiles(profiles::Message::Wizard(
            profiles::WizardAction::Open,
        )));
    }

    #[cfg(test)]
    pub(crate) fn open_skill_find_for_test(&mut self) -> Task<Message> {
        let _ = self.update(Message::Page(Page::Skills));
        let _ = self.update(Message::Skills(skills::Message::Add));
        self.update(Message::Skills(skills::Message::Find(
            document_find::Message::Open,
        )))
    }

    #[cfg(test)]
    pub(crate) fn open_backend_repair_for_test(&mut self) -> Arc<std::sync::atomic::AtomicBool> {
        self.page = Page::LocalModels;
        self.local.open_backend_repair_for_test()
    }

    pub(crate) fn shows_managed_model_status(&self) -> bool {
        self.editable()
            && self.page == Page::LocalModels
            && self.search.trim().is_empty()
            && self.local.has_managed_model_selection()
    }

    fn editable(&self) -> bool {
        self.editable
            && !self.saving
            && !self.skill_rule_writing
            && self.close_token.is_none()
            && !self
                .request
                .as_ref()
                .is_some_and(|request| request.profile.is_cli())
    }

    fn can_open_profile_wizard(&self) -> bool {
        self.editable()
            && self.profile_policy.create
            && self.page == Page::Profiles
            && self.search.trim().is_empty()
            && self.request.is_none()
    }

    fn can_open_guided_setup(&self, role: Option<setup::Role>) -> bool {
        self.editable()
            && self.setup.is_none()
            && self.search.trim().is_empty()
            && (self.page == Page::LocalModels
                || (self.page == Page::LocalPreferences && role == Some(setup::Role::Embedding)))
            && self.general.features_enabled
            && !self.dirty()
            && !self.busy()
            && self.local.automatic_sync_idle()
            && self.request.is_none()
    }

    fn update_guided_setup(
        &mut self,
        message: setup::Message,
        vault: Option<&VaultWindow>,
    ) -> Task<Message> {
        let policy = ClipboardPolicy::load();
        if let setup::Message::Open(role) = message {
            if self.can_open_guided_setup(role) && policy.problem.is_none() && policy.ai_allowed {
                self.setup = Some(setup::Wizard::new(role, self.saved.model_catalog.clone()));
                self.notice.clear();
            }
            return Task::none();
        }
        let editable = self.editable();
        let Some(wizard) = self.setup.as_mut() else {
            return Task::none();
        };
        let scope = setup::Scope {
            profile: &self.profile_dir,
            ai_allowed: editable
                && self.general.features_enabled
                && policy.problem.is_none()
                && policy.ai_allowed
                && policy.ai_user_model_files_allowed(),
            runtime_downloads_allowed: policy.ai_runtime_downloads_allowed(),
            model_downloads_allowed: policy.ai_model_downloads_allowed(),
            vault,
        };
        match wizard.update(message, scope) {
            setup::Outcome::Stay(task) => task.map(Message::Setup),
            setup::Outcome::Close => {
                self.setup = None;
                Task::batch([
                    self.refresh_managed_models(),
                    self.local.open_managed_models().map(Message::Local),
                ])
            }
            setup::Outcome::Commit(commit) => {
                let root = self.profile_dir.clone();
                let baseline = Arc::clone(&self.saved);
                let ticket = self.next_request;
                self.next_request = self.next_request.wrapping_add(1);
                self.setup_commit = Some(ticket);
                self.saving = true;
                let (send, reply) = async_channel::bounded(1);
                let worker = std::thread::Builder::new()
                    .name("kortty-local-ai-setup-save".into())
                    .spawn(move || {
                        let _ = send.try_send(save_guided_setup(&root, &baseline, commit));
                    });
                if let Err(error) = worker {
                    return Task::done(Message::SetupSaved(ticket, Err(error.to_string())));
                }
                Task::perform(
                    async move {
                        reply
                            .recv()
                            .await
                            .unwrap_or_else(|_| Err("The setup save worker stopped.".into()))
                    },
                    move |result| Message::SetupSaved(ticket, result),
                )
            }
        }
    }

    fn profile_form_lock(&self) -> Option<&'static str> {
        let profile = self.profiles.selected()?;
        if self.saved.profiles.profile(profile.id).is_some() {
            (!self.profile_policy.edit).then_some(AI_PROFILE_EDIT_DISABLED)
        } else {
            (!self.profile_policy.create).then_some(AI_PROFILE_CREATION_DISABLED)
        }
    }

    #[cfg(test)]
    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        self.update_with_vault(message, None)
    }

    pub(crate) fn update_with_vault(
        &mut self,
        message: Message,
        vault: Option<&VaultWindow>,
    ) -> Task<Message> {
        self.update_with_profile_policy(message, vault, ProfilePolicy::load)
    }

    fn update_with_profile_policy(
        &mut self,
        message: Message,
        vault: Option<&VaultWindow>,
        load_policy: impl FnOnce() -> ProfilePolicy,
    ) -> Task<Message> {
        if matches!(
            &message,
            Message::Page(Page::Profiles)
                | Message::Profiles(_)
                | Message::RequestDone(..)
                | Message::AutoTestResolved(..)
        ) {
            self.profile_policy = load_policy();
        }
        let editable = self.editable();
        let profile_editable = editable && self.profile_form_lock().is_none();
        match message {
            Message::Setup(event) => return self.update_guided_setup(event, vault),
            Message::SetupSaved(ticket, result) if self.setup_commit == Some(ticket) => {
                match result {
                    Ok(saved) => return Task::done(Message::Saved(Ok((saved, None)))),
                    Err(error) => {
                        self.setup_commit = None;
                        self.saving = false;
                        if let Some(wizard) = &mut self.setup {
                            wizard.commit_finished(Err(error));
                        }
                    }
                }
                return Task::none();
            }
            _ if self.setup.is_some() && !matches!(message, Message::Saved(_)) => {
                return Task::none();
            }
            _ => {}
        }
        if (self.profile_wizard_open() || self.local.backend_repair_open())
            && matches!(
                message,
                Message::Page(_)
                    | Message::Search(_)
                    | Message::SearchStep(_)
                    | Message::SearchChoose
                    | Message::SearchEscape
                    | Message::Reveal(_)
                    | Message::Find
                    | Message::Save
                    | Message::Profiles(profiles::Message::Save | profiles::Message::Refresh)
            )
        {
            return Task::none();
        }
        match message {
            Message::Page(page) => return self.open_page(search::Target::Page(page)),
            Message::ManagedModelStatusTick if self.shows_managed_model_status() => {
                self.local.refresh_managed_model_status();
            }
            Message::Search(value) => {
                let was_searching = !self.search.trim().is_empty();
                self.search = value.chars().take(128).collect();
                self.search_selection = 0;
                self.revealed.setting = None;
                let scroll = iced::widget::operation::snap_to(
                    self.search_scroll.clone(),
                    iced::widget::operation::RelativeOffset::START,
                );
                return if was_searching && self.search.trim().is_empty() {
                    Task::batch([scroll, self.load_profile_usage()])
                } else {
                    scroll
                };
            }
            Message::SearchStep(direction) => {
                let last = self.search_results().count().saturating_sub(1);
                self.search_selection = self
                    .search_selection
                    .saturating_add_signed(direction)
                    .min(last);
                return crate::settings_search::scroll_to_row(
                    self.search_scroll.clone(),
                    self.search_row.clone(),
                );
            }
            Message::SearchChoose => {
                let target = self.search_results().nth(self.search_selection);
                return target.map_or_else(Task::none, |target| self.open_page(target));
            }
            Message::SearchEscape => {
                self.search.clear();
                self.search_selection = 0;
                return self.load_profile_usage();
            }
            Message::Reveal(target) => return self.open_page(target),
            Message::Find => return iced::widget::operation::focus(self.search_id.clone()),
            Message::Features(value) if editable => {
                self.general.features_enabled = value;
                if !value {
                    self.profiles.cancel_cli_discovery();
                    self.profiles.cancel_cli_catalog();
                    self.request = None;
                    self.local.suspend();
                    self.memory.invalidate_cleanup();
                }
            }
            Message::Timeout(value) if editable => {
                self.timeout = value.chars().filter(char::is_ascii_digit).take(4).collect();
            }
            Message::CodeTextLanguage(language) if editable => {
                self.general.code_text_language = language;
            }
            Message::ChatTerminalActions(actions) if editable => {
                self.general.chat_terminal_actions = actions;
            }
            Message::ShowAgentSetup(show) if editable => {
                self.general.show_agent_setup = show;
            }
            Message::AgentShortcutEnabled(enabled) if editable => {
                self.general.agent_shortcut.enabled = enabled;
            }
            Message::AgentShortcutName(mut name) if editable => {
                name.truncate(name.floor_char_boundary(shortcut::MAX_ALIAS_BYTES));
                self.general.agent_shortcut.name = name;
            }
            Message::AgentShortcutCaseInsensitive(enabled) if editable => {
                self.general.agent_shortcut.case_insensitive = enabled;
            }
            Message::Profiles(profiles::Message::Test) if profile_editable => {
                return self.request(false);
            }
            Message::Profiles(profiles::Message::Cli(action))
                if profile_editable || action.completed() =>
            {
                let discovery_root = (self.general.features_enabled && self.request.is_none())
                    .then_some(self.profile_dir.as_path());
                return self
                    .profiles
                    .update_cli(action, profile_editable, discovery_root)
                    .map(Message::Profiles);
            }
            Message::Profiles(profiles::Message::Discover) if profile_editable => {
                return self.request(true);
            }
            Message::Profiles(profiles::Message::LoadUsage) if editable => {
                return self.load_profile_usage();
            }
            Message::Profiles(profiles::Message::UsageLoaded(result)) => {
                let current = self.profile_usage_key().is_some_and(|(reference, days)| {
                    reference == result.reference && days == result.reset_days
                });
                if self.profiles.finish_usage_load(result, current) {
                    return self.load_profile_usage();
                }
            }
            Message::Profiles(profiles::Message::RefreshManagedModels) if profile_editable => {
                return self.refresh_managed_models();
            }
            Message::Profiles(profiles::Message::Wizard(profiles::WizardAction::Scan)) => {
                if profile_editable && self.general.features_enabled && self.request.is_none() {
                    return self.profiles.scan_wizard_servers().map(Message::Profiles);
                }
            }
            Message::Profiles(profiles::Message::WizardScanFinished(result)) => {
                self.profiles
                    .finish_wizard_scan(result, profile_editable && self.general.features_enabled);
            }
            Message::Profiles(profiles::Message::SetDefault(id)) => {
                if profile_editable && self.page == Page::Profiles && self.search.trim().is_empty()
                {
                    return self.set_default_profile(id);
                }
            }
            Message::Profiles(profiles::Message::Save) if !profile_editable => {}
            Message::Profiles(profiles::Message::Save) | Message::Save
                if editable && !self.busy() && !self.local.busy() && self.request.is_none() =>
            {
                return self.save(vault);
            }
            Message::Profiles(profiles::Message::Refresh) if editable => {
                if self.profiles.catalog() == &self.saved.profiles && !self.profiles.draft_dirty() {
                    match self
                        .saved
                        .as_ref()
                        .clone()
                        .with_policy_profiles(&ClipboardPolicy::load())
                    {
                        Ok(current) => {
                            let changed = current.policy_profiles_changed(&self.saved);
                            self.profiles.replace(current.profiles.clone());
                            self.profiles.set_status("Profile list refreshed.");
                            self.saved = Arc::new(current);
                            return Task::batch([
                                self.load_profile_usage(),
                                if self.saved.policy_auto_definitions().next().is_some() {
                                    Task::done(Message::RefreshPolicyAutoModels)
                                } else {
                                    Task::none()
                                },
                                if changed {
                                    Task::done(Message::PolicyProfilesRefreshed(Arc::clone(
                                        &self.saved,
                                    )))
                                } else {
                                    Task::none()
                                },
                            ]);
                        }
                        Err(error) => self.profiles.set_status(error.to_string()),
                    }
                } else {
                    self.profiles
                        .set_status("Save your profile edits before refreshing.");
                }
            }
            Message::Profiles(profiles::Message::Wizard(profiles::WizardAction::Open))
                if !self.can_open_profile_wizard() => {}
            Message::Profiles(
                event @ profiles::Message::Wizard(profiles::WizardAction::Cancel),
            ) => {
                if self.close_token.is_none() && self.profile_wizard_open() {
                    let candidate = self.profiles.selected().map(|profile| profile.id);
                    if self
                        .request
                        .as_ref()
                        .is_some_and(|request| Some(request.profile.id) == candidate)
                    {
                        self.request = None;
                        self.notice.clear();
                    }
                    self.profiles.update(event, editable);
                    return self.load_profile_usage();
                }
            }
            Message::Profiles(event) => {
                let previous_usage = self.profile_usage_key();
                let event_editable = match event {
                    profiles::Message::Add
                    | profiles::Message::Wizard(profiles::WizardAction::Open) => {
                        editable && self.profile_policy.create
                    }
                    profiles::Message::CancelDelete => editable,
                    _ => profile_editable,
                };
                self.profiles.update(event, event_editable);
                if self.profile_usage_key() != previous_usage {
                    return self.load_profile_usage();
                }
            }
            Message::Skills(event) if editable || event.completes_work() => {
                let profile = matches!(event, skills::Message::Shorten)
                    .then(|| self.shortening_profile())
                    .flatten();
                return self
                    .skills
                    .update(&mut self.skill_store, event, profile)
                    .map(Message::Skills);
            }
            Message::Memory(event)
                if editable
                    || matches!(
                        event,
                        memory::Message::Completed(_) | memory::Message::CleanupFinished(..)
                    )
                    || (self.close_token.is_none()
                        && matches!(event, memory::Message::ToggleOrganizationRule(_))) =>
            {
                return self
                    .memory
                    .update_with_cleanup(
                        event,
                        self.general.features_enabled.then_some(&self.saved),
                        vault,
                    )
                    .map(Message::Memory);
            }
            Message::Local(event)
                if editable
                    || matches!(
                        event,
                        local::Message::Finished(..)
                            | local::Message::ManagedBackendLoaded(..)
                            | local::Message::PolicyEmbeddingsLoaded(..)
                    )
                    || (self.close_token.is_none()
                        && matches!(
                            event,
                            local::Message::CloseBackendRepair
                                | local::Message::CancelRuntimeRemoval
                        )) =>
            {
                if let local::Message::HuggingFace(action) = event {
                    if editable
                        && (matches!(action, local::huggingface::Action::Progress(..))
                            || self.page == Page::LocalModels && self.search.trim().is_empty())
                    {
                        return self
                            .local
                            .update_huggingface(action, self.general.features_enabled, vault)
                            .map(Message::Local);
                    }
                    return Task::none();
                }
                if editable
                    && self.page == Page::LocalModels
                    && self.search.trim().is_empty()
                    && let Some(change) =
                        self.local.managed_default_change(&event, &self.local_state)
                {
                    self.saving = true;
                    self.notice = "Saving the default managed model…".into();
                    let profile = self.profile_dir.clone();
                    return Task::perform(
                        async move { change.commit(&profile).map_err(|error| error.to_string()) },
                        Message::ManagedDefaultSaved,
                    );
                }
                return self
                    .local
                    .update_with_vault(
                        event,
                        &mut self.local_state,
                        &self.saved.local,
                        self.editable && self.general.features_enabled,
                        vault,
                    )
                    .map(Message::Local);
            }
            Message::CancelRequest => {
                self.request = None;
                self.notice = "Request cancelled.".into();
            }
            Message::AutoTestResolved(ticket, started, result) => {
                return self.finish_auto_model_test(ticket, started, result);
            }
            Message::RequestDone(ticket, result) => {
                if self
                    .request
                    .as_ref()
                    .is_some_and(|request| request.ticket == ticket)
                    && let Some(request) = self.request.take()
                {
                    if self.profiles.selected() != Some(&request.profile) {
                        self.notice =
                            "Profile changed. Run the request again for the new settings.".into();
                        return Task::none();
                    }
                    if let Some(lock) = self.profile_form_lock() {
                        self.notice = lock.into();
                        return Task::none();
                    }
                    self.notice = match result {
                        Ok(Reply::Models(models)) => {
                            let count = models.len();
                            self.profiles.set_models(
                                request.profile.id,
                                request.profile.endpoint.trim(),
                                models,
                            );
                            if request.test_after_discovery && count > 0 {
                                return self.request(false);
                            }
                            if count == 1 {
                                "Found one model.".into()
                            } else {
                                format!("Found {count} models.")
                            }
                        }
                        Ok(Reply::Tested(model, usage)) => {
                            let mut notice = format!("Model responded successfully ({model}).");
                            if let Some(usage) = usage {
                                notice.push(' ');
                                notice.push_str(&usage);
                            }
                            notice
                        }
                        Err(error) => error,
                    };
                }
            }
            Message::Saved(result) => {
                self.saving = false;
                if self.setup_commit.take().is_some() {
                    if let Ok((saved, _)) = &result {
                        self.local_state = saved.local.clone();
                    }
                    if let Some(wizard) = &mut self.setup {
                        wizard.commit_finished(result.as_ref().map(|_| ()).map_err(Clone::clone));
                    }
                }
                let refresh_usage = result.is_ok();
                self.notice = match result {
                    Ok((saved, warning)) => {
                        self.general = saved.general.clone();
                        self.timeout = self.general.timeout_minutes.to_string();
                        self.profiles.replace(saved.profiles.clone());
                        self.saved = saved;
                        self.memory.mark_saved();
                        self.local.mark_saved();
                        warning.map_or_else(
                            || "AI settings saved.".into(),
                            |warning| format!("AI settings saved. {warning}"),
                        )
                    }
                    Err(error) => format!("Could not save AI settings ({error})."),
                };
                if refresh_usage {
                    return self.load_profile_usage();
                }
            }
            Message::ProfileDefaultSaved(result) => {
                self.saving = false;
                self.notice = match result {
                    Ok(saved) => {
                        self.profiles.adopt_saved_default(&saved.profiles);
                        self.saved = saved;
                        "Default AI profile saved.".into()
                    }
                    Err(error) => format!("Could not save the default AI profile ({error})."),
                };
            }
            Message::ManagedDefaultSaved(result) => {
                self.saving = false;
                self.notice = match result {
                    Ok(id) => {
                        let marked = id.is_some();
                        self.local.set_managed_default(id);
                        if marked {
                            "Default managed model saved.".into()
                        } else {
                            "Default managed model cleared.".into()
                        }
                    }
                    Err(error) => format!("Could not save the default managed model ({error})."),
                };
            }
            _ => {}
        }
        Task::none()
    }

    fn search_results(&self) -> impl Iterator<Item = search::Target> + '_ {
        search::results(&self.search, self.profiles.searchable_profile())
    }

    fn open_page(&mut self, target: search::Target) -> Task<Message> {
        self.page = target.page();
        self.search.clear();
        self.search_selection = 0;
        self.revealed.setting = match target {
            search::Target::Setting(setting) => Some(setting),
            search::Target::Page(_) => None,
        };
        if let Some(search::Setting::Profile(setting)) = self.revealed.setting {
            self.profiles
                .update(profiles::Message::Section(setting.section()), false);
        }
        let scroll = if self.revealed.setting.is_some() {
            self.revealed.apply()
        } else {
            iced::widget::operation::snap_to(
                self.revealed.viewport.clone(),
                iced::widget::operation::RelativeOffset::START,
            )
        };
        let load = match self.page {
            Page::Memory => self.memory.load().map(Message::Memory),
            Page::Profiles => self.refresh_managed_models(),
            Page::LocalModels | Page::Knowledge | Page::LocalPreferences if self.editable() => {
                self.local.open_managed_models().map(Message::Local)
            }
            _ => Task::none(),
        };
        Task::batch([scroll, load, self.load_profile_usage()])
    }

    fn set_default_profile(&mut self, id: ProfileId) -> Task<Message> {
        if !self.profiles.can_set_default(id) {
            return Task::none();
        }
        let mut saved = self.saved.as_ref().clone();
        if !saved.profiles.set_default(id) {
            self.profiles
                .update(profiles::Message::SetDefault(id), true);
            return Task::none();
        }
        saved.origin = Some(self.profile_dir.clone());
        self.saving = true;
        self.notice = "Saving the default AI profile…".into();
        let baseline = Arc::clone(&self.saved);
        let profile = self.profile_dir.clone();
        Task::perform(
            async move {
                saved
                    .save_against(&profile, &baseline)
                    .map(|()| Arc::new(saved))
                    .map_err(|error| error.to_string())
            },
            Message::ProfileDefaultSaved,
        )
    }

    fn save(&mut self, vault: Option<&VaultWindow>) -> Task<Message> {
        let snapshot = (|| {
            let timeout_minutes = self
                .timeout
                .parse::<u16>()
                .ok()
                .filter(|value| (1..=MAX_TIMEOUT_MINUTES).contains(value))
                .ok_or("Enter a request timeout from 1 to 1440 minutes.")?;
            if !self.memory.settings_valid() {
                return Err("Correct the AI Memory token budget first.".to_owned());
            }
            self.general.agent_shortcut.validate()?;
            let policy = ClipboardPolicy::load();
            self.profiles
                .catalog()
                .require_policy_changes(&self.saved.profiles, &policy)
                .map_err(|error| error.to_string())?;
            let profiles = self.profiles.catalog_for_save(
                (!policy.ai_profile_edit_allowed()).then_some(&self.saved.profiles),
                policy.ai_web_search_allowed,
            )?;
            self.local_state.retain_profiles(|reference| {
                profiles.is_policy_profile_reference(reference)
                    || profiles.resolve_profile_reference(reference).is_some()
            });
            self.skills.commit(&mut self.skill_store)?;
            Ok::<_, String>(Arc::new(Configuration {
                version: VERSION,
                general: General {
                    timeout_minutes,
                    ..self.general.clone()
                },
                profiles,
                local: self.local_state.clone(),
                skills: self.skill_store.clone(),
                memory: self.memory.settings().clone(),
                origin: Some(self.profile_dir.clone()),
                model_catalog: self.saved.model_catalog.clone(),
            }))
        })();
        let saved = match snapshot {
            Ok(saved) => saved,
            Err(error) => {
                self.notice = error;
                return Task::none();
            }
        };
        let indexes = match self.local.prepare_save_indexes(
            &saved.local,
            &self.saved.local,
            vault,
            saved.enabled(),
        ) {
            Ok(indexes) => indexes,
            Err(error) => {
                self.notice = error;
                return Task::none();
            }
        };
        self.saving = true;
        let baseline = Arc::clone(&self.saved);
        let profile = self.profile_dir.clone();
        Task::perform(
            async move {
                let warning = saved
                    .save_with_indexes(&profile, &baseline, Some(indexes), None)
                    .map_err(|error| error.to_string())?;
                if saved.skills != baseline.skills {
                    skills::usage::observe_sizes(&profile);
                }
                Ok((saved, warning))
            },
            Message::Saved,
        )
    }

    fn profile_usage_key(&self) -> Option<(ProfileRef, u16)> {
        if self.page != Page::Profiles || !self.search.trim().is_empty() {
            return None;
        }
        let profile = self.profiles.usage_profile()?;
        Some((profile.reference(), profile.usage_budget.reset_days))
    }

    fn load_profile_usage(&mut self) -> Task<Message> {
        let Some((reference, reset_days)) = self.profile_usage_key() else {
            return Task::none();
        };
        if !self.editable() || !self.profiles.begin_usage_load() {
            return Task::none();
        }
        let account = self
            .saved
            .profiles
            .profile_for_reference(&reference)
            .ok_or_else(|| "Save the profile before reading recorded usage.".to_owned())
            .and_then(|saved| {
                let mut budget = saved.usage_budget.clone();
                budget.reset_days = reset_days;
                self.saved
                    .profiles
                    .usage_account(saved, &self.profile_dir, budget)
            });
        Task::perform(
            async move {
                let result = account.and_then(|account| {
                    account
                        .snapshot()
                        .map_err(|_| "The AI usage ledger could not be read.".to_owned())
                });
                Arc::new(profiles::UsageLoaded {
                    reference,
                    reset_days,
                    result,
                })
            },
            |result| Message::Profiles(profiles::Message::UsageLoaded(result)),
        )
    }

    pub(crate) fn install_policy_profile_projection(&mut self, configuration: &Configuration) {
        let mut saved = self.saved.as_ref().clone();
        saved.profiles.copy_policy_profiles(&configuration.profiles);
        self.saved = Arc::new(saved);
        self.profiles
            .install_policy_profiles(&configuration.profiles);
    }

    fn finish_auto_model_test(
        &mut self,
        ticket: u64,
        started: Instant,
        result: Result<local_ai::PolicyAutoModel, String>,
    ) -> Task<Message> {
        if self
            .request
            .as_ref()
            .is_none_or(|request| request.ticket != ticket)
        {
            return Task::none();
        }
        let request = self.request.take().expect("checked request ticket");
        let definition = request.profile.policy_auto_candidate();
        let selected = self.profiles.selected();
        let Some(definition) = definition.filter(|definition| {
            selected.is_some_and(|selected| {
                selected.id == request.profile.id
                    && selected.policy_auto_candidate() == Some(*definition)
            })
        }) else {
            self.notice = "Profile changed. Run the test again for the new settings.".into();
            return Task::none();
        };
        if let Some(lock) = self.profile_form_lock() {
            self.notice = lock.into();
            return Task::none();
        }
        let policy = ClipboardPolicy::load();
        let error = result.as_ref().err().cloned();
        if !policy_auto::publish(Arc::clone(definition), started, result, &policy) {
            self.notice =
                "The policy profile or loaded model changed. Test the current selection again."
                    .into();
            return Task::none();
        }
        let projected = match self.saved.as_ref().clone().with_policy_profiles(&policy) {
            Ok(projected) => projected,
            Err(error) => {
                self.notice = error.to_string();
                return Task::none();
            }
        };
        self.install_policy_profile_projection(&projected);
        let refresh = Task::done(Message::PolicyProfilesRefreshed(Arc::clone(&self.saved)));
        if let Some(error) = error {
            self.notice = error;
            refresh
        } else {
            Task::batch([refresh, self.request_with_resolution(false, false)])
        }
    }

    fn request(&mut self, discover: bool) -> Task<Message> {
        self.request_with_resolution(discover, true)
    }

    fn request_with_resolution(&mut self, discover: bool, resolve_auto: bool) -> Task<Message> {
        if self.profiles.cli_discovery_busy() {
            self.notice = "Finish or cancel LM Studio model discovery first.".into();
            return Task::none();
        }
        if !self.general.features_enabled || self.request.is_some() {
            self.notice = "Enable AI features and finish the current request first.".into();
            return Task::none();
        }
        if resolve_auto
            && !discover
            && let Some(profile) = self.profiles.selected()
            && let Some(definition) = profile.policy_auto_candidate()
        {
            let started = Instant::now();
            let pending = match local_ai::resolve_policy_auto_model(definition) {
                Ok(pending) => pending,
                Err(error) => {
                    self.notice = error;
                    return Task::none();
                }
            };
            let ticket = self.next_request;
            self.next_request = self.next_request.wrapping_add(1);
            self.request = Some(Request {
                ticket,
                profile: profile.clone(),
                test_after_discovery: false,
                _cancel: pending.cancel,
            });
            self.notice = "Checking the loaded LM Studio model…".into();
            return Task::perform(
                async move {
                    pending
                        .reply
                        .recv()
                        .await
                        .unwrap_or_else(|_| Err("The model metadata worker stopped.".into()))
                },
                move |result| Message::AutoTestResolved(ticket, started, result),
            );
        }
        let result = self
            .profiles
            .selected()
            .ok_or_else(|| "Select a profile first.".to_owned())
            .and_then(|profile| {
                if discover && profile.is_managed() {
                    return Err("Choose a registered managed model in the Model section.".into());
                }
                if discover && profile.is_cli() {
                    return Err(
                        "Configured CLI profiles use an optional explicit model argument.".into(),
                    );
                }
                profile.validate_connection()?;
                let endpoint = profile.endpoint.trim().to_owned();
                let test_after_discovery = !profile.is_managed()
                    && !discover
                    && profile.model_mode == profiles::ModelMode::Auto
                    && profile.model.trim().is_empty();
                let pending = if discover || test_after_discovery {
                    let pending = if let Some(definition) = profile.policy_definition() {
                        local_ai::discover_policy_profile(definition)?
                    } else if let Some(protocol) = profile.remote_protocol() {
                        local_ai::discover_remote(&endpoint, protocol, profile.credential()?)?
                    } else if profile.connection_mode == profiles::ConnectionMode::LmStudio {
                        local_ai::discover_lm_studio(&endpoint, profile.credential()?)?
                    } else {
                        match profile.credential()? {
                            Some(name) => {
                                local_ai::discover_with_credential(&endpoint, Some(name))?
                            }
                            None => local_ai::discover(&endpoint)?,
                        }
                    };
                    let local_ai::Pending { reply, cancel } = pending;
                    (
                        Task::future(async move {
                            reply
                                .recv()
                                .await
                                .unwrap_or_else(|_| Err("The worker stopped.".into()))
                                .map(Reply::Models)
                        }),
                        cancel,
                    )
                } else {
                    let timeout = self
                        .timeout
                        .parse()
                        .ok()
                        .filter(|value| (1..=MAX_TIMEOUT_MINUTES).contains(value))
                        .ok_or("Enter a valid global request timeout before testing.")?;
                    let mut config = profile
                        .transport_in(
                            timeout,
                            &self.profile_dir,
                            self.saved.model_catalog.as_deref(),
                        )?
                        .requiring_managed_backend();
                    if !profile.is_cli() && self.saved.profiles.profile(profile.id) == Some(profile)
                    {
                        config = config.with_usage_account(self.saved.profiles.usage_account(
                            profile,
                            &self.profile_dir,
                            profile.usage_budget.clone(),
                        )?);
                    }
                    let managed = profile.is_managed();
                    let cli = profile.is_cli();
                    let saved_cli = (cli
                        && self.saved.saved_root().is_some()
                        && self.saved.profiles.profile(profile.id) == Some(profile))
                    .then(|| {
                        (
                            Arc::clone(&self.saved),
                            profile.id,
                            config.clone(),
                            profile.selection_limit as usize,
                        )
                    });
                    let model_name = managed
                        .then(|| self.profiles.managed_model_name(profile).map(str::to_owned))
                        .flatten();
                    let pending = local_ai::start(config, move |client| {
                        if cli {
                            client
                                .test_cli(|| {
                                    if let Some((saved, id, expected, limit)) = &saved_cli {
                                        saved
                                            .require_current_terminal_cli(*id, expected, *limit)?;
                                    }
                                    Ok(())
                                })
                                .map_err(local_ai::Error::message)?;
                            return Ok(Reply::Tested(client.model().into(), None));
                        }
                        let system =
                            "You are testing a connection. Respond with a short acknowledgement.";
                        let input = "Reply with OK.";
                        let deadline = Instant::now() + Duration::from_secs(86_400);
                        let reply = if managed {
                            client.complete_before(system, input, false, deadline)
                        } else {
                            client.complete_text_before(system, input, false, deadline)
                        };
                        let usage = client.take_usage().and_then(|usage| usage.token_label());
                        let describe = |mut error: String| {
                            if let Some(usage) = &usage {
                                error.push_str(" · ");
                                error.push_str(usage);
                            }
                            error
                        };
                        let reply = reply.map_err(|error| describe(error.message()))?;
                        if managed && reply.trim().is_empty() {
                            return Err(describe(
                                "The managed model returned an empty reply.".into(),
                            ));
                        }
                        Ok(Reply::Tested(
                            model_name.unwrap_or_else(|| client.model().into()),
                            usage,
                        ))
                    })?;
                    let local_ai::Pending { reply, cancel } = pending;
                    (
                        Task::future(async move {
                            reply
                                .recv()
                                .await
                                .unwrap_or_else(|_| Err("The worker stopped.".into()))
                        }),
                        cancel,
                    )
                };
                Ok((profile.clone(), test_after_discovery, pending))
            });
        match result {
            Ok((profile, test_after_discovery, (reply, cancel))) => {
                let ticket = self.next_request;
                self.next_request = self.next_request.wrapping_add(1);
                self.request = Some(Request {
                    ticket,
                    profile,
                    test_after_discovery,
                    _cancel: cancel,
                });
                self.notice = if discover || test_after_discovery {
                    "Loading models…"
                } else {
                    "Testing connection…"
                }
                .into();
                reply.map(move |result| Message::RequestDone(ticket, result))
            }
            Err(error) => {
                self.notice = error;
                Task::none()
            }
        }
    }

    fn refresh_managed_models(&mut self) -> Task<Message> {
        self.profiles
            .refresh_managed_models(self.profile_dir.clone())
            .map(Message::Profiles)
    }

    fn shortening_profile(&self) -> Option<local_ai::Profile> {
        if !self.editable() || !self.general.features_enabled {
            return None;
        }
        self.saved.drawing_profile(false)?.transport.ok()
    }

    fn can_shorten_skill(&self) -> bool {
        self.editable()
            && self.general.features_enabled
            && self
                .saved
                .local_profile_for(local::Role::Text)
                .is_some_and(|entry| {
                    is_policy_model_profile(entry) || self.saved.transport_for(entry).is_ok()
                })
    }

    pub(crate) fn view(
        &self,
        hub_tokens: Vec<CredentialChoice>,
        qdrant_keys: Vec<CredentialChoice>,
    ) -> Element<'_, Message> {
        if let Some(wizard) = &self.setup {
            return container(wizard.view(self.editable(), hub_tokens).map(Message::Setup))
                .padding([10, 14])
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
        }
        let child_form_open = self.profile_wizard_open() || self.local.backend_repair_open();
        let controls = row![
            button("‹").on_press_maybe(self.page.step(false).map(Message::Page)),
            text(self.page.title()),
            button("›").on_press_maybe(self.page.step(true).map(Message::Page)),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let header = row![
            crate::settings_search::input(
                text_input("Search AI settings…", &self.search)
                    .id(self.search_id.clone())
                    .on_input(Message::Search)
                    .on_submit(Message::SearchChoose),
                Message::SearchStep(-1),
                Message::SearchStep(1),
                (!self.search.is_empty()).then_some(Message::SearchEscape),
                None,
            ),
            controls,
            button("Manual").style(button::text).on_press(Message::Help),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        let tabs = scrollable(
            row(Page::ALL.into_iter().map(|page| {
                button(page.title())
                    .style(if page == self.page {
                        button::primary
                    } else {
                        button::secondary
                    })
                    .on_press(Message::Page(page))
                    .into()
            }))
            .spacing(3),
        )
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new().spacing(4),
        ))
        .width(Length::Fill);
        let content: Element<'_, Message> = if self.search.trim().is_empty() {
            match self.page {
                Page::General => self.general_view(),
                Page::Profiles => self
                    .profiles
                    .view(
                        self.editable(),
                        self.can_open_profile_wizard(),
                        self.profile_policy.create,
                        self.profile_form_lock(),
                        &self.revealed,
                    )
                    .map(Message::Profiles),
                Page::Skills if self.editable() => self
                    .skills
                    .view(&self.skill_store, self.can_shorten_skill())
                    .map(Message::Skills),
                Page::Memory => self
                    .memory
                    .view(
                        self.editable()
                            && self.general.features_enabled
                            && self.saved.enabled()
                            && self.saved.profiles.default_profile().is_some(),
                    )
                    .map(Message::Memory),
                Page::Skills => text("AI skill editing is unavailable.").into(),
                Page::LocalModels | Page::Knowledge | Page::LocalPreferences => {
                    let page = match self.page {
                        Page::LocalModels => local::Page::Models(hub_tokens),
                        Page::Knowledge => local::Page::Knowledge(qdrant_keys),
                        _ => local::Page::Preferences,
                    };
                    let content = self
                        .local
                        .view(
                            page,
                            &self.local_state,
                            self.profiles.catalog(),
                            self.editable(),
                            self.general.features_enabled,
                            &self.revealed,
                        )
                        .map(Message::Local);
                    let setup_role = match self.page {
                        Page::LocalModels => Some(None),
                        Page::LocalPreferences if self.local_state.needs_guided_embedding() => {
                            Some(Some(setup::Role::Embedding))
                        }
                        _ => None,
                    };
                    if let Some(role) = setup_role {
                        let catalog = (self.page == Page::LocalModels).then(|| {
                            self.saved.model_catalog.as_ref().map_or_else(
                                || "Model and prompt catalog · Bundled".into(),
                                |catalog| {
                                    format!("Model and prompt catalog · {}", catalog.version())
                                },
                            )
                        });
                        column![
                            button(if role.is_some() {
                                "Set up an embedding model…"
                            } else {
                                "Set up local AI…"
                            })
                            .on_press_maybe(
                                self.can_open_guided_setup(role)
                                    .then_some(Message::Setup(setup::Message::Open(role)))
                            ),
                            catalog.map(|label| text(label).size(13)),
                            content
                        ]
                        .spacing(8)
                        .height(Length::Fill)
                        .into()
                    } else {
                        content
                    }
                }
            }
        } else {
            self.search_view()
        };
        let notice = container(
            scrollable(
                text(self.notice.as_str())
                    .size(13)
                    .wrapping(text::Wrapping::WordOrGlyph),
            )
            .width(Length::Fill)
            .height(Length::Shrink),
        )
        .width(Length::Fill)
        .max_height(MAX_NOTICE_HEIGHT);
        let footer = row![
            notice,
            self.request
                .as_ref()
                .map(|_| button("Cancel request").on_press(Message::CancelRequest)),
            (!child_form_open && self.page != Page::Profiles).then(|| button(if self.saving {
                "Saving…"
            } else {
                "Save"
            })
            .on_press_maybe(
                (self.editable()
                    && !self.busy()
                    && !self.local.busy()
                    && self.request.is_none()
                    && self.dirty())
                .then_some(Message::Save)
            )),
            (!child_form_open).then(|| button("Close").style(button::secondary).on_press_maybe(
                (!self.busy()).then(|| { self.child_close_message().unwrap_or(Message::Close) })
            )),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let content: Element<'_, Message> = if !self.search.trim().is_empty() {
            scrollable(content)
                .id(self.search_scroll.clone())
                .height(Length::Fill)
                .into()
        } else if self.page == Page::General
            || self.page == Page::Memory && !self.memory.reviewing_document()
        {
            scrollable(content)
                .id(self.revealed.viewport.clone())
                .height(Length::Fill)
                .into()
        } else {
            container(content).height(Length::Fill).into()
        };
        let body = column![
            (!child_form_open).then_some(header),
            (!child_form_open).then_some(tabs),
            content,
            footer
        ]
        .spacing(10)
        .padding([10, 14])
        .height(Length::Fill);
        if self.close_token.is_some() {
            iced::widget::stack![
                body,
                iced::widget::opaque(
                    container(
                        column![
                            text("Discard unsaved AI Manager changes?").size(18),
                            row![
                                button("Keep editing").on_press(Message::CloseAnswer(false)),
                                button("Discard").style(button::danger).on_press_maybe(
                                    (!self.busy()).then_some(Message::CloseAnswer(true))
                                ),
                            ]
                            .spacing(8)
                        ]
                        .spacing(14)
                        .padding(18),
                    )
                    .center(Length::Fill)
                    .style(container::dark)
                )
            ]
            .into()
        } else {
            body.into()
        }
    }

    fn general_view(&self) -> Element<'_, Message> {
        use search::Setting::{Features, GlobalTimeout};
        let mut features = checkbox(self.general.features_enabled).label(Features.title());
        let mut timeout = text_input("1–1440", &self.timeout).width(80);
        if self.editable() {
            features = features.on_toggle(Message::Features);
            timeout = timeout.on_input(Message::Timeout);
        }
        let code_text_language: Element<'_, Message> = if self.editable() {
            pick_list(
                CodeTextLanguage::ALL.as_slice(),
                Some(self.general.code_text_language),
                Message::CodeTextLanguage,
            )
            .dismiss_on_escape()
        } else {
            text(self.general.code_text_language.to_string()).into()
        };
        let language_setting = search::Setting::CodeTextLanguage;
        let terminal_actions_setting = search::Setting::ChatTerminalActions;
        let terminal_actions: Element<'_, Message> = if self.editable() {
            pick_list(
                ChatTerminalActions::ALL.as_slice(),
                Some(self.general.chat_terminal_actions),
                Message::ChatTerminalActions,
            )
            .dismiss_on_escape()
        } else {
            text(self.general.chat_terminal_actions.to_string()).into()
        };
        let shortcut_setting = search::Setting::AgentShortcut;
        let setup_setting = search::Setting::ShowAgentSetup;
        let shortcut = &self.general.agent_shortcut;
        let mut enabled = checkbox(shortcut.enabled).label(shortcut_setting.title());
        let mut name = text_input(shortcut::DEFAULT_ALIAS, &shortcut.name).width(200);
        let mut case = checkbox(shortcut.case_insensitive).label("Ignore command name case");
        let mut show_setup = checkbox(self.general.show_agent_setup).label(setup_setting.title());
        if self.editable() {
            enabled = enabled.on_toggle(Message::AgentShortcutEnabled);
            name = name.on_input(Message::AgentShortcutName);
            case = case.on_toggle(Message::AgentShortcutCaseInsensitive);
            show_setup = show_setup.on_toggle(Message::ShowAgentSetup);
        }
        column![
            text("Features").size(19),
            self.revealed.wrap(Features, features),
            text("AI features use configured profiles. Local diagram generation remains available."),
            text("Limits").size(19),
            self.revealed.wrap(
                GlobalTimeout,
                row![text(GlobalTimeout.title()), self.revealed.input(GlobalTimeout, self.editable(), timeout)]
                    .spacing(12).align_y(Alignment::Center),
            ),
            text("AI chat").size(19),
            self.revealed.wrap(
                terminal_actions_setting,
                row![text(terminal_actions_setting.title()), terminal_actions]
                    .spacing(12)
                    .align_y(Alignment::Center)
                    .wrap(),
            ),
            text("Terminal AI Agent").size(19),
            self.revealed.wrap(
                shortcut_setting,
                column![
                    enabled,
                    row![text("Base command name"), self.revealed.input(shortcut_setting, self.editable(), name)]
                        .spacing(8).align_y(Alignment::Center).wrap(),
                    case,
                    text("Type the base command, or add -ask or -plan, at a shell prompt with OSC 133 integration. Tab offers variants or recent prompts. Enter submits the alias.").size(13),
                    text("For local terminals, enable automatic shell integration in Quick Connect or the saved shell profile. For SSH, install the matching snippet on the remote host and reconnect. Right-click the terminal → Set Up Shell Integration… shows shortcut readiness.").size(13),
                ].spacing(6),
            ),
            self.revealed.wrap(
                setup_setting,
                column![
                    show_setup,
                    text("When unchecked, a submitted Run or Ask alias with a task can skip setup when the required details are available. Run still opens the environment probe review.").size(13),
                ].spacing(6),
            ),
            text("Snippet text").size(19),
            self.revealed.wrap(
                language_setting,
                row![text(language_setting.title()), code_text_language]
                    .spacing(12)
                    .align_y(Alignment::Center)
                    .wrap(),
            ),
            text("Providers").size(19),
            text("Choose a local service, a managed GGUF model, or a trusted HTTPS provider in Profiles. Terminal Chat shows the selected destination and masked messages before sending.").size(13),
        ].spacing(12).into()
    }

    fn search_view(&self) -> Element<'_, Message> {
        let mut matches = self.search_results().enumerate().peekable();
        if matches.peek().is_none() {
            return text("No matching settings.").into();
        }
        let mut results = column![text("Matching AI settings").size(17)].spacing(8);
        for (index, target) in matches {
            let selected = index == self.search_selection;
            let result = button(text(target.label()).wrapping(text::Wrapping::WordOrGlyph))
                .style(if selected {
                    button::primary
                } else {
                    button::secondary
                })
                .width(Length::Fill)
                .on_press(Message::Reveal(target));
            let row = container(result);
            results = results.push(if selected {
                row.id(self.search_row.clone())
            } else {
                row
            });
        }
        results.into()
    }
}

#[cfg(test)]
mod tests {
    use iced::advanced::{Layout, layout, widget::Tree};
    use iced::{Font, Pixels, Renderer, Size};

    use super::*;

    #[test]
    fn signed_catalog_changes_auto_provider_revisions_without_changing_saved_preferences() {
        let directory = crate::snippets::tests::Profile::new();
        let root = directory.0.canonicalize().unwrap();
        let profile = |id, name, preset| {
            serde_json::json!({
                "id": id, "name": name, "model": "Qwen3-4B",
                "connection_mode": "remote-open-ai", "endpoint": "https://provider.example/v1",
                "trusted_remote_endpoint": true, "reviewed_agent_https": true,
                "prompt_preset": preset
            })
        };
        let mut config: Configuration = serde_json::from_value(serde_json::json!({
            "profiles": {
                "profiles": [profile(1, "Remote fixture", "auto"), profile(2, "Explicit fixture", "qwen")],
                "default": 1, "last_id": 2
            }
        })).unwrap();
        config.origin = Some(root);
        let preference = serde_json::to_vec(&config).unwrap();
        let revisions = |config: &Configuration, id| {
            (
                config.agent_profile_revision(id).unwrap(),
                config.journal_summary_https_revision(id).unwrap(),
            )
        };
        let automatic = revisions(&config, 1);
        let explicit = revisions(&config, 2);
        let catalog = local_ai::model_catalog::catalog_with_fixture_mapping();
        config.install_model_catalog(Some(Arc::new(catalog)));
        let transport = config.journal_question_https_transport(1).unwrap().0;
        assert_eq!(transport.prompt_preset(), local_ai::PromptPreset::DeepSeek);
        assert_ne!(revisions(&config, 1).0, automatic.0);
        assert_ne!(revisions(&config, 1).1, automatic.1);
        assert_eq!(revisions(&config, 2), explicit);
        assert_eq!(serde_json::to_vec(&config).unwrap(), preference);
    }

    #[test]
    fn guided_setup_shares_a_chat_profile_and_preserves_existing_roles() {
        use local_ai::managed_models::Model;
        use serde_json::json;

        let baseline: Configuration = serde_json::from_value(json!({
            "profiles": {"profiles": [{"id": 1, "name": "Existing", "model": "previous"}], "default": 1, "last_id": 1},
            "local": {"roles": [null, null, 1, 1]}
        })).unwrap();
        let model = |id, purpose| {
            serde_json::from_value::<Model>(json!({
            "id": id, "name": "Guided model", "bytes": 10, "context_tokens": 4096, "purpose": purpose
        })).unwrap()
        };
        let chat = model("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "chat");
        let embedding = model("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "embedding");
        let mut roles = [
            (setup::Role::Text, chat.id.clone()),
            (setup::Role::Coding, chat.id.clone()),
            (setup::Role::Embedding, embedding.id.clone()),
        ];
        let models = [chat, embedding];
        let saved = guided_setup_configuration(&baseline, &models, &roles).unwrap();
        assert_eq!(saved.profiles.entries().count(), 2);
        assert_eq!(saved.profiles.default_profile().unwrap().id, 1);
        assert_eq!(
            saved.local.profile(local::Role::Text),
            Some(&ProfileRef::Numeric(2))
        );
        assert_eq!(
            saved.local.profile(local::Role::Coding),
            Some(&ProfileRef::Numeric(2))
        );
        assert_eq!(
            saved.local.profile(local::Role::Agent),
            Some(&ProfileRef::Numeric(1))
        );
        assert_eq!(
            saved.local.profile(local::Role::Journal),
            Some(&ProfileRef::Numeric(1))
        );
        let root = std::env::temp_dir();
        assert!(
            saved
                .local
                .journal_embedding(Some(&root))
                .is_some_and(|(_, label)| label.starts_with(&models[1].id))
        );
        assert!(
            guided_setup_configuration(&saved, &models, &roles)
                .is_ok_and(|current| current == saved)
        );
        roles[2].1.clone_from(&models[0].id);
        assert!(guided_setup_configuration(&baseline, &models, &roles).is_err());
        assert!(baseline.local.profile(local::Role::Text).is_none());
    }

    #[test]
    fn scheduled_agent_policy_model_revision_binds_the_source_without_changing_ordinary_hashes() {
        let config: Configuration = serde_json::from_slice(
            br#"{"profiles":{"profiles":[
                {"id":1,"name":"Local","model":"policy-ordinary-external-name"},
                {"id":2,"name":"Managed","connection_mode":"managed",
                 "endpoint":"","model":"policy-team-model","vision":"enabled"}
            ],"default":1,"last_id":2}}"#,
        )
        .unwrap();
        let ordinary = serde_json::to_vec(config.profiles.profile(1).unwrap()).unwrap();
        let legacy: [u8; 32] = pgp::crypto::hash::HashAlgorithm::Sha256
            .digest(&ordinary)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(config.agent_profile_revision(1), Some(legacy));

        let revision = |tag| config.agent_profile_revision_with(2, |_| Some([tag; 32]));
        assert!(revision(1).is_some());
        assert_ne!(revision(1), revision(2));
        assert!(config.agent_profile_revision_with(2, |_| None).is_none());
        assert!(matches!(
            config.journal_summary_transport(2, true),
            Err(error) if error.contains("Vision is disabled")
        ));
    }

    #[test]
    fn journal_https_grant_revision_requires_saved_route_and_binds_request_settings() {
        let scratch = crate::snippets::tests::Profile::new();
        let root = scratch.0.canonicalize().unwrap();
        let mut config: Configuration = serde_json::from_value(serde_json::json!({
            "profiles": {
                "profiles": [{
                    "id": 1,
                    "name": "Remote fixture",
                    "connection_mode": "remote-open-ai",
                    "endpoint": "https://example.invalid/v1",
                    "model": "fixture",
                    "trusted_remote_endpoint": true
                }],
                "default": 1,
                "last_id": 1
            }
        }))
        .unwrap();
        assert!(config.journal_summary_https_revision(1).is_err());

        let baseline = Configuration::load_fixture_from(&root).unwrap();
        config.save_fixture_at(&root, &baseline).unwrap();
        let saved = Configuration::load_fixture_from(&root).unwrap();
        let approved = saved.journal_summary_https_revision(1).unwrap();
        assert_eq!(
            Configuration::load_fixture_from(&root)
                .unwrap()
                .journal_summary_https_revision(1)
                .unwrap(),
            approved
        );
        config.general.timeout_minutes += 1;
        config.save_fixture_at(&root, &saved).unwrap();
        let changed = Configuration::load_fixture_from(&root).unwrap();
        assert_ne!(changed.journal_summary_https_revision(1).unwrap(), approved);

        config.general.features_enabled = false;
        config.save_fixture_at(&root, &changed).unwrap();
        assert!(
            Configuration::load_fixture_from(&root)
                .unwrap()
                .journal_summary_https_revision(1)
                .is_err()
        );
    }

    #[test]
    fn ai_manager_tab_scrollbar_reserves_space_only_when_tabs_overflow() {
        let (_scratch, dialog) = default_profile_dialog();
        let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let tab_bounds = |width| {
            let mut view = dialog.view(Vec::new(), Vec::new());
            let mut tree = Tree::new(view.as_widget());
            let limits = layout::Limits::new(Size::ZERO, Size::new(width, 480.0));
            let node = view.as_widget_mut().layout(&mut tree, &renderer, &limits);
            let tabs = Layout::new(&node).children().nth(1).unwrap();
            let buttons = tabs.children().next().unwrap();
            (tabs.bounds(), buttons.bounds())
        };

        let (wide, wide_buttons) = tab_bounds(1600.0);
        assert!(wide_buttons.width <= wide.width);
        assert_eq!(wide.height, wide_buttons.height);

        // The intrinsic width of the labels varies with the platform fonts.
        let (narrow, narrow_buttons) = tab_bounds(wide_buttons.width * 0.75);
        assert!(narrow_buttons.width > narrow.width);
        assert!(narrow.height - narrow_buttons.height >= 10.0);
    }

    #[test]
    fn ai_manager_approves_configured_cli_then_runs_only_the_fixed_test_prompt() {
        use iced::futures::{StreamExt as _, executor::block_on};

        use crate::local_ai::cli::fixture_executable;
        use crate::local_ai::test_support::exclusive_run;
        use profiles::{CliAction, ConnectionMode, Field, Message as ProfileMessage, Number};

        let _exclusive = exclusive_run();
        let scratch = crate::snippets::tests::Profile::new();
        let root = scratch.0.canonicalize().unwrap();
        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            root.clone(),
            Vec::new(),
            true,
            None,
        );
        let send = |dialog: &mut Dialog, message| dialog.update(Message::Profiles(message));
        let finish = |dialog: &mut Dialog, task| {
            let mut actions =
                iced_runtime::task::into_stream(task).expect("scheduled profile operation");
            while let Some(action) = block_on(actions.next()) {
                if let iced_runtime::Action::Output(message) = action {
                    let _ = dialog.update(message);
                    return;
                }
            }
            panic!("missing profile result");
        };

        let _ = dialog.update(Message::Page(Page::Profiles));
        let _ = send(&mut dialog, ProfileMessage::Add);
        let _ = send(
            &mut dialog,
            ProfileMessage::Override(Number::OutputTokens, true),
        );
        let _ = send(
            &mut dialog,
            ProfileMessage::Number(Number::OutputTokens, "invalid".into()),
        );
        assert!(dialog.profiles.draft_dirty());
        let _ = send(
            &mut dialog,
            ProfileMessage::ConnectionMode(ConnectionMode::Cli),
        );
        assert!(!dialog.profiles.draft_dirty());
        let _ = send(
            &mut dialog,
            ProfileMessage::Cli(CliAction::Path(
                fixture_executable(&root).to_str().unwrap().into(),
            )),
        );
        let _ = send(
            &mut dialog,
            ProfileMessage::Edit(Field::Model, "previous/model".into()),
        );
        let _ = send(
            &mut dialog,
            ProfileMessage::Cli(CliAction::LmStudioPreset { loaded_model: true }),
        );
        assert!(dialog.profiles.selected().unwrap().model.is_empty());
        let inspect = send(&mut dialog, ProfileMessage::Cli(CliAction::Inspect));
        finish(&mut dialog, inspect);
        let marker = root.join("fixture-stdin");
        assert!(!marker.exists());
        assert!(iced_runtime::task::into_stream(send(&mut dialog, ProfileMessage::Test)).is_none());
        let _ = send(&mut dialog, ProfileMessage::Cli(CliAction::Approve));
        let test = send(&mut dialog, ProfileMessage::Test);
        finish(&mut dialog, test);
        assert!(dialog.notice.contains("responded successfully"));
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            "System prompt:\nReply with exactly OK.\n\nUser prompt:\nConnection test.\n"
        );

        let save = send(&mut dialog, ProfileMessage::Save);
        finish(&mut dialog, save);
        let saved = Configuration::load_fixture_from(&root).unwrap();
        assert!(saved.is_terminal_cli_profile(1));
        assert!(saved.terminal_chat_transport(1).unwrap().0.is_cli());
        assert!(saved.terminal_chat_profile_id().is_none());
        assert!(saved.profiles.profile(1).unwrap().model.is_empty());
        assert!(saved.profiles.role_choices().iter().any(|(id, _)| *id == 1));
        assert!(saved.profiles.choices().is_empty());
        assert_eq!(saved.profiles.default_profile().unwrap().id, 1);
        assert_eq!(saved.preferred_terminal_chat_cli_id(), Some(1));
        assert_eq!(saved.preferred_agent_cli_id(), Some(1));
        assert_eq!(saved.preferred_journal_summary_cli_id(), Some(1));
        assert!(saved.agent_profile_id().is_none());
        assert!(saved.journal_summary_default_id().is_none());
        let (_, _, _, destination) = saved.reviewed_agent_cli_profiles().next().unwrap();
        let reference = saved.profile_reference(1).unwrap();
        let (transport, limit) = saved
            .reviewed_agent_cli_transport(&reference, &destination)
            .unwrap();
        assert!(transport.is_cli());
        let (journal, journal_limit) = saved.journal_summary_cli_transport(1).unwrap();
        assert!(transport == journal && limit == journal_limit);
        assert!(saved.agent_transport(1).is_err());
        assert!(
            saved
                .reviewed_agent_cli_transport(&reference, "another command")
                .is_err()
        );
        let revision = saved.reviewed_agent_cli_revision(&reference).unwrap();
        let journal_revision = saved.journal_summary_cli_revision(1).unwrap();
        assert_ne!(revision, journal_revision);
        let mut changed = saved.clone();
        changed.general.timeout_minutes += 1;
        assert_ne!(
            changed.reviewed_agent_cli_revision(&reference),
            Some(revision)
        );
        assert_ne!(
            changed.journal_summary_cli_revision(1).unwrap(),
            journal_revision
        );
        changed.general.features_enabled = false;
        assert!(changed.reviewed_agent_cli_revision(&reference).is_none());
        assert!(changed.journal_summary_cli_profiles().next().is_none());
        let _ = send(
            &mut dialog,
            ProfileMessage::Cli(CliAction::LmStudioPreset {
                loaded_model: false,
            }),
        );
        assert_eq!(
            dialog.profiles.selected().unwrap().validate_connection(),
            Err("Review and approve the CLI executable first.".into())
        );
    }

    #[test]
    fn saved_runtime_update_policy_is_opt_in_and_its_permission_tracks_saved_settings() {
        use crate::local_ai::runtime_package::online::policy::{Backend, Mode, Settings};

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let baseline = Configuration::load_fixture_from(&root).unwrap();
        assert_eq!(baseline.runtime_update_settings(), Settings::default());
        let settings = Settings {
            mode: Mode::AutomaticStable,
            backend: Backend::Cpu,
        };
        let mut draft = baseline.clone();
        draft.local.runtime_update = settings;
        assert!(Configuration::authorize_runtime_update(&root, settings).is_err());
        draft.save_fixture_at(&root, &baseline).unwrap();
        let saved = Configuration::load_fixture_from(&root).unwrap();
        assert_eq!(saved.runtime_update_settings(), settings);
        drop(Configuration::authorize_runtime_update(&root, settings).unwrap());

        draft.local.runtime_update.mode = Mode::Notify;
        draft.save_fixture_at(&root, &saved).unwrap();
        assert_eq!(
            Configuration::authorize_runtime_update(&root, settings)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(Configuration::authorize_runtime_update(&root, draft.local.runtime_update).is_ok());

        let saved = Configuration::load_fixture_from(&root).unwrap();
        draft.general.features_enabled = false;
        draft.save_fixture_at(&root, &saved).unwrap();
        assert_eq!(
            Configuration::authorize_runtime_update(&root, draft.local.runtime_update)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn chat_terminal_actions_round_trip_the_java_ids_and_default() {
        use ChatTerminalActions::{InsertAndRun, InsertOnly, Off};

        assert_eq!(
            Configuration::decode(br#"{"general":{}}"#)
                .unwrap()
                .chat_terminal_actions(),
            InsertAndRun
        );
        for (stored, actions, id, insert, run) in [
            (serde_json::json!("off"), Off, "off", false, false),
            (
                serde_json::json!(" INSERT_ONLY "),
                InsertOnly,
                "insert_only",
                true,
                false,
            ),
            (
                serde_json::json!("insert_and_run"),
                InsertAndRun,
                "insert_and_run",
                true,
                true,
            ),
            (
                serde_json::json!("future"),
                InsertAndRun,
                "insert_and_run",
                true,
                true,
            ),
            (
                serde_json::json!(null),
                InsertAndRun,
                "insert_and_run",
                true,
                true,
            ),
        ] {
            let bytes = serde_json::to_vec(&serde_json::json!({
                "general": { "chat_terminal_actions": stored }
            }))
            .unwrap();
            let config = Configuration::decode(&bytes).unwrap();
            assert_eq!(config.chat_terminal_actions(), actions);
            assert_eq!(actions.allows_insert(), insert);
            assert_eq!(actions.allows_run(), run);
            assert_eq!(
                serde_json::to_value(&config).unwrap()["general"]["chat_terminal_actions"],
                serde_json::json!(id)
            );
        }
    }

    #[test]
    fn ai_manager_reveals_and_saves_chat_terminal_actions() {
        use iced::futures::{StreamExt, executor::block_on};

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            root.clone(),
            Vec::new(),
            true,
            None,
        );
        let _ = dialog.update(Message::Page(Page::Skills));
        let _ = dialog.update(Message::Search("chat terminal actions".into()));
        let _ = dialog.update(Message::SearchChoose);
        assert_eq!(dialog.page, Page::General);
        assert_eq!(
            dialog.revealed.setting,
            Some(search::Setting::ChatTerminalActions)
        );

        let _ = dialog.update(Message::ChatTerminalActions(
            ChatTerminalActions::InsertOnly,
        ));
        assert!(dialog.dirty());
        let mut outcomes = iced_runtime::task::into_stream(dialog.update(Message::Save)).unwrap();
        let Some(iced_runtime::Action::Output(event)) = block_on(outcomes.next()) else {
            panic!("expected the saved settings result");
        };
        assert!(matches!(event, Message::Saved(Ok(_))));
        let _ = dialog.update(event);
        assert!(!dialog.dirty());
        let bytes = std::fs::read(root.join(FILE_NAME)).unwrap();
        assert_eq!(
            Configuration::decode(&bytes)
                .unwrap()
                .chat_terminal_actions(),
            ChatTerminalActions::InsertOnly
        );
    }

    #[test]
    fn ai_manager_reveals_and_persists_optional_agent_setup() {
        use iced::futures::{StreamExt, executor::block_on};

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let old = Configuration::decode(br#"{"general":{}}"#).unwrap();
        assert!(old.show_agent_setup());
        let mut dialog = Dialog::new(Arc::new(old), root.clone(), Vec::new(), true, None);
        let _ = dialog.update(Message::Page(Page::Skills));
        let _ = dialog.update(Message::Search("agent setup dialog".into()));
        let _ = dialog.update(Message::SearchChoose);
        assert_eq!(dialog.page, Page::General);
        assert_eq!(
            dialog.revealed.setting,
            Some(search::Setting::ShowAgentSetup)
        );

        let _ = dialog.update(Message::ShowAgentSetup(false));
        assert!(dialog.dirty());
        assert!(
            Configuration::load_fixture_from(&root)
                .unwrap()
                .show_agent_setup()
        );
        let mut outcomes = iced_runtime::task::into_stream(dialog.update(Message::Save)).unwrap();
        let Some(iced_runtime::Action::Output(event)) = block_on(outcomes.next()) else {
            panic!("expected the saved settings result");
        };
        assert!(matches!(event, Message::Saved(Ok(_))));
        let _ = dialog.update(event);
        assert!(!dialog.dirty());
        assert!(
            !Configuration::load_fixture_from(&root)
                .unwrap()
                .show_agent_setup()
        );
    }

    #[test]
    fn ai_settings_writes_refuse_a_stale_configuration() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let original = Configuration::default();
        let mut changed = original.clone();
        changed.general.timeout_minutes = 11;
        changed.save_against(&root, &original).unwrap();
        let mut stale = original.clone();
        stale.general.timeout_minutes = 22;
        assert!(stale.save_against(&root, &original).is_err());
        let bytes = std::fs::read(root.join(FILE_NAME)).unwrap();
        assert_eq!(Configuration::decode(&bytes).unwrap(), changed);
    }

    #[cfg(unix)]
    #[test]
    fn ai_settings_mutation_unlocks_while_a_duplicate_descriptor_is_open() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let directory = crate::private_files::Directory::open(&root, false).unwrap();
        let lock = Configuration::lock_mutation(&directory).unwrap();
        let inherited = lock.try_clone().unwrap();
        let baseline = Configuration::default();
        let mut changed = baseline.clone();
        changed.general.timeout_minutes = 11;

        let error = changed.save_against(&root, &baseline).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        drop(lock);
        changed.save_against(&root, &baseline).unwrap();
        drop(inherited);
    }

    fn default_profile_dialog() -> (crate::snippets::tests::Profile, Dialog) {
        let scratch = crate::snippets::tests::Profile::new();
        let root = scratch.0.canonicalize().unwrap();
        let initial = Configuration::decode(
            br#"{"profiles":{
                "profiles":[
                    {"id":1,"name":"Saved A","endpoint":"127.0.0.1:12345","model":"fixture"},
                    {"id":2,"name":"Saved B","endpoint":"127.0.0.1:12346","model":"fixture"}
                ],
                "default":1,"last_id":2
            }}"#,
        )
        .unwrap();
        initial
            .save_against(&root, &Configuration::default())
            .unwrap();
        let saved = Arc::new(Configuration::load_from(&root).unwrap());
        let mut dialog = Dialog::new(saved, root, Vec::new(), true, None);
        dialog.page = Page::Profiles;
        (scratch, dialog)
    }

    #[test]
    fn ai_profile_creation_policy_locks_add_and_wizard_without_locking_existing_edits() {
        use profiles::{Field, Message as ProfileMessage, WizardAction};

        let (_scratch, mut dialog) = default_profile_dialog();
        let send = |dialog: &mut Dialog, event| {
            dialog.update_with_profile_policy(Message::Profiles(event), None, || ProfilePolicy {
                create: false,
                edit: true,
            })
        };
        let _ = send(&mut dialog, ProfileMessage::Add);
        let _ = send(&mut dialog, ProfileMessage::Wizard(WizardAction::Open));
        assert!(!dialog.profile_wizard_open());
        assert!(!dialog.can_open_profile_wizard());
        assert!(!dialog.dirty());
        let _ = send(
            &mut dialog,
            ProfileMessage::Edit(Field::Name, "Allowed saved profile edit".into()),
        );
        assert_eq!(
            dialog.profiles.selected().unwrap().name,
            "Allowed saved profile edit"
        );
        assert!(dialog.dirty());
    }

    #[test]
    fn ai_profile_edit_policy_preserves_creation_and_checks_wizard_completion() {
        use profiles::{Field, Message as ProfileMessage, WizardAction};

        let (_scratch, mut dialog) = default_profile_dialog();
        let send = |dialog: &mut Dialog, event, create| {
            dialog.update_with_profile_policy(Message::Profiles(event), None, || ProfilePolicy {
                create,
                edit: false,
            })
        };
        let _ = send(&mut dialog, ProfileMessage::Select(2), true);
        assert_eq!(dialog.profile_form_lock(), Some(AI_PROFILE_EDIT_DISABLED));
        for event in [
            ProfileMessage::Edit(Field::Name, "Denied edit".into()),
            ProfileMessage::SetDefault(2),
            ProfileMessage::Delete,
            ProfileMessage::ConfirmDelete,
            ProfileMessage::Save,
        ] {
            assert!(iced_runtime::task::into_stream(send(&mut dialog, event, true)).is_none());
        }
        assert_eq!(dialog.profiles.selected().unwrap().name, "Saved B");
        assert!(!dialog.dirty());

        let _ = send(
            &mut dialog,
            ProfileMessage::Wizard(WizardAction::Open),
            true,
        );
        assert!(dialog.profile_wizard_open());
        assert!(dialog.profile_form_lock().is_none());
        let _ = send(
            &mut dialog,
            ProfileMessage::Wizard(WizardAction::Next),
            true,
        );
        let _ = send(
            &mut dialog,
            ProfileMessage::Edit(Field::Model, "wizard-fixture".into()),
            true,
        );
        for _ in 0..2 {
            let _ = send(
                &mut dialog,
                ProfileMessage::Wizard(WizardAction::Next),
                true,
            );
        }
        let candidate = dialog.profiles.selected().unwrap().id;
        let _ = send(
            &mut dialog,
            ProfileMessage::Wizard(WizardAction::Finish),
            false,
        );
        assert_eq!(
            dialog.profile_form_lock(),
            Some(AI_PROFILE_CREATION_DISABLED)
        );
        assert!(dialog.profile_wizard_open());
        assert!(dialog.profiles.catalog().profile(candidate).is_none());
        let _ = send(
            &mut dialog,
            ProfileMessage::Wizard(WizardAction::Finish),
            true,
        );
        assert!(!dialog.profile_wizard_open());
        assert!(dialog.profiles.catalog().profile(candidate).is_some());
        let _ = send(
            &mut dialog,
            ProfileMessage::Wizard(WizardAction::Open),
            true,
        );
        assert!(dialog.profile_wizard_open());
        let _ = send(
            &mut dialog,
            ProfileMessage::Wizard(WizardAction::Cancel),
            false,
        );
        assert!(!dialog.profile_wizard_open());

        let _ = send(&mut dialog, ProfileMessage::Add, true);
        let new = dialog.profiles.selected().unwrap().id;
        let _ = send(
            &mut dialog,
            ProfileMessage::Edit(Field::Name, "Allowed new profile".into()),
            true,
        );
        let _ = send(&mut dialog, ProfileMessage::SetDefault(new), true);
        assert_eq!(
            dialog.profiles.selected().unwrap().name,
            "Allowed new profile"
        );
        assert_eq!(dialog.profiles.catalog().default_profile().unwrap().id, new);
    }

    #[test]
    fn ai_manager_default_profile_saves_immediately_without_committing_drafts() {
        use iced::futures::{StreamExt, executor::block_on};
        use profiles::{Field, Message as ProfileMessage};

        let (_scratch, mut dialog) = default_profile_dialog();
        let _ = dialog.update(Message::Profiles(ProfileMessage::Edit(
            Field::Name,
            "Draft A".into(),
        )));
        let _ = dialog.update(Message::Timeout("37".into()));
        let _ = dialog.update(Message::Profiles(ProfileMessage::Select(2)));
        assert!(
            iced_runtime::task::into_stream(
                dialog.update(Message::Profiles(ProfileMessage::SetDefault(1)))
            )
            .is_none()
        );
        let task = dialog.update(Message::Profiles(ProfileMessage::SetDefault(2)));
        assert!(dialog.busy());
        assert!(iced_runtime::task::into_stream(dialog.update(Message::Save)).is_none());
        let mut outcomes = iced_runtime::task::into_stream(task).unwrap();
        let Some(iced_runtime::Action::Output(event)) = block_on(outcomes.next()) else {
            panic!("expected the default profile save result");
        };
        assert!(
            matches!(event, Message::ProfileDefaultSaved(Ok(_))),
            "{event:?}"
        );
        let _ = dialog.update(event);
        assert!(!dialog.busy());
        assert_eq!(dialog.notice, "Default AI profile saved.");

        let persisted = Configuration::load_from(&dialog.profile_dir).unwrap();
        assert_eq!(persisted.profiles.default_profile().unwrap().id, 2);
        assert_eq!(persisted.profiles.profile(1).unwrap().name, "Saved A");
        assert_eq!(persisted.general.timeout_minutes, 1);
        assert_eq!(
            dialog.profiles.catalog().profile(1).unwrap().name,
            "Draft A"
        );
        assert_eq!(dialog.timeout, "37");
        assert_eq!(dialog.profiles.catalog().default_profile().unwrap().id, 2);
        assert!(dialog.dirty());

        let _ = dialog.update(Message::Profiles(ProfileMessage::Add));
        let new = dialog.profiles.selected().unwrap().id;
        let _ = dialog.update(Message::Profiles(ProfileMessage::Edit(
            Field::Model,
            "fixture".into(),
        )));
        assert!(
            iced_runtime::task::into_stream(
                dialog.update(Message::Profiles(ProfileMessage::SetDefault(new)))
            )
            .is_none()
        );
        assert_eq!(dialog.profiles.catalog().default_profile().unwrap().id, new);
        assert_eq!(
            Configuration::load_from(&dialog.profile_dir).unwrap(),
            persisted
        );
        let mut outcomes = iced_runtime::task::into_stream(dialog.update(Message::Save)).unwrap();
        let Some(iced_runtime::Action::Output(event)) = block_on(outcomes.next()) else {
            panic!("expected the explicit settings save result");
        };
        assert!(matches!(event, Message::Saved(Ok(_))));
        let _ = dialog.update(event);
        assert!(!dialog.dirty());
        let persisted = Configuration::load_from(&dialog.profile_dir).unwrap();
        assert_eq!(persisted.profiles.default_profile().unwrap().id, new);
        assert_eq!(persisted.profiles.profile(1).unwrap().name, "Draft A");
        assert_eq!(persisted.general.timeout_minutes, 37);
    }

    #[test]
    fn ai_manager_default_profile_refuses_a_stale_saved_configuration() {
        use iced::futures::{StreamExt, executor::block_on};
        use profiles::Message as ProfileMessage;

        let (_scratch, mut dialog) = default_profile_dialog();
        let mut external = dialog.saved.as_ref().clone();
        external.general.timeout_minutes = 19;
        external
            .save_against(&dialog.profile_dir, &dialog.saved)
            .unwrap();
        let _ = dialog.update(Message::Profiles(ProfileMessage::Select(2)));
        let task = dialog.update(Message::Profiles(ProfileMessage::SetDefault(2)));
        let mut outcomes = iced_runtime::task::into_stream(task).unwrap();
        let Some(iced_runtime::Action::Output(event)) = block_on(outcomes.next()) else {
            panic!("expected the stale settings failure");
        };
        assert!(matches!(event, Message::ProfileDefaultSaved(Err(_))));
        let _ = dialog.update(event);
        assert!(!dialog.busy());
        assert!(
            dialog.notice.contains("AI settings changed"),
            "{}",
            dialog.notice
        );
        assert_eq!(dialog.profiles.selected().unwrap().id, 2);
        assert_eq!(dialog.profiles.catalog().default_profile().unwrap().id, 1);
        assert_eq!(
            Configuration::load_from(&dialog.profile_dir).unwrap(),
            external
        );
    }

    #[test]
    fn ai_manager_loads_recorded_quota_for_the_current_profile_and_period() {
        use iced::futures::{StreamExt, executor::block_on};
        use profiles::{Message as ProfileMessage, Section, UsageField};

        fn completed(task: Task<Message>, expected: (u64, u16)) -> Message {
            let mut outcomes = iced_runtime::task::into_stream(task).expect("scheduled usage read");
            while let Some(action) = block_on(outcomes.next()) {
                if let iced_runtime::Action::Output(Message::Profiles(
                    ProfileMessage::UsageLoaded(result),
                )) = action
                {
                    assert_eq!(result.reference, ProfileRef::Numeric(expected.0));
                    assert_eq!(result.reset_days, expected.1);
                    let snapshot = result.result.as_ref().expect("private ledger snapshot");
                    assert_eq!(
                        snapshot.next_day - snapshot.start_day,
                        i32::from(expected.1)
                    );
                    return Message::Profiles(ProfileMessage::UsageLoaded(result));
                }
            }
            panic!("missing usage result");
        }

        let (_scratch, mut dialog) = default_profile_dialog();
        let first = dialog.update(Message::Profiles(ProfileMessage::Section(Section::Limits)));
        assert!(
            iced_runtime::task::into_stream(
                dialog.update(Message::Profiles(ProfileMessage::Select(2)))
            )
            .is_none()
        );
        assert!(
            iced_runtime::task::into_stream(dialog.update(Message::Profiles(
                ProfileMessage::EditUsage(UsageField::Days, "7".into())
            )))
            .is_none()
        );
        let next = dialog.update(completed(first, (1, 30)));
        let _ = dialog.update(completed(next, (2, 7)));

        let pending = dialog.update(Message::Profiles(ProfileMessage::LoadUsage));
        let _ = dialog.update(Message::Page(Page::General));
        assert!(
            iced_runtime::task::into_stream(dialog.update(completed(pending, (2, 7)))).is_none()
        );
        let reopened = dialog.update(Message::Page(Page::Profiles));
        let _ = dialog.update(completed(reopened, (2, 7)));
    }

    #[test]
    fn skill_markdown_chooser_escape_defers_to_an_existing_close_confirmation() {
        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        let _ = dialog.update(Message::Page(Page::Skills));
        for action in [skills::Message::Import, skills::Message::Export] {
            let _ = dialog.update(Message::Skills(action));
            assert!(dialog.skills.file_prompt_open());
            dialog.prompt_close(Some(7));
            assert!(matches!(dialog.escape_message(), Message::Close));
            assert!(dialog.skills.file_prompt_open());

            dialog.prompt_close(None);
            let escape = dialog.escape_message();
            assert!(matches!(escape, Message::Skills(skills::Message::Cancel)));
            let _ = dialog.update(escape);
            assert!(!dialog.skills.file_prompt_open());
            assert!(matches!(dialog.escape_message(), Message::Close));
        }
    }

    #[test]
    fn profile_wizard_blocks_parent_navigation_and_ignores_a_cancelled_request() {
        use crate::local_ai::test_support::{accept, exclusive_run, listener};

        let _exclusive = exclusive_run();
        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        let _ = dialog.update(Message::Timeout("12".into()));
        let _ = dialog.update(Message::Profiles(profiles::Message::Add));
        let existing = dialog.profiles.selected().unwrap().id;
        let (listener, _) = listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let _ = dialog.update(Message::Profiles(profiles::Message::Edit(
            profiles::Field::Endpoint,
            endpoint.clone(),
        )));
        let _parent_request = dialog.update(Message::Profiles(profiles::Message::Discover));
        let parent_ticket = dialog.request.as_ref().unwrap().ticket;
        let parent_connection = accept(&listener);
        dialog.open_profile_wizard_for_test();
        assert!(!dialog.can_open_profile_wizard());
        assert!(!dialog.profile_wizard_open());
        assert_eq!(dialog.request.as_ref().unwrap().ticket, parent_ticket);
        assert_eq!(dialog.profiles.selected().unwrap().id, existing);
        let _ = dialog.update(Message::CancelRequest);
        drop(parent_connection);

        dialog.open_profile_wizard_for_test();
        let candidate = dialog.profiles.selected().unwrap().id;
        assert_ne!(candidate, existing);

        let _ = dialog.update(Message::Profiles(profiles::Message::Edit(
            profiles::Field::Endpoint,
            endpoint,
        )));
        let _ = dialog.update(Message::Page(Page::General));
        let _ = dialog.update(Message::Search("memory".into()));
        let _ = dialog.update(Message::Reveal(search::Target::Page(Page::Memory)));
        let _ = dialog.update(Message::Profiles(profiles::Message::Add));
        let _ = dialog.update(Message::Profiles(profiles::Message::Delete));
        let _ = dialog.update(Message::Profiles(profiles::Message::ConfirmDelete));
        let _ = dialog.update(Message::Save);
        assert_eq!(dialog.page, Page::Profiles);
        assert!(dialog.search.is_empty());
        assert_eq!(dialog.profiles.selected().unwrap().id, candidate);
        assert!(dialog.profiles.catalog().profile(existing).is_some());
        assert!(!dialog.saving);

        let _request = dialog.update(Message::Profiles(profiles::Message::Discover));
        let ticket = dialog.request.as_ref().unwrap().ticket;
        let _connection = accept(&listener);
        dialog.prompt_close(Some(9));
        assert!(matches!(dialog.escape_message(), Message::Close));
        assert!(dialog.profile_wizard_open());
        dialog.prompt_close(None);
        let cancel = dialog.escape_message();
        let _ = dialog.update(cancel);
        assert!(!dialog.profile_wizard_open());
        assert!(dialog.request.is_none());
        assert_eq!(dialog.timeout, "12");
        assert_eq!(dialog.profiles.selected().unwrap().id, existing);
        assert!(dialog.profiles.catalog().profile(candidate).is_none());

        dialog.open_profile_wizard_for_test();
        let next = dialog.profiles.selected().unwrap().clone();
        assert_ne!(next.id, candidate);
        let _ = dialog.update(Message::RequestDone(
            ticket,
            Ok(Reply::Models(vec!["stale-model".into()])),
        ));
        assert!(dialog.profiles.selected() == Some(&next));
        assert!(dialog.notice.is_empty());
    }

    #[test]
    fn code_text_language_defaults_to_preserving_and_survives_config_round_trip() -> io::Result<()>
    {
        let old = Configuration::decode(br#"{"general":{"timeout_minutes":2}}"#)?;
        assert_eq!(old.code_text_language(), CodeTextLanguage::Auto);
        assert_eq!(old.code_text_language().explicit_code(), None);

        let mut dialog = Dialog::new(
            Arc::new(old.clone()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        let _ = dialog.update(Message::CodeTextLanguage(CodeTextLanguage::German));
        assert!(dialog.dirty());
        assert_eq!(old.code_text_language(), CodeTextLanguage::Auto);
        let changed = Configuration {
            general: dialog.general,
            ..old
        };
        let restored = Configuration::decode(&serde_json::to_vec(&changed)?)?;
        assert_eq!(restored.code_text_language().explicit_code(), Some("de"));
        assert!(
            search::results("script comments language", None).any(|target| {
                target == search::Target::Setting(search::Setting::CodeTextLanguage)
            })
        );
        assert!(
            Configuration::decode(br#"{"general":{"code_text_language":"en\ntext"}}"#).is_err()
        );
        Ok(())
    }

    #[test]
    fn managed_configuration_is_available_to_coding_without_an_external_endpoint() {
        let config = Configuration::decode(
            br#"{
            "profiles": {
                "profiles": [{
                    "id": 1,
                    "name": "Private GGUF",
                    "connection_mode": "managed",
                    "endpoint": "",
                    "model_mode": "manual",
                    "vision": "disabled",
                    "model": "0123456789abcdef0123456789abcdef"
                }],
                "default": 1,
                "last_id": 1
            }
        }"#,
        )
        .unwrap();
        let choices: Vec<_> = config.available_ai_profiles(ProfileRoute::Local).collect();
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].0, 1);
        assert!(choices[0].3.contains("Managed"));
        assert!(config.code_analysis_transport(1).is_ok());
        let drawing = config.drawing_profile(true).unwrap();
        assert!(drawing.endpoint.is_none());
        assert!(drawing.transport.is_ok());
    }

    #[test]
    fn long_model_notice_preserves_the_page_and_footer_actions() {
        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        dialog.notice = format!(
            "Model responded successfully ({}).",
            "W".repeat(local_ai::MAX_MODEL_BYTES)
        );
        let mut view = dialog.view(Vec::new(), Vec::new());
        let mut tree = Tree::new(view.as_widget());
        let renderer = Renderer::new(Font::DEFAULT, Pixels(14.0));
        let size = Size::new(600.0, 190.0);
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let mut children = Layout::new(&node).children();
        let content = children.nth(2).unwrap().bounds();
        let footer = children.next().unwrap();
        let mut footer_items = footer.children();
        let notice = footer_items.next().unwrap().children().next().unwrap();
        let notice_text = notice.children().next().unwrap().bounds();
        assert!(notice_text.height > notice.bounds().height);
        assert!(notice.bounds().height <= MAX_NOTICE_HEIGHT);
        assert!(content.height > 24.0, "page bounds: {content:?}");
        let actions: Vec<_> = footer_items
            .map(|item| item.bounds())
            .filter(|bounds| bounds.width > 0.0)
            .collect();
        assert_eq!(actions.len(), 2);
        for action in actions {
            assert!(action.height > 0.0);
            assert!(action.y >= footer.bounds().y);
            assert!(action.x + action.width <= size.width);
            assert!(action.y + action.height <= size.height);
        }
    }

    #[test]
    fn configured_roles_round_trip_and_require_vision_for_screenshots() -> io::Result<()> {
        let original = Configuration::decode(br#"{
            "profiles": {
                "profiles": [
                    {"id": 1, "name": "Text", "endpoint": "127.0.0.1:11434", "model": "text"},
                    {"id": 2, "name": "Code", "endpoint": "127.0.0.1:11435", "model": "code", "vision": "disabled"}
                ],
                "default": 1,
                "last_id": 2
            },
            "local": {"roles": [null, 2, null, 2]}
        }"#)?;
        let serialized = serde_json::to_vec(&original)?;
        let restored = Configuration::decode(&serialized)?;
        assert_eq!(original, restored);
        assert_eq!(restored.drawing_profile(false).unwrap().model, "text");
        assert_eq!(restored.drawing_profile(true).unwrap().model, "code");
        assert!(matches!(restored.journal_profile(false), Some(Ok(_))));
        assert!(matches!(restored.journal_profile(true), Some(Err(_))));
        Ok(())
    }

    #[test]
    fn text_knowledge_is_opt_in_and_does_not_follow_a_reassigned_role() {
        let profile = crate::snippets::tests::Profile::new();
        let mut configuration = serde_json::json!({
            "profiles": {
                "profiles": [
                    {"id": 1, "name": "Text", "endpoint": "127.0.0.1:11434", "model": "text"},
                    {"id": 2, "name": "Code", "endpoint": "127.0.0.1:11435", "model": "code"},
                    {"id": 3, "name": "Other text", "endpoint": "127.0.0.1:11436", "model": "other"}
                ],
                "default": 1,
                "last_id": 3
            },
            "local": {
                "roles": [1, 2, null, null],
                "embedding": "embedding",
                "stores": [{
                    "name": "Guide",
                    "sources": [profile.0.join("guide.txt")],
                    "coding_profile": 2
                }]
            }
        });
        let decode = |value: &serde_json::Value| {
            Configuration::decode(&serde_json::to_vec(value).unwrap()).unwrap()
        };
        let legacy = decode(&configuration);
        assert!(legacy.text_knowledge(1, &profile.0).is_empty());
        assert_eq!(legacy.coding_knowledge(2, &profile.0).len(), 1);
        assert!(legacy.autonomous_knowledge(2, &profile.0).is_empty());

        configuration["local"]["stores"][0]["text_profile"] = 1.into();
        let bound = decode(&configuration);
        let saved = serde_json::to_value(&bound).unwrap();
        assert_eq!(saved["local"]["stores"][0]["text_profile"], 1);
        let reopened = decode(&saved);
        assert_eq!(reopened.text_knowledge(1, &profile.0).len(), 1);
        assert_eq!(reopened.coding_knowledge(2, &profile.0).len(), 1);
        assert!(reopened.text_knowledge(2, &profile.0).is_empty());

        for role in [serde_json::Value::Null, 3.into()] {
            configuration["local"]["roles"][0] = role;
            let reassigned = decode(&configuration);
            for id in [1, 2, 3] {
                assert!(reassigned.text_knowledge(id, &profile.0).is_empty());
            }
            assert_eq!(reassigned.coding_knowledge(2, &profile.0).len(), 1);
        }
        configuration["local"]["stores"][0]["text_profile"] = 0.into();
        assert!(Configuration::decode(&serde_json::to_vec(&configuration).unwrap()).is_err());
    }

    #[test]
    fn autonomous_knowledge_guards_embedding_and_binds_durable_source_evidence() {
        use std::io::Write as _;
        use std::sync::atomic::{AtomicBool, Ordering};

        use crate::local_ai::test_support;

        let fixture = crate::snippets::tests::Profile::new();
        let root = &fixture.0;
        let source = root.join("guide.txt");
        std::fs::write(&source, "Project handover is on Tuesday.").unwrap();
        let (listener, _) = test_support::listener();
        let settings = |autonomous_enabled| {
            Configuration::decode(&serde_json::to_vec(&serde_json::json!({
                "profiles": {"profiles": [{
                    "id": 1, "name": "Agent", "model": "fixture", "endpoint": "127.0.0.1:11434"
                }], "default": 1, "last_id": 1},
                "local": {
                    "endpoint": listener.local_addr().unwrap().to_string(),
                    "embedding": "fixture",
                    "stores": [{"name": "Guide", "sources": [&source], "autonomous_enabled": autonomous_enabled}]
                }
            })).unwrap()).unwrap()
        };
        let disabled = settings(false);
        assert!(disabled.autonomous_knowledge(1, root).is_empty());
        let opted_in = settings(true);
        opted_in
            .save_fixture_at(root, &Configuration::default())
            .unwrap();
        let saved = Configuration::load_from(root).unwrap();
        assert!(saved.text_knowledge(1, root).is_empty());
        assert!(saved.coding_knowledge(1, root).is_empty());
        assert!(saved.autonomous_knowledge(2, root).is_empty());
        let plan = saved.autonomous_knowledge(1, root).pop().unwrap();
        assert!(plan.supports_standing_review());

        let listener_fixture = listener.try_clone().unwrap();
        let worker = std::thread::spawn(move || {
            let mut stream = test_support::accept(&listener_fixture);
            let body = test_support::wire_request(&mut stream, "POST /api/embed HTTP/1.1");
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()["input"],
                serde_json::json!(["Project handover is on Tuesday."])
            );
            let response =
                test_support::fixture_http_response("200 OK", "", r#"{"embeddings":[[1,0]]}"#);
            stream.get_mut().write_all(response.as_bytes()).unwrap();
        });
        plan.index_for_test().unwrap();
        worker.join().unwrap();

        let revoked = Arc::new(AtomicBool::new(false));
        let during_mask = Arc::clone(&revoked);
        let attempt = plan
            .clone()
            .start_masked_guarded(
                "When?".into(),
                4096,
                None,
                move |field| {
                    during_mask.store(true, Ordering::Release);
                    Ok(field.to_owned())
                },
                move || {
                    if revoked.load(Ordering::Acquire) {
                        Err("Request review revoked".into())
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap();
        assert!(
            attempt
                .reply
                .recv_blocking()
                .unwrap()
                .unwrap_err()
                .contains("revoked")
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(attempt);

        let listener_fixture = listener.try_clone().unwrap();
        let worker = std::thread::spawn(move || {
            let mut stream = test_support::accept(&listener_fixture);
            let body = test_support::wire_request(&mut stream, "POST /api/embed HTTP/1.1");
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()["input"],
                serde_json::json!(["When?"])
            );
            let response =
                test_support::fixture_http_response("200 OK", "", r#"{"embeddings":[[1,0]]}"#);
            stream.get_mut().write_all(response.as_bytes()).unwrap();
        });
        let attempt = plan
            .clone()
            .for_standing_review()
            .start("When?".into(), 4096, None)
            .unwrap();
        let result = attempt.reply.recv_blocking().unwrap().unwrap();
        worker.join().unwrap();
        assert!(result.context().contains("Tuesday"));
        let serialized = serde_json::to_vec(&result.authority().unwrap()).unwrap();
        let authority: KnowledgeAuthority = serde_json::from_slice(&serialized).unwrap();
        plan.require_authority(&authority, || Ok(())).unwrap();
        result
            .require_current_guarded(|| Err("Request cancelled".into()))
            .unwrap_err();
        std::fs::write(&source, "Project handover is on Friday.").unwrap();
        assert!(plan.require_authority(&authority, || Ok(())).is_err());
        std::fs::write(&source, "Project handover is on Tuesday.").unwrap();
        plan.require_authority(&authority, || Ok(())).unwrap();
        disabled.save_fixture_at(root, &saved).unwrap();
        assert!(plan.require_authority(&authority, || Ok(())).is_err());
        assert!(result.require_current_guarded(|| Ok(())).is_err());
    }

    #[test]
    fn config_rejects_remote_profiles_and_unknown_versions() {
        assert!(Configuration::decode(br#"{"version":2}"#).is_err());
        assert!(Configuration::decode(br#"{
            "profiles": {
                "profiles":[{"id":1,"name":"Remote","endpoint":"https://example.invalid/v1","model":"x"}],
                "default":1,"last_id":1
            }
        }"#).is_err());
    }

    #[test]
    fn terminal_agent_role_uses_explicit_fallback_and_rejects_remote_profiles() {
        let mut value = serde_json::json!({
            "profiles": {
                "profiles": [
                    {"id":1,"name":"Default","model":"default"},
                    {"id":2,"name":"Coding","model":"code"},
                    {"id":3,"name":"Agent","model":"agent"},
                    {"id":4,"name":"Remote","connection_mode":"remote-open-ai","endpoint":"https://example.invalid/v1","model":"remote","trusted_remote_endpoint":true}
                ],
                "default":1,"last_id":4
            },
            "local": {"roles":[null,2,3,null]}
        });
        for (roles, expected) in [
            (serde_json::json!([null, 2, 3, null]), Some(3)),
            (serde_json::json!([null, 2, null, null]), Some(2)),
            (serde_json::json!([null, null, null, null]), Some(1)),
            (serde_json::json!([null, 2, 4, null]), None),
            (serde_json::json!([null, 2, 19, null]), Some(2)),
        ] {
            value["local"]["roles"] = roles;
            let configuration =
                Configuration::decode(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert_eq!(configuration.agent_profile_id(), expected);
            assert_eq!(configuration.agent_profiles().count(), 3);
            assert!(configuration.agent_transport(4).is_err());
            assert!(configuration.agent_transport(3).is_ok());
        }
    }

    #[test]
    fn configured_cli_roles_preserve_other_profiles_and_named_policy_fallback() {
        let scratch = crate::snippets::tests::Profile::new();
        let root = scratch.0.canonicalize().unwrap();
        let mut value = serde_json::json!({
            "profiles": {"profiles": [
                {"id": 1, "name": "Local", "model": "local"},
                {"id": 2, "name": "CLI", "connection_mode": "cli", "endpoint": "",
                    "vision": "disabled", "cli": {
                        "path": root.join(format!("cli{}", std::env::consts::EXE_SUFFIX)),
                        "arguments": ["{stdinPrompt}"], "fingerprint": vec![42_u8; 32]
                    }}
            ], "default": 1, "last_id": 2},
            "local": {"roles": [2, 2, 99, 2]}
        });
        let draft: Configuration = serde_json::from_value(value.clone()).unwrap();
        draft
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let saved = Configuration::load_fixture_from(&root).unwrap();
        assert_eq!(saved.preferred_terminal_chat_cli_id(), Some(2));
        assert_eq!(saved.preferred_agent_cli_id(), Some(2));
        assert_eq!(saved.preferred_journal_summary_cli_id(), Some(2));
        assert!(saved.journal_profile(false).is_none());
        assert_eq!(saved.code_analysis_profile_id(), Some(1));
        assert!(saved.agent_transport(2).is_err());
        let mut catalog = saved.profiles.clone();
        assert!(catalog.set_default(2));
        assert_eq!(catalog.default_profile().unwrap().id, 2);

        value["local"]["roles"] = serde_json::json!([1, 2, "policy-named", 1]);
        let mut without_policy: Configuration = serde_json::from_value(value).unwrap();
        without_policy.origin = Some(root);
        assert!(without_policy.has_named_agent_profile());
        assert_eq!(without_policy.preferred_agent_cli_id(), None);
        assert_eq!(without_policy.preferred_terminal_chat_cli_id(), None);
        assert_eq!(without_policy.preferred_journal_summary_cli_id(), None);
    }

    #[test]
    fn remote_default_is_available_to_reviewed_chat_and_excluded_from_local_routes() {
        let config = Configuration::decode(br#"{
            "profiles": {
                "profiles":[
                    {"id":1,"name":"Remote","connection_mode":"remote-open-ai","endpoint":"https://example.invalid/v1","model":"remote","trusted_remote_endpoint":true},
                    {"id":2,"name":"Local","model":"local"}
                ],
                "default":1,"last_id":2
            }
        }"#).unwrap();
        assert_eq!(config.terminal_chat_profile_id(), Some(1));
        assert!(config.terminal_chat_transport(1).is_ok());
        assert_eq!(config.terminal_chat_profiles().count(), 2);
        assert_eq!(config.code_analysis_profile_id(), Some(2));
        assert!(config.code_analysis_transport(1).is_err());
        assert_eq!(config.available_ai_profiles(ProfileRoute::Local).count(), 1);
        assert_eq!(config.drawing_profile(false).unwrap().model, "local");
    }

    #[test]
    fn journal_note_translation_uses_text_then_default_and_requires_saved_outbound_profiles() {
        let mut data = serde_json::json!({
            "profiles": {
                "profiles": [
                    {"id":1,"name":"Text","model":"text"},
                    {"id":2,"name":"Default","model":"default"},
                    {"id":3,"name":"Journal","model":"journal"},
                    {"id":4,"name":"Remote","connection_mode":"remote-open-ai",
                        "endpoint":"https://example.invalid/v1","model":"remote",
                        "trusted_remote_endpoint":true}
                ],
                "default":2,"last_id":4
            },
            "local": {"roles":[1,null,null,3]}
        });
        let assert_selected = |data: &serde_json::Value, id| {
            let config = Configuration::decode(data.to_string().as_bytes()).unwrap();
            let expected = config
                .transport_for(config.profiles.profile(id).unwrap())
                .unwrap();
            assert!(
                config
                    .journal_note_translation_profile()
                    .unwrap()
                    .unwrap()
                    .transport
                    == expected
            );
        };
        assert_selected(&data, 1);
        data["local"]["roles"][0] = serde_json::Value::Null;
        assert_selected(&data, 2);
        data["local"]["roles"][0] = serde_json::json!(4);
        let config = Configuration::decode(data.to_string().as_bytes()).unwrap();
        assert!(config.journal_note_translation_profile().unwrap().is_err());
        let scratch = crate::snippets::tests::Profile::new();
        let root = scratch.0.canonicalize().unwrap();
        config
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let saved = Configuration::load_from(&root).unwrap();
        let selected = saved.journal_note_translation_profile().unwrap().unwrap();
        assert_eq!(selected.id, 4);
        assert!(selected.transport.is_remote());
        assert!(
            Configuration::default()
                .journal_note_translation_profile()
                .is_none()
        );
    }

    #[test]
    fn saved_profile_transport_records_usage_in_its_own_profile_root() {
        use crate::local_ai::test_support::{
            accept, exclusive_run, fixture_http_response, listener, request,
        };
        use std::io::Write as _;

        let _exclusive = exclusive_run();
        let (listener, _) = listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let config: Configuration = serde_json::from_value(serde_json::json!({
            "profiles": {
                "profiles": [{"id": 5, "name": "Fixture", "endpoint": endpoint, "model": "fixture", "usage_budget": {"token_limit": 50}}],
                "default": 5, "last_id": 5
            }
        })).unwrap();
        config
            .save_against(&root, &Configuration::default())
            .unwrap();
        let loaded = Configuration::load_from(&root).unwrap();
        let transport = loaded.terminal_chat_transport(5).unwrap().0;
        let worker = std::thread::spawn(move || {
            let mut socket = accept(&listener);
            let _ = request(&mut socket);
            let response = fixture_http_response(
                "200 OK",
                "",
                r#"{"choices":[{"message":{"content":"Answer"}}],"usage":{"prompt_tokens":9,"completion_tokens":4}}"#,
            );
            socket.get_mut().write_all(response.as_bytes()).unwrap();
        });
        let client = local_ai::Client::new(transport);
        assert_eq!(client.complete("System", "Question").unwrap(), "Answer");
        assert!(client.take_usage().is_some());
        assert!(client.take_usage().is_none());
        worker.join().unwrap();
        let profile = loaded.profiles.profile(5).unwrap();
        let account = loaded
            .profiles
            .usage_account(profile, &root, profile.usage_budget.clone())
            .unwrap();
        let usage = account.snapshot().unwrap();
        assert_eq!(usage.counts.requests, 1);
        assert_eq!(usage.counts.input_tokens, 9);
        assert_eq!(usage.counts.output_tokens, 4);
        assert_eq!(usage.in_flight, 0);
        assert!(root.join("ai-usage").join("5.json").is_file());
        assert!(
            serde_json::to_value(&loaded)
                .unwrap()
                .get("origin")
                .is_none()
        );
    }
}
