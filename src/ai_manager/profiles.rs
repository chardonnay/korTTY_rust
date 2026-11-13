//! Saved AI profiles and the draft owned by the native manager.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use iced::Task;
pub(super) use kortty_jobs::ProfileRef;
use serde::{Deserialize, Serialize};

use crate::clipboard_policy::{
    ClipboardPolicy, PolicyAiProfile, PolicyModelRuntime, valid_policy_ai_profile_id,
    valid_policy_model_id,
};
use crate::local_ai::{
    self, ProfileOptions, managed_models, mlx_models, mlx_runtime, policy_models,
};

use super::ProfileId;
use super::model_inventory::ModelInventory;

mod cli_profile;
mod cloud;
pub(super) mod java_backup;
mod managed;
pub(super) mod picker;
mod usage_view;
mod view;
mod web;
mod wizard;

pub(super) use cli_profile::Action as CliAction;
#[cfg(test)]
pub(super) use usage_view::Field as UsageField;
pub(super) use usage_view::Loaded as UsageLoaded;
pub(super) use wizard::Action as WizardAction;
use wizard::Wizard;

const MAX_PROFILES: usize = 128;
const MAX_NAME_BYTES: usize = 160;
const MAX_ENDPOINT_BYTES: usize = 2048;
const MAX_TIMEOUT_MINUTES: u32 = 1440;
const MAX_SELECTION_CHARS: u32 = 50_000_000;
const MANAGED_DESTINATION: &str = "Managed llama.cpp on this computer";
const MLX_DESTINATION: &str = "Managed Apple MLX on this computer";

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Catalog {
    profiles: Vec<Profile>,
    default: Option<u64>,
    last_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    policy_default: Option<String>,
    #[serde(skip)]
    policy_profiles: Vec<Profile>,
}

impl PartialEq for Catalog {
    fn eq(&self, other: &Self) -> bool {
        self.profiles == other.profiles
            && self.default == other.default
            && self.last_id == other.last_id
            && self.policy_default == other.policy_default
    }
}

impl Eq for Catalog {}

impl Catalog {
    pub(super) fn ensure_guided_model_profile(
        &mut self,
        model: &managed_models::Model,
    ) -> Result<ProfileRef, String> {
        if let Some(existing) = self.profiles.iter().find(|profile| {
            profile.connection_mode == ConnectionMode::Managed && profile.model == model.id
        }) {
            return Ok(existing.reference());
        }
        if self.profiles.len() >= MAX_PROFILES {
            return Err("The AI profile limit has been reached.".into());
        }
        let id = self
            .last_id
            .checked_add(1)
            .filter(|id| !self.is_policy_reference(*id))
            .ok_or("The AI profile identifiers are exhausted.")?;
        let base = model.name.trim();
        let base = &base[..base.floor_char_boundary(base.len().min(MAX_NAME_BYTES - 24))];
        let base = if base.is_empty() {
            "Local AI model"
        } else {
            base
        };
        let name = if self
            .entries()
            .any(|profile| profile.name.eq_ignore_ascii_case(base))
        {
            format!("{base} ({id})")
        } else {
            base.to_owned()
        };
        let profile = Profile {
            id: id.into(),
            name,
            connection_mode: ConnectionMode::Managed,
            endpoint: String::new(),
            model: model.id.clone(),
            vision: Vision::Disabled,
            prompt_preset: Some(local_ai::PromptPreset::Auto),
            ..Profile::default()
        };
        profile.validate()?;
        self.profiles.push(profile);
        self.default.get_or_insert(id);
        self.last_id = id;
        Ok(ProfileRef::Numeric(id))
    }

    pub(super) fn prepare_backup(&mut self) {
        self.policy_default = None;
        for profile in &mut self.profiles {
            profile.trusted_local_endpoint = false;
            profile.trusted_remote_endpoint = false;
            profile.reviewed_agent_https = false;
            profile.credential_env.clear();
            profile.clear_web_search();
            profile.restore_review = profile.remote_protocol().is_some() || profile.is_cli();
            if let Some(cli) = &mut profile.cli {
                cli.clear_approval();
            }
        }
    }

    pub(super) fn entries(&self) -> impl Iterator<Item = &Profile> {
        self.profiles.iter().chain(&self.policy_profiles)
    }

    pub(super) fn choices(&self) -> Vec<(ProfileId, String)> {
        self.entries()
            .filter(|profile| profile.allows_local_roles())
            .filter(|profile| profile.policy_definition().is_none() || !profile.model.is_empty())
            .map(|profile| (profile.id, profile.name.clone()))
            .collect()
    }

    pub(super) fn role_choices(&self) -> Vec<(ProfileId, String)> {
        let mut choices = self.choices();
        choices.extend(
            self.entries()
                .filter(|profile| profile.is_cli() && profile.validate_connection().is_ok())
                .map(|profile| (profile.id, format!("{} · CLI", profile.name))),
        );
        choices
    }

    pub(super) fn profile(&self, id: ProfileId) -> Option<&Profile> {
        self.entries().find(|profile| profile.id == id)
    }

    pub(super) fn profile_reference(&self, id: ProfileId) -> Option<ProfileRef> {
        self.profile(id).map(Profile::reference)
    }

    pub(super) fn resolve_profile_reference(&self, reference: &ProfileRef) -> Option<ProfileId> {
        self.profile_for_reference(reference)
            .map(|profile| profile.id)
    }

    pub(super) fn profile_for_reference(&self, reference: &ProfileRef) -> Option<&Profile> {
        match reference {
            ProfileRef::Numeric(id) if self.is_policy_reference(*id) => self
                .policy_profiles
                .iter()
                .find(|profile| self.legacy_alias_for_profile(profile) == Some(*id)),
            ProfileRef::Numeric(id) => self.user_profile(ProfileId::from(*id)),
            ProfileRef::Policy(id) => self.policy_profile(id),
        }
    }

    pub(super) fn default_profile(&self) -> Option<&Profile> {
        self.policy_default
            .as_deref()
            .and_then(|id| self.policy_profile(id))
            .or_else(|| {
                self.default
                    .and_then(|id| self.user_profile(ProfileId::from(id)))
            })
            .or_else(|| self.policy_profiles.first())
    }

    fn policy_profile(&self, id: &str) -> Option<&Profile> {
        self.policy_profiles.iter().find(|profile| {
            profile
                .policy_definition()
                .is_some_and(|definition| definition.id == id)
        })
    }

    fn user_profile(&self, id: ProfileId) -> Option<&Profile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    fn is_marked_default(&self, id: ProfileId) -> bool {
        let marked = match self.policy_default.as_deref() {
            Some(policy_id) => self.policy_profile(policy_id),
            None => self.default_profile(),
        };
        marked.is_some_and(|profile| profile.id == id)
    }

    pub(super) fn set_default(&mut self, id: ProfileId) -> bool {
        let Some(profile) = self.profile(id).filter(|profile| profile.can_be_default()) else {
            return false;
        };
        if self.is_marked_default(id) {
            return false;
        }
        if let Some(definition) = profile.policy_definition() {
            self.policy_default = Some(definition.id.clone());
        } else if let Ok(id) = u64::try_from(id) {
            self.default = Some(id);
            self.policy_default = None;
        } else {
            return false;
        }
        true
    }

    pub(super) fn require_policy_changes(
        &self,
        baseline: &Self,
        policy: &ClipboardPolicy,
    ) -> io::Result<()> {
        if self.policy_default != baseline.policy_default
            && let Some(id) = &self.policy_default
        {
            let definition = self.policy_profile(id).and_then(Profile::policy_definition);
            self.require_current_definition(definition, policy)?;
        }
        if self
            .profiles
            .iter()
            .any(|profile| baseline.user_profile(profile.id).is_none())
        {
            policy.require_ai_profile_creation()?;
        }
        let edited = baseline.profiles.iter().any(|profile| {
            let Some(revised) = self.user_profile(profile.id) else {
                return true;
            };
            if profile.has_web_search()
                && !revised.has_web_search()
                && policy.problem.is_none()
                && !policy.ai_web_search_allowed
            {
                // The independent web policy can turn search off in saved profiles.
                let mut clamped = profile.clone();
                clamped.clear_web_search();
                return &clamped != revised;
            }
            profile != revised
        });
        if edited
            || (self.default != baseline.default
                && self
                    .default
                    .is_some_and(|id| baseline.user_profile(ProfileId::from(id)).is_some()))
            || (self.policy_default != baseline.policy_default
                && (self.policy_default.is_some() || self.default == baseline.default))
            || !baseline.profiles.iter().map(|profile| profile.id).eq(self
                .profiles
                .iter()
                .map(|profile| profile.id)
                .filter(|id| baseline.user_profile(*id).is_some()))
        {
            policy.require_ai_profile_edit()?;
        }
        Ok(())
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        let valid_default = match self.default {
            Some(id) => self.user_profile(ProfileId::from(id)).is_some(),
            None => self
                .profiles
                .iter()
                .all(|profile| profile.remote_protocol().is_some()),
        };
        if self.profiles.len() > MAX_PROFILES
            || !valid_default
            || self
                .policy_default
                .as_deref()
                .is_some_and(|id| !valid_policy_ai_profile_id(id))
        {
            return Err("Invalid AI profile catalog".into());
        }
        let mut ids = BTreeSet::new();
        for profile in &self.profiles {
            if profile.id == 0
                || profile.id > ProfileId::from(self.last_id)
                || !ids.insert(profile.id)
            {
                return Err("Invalid AI profile identifier".into());
            }
            profile.validate()?;
        }
        Ok(())
    }
}

impl fmt::Debug for Catalog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AI profile catalog")
            .field("profile_count", &self.profiles.len())
            .field("default", &self.default)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Profile {
    #[serde(
        serialize_with = "serialize_user_profile_id",
        deserialize_with = "deserialize_user_profile_id"
    )]
    pub(super) id: ProfileId,
    pub(super) name: String,
    #[serde(skip)]
    policy: Option<std::sync::Arc<PolicyAiProfile>>,
    #[serde(skip)]
    policy_auto_model: Option<Result<local_ai::PolicyAutoModel, String>>,
    #[serde(skip_serializing_if = "ConnectionMode::is_external")]
    pub(super) connection_mode: ConnectionMode,
    pub(super) endpoint: String,
    pub(super) model: String,
    pub(super) credential_env: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cli: Option<cli_profile::Settings>,
    pub(super) trusted_local_endpoint: bool,
    pub(super) trusted_remote_endpoint: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) restore_review: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) reviewed_agent_https: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) tavily_search: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) lm_web: Option<local_ai::web_search::McpSettings>,
    pub(super) model_mode: ModelMode,
    pub(super) vision: Vision,
    pub(super) reasoning: Reasoning,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_preset: Option<local_ai::PromptPreset>,
    pub(super) selection_limit: u32,
    pub(super) timeout_minutes: Option<u32>,
    pub(super) max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "local_ai::Tokenizer::is_default")]
    pub(super) tokenizer: local_ai::Tokenizer,
    #[serde(skip_serializing_if = "local_ai::UsageBudget::is_default")]
    pub(super) usage_budget: local_ai::UsageBudget,
}

fn serialize_user_profile_id<S: serde::Serializer>(
    id: &ProfileId,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let id = u64::try_from(*id).map_err(serde::ser::Error::custom)?;
    serializer.serialize_u64(id)
}

fn deserialize_user_profile_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<ProfileId, D::Error> {
    u64::deserialize(deserializer).map(ProfileId::from)
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            policy: None,
            policy_auto_model: None,
            connection_mode: ConnectionMode::External,
            endpoint: local_ai::DEFAULT_ENDPOINT.into(),
            model: String::new(),
            credential_env: String::new(),
            cli: None,
            trusted_local_endpoint: false,
            trusted_remote_endpoint: false,
            restore_review: false,
            reviewed_agent_https: false,
            tavily_search: false,
            lm_web: None,
            model_mode: ModelMode::Manual,
            vision: Vision::Auto,
            reasoning: Reasoning::Default,
            prompt_preset: None,
            selection_limit: 1_000_000,
            timeout_minutes: None,
            max_output_tokens: None,
            tokenizer: local_ai::Tokenizer::default(),
            usage_budget: local_ai::UsageBudget::default(),
        }
    }
}

impl Profile {
    #[cfg(test)]
    pub(super) fn effective_prompt_preset(&self) -> local_ai::PromptPreset {
        self.effective_prompt_preset_in(None, None)
    }

    pub(super) fn effective_prompt_preset_in(
        &self,
        catalog: Option<&local_ai::model_catalog::Catalog>,
        managed_name: Option<&str>,
    ) -> local_ai::PromptPreset {
        let model = self
            .policy_auto_model
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map_or(
                managed_name.unwrap_or(&self.model),
                local_ai::PolicyAutoModel::model,
            );
        let preset = self.prompt_preset.unwrap_or_default();
        match catalog {
            Some(catalog) => preset.resolve_in(model, catalog),
            None => preset.resolve(model),
        }
    }

    pub(super) fn resolved_prompt_preset(
        &self,
        catalog: Option<&local_ai::model_catalog::Catalog>,
        root: Option<&Path>,
    ) -> local_ai::PromptPreset {
        if self.prompt_preset != Some(local_ai::PromptPreset::Auto) || !self.is_managed() {
            return self.effective_prompt_preset_in(catalog, None);
        }
        let name = root.and_then(|root| {
            if self.is_mlx() {
                mlx_models::Registry::new(root).model_name(&self.model).ok()
            } else {
                managed_models::Registry::new(root.to_path_buf())
                    .model_name(&self.model)
                    .ok()
            }
        });
        self.effective_prompt_preset_in(catalog, name.as_deref())
    }

    pub(super) fn policy_definition(&self) -> Option<&PolicyAiProfile> {
        self.policy.as_deref()
    }

    pub(super) fn policy_auto_candidate(&self) -> Option<&std::sync::Arc<PolicyAiProfile>> {
        self.policy
            .as_ref()
            .filter(|definition| local_ai::Profile::supports_policy_auto_model(definition))
    }

    pub(super) fn policy_auto_pending(&self) -> bool {
        self.policy_auto_candidate().is_some() && self.policy_auto_model.is_none()
    }

    fn policy_auto_problem(&self) -> Option<&str> {
        self.policy_auto_model
            .as_ref()
            .and_then(|result| result.as_ref().err())
            .map(String::as_str)
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || !plain_text(&self.name, MAX_NAME_BYTES) {
            return Err("Enter a profile name of up to 160 bytes".into());
        }
        if self.restore_review {
            self.validate_restored_connection()?;
        } else {
            self.validate_connection()?;
        }
        self.validate_model()?;
        self.usage_budget.validate()?;
        if !(1..=MAX_SELECTION_CHARS).contains(&self.selection_limit)
            || self
                .timeout_minutes
                .is_some_and(|minutes| minutes > MAX_TIMEOUT_MINUTES)
            || self
                .max_output_tokens
                .is_some_and(|tokens| !(256..=1_048_576).contains(&tokens))
        {
            return Err("One or more AI profile limits are out of range".into());
        }
        Ok(())
    }

    pub(super) fn validate_connection(&self) -> Result<(), String> {
        if self.restore_review {
            return Err("Review this AI profile before using it after backup restore.".into());
        }
        if let Some(definition) = self.policy_definition() {
            return local_ai::Profile::policy_profile_destination(definition).map(drop);
        }
        if self.is_cli() {
            if !self.endpoint.is_empty()
                || !self.credential_env.is_empty()
                || self.trusted_local_endpoint
                || self.trusted_remote_endpoint
                || self.has_web_search()
            {
                return Err(
                    "CLI profiles do not use HTTP endpoints, credentials, or web search.".into(),
                );
            }
            return self.cli_program().map(|_| ());
        }
        if self.cli.is_some() {
            return Err("CLI approval only applies to a CLI profile.".into());
        }
        if self.is_managed() {
            if !self.endpoint.is_empty()
                || self.trusted_local_endpoint
                || self.trusted_remote_endpoint
            {
                return Err(
                    "Managed profiles use the installed runtime without a custom endpoint".into(),
                );
            }
        } else if let Some(protocol) = self.remote_protocol() {
            local_ai::remote_destination(&self.endpoint, protocol)?;
            if !self.trusted_remote_endpoint || self.trusted_local_endpoint {
                return Err(
                    "Review and trust the HTTPS destination before using this profile".into(),
                );
            }
        } else {
            if !plain_text(&self.endpoint, MAX_ENDPOINT_BYTES) {
                return Err("Enter a valid local API endpoint".into());
            }
            self.local_address()?;
            if self.trusted_remote_endpoint {
                return Err("Local profiles require local endpoint trust settings".into());
            }
        }
        let credential = self.credential()?;
        if let Some(settings) = &self.lm_web {
            if self.connection_mode != ConnectionMode::LmStudio
                || !self.trusted_local_endpoint
                || self.tavily_search
            {
                return Err(
                    "Choose one search mode and trust the local LM Studio endpoint.".into(),
                );
            }
            settings.validate()?;
            if settings.mode.uses_plugin() && credential.is_none() {
                return Err(
                    "Enter the LM Studio API token environment variable for this plugin.".into(),
                );
            }
        }
        Ok(())
    }

    fn validate_restored_connection(&self) -> Result<(), String> {
        if self.policy_definition().is_some()
            || self.trusted_local_endpoint
            || self.trusted_remote_endpoint
            || self.reviewed_agent_https
            || !self.credential_env.is_empty()
            || self.has_web_search()
        {
            return Err("An unreviewed restored AI profile cannot contain approvals.".into());
        }
        if let Some(protocol) = self.remote_protocol().filter(|_| self.cli.is_none()) {
            return local_ai::remote_destination(&self.endpoint, protocol).map(drop);
        }
        if self.is_cli() && self.endpoint.is_empty() {
            return self
                .cli
                .as_ref()
                .ok_or("The restored CLI settings are missing.")?
                .validate_restored(&self.model);
        }
        Err("Invalid unreviewed restored AI profile.".into())
    }

    fn validate_model(&self) -> Result<(), String> {
        if self.is_cli() {
            if self.model_mode != ModelMode::Manual
                || self.vision != Vision::Disabled
                || self.reasoning != Reasoning::Default
                || self.max_output_tokens.is_some()
                || (!self.model.is_empty() && !local_ai::valid_model(&self.model))
            {
                return Err(
                    "CLI profiles use a fixed text protocol and optional model argument.".into(),
                );
            }
            if !self.restore_review {
                self.cli_program()?;
            }
        } else if self.is_managed() {
            if !(managed_models::valid_id(&self.model) || valid_policy_model_id(&self.model))
                || self.model_mode != ModelMode::Manual
            {
                return Err("Choose a managed model".into());
            }
            if self.is_mlx()
                && (self.vision != Vision::Disabled || self.reasoning != Reasoning::Default)
            {
                return Err("Managed MLX profiles require text-only requests and default reasoning settings".into());
            }
            if !self.is_mlx() && self.vision == Vision::Auto {
                return Err("Choose a vision setting for the managed GGUF model".into());
            }
        } else if !plain_text(&self.model, local_ai::MAX_MODEL_BYTES)
            || self.model.trim().chars().any(char::is_whitespace)
        {
            return Err("Enter a valid model identifier".into());
        }
        Ok(())
    }

    pub(super) fn is_managed(&self) -> bool {
        self.connection_mode.is_managed()
    }

    pub(super) fn is_cli(&self) -> bool {
        self.connection_mode == ConnectionMode::Cli
    }

    fn can_be_default(&self) -> bool {
        !self.restore_review && (!self.is_cli() || self.validate_connection().is_ok())
    }

    pub(super) fn allows_local_roles(&self) -> bool {
        !self.is_cli() && self.remote_protocol().is_none()
    }

    pub(super) fn allows_reviewed_agent_https(&self) -> bool {
        self.policy.is_none()
            && self.reviewed_agent_https
            && self.trusted_remote_endpoint
            && self.remote_protocol().is_some()
    }

    fn cli_program(&self) -> Result<local_ai::cli::Program, String> {
        self.cli
            .as_ref()
            .ok_or_else(|| "Review and approve the CLI executable first.".to_owned())?
            .program(&self.model)
    }

    pub(super) fn is_mlx(&self) -> bool {
        self.connection_mode == ConnectionMode::ManagedMlx
    }

    pub(super) fn remote_protocol(&self) -> Option<local_ai::RemoteProtocol> {
        if self
            .policy_definition()
            .is_some_and(local_ai::Profile::policy_auto_model_uses_https)
        {
            return Some(local_ai::RemoteProtocol::OpenAi);
        }
        match self.connection_mode {
            ConnectionMode::RemoteOpenAi => Some(local_ai::RemoteProtocol::OpenAi),
            ConnectionMode::Anthropic => Some(local_ai::RemoteProtocol::Anthropic),
            _ => None,
        }
    }

    fn trusts_credential(&self) -> bool {
        if self.remote_protocol().is_some() {
            self.trusted_remote_endpoint
        } else {
            self.trusted_local_endpoint
        }
    }

    pub(super) fn local_address(&self) -> Result<std::net::SocketAddr, String> {
        if self.connection_mode == ConnectionMode::LmStudio {
            local_ai::lm_studio_endpoint(&self.endpoint)
        } else {
            local_ai::numeric_loopback_endpoint(&self.endpoint)
        }
    }

    pub(super) fn destination(&self) -> Result<String, String> {
        if let Some(definition) = self.policy_definition() {
            local_ai::Profile::policy_profile_destination(definition)
        } else if self.is_cli() {
            Ok(self.cli_program()?.destination())
        } else if self.is_managed() {
            Ok(if self.is_mlx() {
                MLX_DESTINATION
            } else {
                MANAGED_DESTINATION
            }
            .into())
        } else if let Some(protocol) = self.remote_protocol() {
            local_ai::remote_destination(&self.endpoint, protocol)
        } else {
            self.local_address().map(|address| {
                if self.connection_mode == ConnectionMode::LmStudio {
                    format!("http://{address}/api/v1/chat")
                } else {
                    address.to_string()
                }
            })
        }
    }

    pub(super) fn credential(&self) -> Result<Option<String>, String> {
        let name = self.credential_env.trim();
        if name.is_empty() {
            return Ok(None);
        }
        if self.is_managed() {
            return Err("Managed profiles use a temporary runtime credential".into());
        }
        if !self.trusts_credential() {
            return Err("Trust the selected endpoint before adding an API key reference".into());
        }
        if !valid_env_name(name) {
            return Err("Enter the name of an environment variable for the API key".into());
        }
        Ok(Some(name.into()))
    }

    pub(super) fn transport(&self, default_timeout: u16) -> Result<local_ai::Profile, String> {
        self.transport_with_root(default_timeout, None, None)
    }

    pub(super) fn transport_in(
        &self,
        default_timeout: u16,
        root: &Path,
        catalog: Option<&local_ai::model_catalog::Catalog>,
    ) -> Result<local_ai::Profile, String> {
        self.transport_with_root(default_timeout, catalog, Some(root))
    }

    pub(super) fn reference(&self) -> ProfileRef {
        match self.policy_definition() {
            Some(definition) => ProfileRef::Policy(definition.id.clone()),
            None => ProfileRef::Numeric(
                u64::try_from(self.id).expect("user profile IDs fit their saved representation"),
            ),
        }
    }

    fn transport_with_root(
        &self,
        default_timeout: u16,
        catalog: Option<&local_ai::model_catalog::Catalog>,
        root: Option<&Path>,
    ) -> Result<local_ai::Profile, String> {
        let minutes = self.timeout_minutes.unwrap_or(u32::from(default_timeout));
        let timeout = (minutes != 0).then(|| Duration::from_secs(u64::from(minutes) * 60));
        let root_path = || {
            root.map(Path::to_path_buf).map_or_else(
                || {
                    crate::profile::root().map_err(|error| {
                        format!("Could not open the application profile ({error})")
                    })
                },
                Ok,
            )
        };
        let preset = self.resolved_prompt_preset(catalog, root);
        if self.is_cli() {
            self.validate_connection()?;
            self.validate_model()?;
            return Ok(local_ai::Profile::configured_cli(
                self.cli_program()?,
                root_path()?,
                &self.model,
                timeout,
            )
            .with_tokenizer(self.tokenizer)
            .with_prompt_preset(preset));
        }
        let options = ProfileOptions {
            tokenizer: self.tokenizer,
            max_output_tokens: self.max_output_tokens,
            timeout,
            reasoning_effort: self.reasoning.api_value().map(str::to_owned),
            credential_env: self.credential()?,
        };
        if let Some(definition) = self.policy_definition() {
            if self.policy_auto_candidate().is_some() {
                match &self.policy_auto_model {
                    Some(Ok(model)) => local_ai::Profile::policy_profile_with_auto_model(
                        definition, model, options,
                    ),
                    Some(Err(error)) => Err(error.clone()),
                    None => Err(
                        "Waiting for the loaded model on the policy-provided LM Studio endpoint."
                            .into(),
                    ),
                }
            } else {
                local_ai::Profile::policy_profile(definition, &root_path()?, options)
            }
        } else if self.is_managed() {
            self.validate_connection()?;
            self.validate_model()?;
            if self.is_mlx() {
                local_ai::Profile::managed_mlx(root_path()?, &self.model, options)
            } else {
                local_ai::Profile::managed(root_path()?, &self.model, options)
            }
        } else if let Some(protocol) = self.remote_protocol() {
            self.validate_connection()?;
            local_ai::Profile::remote(&self.endpoint, &self.model, protocol, options)
        } else if self.connection_mode == ConnectionMode::LmStudio {
            local_ai::Profile::lm_studio(&self.endpoint, &self.model, options)
        } else {
            local_ai::Profile::configured(&self.endpoint, &self.model, options)
        }
        .map(|profile| profile.with_prompt_preset(preset))
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ConnectionMode {
    #[default]
    External,
    LmStudio,
    RemoteOpenAi,
    Anthropic,
    Managed,
    ManagedMlx,
    Cli,
}

impl ConnectionMode {
    const ALL: [Self; 6] = [
        Self::External,
        Self::Managed,
        Self::LmStudio,
        Self::RemoteOpenAi,
        Self::Anthropic,
        Self::Cli,
    ];

    const WITH_MLX: [Self; 7] = [
        Self::External,
        Self::Managed,
        Self::ManagedMlx,
        Self::LmStudio,
        Self::RemoteOpenAi,
        Self::Anthropic,
        Self::Cli,
    ];

    fn is_managed(self) -> bool {
        matches!(self, Self::Managed | Self::ManagedMlx)
    }

    fn is_external(&self) -> bool {
        *self == Self::External
    }
}

impl fmt::Display for ConnectionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::External => "Local OpenAI-compatible API",
            Self::LmStudio => "LM Studio native API",
            Self::RemoteOpenAi => "Remote OpenAI-compatible HTTPS API",
            Self::Anthropic => "Anthropic Messages HTTPS API",
            Self::Managed => "Managed llama.cpp model",
            Self::ManagedMlx => "Managed Apple MLX model",
            Self::Cli => "Configured local CLI",
        })
    }
}

fn plain_text(value: &str, max: usize) -> bool {
    value.len() <= max && !value.chars().any(char::is_control)
}

fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= 128
        && bytes
            .next()
            .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ModelMode {
    Manual,
    Auto,
}

impl ModelMode {
    const ALL: [Self; 2] = [Self::Manual, Self::Auto];
}

impl fmt::Display for ModelMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Manual => "Manual",
            Self::Auto => "Automatic",
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Vision {
    Auto,
    Enabled,
    Disabled,
}

impl Vision {
    const ALL: [Self; 3] = [Self::Auto, Self::Enabled, Self::Disabled];
}

impl fmt::Display for Vision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "Automatic",
            Self::Enabled => "Enabled",
            Self::Disabled => "Disabled",
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Reasoning {
    Default,
    None,
    Minimal,
    Low,
    Medium,
    High,
    ExtraHigh,
}

impl Reasoning {
    const ALL: [Self; 7] = [
        Self::Default,
        Self::None,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::ExtraHigh,
    ];

    pub(super) fn api_value(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::None => Some("none"),
            Self::Minimal => Some("minimal"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::ExtraHigh => Some("xhigh"),
        }
    }
}

impl fmt::Display for Reasoning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Default => "Model default",
            Self::None => "None",
            Self::Minimal => "Minimal",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::ExtraHigh => "Extra high",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Section {
    #[default]
    Connection,
    Model,
    Limits,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Field {
    Name,
    Endpoint,
    Model,
    Credential,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Number {
    Selection,
    Timeout,
    OutputTokens,
}

#[derive(Clone)]
pub(crate) enum Message {
    Add,
    Wizard(wizard::Action),
    WizardScanFinished(wizard::ScanResult),
    Select(ProfileId),
    Section(Section),
    Edit(Field, String),
    Number(Number, String),
    Override(Number, bool),
    EditUsage(usage_view::Field, String),
    Tokenizer(local_ai::Tokenizer),
    LoadUsage,
    UsageLoaded(std::sync::Arc<usage_view::Loaded>),
    ConnectionMode(ConnectionMode),
    CloudProvider(cloud::Provider),
    Cli(CliAction),
    ManagedModel(String),
    RefreshManagedModels,
    ManagedModelsLoaded(PolicyModelRuntime, ManagedModels),
    TrustLocal(bool),
    TrustRemote(bool),
    ReviewedAgentHttps(bool),
    Web(web::Action),
    ModelMode(ModelMode),
    Vision(Vision),
    Reasoning(Reasoning),
    PromptPreset(local_ai::PromptPreset),
    SetDefault(ProfileId),
    Delete,
    ConfirmDelete,
    CancelDelete,
    Test,
    Discover,
    Save,
    Refresh,
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AI profile editor event")
    }
}

#[derive(Clone, Eq, PartialEq)]
struct ManagedChoice {
    id: String,
    name: String,
    vision: bool,
}

impl fmt::Display for ManagedChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if valid_policy_model_id(&self.id) {
            write!(f, "{} (Managed by policy)", self.name)
        } else {
            let suffix = self.id.get(..8).unwrap_or(&self.id);
            write!(f, "{} ({suffix})", self.name)
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct ManagedModels {
    choices: Vec<ManagedChoice>,
    loading: bool,
    problem: Option<String>,
    policy_only: bool,
    policy_model_unavailable: bool,
}

impl ManagedModels {
    fn load_task(root: PathBuf, runtime: PolicyModelRuntime) -> (ModelInventory, Task<Message>) {
        ModelInventory::start(
            move |cancelled| Self::load(&root, runtime, cancelled),
            || Self {
                problem: Some("Could not load the managed model inventory.".into()),
                ..Self::default()
            },
            move |models| Message::ManagedModelsLoaded(runtime, models),
        )
    }

    fn load(root: &Path, runtime: PolicyModelRuntime, cancelled: &AtomicBool) -> Self {
        let policy = ClipboardPolicy::load();
        let policy_only = !policy.ai_user_model_files_allowed();
        let mut result = Self {
            policy_only,
            ..Self::default()
        };
        for model in policy
            .ai_runtime_models()
            .iter()
            .filter(|model| model.runtime == runtime)
        {
            if cancelled.load(Ordering::Acquire) {
                break;
            }
            if policy_models::available(model, root, cancelled) {
                result.choices.push(ManagedChoice {
                    id: model.id.clone(),
                    name: model.name.clone(),
                    vision: false,
                });
            } else {
                result.policy_model_unavailable = true;
            }
        }
        if policy_only {
            return result;
        }
        let registered: std::io::Result<Vec<_>> =
            match runtime {
                PolicyModelRuntime::Gguf => managed_models::Registry::new(root.to_owned())
                    .list()
                    .map(|models| {
                        models
                            .into_iter()
                            .filter(|model| model.purpose == managed_models::Purpose::Chat)
                            .map(|model| ManagedChoice {
                                id: model.id,
                                name: model.name,
                                vision: model.projector_bytes.is_some(),
                            })
                            .collect()
                    }),
                PolicyModelRuntime::Mlx => mlx_models::Registry::new(root).list().map(|models| {
                    models
                        .into_iter()
                        .map(|model| ManagedChoice {
                            id: model.id,
                            name: model.name,
                            vision: false,
                        })
                        .collect()
                }),
            };
        match registered {
            Ok(registered) => result.choices.extend(registered),
            Err(error) => {
                result.problem = Some(format!("Could not read the registered models ({error})"))
            }
        }
        result
    }
}

#[derive(Default)]
pub(super) struct Editor {
    catalog: Catalog,
    model_catalog: Option<std::sync::Arc<local_ai::model_catalog::Catalog>>,
    selected: Option<ProfileId>,
    section: Section,
    invalid: BTreeMap<(ProfileId, Number), String>,
    usage: usage_view::Editor,
    models: Vec<String>,
    managed_models: ManagedModels,
    managed_models_load: Option<ModelInventory>,
    mlx_models: ManagedModels,
    mlx_models_load: Option<ModelInventory>,
    status: Option<String>,
    deleting: bool,
    reserved_id: u64,
    wizard: Option<Wizard>,
    web_search_denied: bool,
    cli_review: cli_profile::Review,
    cli_catalog: Option<cli_profile::CatalogState>,
    cli_discovery: Option<cli_profile::DiscoveryState>,
}

impl Editor {
    pub(super) fn new(catalog: Catalog) -> Self {
        Self {
            selected: catalog
                .default_profile()
                .or_else(|| catalog.profiles.first())
                .map(|profile| profile.id),
            catalog,
            ..Self::default()
        }
    }

    pub(super) fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub(super) fn install_model_catalog(
        &mut self,
        catalog: Option<std::sync::Arc<local_ai::model_catalog::Catalog>>,
    ) {
        self.model_catalog = catalog;
    }

    pub(super) fn install_policy_profiles(&mut self, catalog: &Catalog) {
        let before = self
            .selected()
            .filter(|profile| profile.policy_definition().is_some())
            .cloned();
        self.catalog.copy_policy_profiles(catalog);
        if self
            .selected
            .is_none_or(|id| self.catalog.profile(id).is_none())
        {
            self.selected = self
                .catalog
                .default_profile()
                .or_else(|| self.catalog.profiles.first())
                .map(|profile| profile.id);
        }
        if before
            .as_ref()
            .is_some_and(|profile| self.selected() != Some(profile))
        {
            self.clear_selection_feedback();
        }
    }

    pub(super) fn deny_web_search(&mut self, denied: bool) {
        self.web_search_denied = denied;
    }

    pub(super) fn wizard_open(&self) -> bool {
        self.wizard.is_some()
    }

    pub(super) fn searchable_profile(&self) -> Option<&Profile> {
        if self.wizard.is_some() {
            return None;
        }
        self.selected()
    }

    pub(super) fn draft_dirty(&self) -> bool {
        !self.invalid.is_empty() || self.usage.invalid() || self.wizard.is_some()
    }

    #[cfg(test)]
    fn validated_catalog(&self) -> Result<Catalog, String> {
        self.catalog_for_save(None, !self.web_search_denied)
    }

    pub(super) fn catalog_for_save(
        &self,
        read_only: Option<&Catalog>,
        web_search_allowed: bool,
    ) -> Result<Catalog, String> {
        if self.wizard.is_some() {
            return Err("Finish or cancel the profile setup wizard first".into());
        }
        if !self.invalid.is_empty() || self.usage.invalid() {
            return Err(
                "Correct the invalid numbers or usage settings in the profile editor".into(),
            );
        }
        let mut catalog = self.catalog.clone();
        for profile in &mut catalog.profiles {
            if !read_only.is_some_and(|saved| saved.profile(profile.id) == Some(&*profile)) {
                profile.name = profile.name.trim().into();
                profile.endpoint = profile.endpoint.trim().into();
                profile.model = profile.model.trim().into();
                profile.credential_env = profile.credential_env.trim().into();
            }
            if !web_search_allowed {
                profile.clear_web_search();
            }
        }
        catalog.validate()?;
        Ok(catalog)
    }

    pub(super) fn replace(&mut self, catalog: Catalog) {
        let selection = self.selected;
        let section = self.section;
        let reserved_id = self.reserved_id;
        let managed_models = std::mem::take(&mut self.managed_models);
        let mlx_models = std::mem::take(&mut self.mlx_models);
        let mut usage = std::mem::take(&mut self.usage);
        usage.reset_for_catalog();
        *self = Self::new(catalog);
        self.managed_models = managed_models;
        self.mlx_models = mlx_models;
        self.usage = usage;
        self.reserved_id = reserved_id;
        if selection.is_some_and(|id| self.catalog.profile(id).is_some()) {
            self.selected = selection;
        }
        self.section = section;
    }

    pub(super) fn selected(&self) -> Option<&Profile> {
        if let Some(wizard) = &self.wizard {
            return Some(&wizard.profile);
        }
        self.selected.and_then(|id| self.catalog.profile(id))
    }

    pub(super) fn can_set_default(&self, id: ProfileId) -> bool {
        !self.wizard_open()
            && !self.catalog.is_marked_default(id)
            && self
                .selected()
                .is_some_and(|profile| profile.id == id && profile.can_be_default())
    }

    pub(super) fn adopt_saved_default(&mut self, saved: &Catalog) {
        self.catalog.default = saved.default;
        self.catalog
            .policy_default
            .clone_from(&saved.policy_default);
    }

    fn selected_mut(&mut self) -> Option<&mut Profile> {
        if let Some(wizard) = &mut self.wizard {
            return Some(&mut wizard.profile);
        }
        self.catalog
            .profiles
            .iter_mut()
            .find(|profile| Some(profile.id) == self.selected)
    }

    pub(super) fn set_models(
        &mut self,
        profile_id: ProfileId,
        endpoint: &str,
        mut models: Vec<String>,
    ) {
        if self.selected().is_some_and(|profile| {
            !profile.is_managed() && profile.id == profile_id && profile.endpoint.trim() == endpoint
        }) {
            models.retain(|model| {
                !model.is_empty()
                    && plain_text(model, local_ai::MAX_MODEL_BYTES)
                    && !model.chars().any(char::is_whitespace)
            });
            if let Some(profile) = self.selected_mut()
                && profile.model_mode == ModelMode::Auto
            {
                profile.model = models.first().cloned().unwrap_or_default();
            }
            models.sort_unstable();
            models.dedup();
            models.truncate(512);
            self.models = models;
        }
    }

    pub(super) fn refresh_managed_models(&mut self, root: PathBuf) -> Task<Message> {
        if self.managed_models.loading {
            return Task::none();
        }
        self.managed_models.loading = true;
        self.managed_models.problem = None;
        let mlx = (mlx_runtime::supported() || self.catalog.entries().any(Profile::is_mlx))
            && !self.mlx_models.loading;
        let mlx_task = if mlx {
            self.mlx_models.loading = true;
            self.mlx_models.problem = None;
            let (load, task) = ManagedModels::load_task(root.clone(), PolicyModelRuntime::Mlx);
            self.mlx_models_load = Some(load);
            task
        } else {
            Task::none()
        };
        let (load, gguf_task) = ManagedModels::load_task(root, PolicyModelRuntime::Gguf);
        self.managed_models_load = Some(load);
        Task::batch([gguf_task, mlx_task])
    }

    fn managed_catalog(&self, profile: &Profile) -> &ManagedModels {
        if profile.is_mlx() {
            &self.mlx_models
        } else {
            &self.managed_models
        }
    }

    pub(super) fn managed_model_name(&self, profile: &Profile) -> Option<&str> {
        self.managed_catalog(profile)
            .choices
            .iter()
            .find(|choice| choice.id == profile.model)
            .map(|choice| choice.name.as_str())
    }

    pub(super) fn set_status(&mut self, status: impl Into<String>) {
        self.status = Some(status.into());
    }

    pub(super) fn usage_profile(&self) -> Option<&Profile> {
        self.selected().filter(|profile| {
            self.wizard.is_none()
                && !profile.is_cli()
                && (self.section == Section::Limits || profile.policy_definition().is_some())
        })
    }

    pub(super) fn begin_usage_load(&mut self) -> bool {
        self.usage.begin()
    }

    pub(super) fn finish_usage_load(
        &mut self,
        result: std::sync::Arc<UsageLoaded>,
        current: bool,
    ) -> bool {
        self.usage.finish(result, current)
    }

    pub(super) fn update(&mut self, message: Message, editable: bool) {
        if self.wizard_open()
            && matches!(
                message,
                Message::Add
                    | Message::Select(_)
                    | Message::Section(_)
                    | Message::Web(_)
                    | Message::SetDefault(_)
                    | Message::Delete
                    | Message::ConfirmDelete
                    | Message::CancelDelete
            )
        {
            return;
        }
        match message {
            Message::Wizard(WizardAction::Cancel) => {
                self.update_wizard(WizardAction::Cancel);
                return;
            }
            Message::Select(id) if self.catalog.profile(id).is_some() => {
                self.selected = Some(id);
                self.clear_selection_feedback();
                return;
            }
            Message::Section(section) => {
                self.section = section;
                self.cancel_cli_catalog();
                self.cli_discovery = None;
                return;
            }
            Message::ManagedModelsLoaded(runtime, models) => {
                match runtime {
                    PolicyModelRuntime::Gguf => {
                        self.managed_models = models;
                        self.managed_models_load = None;
                    }
                    PolicyModelRuntime::Mlx => {
                        self.mlx_models = models;
                        self.mlx_models_load = None;
                    }
                }
                return;
            }
            _ => {}
        }
        if !editable {
            return;
        }
        match message {
            Message::Add => {
                if let Some(profile) = self.new_profile() {
                    self.insert_profile(profile);
                }
            }
            Message::Wizard(action) => self.update_wizard(action),
            Message::Edit(field, value) => self.edit(field, value),
            Message::Number(field, input) => self.edit_number(field, input),
            Message::Override(field, enabled) => self.toggle_override(field, enabled),
            Message::ReviewedAgentHttps(allow) if self.wizard.is_none() => {
                if let Some(profile) = self
                    .selected_mut()
                    .filter(|profile| profile.remote_protocol().is_some())
                {
                    profile.reviewed_agent_https = allow && profile.trusted_remote_endpoint;
                }
            }
            Message::Tokenizer(tokenizer) if self.wizard.is_none() => {
                if let Some(profile) = self.selected_mut() {
                    profile.tokenizer = tokenizer;
                }
            }
            Message::EditUsage(field, value) if self.wizard.is_none() => {
                if let Some(profile) = self
                    .catalog
                    .profiles
                    .iter_mut()
                    .find(|profile| Some(profile.id) == self.selected)
                {
                    self.usage.edit(profile, field, value);
                    self.status = None;
                }
            }
            Message::ConnectionMode(mode) => self.select_connection_mode(mode),
            Message::CloudProvider(provider) => self.select_cloud_provider(provider),
            Message::ManagedModel(model) => self.select_managed_model(model),
            Message::TrustLocal(trusted) => {
                if let Some(profile) = self
                    .selected_mut()
                    .filter(|profile| !profile.is_managed() && profile.allows_local_roles())
                {
                    profile.trusted_local_endpoint = trusted;
                    if profile.model_mode == ModelMode::Auto {
                        profile.model.clear();
                    }
                    if !trusted {
                        profile.credential_env.clear();
                        profile.lm_web = None;
                    }
                }
                self.clear_selection_feedback();
            }
            Message::Web(action) if !self.web_search_denied => self.update_web_search(action),
            Message::TrustRemote(trusted) => {
                if let Some(profile) = self.selected_mut().filter(|profile| {
                    profile.remote_protocol().is_some() && profile.destination().is_ok()
                }) {
                    profile.trusted_remote_endpoint = trusted;
                    if trusted {
                        profile.restore_review = false;
                    }
                    if !trusted {
                        profile.reviewed_agent_https = false;
                    }
                    if profile.model_mode == ModelMode::Auto {
                        profile.model.clear();
                    }
                    if !trusted {
                        profile.credential_env.clear();
                    }
                }
                self.clear_selection_feedback();
            }
            Message::ModelMode(mode) => {
                if let Some(profile) = self.selected_mut()
                    && !profile.is_managed()
                    && !profile.is_cli()
                    && profile.model_mode != mode
                {
                    profile.model_mode = mode;
                    if mode == ModelMode::Auto {
                        profile.model.clear();
                    }
                    self.status = None;
                }
            }
            Message::Vision(vision) => {
                let supported = self.selected().is_some_and(|profile| {
                    !profile.is_managed() && !profile.is_cli()
                        || vision == Vision::Disabled
                        || (vision == Vision::Enabled
                            && self
                                .managed_catalog(profile)
                                .choices
                                .iter()
                                .any(|choice| choice.id == profile.model && choice.vision))
                });
                if supported && let Some(profile) = self.selected_mut() {
                    profile.vision = vision;
                }
            }
            Message::Reasoning(reasoning) => {
                if let Some(profile) = self
                    .selected_mut()
                    .filter(|profile| !profile.is_mlx() && !profile.is_cli())
                {
                    profile.reasoning = reasoning;
                }
                self.status = None;
            }
            Message::PromptPreset(preset) => {
                if let Some(profile) = self.selected_mut() {
                    profile.prompt_preset = Some(preset);
                    self.status = None;
                }
            }
            Message::SetDefault(id) if self.can_set_default(id) => {
                self.catalog.set_default(id);
            }
            Message::Delete
                if self
                    .selected()
                    .is_some_and(|profile| profile.policy_definition().is_none()) =>
            {
                self.deleting = true
            }
            Message::ConfirmDelete if self.deleting => self.delete(),
            Message::CancelDelete => self.deleting = false,
            _ => {}
        }
    }

    fn select_connection_mode(&mut self, mode: ConnectionMode) {
        if mode == ConnectionMode::ManagedMlx && !mlx_runtime::supported() {
            return;
        }
        let Some(profile) = self
            .selected_mut()
            .filter(|profile| profile.connection_mode != mode)
        else {
            return;
        };
        profile.connection_mode = mode;
        profile.restore_review = false;
        profile.trusted_local_endpoint = false;
        profile.trusted_remote_endpoint = false;
        profile.reviewed_agent_https = false;
        profile.credential_env.clear();
        profile.cli = (mode == ConnectionMode::Cli).then(cli_profile::Settings::default);
        profile.lm_web = None;
        profile.model.clear();
        profile.model_mode = ModelMode::Manual;
        if mode == ConnectionMode::Cli {
            profile.endpoint.clear();
            profile.tavily_search = false;
            profile.vision = Vision::Disabled;
            profile.reasoning = Reasoning::Default;
            profile.max_output_tokens = None;
            let id = profile.id;
            self.invalid.remove(&(id, Number::OutputTokens));
            self.usage.forget(id);
        } else if mode.is_managed() {
            profile.endpoint.clear();
            profile.vision = Vision::Disabled;
            if mode == ConnectionMode::ManagedMlx {
                profile.reasoning = Reasoning::Default;
            }
        } else {
            profile.endpoint = match mode {
                ConnectionMode::LmStudio => "127.0.0.1:1234",
                ConnectionMode::RemoteOpenAi => cloud::Provider::OpenAi.endpoint(),
                ConnectionMode::Anthropic => cloud::Provider::Anthropic.endpoint(),
                _ => local_ai::DEFAULT_ENDPOINT,
            }
            .into();
            profile.vision = Vision::Auto;
        }
        self.clear_selection_feedback();
    }

    fn select_cloud_provider(&mut self, provider: cloud::Provider) {
        if !self.selected().is_some_and(|profile| {
            (self.wizard_open() || profile.remote_protocol().is_some())
                && !provider.matches_endpoint(profile)
        }) {
            return;
        }
        self.select_connection_mode(provider.mode());
        self.edit(Field::Endpoint, provider.endpoint().into());
        if let Some(profile) = self.selected_mut() {
            profile.model.clear();
            profile.model_mode = ModelMode::Manual;
            profile.reasoning = Reasoning::Default;
            profile.vision = if provider == cloud::Provider::MiniMax {
                Vision::Disabled
            } else {
                Vision::Auto
            };
        }
        self.clear_selection_feedback();
    }

    fn select_managed_model(&mut self, model: String) {
        if let Some(vision) = self
            .selected()
            .and_then(|profile| {
                let catalog = self.managed_catalog(profile);
                if catalog.loading {
                    return None;
                }
                catalog.choices.iter().find(|choice| choice.id == model)
            })
            .map(|choice| choice.vision)
            && let Some(profile) = self.selected_mut().filter(|profile| profile.is_managed())
        {
            profile.model = model;
            if !vision {
                profile.vision = Vision::Disabled;
            }
            self.status = None;
        }
    }

    fn clear_selection_feedback(&mut self) {
        self.models.clear();
        self.cancel_cli_catalog();
        self.cli_discovery = None;
        self.status = None;
        self.deleting = false;
    }

    fn new_profile(&mut self) -> Option<Profile> {
        if self.catalog.profiles.len() >= MAX_PROFILES || self.wizard.is_some() {
            self.set_status("AI profile limit reached or setup already in progress");
            return None;
        }
        let Some(id) = self
            .catalog
            .last_id
            .max(self.reserved_id)
            .checked_add(1)
            .filter(|id| !self.catalog.is_policy_reference(*id))
        else {
            self.set_status("AI profile identifiers exhausted");
            return None;
        };
        let name = (1..=MAX_PROFILES)
            .map(|number| match number {
                1 => "New AI profile".into(),
                _ => format!("New AI profile {number}"),
            })
            .find(|name| {
                !self
                    .catalog
                    .profiles
                    .iter()
                    .any(|profile| profile.name.eq_ignore_ascii_case(name))
            })
            .unwrap_or_else(|| format!("AI profile {id}"));
        self.reserved_id = id;
        Some(Profile {
            id: ProfileId::from(id),
            name,
            prompt_preset: Some(local_ai::PromptPreset::Auto),
            ..Profile::default()
        })
    }

    fn insert_profile(&mut self, profile: Profile) {
        let id = profile.id;
        let saved_id = u64::try_from(id).expect("allocated user profile ID fits its saved format");
        self.catalog.last_id = saved_id;
        self.catalog.profiles.insert(0, profile);
        self.catalog.default.get_or_insert(saved_id);
        self.selected = Some(id);
        self.section = Section::Connection;
        self.clear_selection_feedback();
    }

    fn delete(&mut self) {
        let Some(id) = self.selected else { return };
        if self.catalog.user_profile(id).is_none() {
            return;
        }
        self.catalog.profiles.retain(|profile| profile.id != id);
        self.invalid.retain(|(profile_id, _), _| *profile_id != id);
        self.usage.forget(id);
        self.selected = self.catalog.profiles.first().map(|profile| profile.id);
        if self.catalog.default.map(ProfileId::from) == Some(id) {
            self.catalog.default = self
                .catalog
                .profiles
                .iter()
                .find(|profile| !profile.restore_review)
                .and_then(|profile| u64::try_from(profile.id).ok());
        }
        if self.selected.is_none() {
            self.selected = self.catalog.default_profile().map(|profile| profile.id);
        }
        self.clear_selection_feedback();
    }

    fn edit(&mut self, field: Field, value: String) {
        let Some(profile) = self.selected_mut() else {
            return;
        };
        if (profile.is_managed() && !matches!(field, Field::Name))
            || (profile.is_cli() && !matches!(field, Field::Name | Field::Model))
            || (matches!(field, Field::Credential) && !profile.trusts_credential())
            || (matches!(field, Field::Model) && profile.model_mode == ModelMode::Auto)
        {
            return;
        }
        let (value, max, destination) = match field {
            Field::Name => (value, MAX_NAME_BYTES, &mut profile.name),
            Field::Endpoint => (value, MAX_ENDPOINT_BYTES, &mut profile.endpoint),
            Field::Model => (value, local_ai::MAX_MODEL_BYTES, &mut profile.model),
            Field::Credential => (value, 128, &mut profile.credential_env),
        };
        if value.len() <= max && value != *destination {
            *destination = value;
            if matches!(field, Field::Model)
                && let Some(settings) = &mut profile.cli
            {
                settings.clear_approval();
            }
            if matches!(field, Field::Endpoint) {
                profile.trusted_local_endpoint = false;
                profile.trusted_remote_endpoint = false;
                profile.reviewed_agent_https = false;
                profile.credential_env.clear();
            }
            if matches!(field, Field::Endpoint | Field::Credential) {
                if profile.model_mode == ModelMode::Auto {
                    profile.model.clear();
                }
                self.models.clear();
            }
            if matches!(field, Field::Model) {
                self.cancel_cli_catalog();
            }
            self.status = None;
        }
    }

    fn edit_number(&mut self, field: Number, input: String) {
        let Some(id) = self.selected else { return };
        let parsed = input
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|value| match field {
                Number::Timeout => *value <= MAX_TIMEOUT_MINUTES,
                Number::Selection => (1..=MAX_SELECTION_CHARS).contains(value),
                Number::OutputTokens => (256..=1_048_576).contains(value),
            });
        let Some(value) = parsed else {
            self.invalid
                .insert((id, field), input.chars().take(32).collect());
            return;
        };
        if let Some(profile) = self.selected_mut() {
            match field {
                Number::Selection => profile.selection_limit = value,
                Number::Timeout if profile.timeout_minutes.is_some() => {
                    profile.timeout_minutes = Some(value)
                }
                Number::OutputTokens if profile.max_output_tokens.is_some() => {
                    profile.max_output_tokens = Some(value)
                }
                _ => return,
            }
        }
        self.invalid.remove(&(id, field));
        self.status = None;
    }

    fn toggle_override(&mut self, field: Number, enabled: bool) {
        let Some(profile) = self.selected_mut() else {
            return;
        };
        if profile.is_cli() && field == Number::OutputTokens {
            return;
        }
        match field {
            Number::Timeout => profile.timeout_minutes = enabled.then_some(1),
            Number::OutputTokens => profile.max_output_tokens = enabled.then_some(16_000),
            _ => return,
        }
        let id = profile.id;
        self.invalid.remove(&(id, field));
        self.status = None;
    }
}

#[cfg(test)]
mod tests;
