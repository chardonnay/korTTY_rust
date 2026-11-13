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
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
use ureq::{Agent, config::Config};
use zeroize::Zeroizing;

use crate::clipboard_policy::{ClipboardPolicy, PolicyModelRuntime, valid_policy_model_id};

pub(crate) mod cli;
pub(crate) mod managed_models;
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
use managed_models::Purpose;
pub(crate) use models::discover_policy_profile;
pub(crate) use models::{
    LocalModelServer, MAX_LOCAL_SCAN_ENDPOINTS, discover_lm_studio, discover_remote,
    scan_local_models,
};
pub(crate) use models::{discover, discover_with_credential};
pub(crate) use policy_authority::PolicyModelAuthority;
use policy_profile::PolicyProfileAuthority;
pub(crate) use policy_profile::{PolicyAutoModel, resolve_policy_auto_model};
pub(crate) use prompt_preset::PromptPreset;
pub(crate) use providers::{RemoteProtocol, remote_api_base, remote_destination};
use runtime_package::BackendUse;
pub(crate) use streaming::StreamDelta;
pub(crate) use tokenizer::Tokenizer;
pub(crate) use usage::Usage;
pub(crate) use usage_store::take_notice as take_usage_notice;
pub(crate) use usage_store::{
    Account as UsageAccount, Budget as UsageBudget, Counters as UsageCounters,
    Snapshot as UsageSnapshot, Warning as UsageWarning,
};

pub(crate) const DEFAULT_ENDPOINT: &str = "127.0.0.1:11434";

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
    policy_model: Option<PolicyModelAuthority>,
    policy_profile: Option<PolicyProfileAuthority>,
}

#[derive(Clone, Eq, PartialEq)]
enum Target {
    External(SocketAddr),
    LmStudio(SocketAddr),
    PolicyAuto(policy_profile::AutoModelTarget),
    Remote(String, RemoteProtocol),
    Managed(PathBuf, Purpose, BackendUse),
    ManagedMlx(PathBuf),
    Cli(cli::Program, PathBuf),
}

impl Profile {
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
            || matches!(&self.target, Target::PolicyAuto(target) if target.is_https())
    }

    fn uses_lm_studio_protocol(&self) -> bool {
        matches!(self.target, Target::LmStudio(_))
            || matches!(&self.target, Target::PolicyAuto(target) if target.uses_native_protocol())
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
        self.options.max_output_tokens = Some(
            self.options
                .max_output_tokens
                .map_or(limit, |configured| configured.min(limit)),
        );
        self
    }

    pub(crate) fn effective_output_limit(&self) -> u32 {
        self.options.max_output_tokens.unwrap_or(4096)
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
        Self::validate_options(&options)?;
        let mut profile = Self::from_address(lm_studio_endpoint(endpoint)?, model, options)?;
        if let Target::External(address) = profile.target {
            profile.target = Target::LmStudio(address);
        }
        Ok(profile)
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
        Ok(Self {
            target: Target::Remote(providers::remote_api_base(endpoint, protocol)?, protocol),
            model: model.into(),
            options,
            prompt_preset: PromptPreset::Generic,
            usage_account: None,
            policy_model: None,
            policy_profile: None,
        })
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
                "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
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
        let model = model.trim();
        if !valid_model(model) {
            return Err("Enter the installed model name".into());
        }
        Ok(Self {
            target: Target::External(address),
            model: model.into(),
            options,
            prompt_preset: PromptPreset::Generic,
            usage_account: None,
            policy_model: None,
            policy_profile: None,
        })
    }

    #[cfg(test)]
    fn external_address(&self) -> SocketAddr {
        match &self.target {
            Target::External(address) | Target::LmStudio(address) => *address,
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
    if protocol == RemoteProtocol::OpenAi {
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
        let client = Client::new(profile);
        let cancel = Cancel {
            activity: Arc::clone(&client.activity),
        };
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
        let Target::Remote(base, _) = &self.client.profile.target else {
            return Err(Error::Service);
        };
        let url = url::Url::parse(base).map_err(|_| Error::Service)?;
        if !matches!(url.host(), Some(url::Host::Ipv4(address)) if address.is_loopback())
            && !matches!(url.host(), Some(url::Host::Ipv6(address)) if address.is_loopback())
        {
            return Err(Error::Service);
        }
        let certificate = ureq::tls::Certificate::from_der(certificate_der).to_owned();
        let roots = RootCerts::new_with_certs(&[certificate]);
        let timeout = self
            .client
            .profile
            .options
            .timeout
            .unwrap_or(DEFAULT_TIMEOUT);
        let agent = connection_agent(connection_config_with_roots(timeout, roots));
        let connection =
            Connection::attach(base.clone(), agent, Arc::clone(&self.client.activity))?;
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
    pub(crate) fn render_system_prompt<'a>(&self, system: &'a str) -> Cow<'a, str> {
        self.profile.render_system_prompt(system)
    }

    pub(crate) fn new(profile: Profile) -> Self {
        let transport = OnceLock::new();
        let timeout = profile.options.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let connection = match &profile.target {
            Target::External(address) | Target::LmStudio(address) => {
                Some(Connection::new(*address, timeout))
            }
            Target::PolicyAuto(target) => Some(target.connection(timeout)),
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

    pub(crate) fn take_usage(&self) -> Option<Usage> {
        self.usage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
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
        if !embedded && self.profile.policy_profile.is_none() {
            return Ok(());
        }
        let policy = read_policy();
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
        self.complete_request(system, RequestContent::Text(input), true, None, false)
    }

    #[cfg(test)]
    pub(crate) fn complete_chat(
        &self,
        system: &str,
        history: &[(&str, &str)],
        input: &str,
    ) -> Result<String, Error> {
        let messages = chat_messages(system, history, input);
        self.complete_messages(&messages, false, None, false)
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
        before_dispatch: impl FnOnce() -> Result<(), String>,
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
        before_dispatch: impl FnOnce() -> Result<(), String>,
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
        before_dispatch: impl FnOnce() -> Result<(), String>,
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
        before_dispatch: impl FnOnce() -> Result<(), String>,
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
        before_dispatch: impl FnOnce() -> Result<(), String>,
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

    fn complete_reviewed_https_before(
        &self,
        system: &str,
        input: &str,
        response_format: bool,
        deadline: Instant,
        credential: Option<Zeroizing<String>>,
        before_dispatch: impl FnOnce() -> Result<(), String>,
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
        before_dispatch: impl FnOnce() -> Result<(), String>,
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
        &self.profile.model
    }

    pub(crate) fn complete_vision_before(
        &self,
        system: &str,
        input: &str,
        png_data_url: &str,
        response_format: bool,
        deadline: Instant,
    ) -> Result<String, Error> {
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
        )
    }

    fn complete_request(
        &self,
        system: &str,
        input: RequestContent<'_>,
        response_format: bool,
        deadline: Option<Instant>,
        require_complete: bool,
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
        )
    }

    fn complete_messages(
        &self,
        messages: &[RequestMessage<'_>],
        response_format: bool,
        deadline: Option<Instant>,
        require_complete: bool,
    ) -> Result<String, Error> {
        self.with_completion(|attempt| {
            self.complete_messages_inner(
                messages,
                response_format,
                deadline,
                require_complete,
                || Ok(()),
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
        before_dispatch: impl FnOnce() -> Result<(), String>,
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
        let deadline =
            self.before_completion_dispatch(transport, deadline, before_dispatch, attempt)?;
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
        } else if matches!(
            self.profile.target,
            Target::Remote(_, RemoteProtocol::Anthropic)
        ) {
            let body = providers::anthropic_request(self, messages, response_format, false)?;
            let mut response = self.completion_response(transport, &body, deadline, attempt)?;
            let bytes = self.cancellable_response_bytes(
                &mut response,
                deadline.is_some() || self.profile.options.timeout.is_some(),
            )?;
            providers::anthropic_response_with_stop(
                &bytes,
                require_complete,
                &mut attempt.usage,
                attempt.agent_completion.as_mut(),
            )
            .map(|(answer, _)| answer)
        } else {
            let response_format = response_format && !self.profile.is_mlx();
            let body = self.openai_request(messages, response_format, false)?;
            self.read_completion(
                transport,
                &body,
                response_format,
                deadline,
                require_complete,
                attempt,
            )
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
            && self.profile.is_remote()
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

    fn openai_request(
        &self,
        messages: &[RequestMessage<'_>],
        response_format: bool,
        stream: bool,
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        let openai = matches!(&self.profile.target, Target::Remote(base, RemoteProtocol::OpenAi)
            if base.starts_with("https://api.openai.com/"));
        let limit = self.profile.effective_output_limit();
        let mlx = self.profile.is_mlx();
        serde_json::to_vec(&Request {
            model: (!mlx).then_some(self.model()),
            stream,
            temperature: (!openai).then_some(0),
            max_tokens: (!openai).then_some(limit),
            max_completion_tokens: openai.then_some(limit),
            store: openai.then_some(false),
            stream_options: (openai && stream).then_some(StreamOptions {
                include_usage: true,
            }),
            reasoning_effort: (!mlx)
                .then_some(self.profile.options.reasoning_effort.as_deref())
                .flatten(),
            messages,
            response_format: (response_format && !mlx).then_some(ResponseFormat {
                kind: "json_object",
            }),
        })
        .map(Zeroizing::new)
        .map_err(|_| Error::Service)
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
        let deadline = if let Some(resolved) = self
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
        if credential.is_none() && matches!(self.profile.target, Target::Remote(..)) {
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
                "The HTTPS credential changed. Review the request again.".into(),
            ));
        }
        Ok(credential)
    }

    fn read_completion(
        &self,
        transport: &Transport,
        body: &[u8],
        response_format: bool,
        deadline: Option<Instant>,
        require_complete: bool,
        attempt: &mut UsageAttempt,
    ) -> Result<String, Error> {
        let mut response = self.completion_response(transport, body, deadline, attempt)?;
        let bounded = deadline.is_some() || self.profile.options.timeout.is_some();
        completion_status(&mut response, deadline.is_some() && response_format)?;
        let bytes = response_bytes(&mut response, bounded)?;
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
            Target::LmStudio(_) => "/api/v1/chat",
            Target::PolicyAuto(target) => target.completion_path(),
            Target::Remote(_, protocol) => protocol.completion_path(),
            _ => "/v1/chat/completions",
        });
        let mut request = transport
            .connection
            .agent
            .post(endpoint)
            .content_type("application/json");
        attempt.credential = self.completion_credential(attempt)?;
        if let Target::Remote(_, protocol) = self.profile.target {
            if protocol == RemoteProtocol::Anthropic {
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

fn completion_status(
    response: &mut ureq::http::Response<ureq::Body>,
    detect_unsupported_format: bool,
) -> Result<(), Error> {
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status().as_u16();
    if detect_unsupported_format && matches!(status, 400 | 422) {
        let error = response
            .body_mut()
            .with_config()
            .limit(MAX_ERROR_RESPONSE_BYTES)
            .read_to_vec();
        if error.as_deref().is_ok_and(unsupported_response_format) {
            return Err(Error::UnsupportedResponseFormat);
        }
    }
    Err(Error::Status(status))
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
            }
            return Err(Error::Invalid("invalid chat response"));
        }
    };
    usage.observe_openai(
        completion.usage.as_ref(),
        completion.model.as_ref(),
        completion.id.as_ref(),
    );
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
}

#[derive(Deserialize)]
struct CompletionMetadata {
    usage: Option<Value>,
    model: Option<Value>,
    id: Option<Value>,
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
    messages: &'a [RequestMessage<'a>],
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
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
fn mlx_openai_request_keeps_the_pinned_model_out_of_the_wire_payload() {
    let root = crate::snippets::tests::Profile::new();
    let profile = Profile::managed_mlx(
        root.0.clone(),
        "0123456789abcdef0123456789abcdef",
        ProfileOptions {
            max_output_tokens: Some(256),
            reasoning_effort: Some("low".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!profile.is_remote());
    let client = Client::new(profile);
    let messages = [RequestMessage::text("user", "fixture")];
    for stream in [false, true] {
        let payload: Value =
            serde_json::from_slice(&client.openai_request(&messages, true, stream).unwrap())
                .unwrap();
        assert!(payload.get("model").is_none());
        assert!(payload.get("response_format").is_none());
        assert!(payload.get("reasoning_effort").is_none());
        assert_eq!(payload["stream"], stream);
        assert_eq!(payload["max_tokens"], 256);
        assert_eq!(payload["messages"][0]["content"], "fixture");
    }
}
