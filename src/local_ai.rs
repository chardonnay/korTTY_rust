//! Bounded requests to explicitly chosen local and HTTPS model services.

use std::borrow::Cow;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use async_channel::Receiver;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
use ureq::{Agent, config::Config};
use zeroize::Zeroizing;

use crate::clipboard_policy::{ClipboardPolicy, PolicyModelRuntime, valid_policy_model_id};

pub(crate) mod cli;
mod lm_studio_endpoint;
pub(crate) mod managed_models;
#[cfg(test)]
mod minimax_tests;
pub(crate) mod mlx_models;
pub(crate) mod mlx_runtime;
pub(crate) mod model_catalog;
mod models;
mod policy_authority;
mod policy_auto_model;
pub(crate) mod policy_models;
mod policy_profile;
mod prompt_preset;
mod providers;
pub(crate) mod reply;
pub(crate) mod runtime_index;
pub(crate) mod runtime_package;
mod streaming;
mod tokenizer;
mod usage;
mod usage_store;
pub(crate) mod web_search;
#[cfg(windows)]
mod windows_transport;
pub(crate) use lm_studio_endpoint::LmStudioEndpoint;
use managed_models::Purpose;
pub(crate) use models::discover_policy_profile;
pub(crate) use models::{
    LmStudioCapabilities, LmStudioCatalog, LmStudioReasoningOption, LocalModelServer,
    MAX_LOCAL_SCAN_ENDPOINTS, discover_lm_studio_catalog, discover_policy_lm_studio_catalog,
    discover_remote, scan_local_models,
};
pub(crate) use models::{discover, discover_with_credential};
pub(crate) use policy_authority::PolicyModelAuthority;
#[cfg(test)]
pub(crate) use policy_auto_model::resolve_lm_studio_auto_with_test_root;
pub(crate) use policy_auto_model::{LmStudioAutoModel, resolve_lm_studio_auto};
use policy_profile::PolicyProfileAuthority;
pub(crate) use policy_profile::{PolicyAutoModel, resolve_policy_auto_model};
pub(crate) use prompt_preset::PromptPreset;
pub(crate) use providers::{
    MINIMAX_NATIVE_ENDPOINT, MINIMAX_NATIVE_PATH, RemoteProtocol, is_messages_path,
    remote_api_base, remote_destination,
};
use runtime_package::BackendUse;
pub(crate) use streaming::StreamDelta;
pub(crate) use tokenizer::Tokenizer;
pub(crate) use usage::Usage;
pub(crate) use usage_store::take_notice as take_usage_notice;
pub(crate) use usage_store::{
    Account as UsageAccount, Budget as UsageBudget, Counters as UsageCounters, JournalCost,
    Snapshot as UsageSnapshot, Warning as UsageWarning,
};

pub(crate) const DEFAULT_ENDPOINT: &str = "127.0.0.1:11434";
pub(crate) const ANTHROPIC_LOOPBACK_ENDPOINT: &str = "http://localhost:1234/v1/messages";

pub(crate) fn start_mlx_model(
    profile: &std::path::Path,
    id: &str,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let _power = kortty_macos::AppNapActivity::begin();
    runtime_package::acquire_mlx(
        profile,
        id,
        None,
        Instant::now() + Duration::from_secs(300),
        cancelled,
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}
pub(crate) const MAX_MODEL_BYTES: usize = 256;
const MAX_RESPONSE_BYTES: u64 = 256 * 1024;
const MAX_ERROR_RESPONSE_BYTES: u64 = 4 * 1024;
pub(crate) const MAX_WORKERS: usize = 2;
pub(crate) const WORKERS_BUSY: &str = "Local AI workers are busy";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
static WORKERS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct ProfileOptions {
    pub(crate) tokenizer: Tokenizer,
    pub(crate) max_output_tokens: Option<u32>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) credential_env: Option<String>,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Profile {
    target: Target,
    model: String,
    options: ProfileOptions,
    prompt_preset: PromptPreset,
    usage_account: Option<UsageAccount>,
    journal_identity: Option<Arc<JournalProfileIdentity>>,
    vision_mode: VisionMode,
    lm_studio_auto: Option<Arc<LmStudioAutoModel>>,
    policy_model: Option<PolicyModelAuthority>,
    policy_profile: Option<PolicyProfileAuthority>,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub(crate) enum VisionMode {
    #[default]
    Auto,
    Enabled,
    Disabled,
}

#[derive(Debug)]
pub(crate) struct JournalProfileIdentity {
    pub(crate) reference: kortty_jobs::ProfileRef,
    pub(crate) name: String,
    pub(crate) java_id: Option<String>,
}

impl PartialEq for JournalProfileIdentity {
    fn eq(&self, other: &Self) -> bool {
        // A label edit does not change in-flight transport authority.
        self.reference == other.reference
    }
}

impl Eq for JournalProfileIdentity {}

#[derive(Clone, Eq, PartialEq)]
enum Target {
    External(SocketAddr),
    LmStudio(LmStudioEndpoint),
    LoopbackAnthropic(LmStudioEndpoint),
    Remote(String, RemoteProtocol),
    Managed(PathBuf, Purpose, BackendUse),
    ManagedMlx(PathBuf),
    Cli(cli::Program, PathBuf),
}

impl Profile {
    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    pub(crate) fn uses_server_default_model(&self) -> bool {
        // Only the server-default constructors permit an empty HTTP chat model.
        self.model.is_empty()
            && match self.target {
                Target::External(_) | Target::LmStudio(_) => true,
                Target::Remote(_, protocol) => protocol.is_openai(),
                _ => false,
            }
    }

    fn requested_chat_model(&self) -> Option<&str> {
        (!self.uses_server_default_model()).then_some(self.model())
    }

    pub(crate) fn with_vision_mode(mut self, mode: VisionMode) -> Self {
        self.vision_mode = mode;
        self
    }

    pub(crate) fn journal_name(&self) -> &str {
        self.journal_identity
            .as_ref()
            .map_or(&self.model, |identity| identity.name.as_str())
    }

    pub(crate) fn probe_credential(&self) -> Result<Option<Zeroizing<String>>, Error> {
        if let Target::Cli(program, _) = &self.target {
            return program.probe_credential();
        }
        self.current_probe_credential()
    }

    pub(crate) fn current_probe_credential(&self) -> Result<Option<Zeroizing<String>>, Error> {
        if let Target::Cli(program, _) = &self.target {
            return program.current_probe_credential();
        }
        if self.starts_sidecar() {
            return Ok(None);
        }
        if let Some(credential) = self.policy_credential()? {
            return Ok(Some(credential));
        }
        let credential = self
            .options
            .credential_env
            .as_deref()
            .map(credential_value)
            .transpose()?;
        if let Some(resolved) = &self.lm_studio_auto {
            resolved.require_credential(credential.as_deref().map(String::as_str))?;
        }
        Ok(credential)
    }

    pub(crate) fn with_journal_identity(mut self, identity: JournalProfileIdentity) -> Self {
        self.journal_identity = Some(Arc::new(identity));
        self
    }

    pub(crate) fn with_prompt_preset(mut self, configured: PromptPreset) -> Self {
        self.prompt_preset = configured.resolve(&self.model);
        self
    }

    pub(crate) fn prompt_preset(&self) -> PromptPreset {
        self.prompt_preset
    }

    pub(crate) fn render_system_prompt<'a>(&self, system: &'a str) -> Cow<'a, str> {
        self.prompt_preset
            .render(system, self.structured_system_suffix())
    }

    pub(crate) fn with_tokenizer(mut self, tokenizer: Tokenizer) -> Self {
        self.options.tokenizer = tokenizer;
        self
    }

    pub(crate) fn count_input<'a>(&self, parts: impl IntoIterator<Item = &'a str>) -> u64 {
        self.options.tokenizer.count_messages(parts)
    }

    pub(crate) fn tokenizer(&self) -> Tokenizer {
        self.options.tokenizer
    }

    pub(crate) fn usage_estimate_profile(&self) -> (&str, Option<&UsageBudget>) {
        (
            &self.model,
            self.usage_account.as_ref().map(UsageAccount::budget),
        )
    }

    pub(crate) fn with_usage_account(mut self, account: UsageAccount) -> Self {
        self.usage_account = Some(account);
        self
    }

    pub(crate) fn without_usage_account(mut self) -> Self {
        self.usage_account = None;
        self
    }

    pub(crate) fn has_quota(&self) -> bool {
        self.usage_account
            .as_ref()
            .is_some_and(UsageAccount::has_quota)
    }

    pub(crate) fn quota_forecast(&self, estimated_input: u64) -> Result<String, String> {
        self.quota_projection(estimated_input, u64::from(self.effective_output_limit()))
    }

    pub(crate) fn quota_projection(
        &self,
        estimated_input: u64,
        estimated_output: u64,
    ) -> Result<String, String> {
        self.usage_account
            .as_ref()
            .filter(|account| account.has_quota())
            .ok_or_else(|| "The selected profile has no recorded quota.".to_owned())?
            .quota_forecast(estimated_input, estimated_output)
    }

    pub(crate) fn is_remote(&self) -> bool {
        matches!(self.target, Target::Remote(..))
            || matches!(&self.target, Target::LmStudio(endpoint) | Target::LoopbackAnthropic(endpoint) if endpoint.is_https())
    }

    fn uses_anthropic_protocol(&self) -> bool {
        match self.target {
            Target::Remote(_, protocol) => protocol.is_anthropic(),
            Target::LoopbackAnthropic(_) => true,
            _ => false,
        }
    }

    fn is_loopback_anthropic(&self) -> bool {
        matches!(self.target, Target::LoopbackAnthropic(_))
    }

    fn require_loopback_anthropic_allowed(policy: &ClipboardPolicy) -> Result<(), Error> {
        if policy.problem.is_some() || !policy.ai_allowed {
            return Err(Error::Review(
                "Managed policy blocks the loopback Anthropic Messages endpoint.".into(),
            ));
        }
        Ok(())
    }

    fn uses_lm_studio_protocol(&self) -> bool {
        matches!(&self.target, Target::LmStudio(endpoint) if endpoint.uses_native_protocol())
    }

    fn supports_lm_studio_metadata(&self) -> bool {
        matches!(self.target, Target::LmStudio(_))
            || self
                .policy_profile
                .as_ref()
                .is_some_and(|authority| authority.is_lm_studio())
    }

    pub(crate) fn is_cli(&self) -> bool {
        matches!(self.target, Target::Cli(..))
    }

    pub(crate) fn configured_cli(
        program: cli::Program,
        root: PathBuf,
        model: &str,
        timeout: Option<Duration>,
    ) -> Self {
        Self {
            target: Target::Cli(program, root),
            model: cli::display_model(model).into(),
            options: ProfileOptions {
                timeout,
                ..ProfileOptions::default()
            },
            prompt_preset: PromptPreset::Generic,
            usage_account: None,
            journal_identity: None,
            vision_mode: VisionMode::Auto,
            lm_studio_auto: None,
            policy_model: None,
            policy_profile: None,
        }
    }

    pub(crate) fn prepare_cli_chat(
        &self,
        system: &str,
        history: &[(&str, &str)],
        input: &str,
    ) -> Result<cli::Request, Error> {
        let Target::Cli(program, root) = &self.target else {
            return Err(Error::Service);
        };
        cli::Request::prepare(program, root, system, history, input)
    }

    pub(crate) fn cli_command_review(&self) -> Option<String> {
        match &self.target {
            Target::Cli(program, _) => Some(program.command_review()),
            _ => None,
        }
    }

    fn is_mlx(&self) -> bool {
        matches!(self.target, Target::ManagedMlx(_))
    }

    fn starts_sidecar(&self) -> bool {
        matches!(self.target, Target::Managed(..) | Target::ManagedMlx(_))
    }

    pub(crate) fn policy_model_fingerprint(&self) -> Option<[u8; 32]> {
        self.policy_model
            .as_ref()
            .map(PolicyModelAuthority::fingerprint)
    }

    pub(crate) fn require_current_policy_model(&self) -> Result<(), String> {
        if let Some(authority) = &self.policy_model
            && let Target::Managed(root, ..) | Target::ManagedMlx(root) = &self.target
        {
            authority
                .require_current_source(root)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub(crate) fn requiring_managed_backend(mut self) -> Self {
        if let Target::Managed(_, _, backend) = &mut self.target {
            *backend = BackendUse::RequireMatch;
        }
        self
    }

    pub(crate) fn with_output_limit(mut self, limit: u32) -> Self {
        let configured = if self.uses_anthropic_protocol() {
            Some(self.anthropic_output_limit())
        } else {
            self.options.max_output_tokens
        };
        self.options.max_output_tokens = Some(configured.map_or(limit, |value| value.min(limit)));
        self
    }

    pub(crate) fn effective_output_limit(&self) -> u32 {
        if self.uses_anthropic_protocol() {
            self.anthropic_output_limit()
        } else {
            self.options.max_output_tokens.unwrap_or(4096)
        }
    }

    pub(crate) fn new(endpoint: &str, model: &str) -> Result<Self, String> {
        Self::from_address(
            parse_loopback(endpoint.trim())?,
            model,
            ProfileOptions::default(),
        )
    }

    pub(crate) fn configured(
        endpoint: &str,
        model: &str,
        options: ProfileOptions,
    ) -> Result<Self, String> {
        Self::validate_options(&options)?;
        Self::from_address(numeric_loopback_endpoint(endpoint)?, model, options)
    }

    pub(crate) fn lm_studio(
        endpoint: &str,
        model: &str,
        options: ProfileOptions,
    ) -> Result<Self, String> {
        Self::lm_studio_profile(endpoint, model, options, true)
    }

    pub(crate) fn lm_studio_compatible(
        endpoint: &str,
        model: &str,
        options: ProfileOptions,
    ) -> Result<Self, String> {
        Self::lm_studio_profile(endpoint, model, options, false)
    }

    pub(crate) fn lm_studio_with_server_default(
        endpoint: &str,
        options: ProfileOptions,
        native: bool,
    ) -> Result<Self, String> {
        let endpoint = LmStudioEndpoint::parse(endpoint, native)?;
        Self::with_server_default(Target::LmStudio(endpoint), options)
    }

    pub(crate) fn configured_with_server_default(
        endpoint: &str,
        options: ProfileOptions,
    ) -> Result<Self, String> {
        let address = numeric_loopback_endpoint(endpoint)?;
        Self::with_server_default(Target::External(address), options)
    }

    pub(crate) fn openai_with_server_default(
        endpoint: &str,
        protocol: RemoteProtocol,
        options: ProfileOptions,
    ) -> Result<Self, String> {
        if !protocol.is_openai() {
            return Err(
                "Choose an OpenAI-compatible HTTPS endpoint for provider model selection.".into(),
            );
        }
        let base = providers::remote_api_base(endpoint, protocol)?;
        Self::with_server_default(Target::Remote(base, protocol), options)
    }

    fn with_server_default(target: Target, options: ProfileOptions) -> Result<Self, String> {
        Self::validate_options(&options)?;
        if options.reasoning_effort.is_some() {
            return Err(
                "Choose Model default for reasoning when the AI server selects the model.".into(),
            );
        }
        Ok(Self::for_target(target, String::new(), options))
    }

    fn lm_studio_profile(
        endpoint: &str,
        model: &str,
        options: ProfileOptions,
        native: bool,
    ) -> Result<Self, String> {
        Self::validate_options(&options)?;
        if !native && options.reasoning_effort.as_deref() == Some("on") {
            return Err(
                "Explicit reasoning On is available only for native LM Studio profiles.".into(),
            );
        }
        let endpoint = LmStudioEndpoint::parse(endpoint, native)?;
        Self::with_target(Target::LmStudio(endpoint), model, options)
    }

    pub(crate) fn lm_studio_with_auto_model(
        endpoint: &str,
        preferred: &str,
        options: ProfileOptions,
        resolved: &LmStudioAutoModel,
    ) -> Result<Self, String> {
        if !resolved.matches(endpoint, preferred, options.credential_env.as_deref(), true) {
            return Err(
                "The automatic LM Studio model selection changed. Resolve it again.".into(),
            );
        }
        let mut profile = Self::lm_studio(endpoint, resolved.model(), options)?;
        profile.lm_studio_auto = Some(Arc::new(resolved.clone()));
        Ok(profile)
    }

    pub(crate) fn lm_studio_compatible_with_auto_model(
        endpoint: &str,
        preferred: &str,
        options: ProfileOptions,
        resolved: &LmStudioAutoModel,
    ) -> Result<Self, String> {
        if !resolved.matches(
            endpoint,
            preferred,
            options.credential_env.as_deref(),
            false,
        ) {
            return Err(
                "The automatic LM Studio model selection changed. Resolve it again.".into(),
            );
        }
        let mut profile = Self::lm_studio_compatible(endpoint, resolved.model(), options)?;
        profile.lm_studio_auto = Some(Arc::new(resolved.clone()));
        Ok(profile)
    }

    pub(crate) fn lm_studio_auto_binding(&self) -> Option<[u8; 32]> {
        self.lm_studio_auto
            .as_deref()
            .map(LmStudioAutoModel::binding)
    }

    pub(crate) fn reviewed_model_fingerprint(&self) -> Option<[u8; 32]> {
        self.policy_authority_fingerprint()
            .or_else(|| self.lm_studio_auto_binding())
    }

    pub(crate) fn remote(
        endpoint: &str,
        model: &str,
        protocol: RemoteProtocol,
        options: ProfileOptions,
    ) -> Result<Self, String> {
        Self::validate_options(&options)?;
        let model = model.trim();
        if !valid_model(model) {
            return Err("Enter a valid model identifier".into());
        }
        let target = Target::Remote(providers::remote_api_base(endpoint, protocol)?, protocol);
        Ok(Self::for_target(target, model.into(), options))
    }

    pub(crate) fn loopback_anthropic(
        endpoint: &str,
        model: &str,
        options: ProfileOptions,
    ) -> Result<Self, String> {
        Self::validate_options(&options)?;
        let endpoint = LmStudioEndpoint::parse_anthropic(endpoint)?;
        Self::with_target(Target::LoopbackAnthropic(endpoint), model, options)
    }

    pub(crate) fn managed(
        profile_dir: PathBuf,
        model_id: &str,
        options: ProfileOptions,
    ) -> Result<Self, String> {
        Self::managed_for(profile_dir, model_id, Purpose::Chat, options)
    }

    pub(crate) fn managed_mlx(
        profile_dir: PathBuf,
        model_id: &str,
        options: ProfileOptions,
    ) -> Result<Self, String> {
        Self::validate_options(&options)?;
        if !profile_dir.is_absolute()
            || !(managed_models::valid_id(model_id) || valid_policy_model_id(model_id))
        {
            return Err("Select a registered managed MLX model".into());
        }
        if options.credential_env.is_some() {
            return Err("The managed runtime creates its own one-time credential".into());
        }
        let policy_model =
            Self::managed_policy_model(&profile_dir, model_id, PolicyModelRuntime::Mlx)?;
        Ok(Self {
            target: Target::ManagedMlx(profile_dir),
            model: model_id.into(),
            options,
            prompt_preset: PromptPreset::Generic,
            usage_account: None,
            journal_identity: None,
            vision_mode: VisionMode::Auto,
            lm_studio_auto: None,
            policy_model,
            policy_profile: None,
        })
    }

    pub(crate) fn managed_embedding(profile_dir: PathBuf, model_id: &str) -> Result<Self, String> {
        Self::managed_for(
            profile_dir,
            model_id,
            Purpose::Embedding,
            ProfileOptions::default(),
        )
    }

    fn managed_for(
        profile_dir: PathBuf,
        model_id: &str,
        purpose: Purpose,
        options: ProfileOptions,
    ) -> Result<Self, String> {
        Self::validate_options(&options)?;
        if !profile_dir.is_absolute()
            || !(managed_models::valid_id(model_id) || valid_policy_model_id(model_id))
        {
            return Err("Select a registered managed GGUF model".into());
        }
        if options.credential_env.is_some() {
            return Err("The managed runtime creates its own one-time credential".into());
        }
        let policy_model =
            Self::managed_policy_model(&profile_dir, model_id, PolicyModelRuntime::Gguf)?;
        Ok(Self {
            target: Target::Managed(profile_dir, purpose, BackendUse::AllowCpuFallback),
            model: model_id.into(),
            options,
            prompt_preset: PromptPreset::Generic,
            usage_account: None,
            journal_identity: None,
            vision_mode: VisionMode::Auto,
            lm_studio_auto: None,
            policy_model,
            policy_profile: None,
        })
    }

    fn managed_policy_model(
        root: &Path,
        id: &str,
        runtime: PolicyModelRuntime,
    ) -> Result<Option<PolicyModelAuthority>, String> {
        valid_policy_model_id(id)
            .then(|| PolicyModelAuthority::resolve(root, id, runtime))
            .transpose()
            .map_err(|error| error.to_string())
    }

    fn validate_options(options: &ProfileOptions) -> Result<(), String> {
        if options
            .max_output_tokens
            .is_some_and(|limit| !(256..=1_048_576).contains(&limit))
        {
            return Err("Max output tokens must be between 256 and 1,048,576".into());
        }
        if options
            .timeout
            .is_some_and(|timeout| timeout.is_zero() || timeout > Duration::from_secs(86_400))
        {
            return Err("Request timeout must be greater than zero and at most 24 hours".into());
        }
        if options.reasoning_effort.as_deref().is_some_and(|effort| {
            !matches!(
                effort,
                "none" | "on" | "minimal" | "low" | "medium" | "high" | "xhigh"
            )
        }) {
            return Err("Select a supported reasoning effort".into());
        }
        validate_credential(options.credential_env.as_deref())
    }

    fn from_address(
        address: SocketAddr,
        model: &str,
        options: ProfileOptions,
    ) -> Result<Self, String> {
        Self::with_target(Target::External(address), model, options)
    }

    fn with_target(target: Target, model: &str, options: ProfileOptions) -> Result<Self, String> {
        let model = model.trim();
        if !valid_model(model) {
            return Err("Enter the installed model name".into());
        }
        Ok(Self::for_target(target, model.into(), options))
    }

    fn for_target(target: Target, model: String, options: ProfileOptions) -> Self {
        Self {
            target,
            model,
            options,
            prompt_preset: PromptPreset::Generic,
            usage_account: None,
            journal_identity: None,
            vision_mode: VisionMode::Auto,
            lm_studio_auto: None,
            policy_model: None,
            policy_profile: None,
        }
    }

    #[cfg(test)]
    fn external_address(&self) -> SocketAddr {
        match &self.target {
            Target::External(address) => *address,
            Target::LmStudio(endpoint) => endpoint.http_address().expect("numeric HTTP test route"),
            _ => panic!("expected a loopback test profile"),
        }
    }
}

pub(crate) fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= MAX_MODEL_BYTES
        && !model
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

pub(crate) fn numeric_loopback_endpoint(endpoint: &str) -> Result<SocketAddr, String> {
    let endpoint = endpoint.trim();
    let endpoint = endpoint.strip_prefix("http://").unwrap_or(endpoint);
    let endpoint = endpoint.strip_suffix('/').unwrap_or(endpoint);
    let endpoint = endpoint.strip_suffix("/v1").unwrap_or(endpoint);
    parse_loopback(endpoint)
}

pub(crate) fn lm_studio_endpoint(endpoint: &str) -> Result<SocketAddr, String> {
    let endpoint = endpoint.trim().trim_end_matches('/');
    let endpoint = endpoint
        .strip_suffix("/api/v1/chat")
        .or_else(|| endpoint.strip_suffix("/api/v1"))
        .unwrap_or(endpoint);
    numeric_loopback_endpoint(endpoint)
}

pub(crate) fn lm_studio_compatible_endpoint(endpoint: &str) -> Result<SocketAddr, String> {
    let endpoint = endpoint.trim().trim_end_matches('/');
    let endpoint = endpoint.strip_prefix("http://").unwrap_or(endpoint);
    let endpoint = endpoint
        .strip_suffix("/v1/chat/completions")
        .or_else(|| endpoint.strip_suffix("/v1"))
        .unwrap_or(endpoint);
    parse_loopback(endpoint)
}

fn parse_loopback(endpoint: &str) -> Result<SocketAddr, String> {
    endpoint
        .parse::<SocketAddr>()
        .ok()
        .filter(|address| address.ip().is_loopback() && address.port() != 0)
        .ok_or_else(|| "Enter a numeric loopback address and port".into())
}

pub(crate) fn validate_credential(name: Option<&str>) -> Result<(), String> {
    if let Some(name) = name
        && (name.len() > 128
            || !name.bytes().enumerate().all(|(index, byte)| {
                byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
            })
            || name.is_empty())
    {
        return Err("Enter a valid credential environment variable name".into());
    }
    Ok(())
}

fn authorization(name: Option<&str>) -> Result<Option<ureq::http::HeaderValue>, Error> {
    name.map(|name| bearer_header(&credential_value(name)?))
        .transpose()
}

pub(crate) fn credential_value(name: &str) -> Result<Zeroizing<String>, Error> {
    let value = Zeroizing::new(std::env::var(name).map_err(|_| Error::Credential)?);
    require_credential_value(&value)?;
    Ok(value)
}

fn require_credential_value(value: &str) -> Result<(), Error> {
    if value.is_empty() || value.len() > 4096 || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(Error::Credential);
    }
    Ok(())
}

pub(crate) fn credential_echo(response: &str, secret: &str) -> bool {
    fn contains(value: &Value, secret: &str) -> bool {
        match value {
            Value::String(text) => text.contains(secret),
            Value::Array(items) => items.iter().any(|value| contains(value, secret)),
            Value::Object(fields) => fields
                .iter()
                .any(|(name, value)| name.contains(secret) || contains(value, secret)),
            _ => false,
        }
    }
    !secret.is_empty()
        && (response.contains(secret)
            || serde_json::from_str::<Value>(response).is_ok_and(|value| contains(&value, secret)))
}

fn remote_credential(
    name: Option<&str>,
    protocol: RemoteProtocol,
) -> Result<Option<(&'static str, ureq::http::HeaderValue)>, Error> {
    name.map(|name| remote_header(&credential_value(name)?, protocol))
        .transpose()
}

fn remote_header(
    value: &str,
    protocol: RemoteProtocol,
) -> Result<(&'static str, ureq::http::HeaderValue), Error> {
    if !protocol.is_anthropic() {
        return Ok(("authorization", bearer_header(value)?));
    }
    let mut header = ureq::http::HeaderValue::from_str(value).map_err(|_| Error::Credential)?;
    header.set_sensitive(true);
    Ok(("x-api-key", header))
}

const ANTHROPIC_VERSION: &str = "2023-06-01";

fn bearer_header(value: &str) -> Result<ureq::http::HeaderValue, Error> {
    let bearer = Zeroizing::new(format!("Bearer {value}"));
    let mut header = ureq::http::HeaderValue::from_str(&bearer).map_err(|_| Error::Credential)?;
    header.set_sensitive(true);
    Ok(header)
}

pub(crate) struct Pending<T> {
    pub(crate) reply: Receiver<Result<T, String>>,
    pub(crate) cancel: Cancel,
}

pub(crate) struct Cancel {
    activity: Arc<Activity>,
}

impl Drop for Cancel {
    fn drop(&mut self) {
        let mut current = self
            .activity
            .agent
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.activity.cancelled.store(true, Ordering::Release);
        let agent = current.take();
        self.activity.changed.notify_all();
        drop(current);
        if let Some(agent) = agent {
            agent.shutdown();
        }
    }
}

#[derive(Default)]
struct Activity {
    cancelled: AtomicBool,
    agent: Mutex<Option<Agent>>,
    changed: Condvar,
}

impl Activity {
    fn active(&self) -> bool {
        !self.cancelled.load(Ordering::Acquire)
    }

    fn attach(&self, agent: &Agent) -> Result<(), Error> {
        let mut current = self.agent.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.active() {
            return Err(Error::Cancelled);
        }
        *current = Some(agent.clone());
        Ok(())
    }
}

/// A reusable local chat connection holding one of the application's worker slots.
pub(crate) struct Session {
    client: Client,
    _permit: WorkerPermit,
}

impl Session {
    pub(crate) fn begin(profile: Profile) -> Option<(Self, Cancel)> {
        let permit = WorkerPermit::acquire()?;
        let (client, cancel) = Client::new_cancellable(profile);
        Some((
            Self {
                client,
                _permit: permit,
            },
            cancel,
        ))
    }

    pub(crate) fn client(&self) -> &Client {
        &self.client
    }

    #[cfg(test)]
    pub(crate) fn trust_test_server(&mut self, certificate_der: &[u8]) -> Result<(), Error> {
        let certificate = ureq::tls::Certificate::from_der(certificate_der).to_owned();
        let roots = RootCerts::new_with_certs(&[certificate]);
        let timeout = self
            .client
            .profile
            .options
            .timeout
            .unwrap_or(DEFAULT_TIMEOUT);
        let config = connection_config_with_roots(timeout, roots);
        let activity = Arc::clone(&self.client.activity);
        let connection = match &self.client.profile.target {
            Target::LmStudio(endpoint) | Target::LoopbackAnthropic(endpoint)
                if endpoint.is_https() =>
            {
                endpoint.connect(config, activity)
            }
            Target::Remote(base, _) => {
                let url = url::Url::parse(base).map_err(|_| Error::Service)?;
                if !matches!(url.host(), Some(url::Host::Ipv4(address)) if address.is_loopback())
                    && !matches!(url.host(), Some(url::Host::Ipv6(address)) if address.is_loopback())
                {
                    return Err(Error::Service);
                }
                Connection::attach(base.clone(), connection_agent(config), activity)
            }
            _ => Err(Error::Service),
        }?;
        self.client
            .transport
            .get_mut()
            .and_then(|transport| transport.as_mut().ok())
            .ok_or(Error::Service)?
            .connection = connection;
        Ok(())
    }
}

pub(crate) fn start<T: Send + 'static>(
    profile: Profile,
    run: impl FnOnce(&Client) -> Result<T, String> + Send + 'static,
) -> Result<Pending<T>, String> {
    let (session, cancel) = Session::begin(profile).ok_or(WORKERS_BUSY)?;
    #[cfg(test)]
    let session = {
        let mut session = session;
        test_support::configure_https_fixture(&mut session).map_err(Error::message)?;
        session
    };
    start_worker(cancel, move || run(session.client()))
}

fn start_worker<T: Send + 'static>(
    cancel: Cancel,
    run: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<Pending<T>, String> {
    let (send, reply) = async_channel::bounded(1);
    thread::Builder::new()
        .name("kortty-local-ai".into())
        .spawn(move || {
            let _ = send.try_send(run());
        })
        .map_err(|error| format!("Could not start the AI worker ({error})"))?;
    Ok(Pending { reply, cancel })
}

struct WorkerPermit(&'static AtomicUsize);

impl WorkerPermit {
    fn acquire() -> Option<Self> {
        Self::acquire_from(&WORKERS, MAX_WORKERS)
    }

    fn acquire_from(workers: &'static AtomicUsize, limit: usize) -> Option<Self> {
        workers
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < limit).then_some(current + 1)
            })
            .ok()
            .map(|_| Self(workers))
    }
}

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Error {
    Service,
    Credential,
    Status(u16),
    Invalid(&'static str),
    Review(String),
    Cancelled,
    TimedOut,
    UnsupportedResponseFormat,
    Runtime(String),
}

impl Error {
    pub(crate) fn message(self) -> String {
        match self {
            Self::Service => "Could not read the local AI response".into(),
            Self::Credential => {
                "The configured local AI credential is unavailable or invalid".into()
            }
            Self::Status(status) => format!("The local AI server returned HTTP {status}"),
            Self::Invalid(reason) => format!("The local AI response was rejected ({reason})"),
            Self::Review(reason) => reason,
            Self::Cancelled => "The local AI request was cancelled".into(),
            Self::TimedOut => "The local AI request timed out".into(),
            Self::UnsupportedResponseFormat => {
                "The local AI server rejected JSON response mode".into()
            }
            Self::Runtime(reason) => format!("The managed model is unavailable ({reason})"),
        }
    }
}

struct Connection {
    base: String,
    agent: Agent,
    activity: Arc<Activity>,
}

fn connection_config(timeout: Duration) -> Config {
    connection_config_with_roots(timeout, RootCerts::PlatformVerifier)
}

fn connection_config_with_roots(timeout: Duration, roots: RootCerts) -> Config {
    Config::builder()
        .proxy(None)
        .max_redirects(0)
        .max_response_header_size(16 * 1024)
        .timeout_connect(Some(Duration::from_secs(3)))
        .timeout_global(Some(timeout))
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                .root_certs(roots)
                .build(),
        )
        .build()
}

pub(crate) fn service_agent(client: Option<&Client>, timeout: Duration) -> Result<Agent, String> {
    let agent = connection_agent(connection_config(timeout));
    if let Some(client) = client {
        client.activity.attach(&agent).map_err(Error::message)?;
    }
    Ok(agent)
}

#[cfg(windows)]
fn connection_agent(config: Config) -> Agent {
    windows_transport::agent(
        config,
        ureq::unversioned::resolver::DefaultResolver::default(),
    )
}

#[cfg(not(windows))]
fn connection_agent(config: Config) -> Agent {
    Agent::new_with_config(config)
}

impl Connection {
    fn new(address: SocketAddr, timeout: Duration) -> Self {
        Self::with_base(format!("http://{address}"), timeout)
    }

    fn with_base(base: String, timeout: Duration) -> Self {
        let agent = connection_agent(connection_config(timeout));
        let activity = Arc::new(Activity::default());
        Self::attach(base, agent, activity).expect("new connection activity is active")
    }

    fn attach(base: String, agent: Agent, activity: Arc<Activity>) -> Result<Self, Error> {
        activity.attach(&agent)?;
        Ok(Self {
            base,
            agent,
            activity,
        })
    }

    fn active(&self) -> bool {
        self.activity.active()
    }

    fn cancel_handle(&self) -> Cancel {
        Cancel {
            activity: Arc::clone(&self.activity),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

pub(crate) struct Client {
    profile: Profile,
    activity: Arc<Activity>,
    transport: OnceLock<Result<Transport, Error>>,
    usage: Mutex<Option<Usage>>,
}

const LM_STUDIO_METADATA_TIMEOUT: Duration = Duration::from_secs(5);
const LM_STUDIO_METADATA_TTL: Duration = Duration::from_secs(10 * 60);
const LM_STUDIO_METADATA_CACHE_SIZE: usize = 64;

#[derive(PartialEq, Eq)]
struct LmStudioMetadataKey {
    endpoint: String,
    model: String,
    credential: Option<[u8; 32]>,
}

struct LmStudioMetadata {
    key: LmStudioMetadataKey,
    capabilities: Option<LmStudioCapabilities>,
    checked: Instant,
}

static LM_STUDIO_METADATA: Mutex<Vec<LmStudioMetadata>> = Mutex::new(Vec::new());

fn lm_studio_credential_digest(credential: Option<&str>) -> Option<[u8; 32]> {
    credential.map(|value| {
        let mut digest = Sha256::new();
        digest.update(b"kortty LM Studio metadata credential v1\0");
        digest.update(value.as_bytes());
        digest.finalize().into()
    })
}

fn lm_studio_model_suggests_vision(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    let bare = model.rsplit('/').next().unwrap_or_default();
    [
        "vision",
        "multimodal",
        "-vl",
        "vlm",
        "gemini-",
        "gemma-3",
        "gemma3",
        "pixtral",
        "llava",
        "internvl",
        "minicpm-v",
        "molmo",
        "glm-4v",
        "llama-4",
        "llama4",
    ]
    .iter()
    .any(|part| model.contains(part))
        || [
            "chatgpt-4o",
            "gpt-4o",
            "gpt-4.1",
            "gpt-4-turbo",
            "gpt-5",
            "o4",
            "minimax-m3",
        ]
        .iter()
        .any(|prefix| bare.starts_with(prefix))
        || bare.starts_with("o3") && !bare.starts_with("o3-mini")
        || bare.starts_with("o1") && !bare.starts_with("o1-mini")
        || bare.starts_with("claude-")
            && !["claude-1", "claude-2", "claude-instant"]
                .iter()
                .any(|prefix| bare.starts_with(prefix))
}

struct Transport {
    connection: Connection,
    lease: Option<runtime_package::Lease>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompletionLimit {
    Output,
    Context,
    LikelyOutput,
}

#[derive(Default)]
struct AgentCompletion {
    stop: Option<CompletionLimit>,
}

struct UsageAttempt {
    dispatched: bool,
    usage: Usage,
    agent_completion: Option<AgentCompletion>,
    credential: Option<Zeroizing<String>>,
    reviewed_https_credential: Option<Option<Zeroizing<String>>>,
}

impl Client {
    pub(crate) fn profile(&self) -> &Profile {
        &self.profile
    }

    pub(crate) fn render_system_prompt<'a>(&self, system: &'a str) -> Cow<'a, str> {
        self.profile.render_system_prompt(system)
    }

    pub(crate) fn new(profile: Profile) -> Self {
        let transport = OnceLock::new();
        let timeout = profile.options.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let connection = match &profile.target {
            Target::External(address) => Some(Connection::new(*address, timeout)),
            Target::LmStudio(endpoint) | Target::LoopbackAnthropic(endpoint) => {
                Some(endpoint.connection(timeout))
            }
            Target::Remote(base, _) => Some(Connection::with_base(base.clone(), timeout)),
            Target::Managed(..) | Target::ManagedMlx(_) | Target::Cli(..) => None,
        };
        let activity = if let Some(connection) = connection {
            let activity = Arc::clone(&connection.activity);
            let _ = transport.set(Ok(Transport {
                connection,
                lease: None,
            }));
            activity
        } else {
            Arc::new(Activity::default())
        };
        Self {
            profile,
            activity,
            transport,
            usage: Mutex::new(None),
        }
    }

    pub(crate) fn new_cancellable(profile: Profile) -> (Self, Cancel) {
        let client = Self::new(profile);
        let cancel = Cancel {
            activity: Arc::clone(&client.activity),
        };
        (client, cancel)
    }

    pub(crate) fn take_usage(&self) -> Option<Usage> {
        self.usage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    pub(crate) fn estimated_usage_cost(
        &self,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
    ) -> Option<JournalCost> {
        self.profile
            .usage_account
            .as_ref()?
            .budget()
            .journal_cost(input_tokens, output_tokens)
    }

    pub(crate) fn is_local_usage(&self) -> bool {
        !self.profile.is_remote() && !self.profile.is_cli()
    }

    pub(crate) fn journal_identity(&self) -> Option<Arc<JournalProfileIdentity>> {
        self.profile.journal_identity.clone()
    }

    fn record_usage(&self, usage: Usage) {
        *self.usage.lock().unwrap_or_else(PoisonError::into_inner) = usage.reported();
    }

    fn with_completion<T>(
        &self,
        complete: impl FnOnce(&mut UsageAttempt) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let _ = self.take_usage();
        let mut attempt = UsageAttempt {
            dispatched: false,
            usage: Usage::default(),
            agent_completion: None,
            credential: None,
            reviewed_https_credential: None,
        };
        let account = self.profile.usage_account.as_ref();
        let reservation = account.and_then(|account| {
            match account.reserve(self.profile.effective_output_limit()) {
                Ok(reservation) => {
                    attempt.usage.accounting_unavailable |= reservation.sync_failed;
                    Some(reservation)
                }
                Err(_) => {
                    attempt.usage.accounting_unavailable = true;
                    None
                }
            }
        });
        let result = {
            #[cfg(target_os = "macos")]
            let _power = kortty_macos::AppNapActivity::begin();
            complete(&mut attempt)
        };
        if let Some(credential) = &attempt.credential {
            attempt.usage.remove_credential_echo(credential);
        }
        if let Some((account, reservation)) = account.zip(reservation) {
            let outcome = if !attempt.dispatched {
                usage_store::Outcome::Unsent
            } else if result.is_ok() {
                usage_store::Outcome::Complete
            } else {
                usage_store::Outcome::Interrupted
            };
            attempt.usage.accounting_unavailable |= account
                .settle(&reservation, outcome, Some(&attempt.usage))
                .unwrap_or(true);
        }
        if let Some(account) =
            account.filter(|_| attempt.dispatched && attempt.usage.accounting_unavailable)
        {
            account.note_issue();
        }
        self.record_usage(attempt.usage);
        result
    }

    fn transport(&self, deadline: Option<Instant>) -> Result<&Transport, Error> {
        self.transport_with_policy(deadline, ClipboardPolicy::load)
    }

    fn require_transport_policy(
        &self,
        read_policy: impl FnOnce() -> ClipboardPolicy,
    ) -> Result<(), Error> {
        let embedded = self.profile.starts_sidecar();
        if !embedded
            && self.profile.policy_profile.is_none()
            && self.profile.lm_studio_auto.is_none()
            && !self.profile.is_loopback_anthropic()
        {
            return Ok(());
        }
        let policy = read_policy();
        if self.profile.is_loopback_anthropic() {
            Profile::require_loopback_anthropic_allowed(&policy)?;
        }
        if self.profile.lm_studio_auto.is_some() {
            policy_auto_model::require_lm_studio_auto_allowed(&policy)?;
        }
        if let Some(authority) = &self.profile.policy_profile {
            authority.require_current(&policy)?;
        }
        if embedded {
            let result = match &self.profile.policy_model {
                Some(authority) => authority.require_current(&policy),
                None => policy.require_ai_user_model_files(),
            };
            result.map_err(|error| Error::Review(error.to_string()))?;
        }
        Ok(())
    }

    fn transport_with_policy(
        &self,
        deadline: Option<Instant>,
        read_policy: impl FnOnce() -> ClipboardPolicy,
    ) -> Result<&Transport, Error> {
        self.require_transport_policy(read_policy)?;
        let transport = self
            .transport
            .get_or_init(|| {
                let timeout = self.profile.options.timeout.unwrap_or(DEFAULT_TIMEOUT);
                let request_limit = Instant::now() + timeout;
                let deadline =
                    deadline.map_or(request_limit, |deadline| deadline.min(request_limit));
                let lease = match &self.profile.target {
                    Target::Managed(profile_dir, purpose, backend) => {
                        runtime_package::acquire_model(
                            profile_dir,
                            self.model(),
                            self.profile.policy_model.as_ref(),
                            Some(*purpose),
                            *backend,
                            deadline,
                            &self.activity.cancelled,
                        )
                    }
                    Target::ManagedMlx(profile_dir) => runtime_package::acquire_mlx(
                        profile_dir,
                        self.model(),
                        self.profile.policy_model.as_ref(),
                        deadline,
                        &self.activity.cancelled,
                    ),
                    _ => return Err(Error::Service),
                }
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::Interrupted => Error::Cancelled,
                    std::io::ErrorKind::TimedOut => Error::TimedOut,
                    _ => Error::Runtime(error.to_string()),
                })?;
                let connection = Connection::attach(
                    format!("http://{}", lease.sidecar().address()),
                    lease.sidecar().agent(timeout),
                    Arc::clone(&self.activity),
                )?;
                Ok(Transport {
                    connection,
                    lease: Some(lease),
                })
            })
            .as_ref()
            .map_err(Clone::clone)?;
        if self.profile.policy_model.is_some() {
            self.validate_lease(transport)?;
        }
        Ok(transport)
    }

    pub(crate) fn active(&self) -> bool {
        self.activity.active()
    }

    pub(crate) fn test_cli(
        &self,
        before_dispatch: impl Fn() -> Result<(), String>,
    ) -> Result<(), Error> {
        self.run_cli(
            self.profile
                .prepare_cli_chat("Reply with exactly OK.", &[], "Connection test.")?,
            Duration::from_secs(30),
            false,
            before_dispatch,
            |_| {},
        )
        .map(|_| ())
    }

    pub(crate) fn reviewed_cli_chat(
        &self,
        request: cli::Request,
        before_dispatch: impl Fn() -> Result<(), String>,
        emit: impl FnMut(StreamDelta),
    ) -> Result<String, Error> {
        self.run_cli(
            request,
            self.profile.options.timeout.unwrap_or(DEFAULT_TIMEOUT),
            true,
            before_dispatch,
            emit,
        )
    }

    /// Runs one explicitly authorized text prompt through a configured CLI.
    /// The caller supplies the workflow-specific dispatch check and its deadline.
    pub(crate) fn complete_cli_before_guarded(
        &self,
        system: &str,
        input: &str,
        deadline: Instant,
        before_dispatch: impl Fn() -> Result<(), String>,
    ) -> Result<String, Error> {
        let request = self.profile.prepare_cli_chat(system, &[], input)?;
        self.complete_cli_request_before_guarded(request, deadline, before_dispatch)
    }

    /// Executes the same prepared command and prompt files shown in a review.
    pub(crate) fn complete_cli_request_before_guarded(
        &self,
        request: cli::Request,
        deadline: Instant,
        before_dispatch: impl Fn() -> Result<(), String>,
    ) -> Result<String, Error> {
        let timeout = self.profile.options.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let deadline = deadline.min(Instant::now() + timeout);
        self.run_cli_before(request, deadline, false, before_dispatch, |_| {})
    }

    fn run_cli(
        &self,
        request: cli::Request,
        timeout: Duration,
        chat: bool,
        before_dispatch: impl Fn() -> Result<(), String>,
        emit: impl FnMut(StreamDelta),
    ) -> Result<String, Error> {
        self.run_cli_before(
            request,
            Instant::now() + timeout,
            chat,
            before_dispatch,
            emit,
        )
    }

    fn run_cli_before(
        &self,
        request: cli::Request,
        deadline: Instant,
        chat: bool,
        before_dispatch: impl Fn() -> Result<(), String>,
        emit: impl FnMut(StreamDelta),
    ) -> Result<String, Error> {
        let Target::Cli(program, root) = &self.profile.target else {
            return Err(Error::Service);
        };
        cli::require_policy(chat)?;
        #[cfg(target_os = "macos")]
        let _power = kortty_macos::AppNapActivity::begin();
        cli::run(
            program,
            root,
            request,
            deadline,
            &self.activity.cancelled,
            || {
                before_dispatch()?;
                cli::require_policy(chat).map_err(Error::message)
            },
            emit,
        )
    }

    pub(crate) fn complete(&self, system: &str, input: &str) -> Result<String, Error> {
        self.complete_guarded(system, input, || Ok(()))
    }

    pub(crate) fn complete_guarded(
        &self,
        system: &str,
        input: &str,
        before_dispatch: impl FnMut() -> Result<(), String>,
    ) -> Result<String, Error> {
        self.complete_request(
            system,
            RequestContent::Text(input),
            true,
            None,
            false,
            before_dispatch,
        )
    }

    #[cfg(test)]
    pub(crate) fn complete_chat(
        &self,
        system: &str,
        history: &[(&str, &str)],
        input: &str,
    ) -> Result<String, Error> {
        let messages = chat_messages(system, history, input);
        self.complete_messages(&messages, false, None, false, || Ok(()))
    }

    pub(crate) fn complete_before(
        &self,
        system: &str,
        input: &str,
        response_format: bool,
        deadline: Instant,
    ) -> Result<String, Error> {
        self.complete_before_guarded(system, input, response_format, deadline, || Ok(()))
    }

    pub(crate) fn complete_before_guarded(
        &self,
        system: &str,
        input: &str,
        response_format: bool,
        deadline: Instant,
        before_dispatch: impl FnMut() -> Result<(), String>,
    ) -> Result<String, Error> {
        self.complete_text_request(
            system,
            input,
            response_format,
            deadline,
            false,
            before_dispatch,
        )
    }

    /// Reports any observed limit separately from the ordinary completion result.
    #[cfg(test)]
    pub(crate) fn complete_agent_before(
        &self,
        system: &str,
        input: &str,
        response_format: bool,
        deadline: Instant,
    ) -> (Result<String, Error>, Option<CompletionLimit>) {
        self.complete_agent_before_guarded(system, input, response_format, deadline, || Ok(()))
    }

    pub(crate) fn complete_agent_before_guarded(
        &self,
        system: &str,
        input: &str,
        response_format: bool,
        deadline: Instant,
        before_dispatch: impl FnMut() -> Result<(), String>,
    ) -> (Result<String, Error>, Option<CompletionLimit>) {
        let mut stop = None;
        let result = self.with_completion(|attempt| {
            attempt.agent_completion = Some(AgentCompletion::default());
            let result = self.complete_messages_inner(
                &[
                    RequestMessage::text("system", system),
                    RequestMessage::text("user", input),
                ],
                response_format,
                Some(deadline),
                false,
                before_dispatch,
                attempt,
            );
            stop = attempt
                .agent_completion
                .take()
                .and_then(|metadata| metadata.stop);
            result
        });
        (result, stop)
    }

    /// Returns a complete text response for workflows that replace user content.
    pub(crate) fn complete_text_before(
        &self,
        system: &str,
        input: &str,
        response_format: bool,
        deadline: Instant,
    ) -> Result<String, Error> {
        self.complete_text_before_guarded(system, input, response_format, deadline, || Ok(()))
    }

    pub(crate) fn complete_text_before_guarded(
        &self,
        system: &str,
        input: &str,
        response_format: bool,
        deadline: Instant,
        before_dispatch: impl FnMut() -> Result<(), String>,
    ) -> Result<String, Error> {
        self.complete_text_request(
            system,
            input,
            response_format,
            deadline,
            true,
            before_dispatch,
        )
    }

    /// Rejects any change to the credential used when the prompt was reviewed.
    pub(crate) fn complete_reviewed_https_text_before(
        &self,
        system: &str,
        input: &str,
        deadline: Instant,
        credential: Option<Zeroizing<String>>,
        before_dispatch: impl FnMut() -> Result<(), String>,
    ) -> Result<String, Error> {
        self.complete_reviewed_https_before(
            system,
            input,
            true,
            deadline,
            credential,
            before_dispatch,
        )
    }

    pub(crate) fn complete_reviewed_https_plain_text_before(
        &self,
        system: &str,
        input: &str,
        deadline: Instant,
        credential: Option<Zeroizing<String>>,
        before_dispatch: impl FnMut() -> Result<(), String>,
    ) -> Result<String, Error> {
        self.complete_reviewed_https_before(
            system,
            input,
            false,
            deadline,
            credential,
            before_dispatch,
        )
    }

    pub(crate) fn complete_reviewed_https_before(
        &self,
        system: &str,
        input: &str,
        response_format: bool,
        deadline: Instant,
        credential: Option<Zeroizing<String>>,
        before_dispatch: impl FnMut() -> Result<(), String>,
    ) -> Result<String, Error> {
        if !self.profile.is_remote() {
            return Err(Error::Invalid("choose a reviewed HTTPS profile"));
        }
        self.with_completion(|attempt| {
            attempt.reviewed_https_credential = Some(credential);
            self.complete_messages_inner(
                &[
                    RequestMessage::text("system", system),
                    RequestMessage::text("user", input),
                ],
                response_format,
                Some(deadline),
                true,
                before_dispatch,
                attempt,
            )
        })
    }

    fn complete_text_request(
        &self,
        system: &str,
        input: &str,
        response_format: bool,
        deadline: Instant,
        require_complete: bool,
        before_dispatch: impl FnMut() -> Result<(), String>,
    ) -> Result<String, Error> {
        self.with_completion(|attempt| {
            self.complete_messages_inner(
                &[
                    RequestMessage::text("system", system),
                    RequestMessage::text("user", input),
                ],
                response_format,
                Some(deadline),
                require_complete,
                before_dispatch,
                attempt,
            )
        })
    }

    pub(crate) fn model(&self) -> &str {
        self.profile.model()
    }

    pub(crate) fn tokenizer(&self) -> Tokenizer {
        self.profile.tokenizer()
    }

    pub(crate) fn prepare_cli_payload(
        &self,
        system: &str,
        input: &str,
    ) -> Result<cli::Request, Error> {
        self.profile.prepare_cli_chat(system, &[], input)
    }

    pub(crate) fn require_vision_supported(
        &self,
        deadline: Instant,
        authorize: impl Fn() -> Result<(), String>,
    ) -> Result<(), Error> {
        let guard = || authorize().map_err(Error::Review);
        guard()?;
        if !self.active() {
            return Err(Error::Cancelled);
        }
        if self.profile.vision_mode == VisionMode::Disabled {
            return Err(Error::Review(
                "Vision is disabled for the selected AI profile.".into(),
            ));
        }
        if self.profile.vision_mode == VisionMode::Enabled {
            return Ok(());
        }
        if self.profile.uses_server_default_model() {
            return Err(Error::Review(
                "The AI server selects the model. Enable vision explicitly after checking the server's model before sending images.".into(),
            ));
        }
        if !self.profile.supports_lm_studio_metadata() {
            return Ok(());
        }
        let capabilities = self.lm_studio_capabilities(Some(deadline), guard)?;
        match capabilities.and_then(|capabilities| capabilities.vision) {
            Some(true) => Ok(()),
            Some(false) => Err(Error::Review("The selected LM Studio model does not support image input.".into())),
            None if lm_studio_model_suggests_vision(&self.profile.model) => Ok(()),
            None => Err(Error::Review("Image support for the selected LM Studio model could not be determined. Choose a vision-capable model or enable vision in the profile.".into())),
        }
    }

    fn lm_studio_capabilities(
        &self,
        deadline: Option<Instant>,
        authorize: impl Fn() -> Result<(), Error>,
    ) -> Result<Option<LmStudioCapabilities>, Error> {
        if matches!(&self.profile.target, Target::LmStudio(endpoint) if !endpoint.supports_metadata())
        {
            return Ok(None);
        }
        let limit = Instant::now()
            + self
                .profile
                .options
                .timeout
                .unwrap_or(LM_STUDIO_METADATA_TIMEOUT)
                .min(LM_STUDIO_METADATA_TIMEOUT);
        let deadline = deadline.map_or(limit, |deadline| deadline.min(limit));
        if deadline <= Instant::now() {
            return Err(Error::TimedOut);
        }
        let transport = self.transport(Some(deadline))?;
        let credential = self.profile.current_probe_credential()?;
        let secret = credential.as_deref().map(String::as_str);
        let key = LmStudioMetadataKey {
            endpoint: transport.connection.base.clone(),
            model: self.profile.model.clone(),
            credential: lm_studio_credential_digest(secret),
        };
        let require_current = || {
            self.validate_transport(transport)?;
            authorize()?;
            if self.profile.current_probe_credential()? != credential {
                return Err(Error::Review(
                    "The model credential changed. Review the request again.".into(),
                ));
            }
            Ok(())
        };
        require_current()?;
        {
            let mut cache = LM_STUDIO_METADATA
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            cache.retain(|entry| entry.checked.elapsed() < LM_STUDIO_METADATA_TTL);
            if let Some(entry) = cache.iter().find(|entry| entry.key == key) {
                return Ok(entry.capabilities.clone());
            }
        }
        let response = models::lm_studio_catalog_guarded(
            &transport.connection,
            secret,
            deadline,
            require_current,
        );
        require_current()?;
        let capabilities = match response {
            Ok(catalog) => catalog.capability(&self.profile.model).cloned(),
            Err(Error::Service | Error::Status(_) | Error::Invalid(_)) => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut cache = LM_STUDIO_METADATA
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        cache.retain(|entry| entry.key != key);
        if cache.len() == LM_STUDIO_METADATA_CACHE_SIZE {
            cache.remove(0);
        }
        cache.push(LmStudioMetadata {
            key,
            capabilities: capabilities.clone(),
            checked: Instant::now(),
        });
        Ok(capabilities)
    }

    fn require_lm_studio_reasoning(&self, deadline: Option<Instant>) -> Result<(), Error> {
        let Some(option) = self
            .profile
            .options
            .reasoning_effort
            .as_deref()
            .map(providers::lm_reasoning)
        else {
            return Ok(());
        };
        let available = self
            .lm_studio_capabilities(deadline, || Ok(()))?
            .and_then(|capabilities| capabilities.reasoning);
        if available.is_some_and(|reasoning| {
            !reasoning
                .allowed
                .iter()
                .any(|allowed| allowed.as_str() == option)
        }) {
            return Err(Error::Review("The selected LM Studio model does not support this reasoning setting. Select a supported setting or Model default in AI Manager.".into()));
        }
        Ok(())
    }

    pub(crate) fn complete_vision_before_guarded(
        &self,
        system: &str,
        input: &str,
        png_data_url: &str,
        response_format: bool,
        deadline: Instant,
        before_dispatch: impl FnMut() -> Result<(), String>,
    ) -> Result<String, Error> {
        drop(self.take_usage());
        self.require_vision_supported(deadline, || Ok(()))?;
        self.complete_request(
            system,
            RequestContent::Vision([
                VisionPart::Text { text: input },
                VisionPart::ImageUrl {
                    image_url: ImageUrl { url: png_data_url },
                },
            ]),
            response_format,
            Some(deadline),
            true,
            before_dispatch,
        )
    }

    fn complete_request(
        &self,
        system: &str,
        input: RequestContent<'_>,
        response_format: bool,
        deadline: Option<Instant>,
        require_complete: bool,
        before_dispatch: impl FnMut() -> Result<(), String>,
    ) -> Result<String, Error> {
        self.complete_messages(
            &[
                RequestMessage::text("system", system),
                RequestMessage {
                    role: "user",
                    content: input,
                },
            ],
            response_format,
            deadline,
            require_complete,
            before_dispatch,
        )
    }

    fn complete_messages(
        &self,
        messages: &[RequestMessage<'_>],
        response_format: bool,
        deadline: Option<Instant>,
        require_complete: bool,
        before_dispatch: impl FnMut() -> Result<(), String>,
    ) -> Result<String, Error> {
        self.with_completion(|attempt| {
            self.complete_messages_inner(
                messages,
                response_format,
                deadline,
                require_complete,
                before_dispatch,
                attempt,
            )
        })
    }

    fn complete_messages_inner(
        &self,
        messages: &[RequestMessage<'_>],
        response_format: bool,
        deadline: Option<Instant>,
        require_complete: bool,
        mut before_dispatch: impl FnMut() -> Result<(), String>,
        attempt: &mut UsageAttempt,
    ) -> Result<String, Error> {
        if !self.active() {
            return Err(Error::Cancelled);
        }
        if matches!(
            self.profile.target,
            Target::Managed(_, Purpose::Embedding, _)
        ) {
            return Err(Error::Invalid(
                "an embedding model cannot complete a chat request",
            ));
        }
        let deadline = if self.profile.starts_sidecar() {
            let limit = Instant::now() + self.profile.options.timeout.unwrap_or(DEFAULT_TIMEOUT);
            Some(deadline.map_or(limit, |deadline| deadline.min(limit)))
        } else {
            deadline
        };
        self.require_text_for_mlx(messages)?;
        let transport = self.transport(deadline)?;
        if self.profile.uses_lm_studio_protocol() {
            self.require_lm_studio_reasoning(deadline)?;
        }
        let deadline =
            self.before_completion_dispatch(transport, deadline, &mut before_dispatch, attempt)?;
        if transport
            .lease
            .as_ref()
            .is_some_and(|lease| !lease.sidecar().has_vision())
            && messages
                .iter()
                .any(|message| matches!(&message.content, RequestContent::Vision(_)))
        {
            return Err(Error::Invalid(
                "choose a managed GGUF with a vision projector for image requests",
            ));
        }
        let result = if self.profile.uses_lm_studio_protocol() {
            let body = providers::lm_request(self, messages, response_format, false)?;
            let mut response = self.completion_response(transport, &body, deadline, attempt)?;
            let bytes = self.cancellable_response_bytes(&mut response, deadline.is_some())?;
            providers::lm_response_with_stop(
                &bytes,
                Some(self.profile.effective_output_limit()),
                require_complete,
                &mut attempt.usage,
                attempt.agent_completion.as_mut(),
            )
        } else if self.profile.uses_anthropic_protocol() {
            let (mut response, _) = self.anthropic_completion(
                transport,
                (messages, response_format, false),
                deadline,
                before_dispatch,
                attempt,
            )?;
            let bytes = self.cancellable_response_bytes(&mut response, true)?;
            providers::anthropic_response_with_stop(
                &bytes,
                require_complete,
                &mut attempt.usage,
                attempt.agent_completion.as_mut(),
            )
            .map(|(answer, _)| answer)
        } else {
            let response_format = response_format && !self.profile.is_mlx();
            let controls = self.openai_reasoning(transport)?;
            let request = self.openai_payload(messages, response_format, false, controls)?;
            self.openai_response(transport, request, deadline, before_dispatch, attempt)
                .and_then(|response| self.read_completion(response, require_complete, attempt))
                .and_then(|answer| {
                    if controls == mlx_models::ReasoningControls::GptOssEffort {
                        streaming::decode_harmony_content(answer).map(|(answer, _)| answer)
                    } else {
                        Ok(answer)
                    }
                })
        };
        if let Err(error) = self.validate_transport(transport) {
            attempt.agent_completion = None;
            return Err(error);
        }
        result.and_then(|answer| {
            self.completion_answer(
                answer,
                attempt.credential.as_deref().map(String::as_str),
                require_complete || response_format || attempt.agent_completion.is_some(),
            )
        })
    }

    fn completion_answer(
        &self,
        answer: String,
        credential: Option<&str>,
        requires_intact_answer: bool,
    ) -> Result<String, Error> {
        if requires_intact_answer
            && (self.profile.is_remote() || self.profile.is_loopback_anthropic())
            && credential.is_some_and(|secret| credential_echo(&answer, secret))
        {
            // A redacted command or generated artifact must not become a new proposal.
            drop(Zeroizing::new(answer));
            return Err(Error::Review(
                "The HTTPS model response echoed a credential.".into(),
            ));
        }
        Ok(policy_profile::redact_echo(answer, credential))
    }

    #[cfg(test)]
    fn openai_request(
        &self,
        messages: &[RequestMessage<'_>],
        response_format: bool,
        stream: bool,
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        self.openai_payload(
            messages,
            response_format,
            stream,
            mlx_models::ReasoningControls::DefaultOnly,
        )?
        .encode()
    }

    fn openai_payload<'a>(
        &'a self,
        messages: &'a [RequestMessage<'a>],
        response_format: bool,
        stream: bool,
        mlx_reasoning: mlx_models::ReasoningControls,
    ) -> Result<Request<'a>, Error> {
        let openai = matches!(&self.profile.target, Target::Remote(base, protocol)
            if protocol.is_openai() && base.starts_with("https://api.openai.com/"));
        let limit = self.profile.effective_output_limit();
        let mlx = self.profile.is_mlx();
        let effort = self.profile.options.reasoning_effort.as_deref();
        let chat_template_kwargs = if mlx {
            effort
                .map(|effort| mlx_reasoning.arguments(effort))
                .transpose()
                .map_err(Error::Invalid)?
        } else {
            None
        };
        let reasoning_effort = if mlx { None } else { effort };
        let is_minimax_flash = self
            .model()
            .rsplit('/')
            .next()
            .and_then(|model| model.split(':').next())
            .is_some_and(|model| model.eq_ignore_ascii_case("MiniMax-M3.1-Flash"));
        if is_minimax_flash && reasoning_effort == Some("none") {
            return Err(Error::Invalid(
                "MiniMax-M3.1-Flash does not support the None reasoning setting. Choose Model default or another reasoning level.",
            ));
        }
        let thinking = !mlx
            && !self.profile.uses_server_default_model()
            && !is_minimax_flash
            && matches!(reasoning_effort, None | Some("none"))
            && (contains_minimax(self.model())
                || matches!(&self.profile.target, Target::Remote(base, _) if contains_minimax(base)));
        Ok(Request {
            model: self.profile.requested_chat_model().filter(|_| !mlx),
            stream,
            temperature: (!openai).then_some(0),
            max_tokens: (!openai).then_some(limit),
            max_completion_tokens: openai.then_some(limit),
            store: openai.then_some(false),
            stream_options: (openai && stream).then_some(StreamOptions {
                include_usage: true,
            }),
            reasoning_effort,
            chat_template_kwargs,
            thinking: thinking.then_some(Thinking { kind: "disabled" }),
            messages,
            response_format: (response_format && !mlx).then_some(ResponseFormat {
                kind: "json_object",
            }),
        })
    }

    fn require_text_for_mlx(&self, messages: &[RequestMessage<'_>]) -> Result<(), Error> {
        if self.profile.is_mlx()
            && messages
                .iter()
                .any(|message| matches!(message.content, RequestContent::Vision(_)))
        {
            return Err(Error::Invalid("managed MLX profiles accept text only"));
        }
        Ok(())
    }

    fn openai_reasoning(
        &self,
        transport: &Transport,
    ) -> Result<mlx_models::ReasoningControls, Error> {
        if !self.profile.is_mlx() {
            return Ok(mlx_models::ReasoningControls::DefaultOnly);
        }
        // Recheck the leased model and signed runtime immediately before using capabilities.
        self.validate_lease(transport)?;
        Ok(transport
            .lease
            .as_ref()
            .map_or(mlx_models::ReasoningControls::DefaultOnly, |lease| {
                lease.sidecar().mlx_reasoning_controls()
            }))
    }

    fn validate_transport(&self, transport: &Transport) -> Result<(), Error> {
        self.validate_lease(transport)?;
        if let Some(authority) = &self.profile.policy_profile {
            authority.require_current(&ClipboardPolicy::load())?;
        }
        Ok(())
    }

    fn validate_lease(&self, transport: &Transport) -> Result<(), Error> {
        if !self.active() {
            return Err(Error::Cancelled);
        }
        if let Some(lease) = &transport.lease {
            lease
                .sidecar()
                .validate()
                .map_err(|error| Error::Runtime(error.to_string()))?;
        }
        Ok(())
    }

    fn before_completion_dispatch(
        &self,
        transport: &Transport,
        deadline: Option<Instant>,
        before_dispatch: impl FnOnce() -> Result<(), String>,
        attempt: &UsageAttempt,
    ) -> Result<Option<Instant>, Error> {
        let deadline = if let Some(resolved) = &self.profile.lm_studio_auto {
            let limit = Instant::now() + self.profile.options.timeout.unwrap_or(DEFAULT_TIMEOUT);
            let deadline = Some(deadline.map_or(limit, |deadline| deadline.min(limit)));
            let credential = self.completion_credential(attempt)?;
            resolved.require_current_model(
                &transport.connection,
                credential.as_deref().map(String::as_str),
                deadline,
                || {
                    self.require_transport_policy(ClipboardPolicy::load)?;
                    self.completion_credential(attempt).map(drop)
                },
            )?;
            deadline
        } else if let Some(resolved) = self
            .profile
            .policy_profile
            .as_ref()
            .and_then(PolicyProfileAuthority::auto_model)
        {
            let limit = Instant::now() + self.profile.options.timeout.unwrap_or(DEFAULT_TIMEOUT);
            let deadline = Some(deadline.map_or(limit, |deadline| deadline.min(limit)));
            let credential = self.completion_credential(attempt)?;
            resolved.require_current_model(
                &transport.connection,
                credential.as_deref().map(String::as_str),
                deadline,
                || self.require_transport_policy(ClipboardPolicy::load),
            )?;
            deadline
        } else {
            deadline
        };
        before_dispatch().map_err(Error::Review)?;
        Ok(deadline)
    }

    fn request_authorization(
        &self,
        transport: &Transport,
        policy_credential: Option<&str>,
    ) -> Result<Option<ureq::http::HeaderValue>, Error> {
        match &transport.lease {
            Some(lease) => Ok(Some(lease.sidecar().authorization().clone())),
            None => match policy_credential {
                Some(value) => bearer_header(value).map(Some),
                None => authorization(self.profile.options.credential_env.as_deref()),
            },
        }
    }

    fn completion_credential(
        &self,
        attempt: &UsageAttempt,
    ) -> Result<Option<Zeroizing<String>>, Error> {
        let mut credential = self.profile.policy_credential()?;
        if credential.is_none() && !self.profile.starts_sidecar() {
            credential = self
                .profile
                .options
                .credential_env
                .as_deref()
                .map(credential_value)
                .transpose()?;
        }
        if attempt
            .reviewed_https_credential
            .as_ref()
            .is_some_and(|reviewed| *reviewed != credential)
        {
            return Err(Error::Review(
                if self.profile.is_remote() {
                    "The HTTPS credential changed. Review the request again."
                } else {
                    "The model credential changed. Review the request again."
                }
                .into(),
            ));
        }
        if let Some(resolved) = &self.profile.lm_studio_auto {
            resolved.require_credential(credential.as_deref().map(String::as_str))?;
        }
        Ok(credential)
    }

    fn openai_response(
        &self,
        transport: &Transport,
        mut request: Request<'_>,
        deadline: Option<Instant>,
        before_retry: impl FnMut() -> Result<(), String>,
        attempt: &mut UsageAttempt,
    ) -> Result<OpenAiResponse, Error> {
        let detect_format = deadline.is_some() && request.response_format.is_some();
        let limit = self.profile.options.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let deadline = if request.thinking.is_some() {
            let cutoff = Instant::now() + limit;
            Some(deadline.map_or(cutoff, |deadline| deadline.min(cutoff)))
        } else {
            deadline
        };
        let mut response =
            self.completion_response(transport, &request.encode()?, deadline, attempt)?;
        let mut buffered = None;
        let mut retry = false;
        if request.thinking.is_some() {
            if response.status().as_u16() == 400 {
                let error = completion_error_bytes(&mut response).ok();
                if let Some(bytes) = &error {
                    retry = unsupported_thinking(bytes, &mut attempt.usage);
                }
                if !retry {
                    return Err(
                        if detect_format
                            && error
                                .as_ref()
                                .is_some_and(|bytes| unsupported_response_format(bytes))
                        {
                            Error::UnsupportedResponseFormat
                        } else {
                            Error::Status(400)
                        },
                    );
                }
            } else if response.status().is_success() && !is_event_stream(&response) {
                let bytes = self.cancellable_response_bytes(&mut response, true)?;
                retry = providers::minimax_thinking_rejected_envelope(&bytes);
                buffered = Some(bytes);
            }
        }
        if retry {
            self.validate_transport(transport)?;
            if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
                return Err(Error::TimedOut);
            }
            // Never replay with another credential, even for unreviewed loopback requests.
            attempt.reviewed_https_credential = Some(attempt.credential.clone());
            self.before_completion_dispatch(transport, deadline, before_retry, attempt)?;
            request.thinking = None;
            response =
                self.completion_response(transport, &request.encode()?, deadline, attempt)?;
            buffered = None;
        }
        let status = response.status().as_u16();
        if detect_format && matches!(status, 400 | 422) {
            if let Ok(bytes) = completion_error_bytes(&mut response) {
                let _ = observe_openai_error_usage(&bytes, &mut attempt.usage);
                if unsupported_response_format(&bytes) {
                    return Err(Error::UnsupportedResponseFormat);
                }
            }
            return Err(Error::Status(status));
        }
        completion_status(&response)?;
        Ok(OpenAiResponse {
            response,
            buffered,
            deadline,
        })
    }

    fn read_completion(
        &self,
        mut response: OpenAiResponse,
        require_complete: bool,
        attempt: &mut UsageAttempt,
    ) -> Result<String, Error> {
        let bounded = response.deadline.is_some() || self.profile.options.timeout.is_some();
        let bytes = match response.buffered {
            Some(bytes) => bytes,
            None => response_bytes(&mut response.response, bounded)?,
        };
        let answer = if !require_complete {
            let choice = first_choice::<String>(&bytes, &mut attempt.usage)?;
            if let Some(metadata) = &mut attempt.agent_completion
                && choice.finish_reason.as_ref().and_then(Value::as_str) == Some("length")
            {
                metadata.stop = Some(CompletionLimit::Output);
            }
            choice
                .message
                .content
                .ok_or(Error::Invalid("missing message content"))?
        } else {
            let choice = first_choice::<Value>(&bytes, &mut attempt.usage)?;
            if choice.finish_reason.as_ref().and_then(Value::as_str) == Some("length") {
                return Err(Error::Invalid("model output reached its token limit"));
            }
            match choice.message.content {
                Some(Value::String(text)) => text,
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
                Some(_) => return Err(Error::Invalid("invalid chat response")),
                None => return Err(Error::Invalid("missing message content")),
            }
        };
        Ok(answer)
    }

    fn completion_response(
        &self,
        transport: &Transport,
        body: &[u8],
        deadline: Option<Instant>,
        attempt: &mut UsageAttempt,
    ) -> Result<ureq::http::Response<ureq::Body>, Error> {
        let endpoint = transport.connection.url(match &self.profile.target {
            Target::LmStudio(endpoint) | Target::LoopbackAnthropic(endpoint) => {
                endpoint.completion_path()
            }
            Target::Remote(_, protocol) => protocol.completion_path(),
            _ => "/v1/chat/completions",
        });
        let mut request = transport
            .connection
            .agent
            .post(endpoint)
            .content_type("application/json");
        attempt.credential = self.completion_credential(attempt)?;
        let credential_protocol = match self.profile.target {
            Target::Remote(_, protocol) => Some(protocol),
            Target::LoopbackAnthropic(_) => Some(RemoteProtocol::Anthropic),
            _ => None,
        };
        if let Some(protocol) = credential_protocol {
            if protocol.is_anthropic() {
                request = request.header("anthropic-version", ANTHROPIC_VERSION);
            }
            if let Some(secret) = &attempt.credential {
                let (name, value) = remote_header(secret, protocol)?;
                request = request.header(name, value);
            }
        } else if let Some(header) = self
            .request_authorization(transport, attempt.credential.as_deref().map(String::as_str))?
        {
            request = request.header("authorization", header);
        }
        let mut request = request.config().http_status_as_error(false);
        if let Some(deadline) = deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::TimedOut);
            }
            request = request.timeout_global(Some(
                self.profile
                    .options
                    .timeout
                    .map_or(remaining, |timeout| timeout.min(remaining)),
            ));
        }
        let bounded = deadline.is_some() || self.profile.options.timeout.is_some();
        if !self.active() {
            return Err(Error::Cancelled);
        }
        self.require_transport_policy(ClipboardPolicy::load)?;
        attempt.dispatched = true;
        let response = request.build().send(body).map_err(|error| {
            if self.active() {
                transport_error(error, bounded)
            } else {
                Error::Cancelled
            }
        })?;
        attempt.usage.observe_headers(response.headers());
        Ok(response)
    }

    fn cancellable_response_bytes(
        &self,
        response: &mut ureq::http::Response<ureq::Body>,
        bounded: bool,
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        response_bytes(response, bounded).map_err(|error| {
            if self.active() {
                error
            } else {
                Error::Cancelled
            }
        })
    }

    /// Waits between requests without keeping a socket open or delaying a UI abort.
    pub(crate) fn wait_until(&self, wake_at: Instant, cancelled: &dyn Fn() -> bool) -> bool {
        loop {
            if cancelled() {
                return false;
            }
            let activity = self
                .activity
                .agent
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if !self.active() {
                return false;
            }
            let remaining = wake_at.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return true;
            }
            // Dropping Cancel wakes immediately. The short ceiling also polls callers
            // whose cancellation flag may change independently of the socket handle.
            drop(
                self.activity
                    .changed
                    .wait_timeout(activity, remaining.min(Duration::from_millis(50))),
            );
        }
    }
}

fn completion_status(response: &ureq::http::Response<ureq::Body>) -> Result<(), Error> {
    if response.status().is_success() {
        return Ok(());
    }
    Err(Error::Status(response.status().as_u16()))
}

fn completion_error_bytes(
    response: &mut ureq::http::Response<ureq::Body>,
) -> Result<Zeroizing<Vec<u8>>, ureq::Error> {
    response
        .body_mut()
        .with_config()
        .limit(MAX_ERROR_RESPONSE_BYTES)
        .read_to_vec()
        .map(Zeroizing::new)
}

fn contains_minimax(value: &str) -> bool {
    value
        .as_bytes()
        .windows(b"minimax".len())
        .any(|part| part.eq_ignore_ascii_case(b"minimax"))
}

fn is_event_stream(response: &ureq::http::Response<ureq::Body>) -> bool {
    response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("text/event-stream"))
}

fn unsupported_thinking(bytes: &[u8], usage: &mut Usage) -> bool {
    let Some(metadata) = observe_openai_error_usage(bytes, usage) else {
        return false;
    };
    if metadata.usage.is_some()
        || metadata
            .choices
            .as_ref()
            .is_some_and(|choices| choices.as_array().is_none_or(|choices| !choices.is_empty()))
    {
        return false;
    }
    let Ok(response) = serde_json::from_slice::<ErrorResponse>(bytes) else {
        return false;
    };
    let error = response.error;
    let explicit = matches!(error.param.as_deref(), Some("thinking" | "thinking.type"));
    if error.param.is_some() && !explicit {
        return false;
    }
    error.code.as_deref() == Some("unsupported_thinking")
        || explicit
            && matches!(
                error.code.as_deref(),
                Some("unsupported_parameter" | "unknown_parameter" | "unsupported_value")
            )
        || error
            .message
            .as_deref()
            .is_some_and(providers::thinking_rejection_message)
}

fn observe_openai_error_usage(bytes: &[u8], usage: &mut Usage) -> Option<CompletionMetadata> {
    let metadata: CompletionMetadata = serde_json::from_slice(bytes).ok()?;
    usage.observe_openai(
        metadata.usage.as_ref(),
        metadata.model.as_ref(),
        metadata.id.as_ref(),
    );
    Some(metadata)
}

fn response_bytes(
    response: &mut ureq::http::Response<ureq::Body>,
    bounded: bool,
) -> Result<Zeroizing<Vec<u8>>, Error> {
    if !response.status().is_success() {
        return Err(Error::Status(response.status().as_u16()));
    }
    response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_to_vec()
        .map(Zeroizing::new)
        .map_err(|error| match error {
            ureq::Error::BodyExceedsLimit(_) => Error::Invalid("response exceeds 256 KiB"),
            error => transport_error(error, bounded),
        })
}

fn transport_error(error: ureq::Error, bounded: bool) -> Error {
    if bounded && matches!(error, ureq::Error::Timeout(_)) {
        Error::TimedOut
    } else {
        Error::Service
    }
}

fn unsupported_response_format(bytes: &[u8]) -> bool {
    let Ok(response) = serde_json::from_slice::<ErrorResponse>(bytes) else {
        return false;
    };
    let error = response.error;
    let format_parameter = error.param.as_deref() == Some("response_format");
    if error.code.as_deref() == Some("unsupported_response_format")
        || (format_parameter
            && matches!(
                error.code.as_deref(),
                Some("unsupported_value" | "unsupported_parameter")
            ))
    {
        return true;
    }
    if error.param.is_some() && !format_parameter {
        return false;
    }
    error.message.is_some_and(|message| {
        let message = message.to_ascii_lowercase().replace(['\'', '"', '`'], "");
        (format_parameter && (message.contains("not supported") || message.contains("unsupported")))
            || ["response_format", "json_object", "json mode"]
                .iter()
                .any(|format| {
                    [
                        format!("{format} is not supported"),
                        format!("{format} not supported"),
                        format!("{format} is unsupported"),
                        format!("does not support {format}"),
                        format!("unsupported {format}"),
                        format!("unsupported parameter: {format}"),
                    ]
                    .iter()
                    .any(|evidence| message.contains(evidence))
                })
    })
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: ErrorDetails,
}

#[derive(Deserialize)]
struct ErrorDetails {
    param: Option<String>,
    code: Option<String>,
    message: Option<String>,
}

fn first_choice<T: DeserializeOwned>(bytes: &[u8], usage: &mut Usage) -> Result<Choice<T>, Error> {
    let completion: Completion<T> = match serde_json::from_slice(bytes) {
        Ok(completion) => completion,
        Err(_) => {
            // Recover usage separately after the full typed choices check fails.
            if bytes.trim_ascii_start().starts_with(b"{")
                && let Ok(metadata) = serde_json::from_slice::<CompletionMetadata>(bytes)
            {
                usage.observe_openai(
                    metadata.usage.as_ref(),
                    metadata.model.as_ref(),
                    metadata.id.as_ref(),
                );
                if let Some(error) = providers::openai_provider_error(
                    metadata.error.as_ref(),
                    metadata.base_resp.as_ref(),
                    metadata
                        .choices
                        .as_ref()
                        .and_then(Value::as_array)
                        .is_some_and(|choices| !choices.is_empty()),
                ) {
                    return Err(error);
                }
            }
            return Err(Error::Invalid("invalid chat response"));
        }
    };
    usage.observe_openai(
        completion.usage.as_ref(),
        completion.model.as_ref(),
        completion.id.as_ref(),
    );
    if let Some(error) = providers::openai_provider_error(
        completion.error.as_ref(),
        completion.base_resp.as_ref(),
        !completion.choices.is_empty(),
    ) {
        return Err(error);
    }
    completion
        .choices
        .into_iter()
        .next()
        .ok_or(Error::Invalid("missing message content"))
}

#[derive(Deserialize)]
struct Completion<T> {
    choices: Vec<Choice<T>>,
    usage: Option<Value>,
    model: Option<Value>,
    id: Option<Value>,
    error: Option<Value>,
    base_resp: Option<Value>,
}

#[derive(Deserialize)]
struct CompletionMetadata {
    usage: Option<Value>,
    model: Option<Value>,
    id: Option<Value>,
    error: Option<Value>,
    base_resp: Option<Value>,
    choices: Option<Value>,
}

#[derive(Deserialize)]
struct Choice<T> {
    message: ChatMessage<T>,
    finish_reason: Option<Value>,
}

#[derive(Deserialize)]
struct ChatMessage<T> {
    content: Option<T>,
}

#[derive(Serialize)]
struct Request<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<mlx_models::TemplateArguments<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Thinking>,
    messages: &'a [RequestMessage<'a>],
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

impl Request<'_> {
    fn encode(&self) -> Result<Zeroizing<Vec<u8>>, Error> {
        serde_json::to_vec(self)
            .map(Zeroizing::new)
            .map_err(|_| Error::Service)
    }
}

struct OpenAiResponse {
    response: ureq::http::Response<ureq::Body>,
    buffered: Option<Zeroizing<Vec<u8>>>,
    deadline: Option<Instant>,
}

#[derive(Serialize)]
struct Thinking {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct RequestMessage<'a> {
    role: &'static str,
    content: RequestContent<'a>,
}

fn chat_messages<'a>(
    system: &'a str,
    history: &[(&'a str, &'a str)],
    input: &'a str,
) -> Vec<RequestMessage<'a>> {
    let mut messages = Vec::with_capacity(2 + 2 * history.len());
    messages.push(RequestMessage::text("system", system));
    for &(question, answer) in history {
        if !question.is_empty() {
            messages.push(RequestMessage::text("user", question));
        }
        if !answer.is_empty() {
            messages.push(RequestMessage::text("assistant", answer));
        }
    }
    messages.push(RequestMessage::text("user", input));
    messages
}

pub(crate) fn preview_chat(
    system: &str,
    history: &[(&str, &str)],
    input: &str,
) -> Result<String, Error> {
    serde_json::to_string_pretty(&chat_messages(system, history, input)).map_err(|_| Error::Service)
}

impl<'a> RequestMessage<'a> {
    fn text(role: &'static str, text: &'a str) -> Self {
        Self {
            role,
            content: RequestContent::Text(text),
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum RequestContent<'a> {
    Text(&'a str),
    Vision([VisionPart<'a>; 2]),
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum VisionPart<'a> {
    Text { text: &'a str },
    ImageUrl { image_url: ImageUrl<'a> },
}

#[derive(Serialize)]
struct ImageUrl<'a> {
    url: &'a str,
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
#[test]
fn embedded_model_policy_blocks_new_and_cached_transports_for_both_runtimes() {
    let root = crate::snippets::tests::Profile::new();
    let id = "0123456789abcdef0123456789abcdef";
    let denied = || ClipboardPolicy {
        user_model_files_allowed: false,
        ..ClipboardPolicy::default()
    };
    for profile in [
        Profile::managed(root.0.clone(), id, ProfileOptions::default()).unwrap(),
        Profile::managed_embedding(root.0.clone(), id).unwrap(),
        Profile::managed_mlx(root.0.clone(), id, ProfileOptions::default()).unwrap(),
    ] {
        let client = Client::new(profile);
        let check_denial = || {
            assert!(matches!(
                client.transport_with_policy(None, denied),
                Err(Error::Review(message)) if message.contains("organization's policy")
            ));
        };
        check_denial();
        assert!(client.transport.get().is_none());
        assert!(
            client
                .transport
                .set(Ok(Transport {
                    connection: Connection::new("127.0.0.1:1".parse().unwrap(), DEFAULT_TIMEOUT),
                    lease: None,
                }))
                .is_ok()
        );
        assert!(
            client
                .transport_with_policy(None, ClipboardPolicy::default)
                .is_ok()
        );
        check_denial();
    }

    let external = Client::new(Profile::new("127.0.0.1:1", "fixture").unwrap());
    assert!(
        external
            .transport_with_policy(None, || panic!("external models are outside this rule"))
            .is_ok()
    );
}

#[cfg(test)]
#[test]
fn remote_structured_credential_echo_is_rejected_without_altering_the_proposal() {
    let client = Client::new(
        Profile::remote(
            "https://example.invalid/v1",
            "fixture",
            RemoteProtocol::OpenAi,
            ProfileOptions::default(),
        )
        .unwrap(),
    );
    let credential = Some("fixture-secret");
    for answer in [
        r#"{"command":"echo fixture-secret"}"#,
        r#"{"command":"echo fixture-\u0073ecret"}"#,
    ] {
        assert!(matches!(
            client.completion_answer(answer.into(), credential, true),
            Err(Error::Review(message)) if message == "The HTTPS model response echoed a credential."
        ));
    }
    assert_eq!(
        client
            .completion_answer("Output: fixture-secret".into(), credential, false)
            .unwrap(),
        "Output: █",
    );
}

#[cfg(test)]
#[test]
fn json_format_refusal_requires_explicit_unsupported_evidence() {
    use serde_json::json;

    for error in [
        json!({"code":"unsupported_response_format"}),
        json!({"code":"unsupported_value","param":"response_format"}),
        json!({"param":"response_format","code":null,"message":"Invalid parameter: 'response_format' of type 'json_object' is not supported with this model."}),
        json!({"message":"This model does not support JSON mode."}),
        json!({"message":"`json_object` not supported."}),
        json!({"message":"Unsupported parameter: 'response_format'"}),
    ] {
        let envelope = json!({"error":error}).to_string();
        assert!(
            unsupported_response_format(envelope.as_bytes()),
            "{envelope}"
        );
    }
    for error in [
        json!({"code":"unsupported_value","param":"temperature"}),
        json!({"param":"response_format","message":"Expected a valid json_object."}),
        json!({"param":"messages","message":"The prompt contains the text JSON mode is not supported."}),
        json!({"message":"Invalid response_format json_object: supply JSON mode settings."}),
        json!({"message":"The requested model is not supported with this input."}),
    ] {
        let envelope = json!({"error":error}).to_string();
        assert!(
            !unsupported_response_format(envelope.as_bytes()),
            "{envelope}"
        );
    }
}

#[cfg(test)]
#[test]
fn agent_completion_observes_openai_length_without_changing_ordinary_completion() {
    use serde_json::json;

    let response = |content, reason| {
        test_support::fixture_http_response(
            "200 OK",
            "",
            &json!({
                "choices": [{"finish_reason": reason, "message": {"content": content}}],
                "usage": {"prompt_tokens": 15, "completion_tokens": 512, "total_tokens": 527},
            })
            .to_string(),
        )
    };
    let empty = response(Value::Null, "length");
    let full = json!(r#"{"valid":true}"#);
    let (profile, endpoint) = test_support::start_test_double(vec![
        empty.clone(),
        empty.clone(),
        empty,
        response(full.clone(), "length"),
        response(full, "stop"),
    ]);
    let client = Client::new(profile.with_output_limit(512));
    let deadline = || Instant::now() + Duration::from_secs(4);
    let (answer, stop) = client.complete_agent_before("system", "request", true, deadline());
    assert!(matches!(
        answer,
        Err(Error::Invalid("missing message content"))
    ));
    assert_eq!(stop, Some(CompletionLimit::Output));
    let usage = client.take_usage().unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.total_tokens),
        (Some(15), Some(512), Some(527))
    );
    assert!(matches!(
        client.complete_before("system", "request", true, deadline()),
        Err(Error::Invalid("missing message content"))
    ));
    assert!(matches!(
        client.complete_text_before("system", "request", true, deadline()),
        Err(Error::Invalid("model output reached its token limit"))
    ));
    for expected in [Some(CompletionLimit::Output), None] {
        let (answer, stop) = client.complete_agent_before("system", "request", true, deadline());
        assert_eq!(answer.unwrap(), r#"{"valid":true}"#);
        assert_eq!(stop, expected);
    }
    let requests = endpoint.join().unwrap();
    assert_eq!(requests.len(), 5);
    assert!(requests.iter().all(|request| request["max_tokens"] == 512));
}

#[cfg(test)]
#[test]
fn mlx_openai_request_keeps_the_pinned_model_and_unsupported_fields_out_of_the_payload() {
    use mlx_models::ReasoningControls::{DefaultOnly, GptOssEffort, QwenThinking};

    let root = crate::snippets::tests::Profile::new();
    let profile = Profile::managed_mlx(
        root.0.clone(),
        "0123456789abcdef0123456789abcdef",
        ProfileOptions {
            max_output_tokens: Some(256),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!profile.is_remote());
    let messages = [RequestMessage::text("user", "fixture")];
    for (effort, controls, expected) in [
        (None, DefaultOnly, Value::Null),
        (
            Some("low"),
            GptOssEffort,
            serde_json::json!({"reasoning_effort": "low"}),
        ),
        (
            Some("none"),
            QwenThinking,
            serde_json::json!({"enable_thinking": false}),
        ),
        (
            Some("on"),
            QwenThinking,
            serde_json::json!({"enable_thinking": true}),
        ),
    ] {
        let mut profile = profile.clone();
        profile.options.reasoning_effort = effort.map(str::to_owned);
        let client = Client::new(profile);
        for stream in [false, true] {
            let payload: Value = serde_json::from_slice(
                &client
                    .openai_payload(&messages, true, stream, controls)
                    .unwrap()
                    .encode()
                    .unwrap(),
            )
            .unwrap();
            assert!(payload.get("model").is_none());
            assert!(payload.get("response_format").is_none());
            assert!(payload.get("reasoning_effort").is_none());
            assert!(payload.get("thinking").is_none());
            assert_eq!(payload["chat_template_kwargs"], expected);
            assert_eq!(payload["stream"], stream);
            assert_eq!(payload["max_tokens"], 256);
            assert_eq!(payload["messages"][0]["content"], "fixture");
        }
    }
    let mut profile = profile;
    profile.options.reasoning_effort = Some("high".into());
    let client = Client::new(profile);
    assert!(matches!(
        client.openai_payload(&messages, false, true, DefaultOnly),
        Err(Error::Invalid(_)),
    ));
}
