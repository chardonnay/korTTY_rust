//! Request authority for administrator-provided AI profiles.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

use pgp::crypto::hash::HashAlgorithm;
#[cfg(not(windows))]
use ureq::Agent;
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
#[cfg(not(windows))]
use ureq::unversioned::transport::DefaultConnector;
use ureq::unversioned::transport::NextTimeout;
use ureq::{config::Config, http::Uri};
use zeroize::Zeroizing;

use crate::clipboard_policy::{ClipboardPolicy, PolicyAiProfile, PolicyAiProvider};

use super::{
    Activity, Connection, Error, Pending, Profile, ProfileOptions, RemoteProtocol, Target,
    WorkerPermit, connection_config, policy_auto_model, start_worker, valid_model,
};

const ANTHROPIC_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const CHANGED: &str = "The policy-provided AI profile changed or is unavailable. Reopen the profile or review the request again.";
const AUTO_CHANGED: &str =
    "The loaded LM Studio model changed. Resolve the profile and review the request again.";
const AUTO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_AUTO_RESOLVERS: usize = 4;
static AUTO_RESOLVERS: AtomicUsize = AtomicUsize::new(0);

impl Profile {
    pub(crate) fn policy_profile_id(&self) -> Option<&str> {
        self.policy_profile
            .as_ref()
            .map(|authority| authority.definition().id.as_str())
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(super) enum PolicyProfileAuthority {
    Declared(Arc<PolicyAiProfile>),
    Resolved(Arc<PolicyAutoModel>),
}

impl PolicyProfileAuthority {
    pub(super) fn new(definition: &PolicyAiProfile) -> Self {
        Self::Declared(Arc::new(definition.clone()))
    }

    fn definition(&self) -> &PolicyAiProfile {
        match self {
            Self::Declared(definition) => definition,
            Self::Resolved(resolved) => &resolved.definition,
        }
    }

    pub(super) fn auto_model(&self) -> Option<&PolicyAutoModel> {
        match self {
            Self::Declared(_) => None,
            Self::Resolved(resolved) => Some(resolved),
        }
    }

    pub(super) fn require_current(&self, policy: &ClipboardPolicy) -> Result<(), Error> {
        if policy.problem.is_none()
            && policy.ai_allowed
            && policy.ai_profiles().contains(self.definition())
        {
            Ok(())
        } else {
            Err(Error::Review(CHANGED.into()))
        }
    }

    pub(super) fn credential(&self) -> Result<Option<Zeroizing<String>>, Error> {
        self.definition()
            .encrypted_key
            .as_deref()
            .map(|envelope| {
                let value = kortty_control::policy_value::decrypt(envelope)
                    .map_err(|_| Error::Credential)?;
                super::require_credential_value(&value)?;
                Ok(value)
            })
            .transpose()
    }

    fn fingerprint(&self) -> [u8; 32] {
        let definition = self.definition().fingerprint();
        let Some(resolved) = self.auto_model() else {
            return definition;
        };
        let mut bytes = b"kortty policy automatic LM Studio model v1\0".to_vec();
        bytes.extend_from_slice(&definition);
        let destination = resolved.target.destination();
        for value in [&destination, &resolved.model] {
            bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
            bytes.extend_from_slice(value.as_bytes());
        }
        HashAlgorithm::Sha256
            .digest(&bytes)
            .expect("SHA-256 is supported")
            .try_into()
            .expect("SHA-256 has 32 bytes")
    }
}

/// The exact policy definition, route and concrete model from a discovery request.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct PolicyAutoModel {
    definition: Arc<PolicyAiProfile>,
    target: AutoModelTarget,
    model: String,
}

impl fmt::Debug for PolicyAutoModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PolicyAutoModel")
            .finish_non_exhaustive()
    }
}

impl PolicyAutoModel {
    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    pub(crate) fn matches_definition(&self, definition: &PolicyAiProfile) -> bool {
        self.definition.as_ref() == definition
    }

    pub(super) fn require_current_model(
        &self,
        connection: &Connection,
        credential: Option<&str>,
        deadline: Option<Instant>,
        authorize: impl Fn() -> Result<(), Error>,
    ) -> Result<(), Error> {
        let model = policy_auto_model::resolve(connection, credential, deadline, authorize)
            .map_err(|error| match error {
                Error::Invalid(_) | Error::Service | Error::Status(_) => Error::Review(format!(
                    "{}. Resolve the profile and review the request again.",
                    error.message()
                )),
                error => error,
            })?;
        if model == self.model {
            Ok(())
        } else {
            Err(Error::Review(AUTO_CHANGED.into()))
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(super) struct AutoModelTarget {
    origin: String,
    native: bool,
}

impl AutoModelTarget {
    fn from_definition(definition: &PolicyAiProfile) -> Result<Self, String> {
        let invalid =
            || "Automatic model selection requires a loopback LM Studio chat endpoint.".to_owned();
        if definition.model.is_some()
            || !matches!(
                definition.provider,
                PolicyAiProvider::OpenAiCompatible | PolicyAiProvider::LmStudio
            )
        {
            return Err(invalid());
        }
        let endpoint = definition.endpoint.as_deref().ok_or_else(invalid)?;
        if endpoint.len() > 2048
            || endpoint
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control() || matches!(ch, '\\' | '?' | '#'))
        {
            return Err(invalid());
        }
        let (scheme, rest) = if let Some(rest) = strip_ascii_prefix(endpoint, "https://") {
            ("https", rest)
        } else {
            (
                "http",
                strip_ascii_prefix(endpoint, "http://").unwrap_or(endpoint),
            )
        };
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        if !authority
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'[' | b']'))
        {
            return Err(invalid());
        }
        let native = match path.trim_end_matches('/') {
            "" | "v1" | "v1/chat/completions" => false,
            "api/v1/chat" => true,
            _ => return Err(invalid()),
        };
        let origin = url::Url::parse(&format!("{scheme}://{authority}")).map_err(|_| invalid())?;
        let host = origin.host_str().ok_or_else(invalid)?;
        if loopback_ip(host).is_none() && host != "localhost" || origin.port() == Some(0) {
            return Err(invalid());
        }
        Ok(Self {
            origin: origin.origin().ascii_serialization(),
            native,
        })
    }

    pub(super) fn is_https(&self) -> bool {
        self.origin.starts_with("https://")
    }

    pub(super) fn uses_native_protocol(&self) -> bool {
        self.native
    }

    pub(super) fn completion_path(&self) -> &'static str {
        if self.native {
            "/api/v1/chat"
        } else {
            "/v1/chat/completions"
        }
    }

    fn destination(&self) -> String {
        format!("{}{}", self.origin, self.completion_path())
    }

    pub(super) fn connection(&self, timeout: Duration) -> Connection {
        self.connect(connection_config(timeout), Arc::new(Activity::default()))
            .expect("new connection activity is active")
    }

    pub(super) fn connect(
        &self,
        config: Config,
        activity: Arc<Activity>,
    ) -> Result<Connection, Error> {
        #[cfg(windows)]
        let agent = super::windows_transport::agent(config, LoopbackResolver);
        #[cfg(not(windows))]
        let agent = Agent::with_parts(config, DefaultConnector::default(), LoopbackResolver);
        Connection::attach(self.origin.clone(), agent, activity)
    }
}

fn loopback_ip(host: &str) -> Option<IpAddr> {
    let host = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    host.parse::<IpAddr>().ok().filter(IpAddr::is_loopback)
}

/// Keep localhost local without changing the host name presented to TLS.
#[derive(Debug)]
struct LoopbackResolver;

impl Resolver for LoopbackResolver {
    fn resolve(
        &self,
        uri: &Uri,
        _config: &Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let host = uri.host().ok_or(ureq::Error::HostNotFound)?;
        let default_port = match uri.scheme_str() {
            Some("http") => 80,
            Some("https") => 443,
            _ => return Err(ureq::Error::HostNotFound),
        };
        let port = uri.port_u16().unwrap_or(default_port);
        let mut addresses = self.empty();
        if host == "localhost" {
            addresses.push(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port));
            addresses.push(SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port));
        } else if let Some(ip) = loopback_ip(host) {
            addresses.push(SocketAddr::new(ip, port));
        } else {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(addresses)
    }
}

pub(crate) fn resolve_policy_auto_model(
    definition: &PolicyAiProfile,
) -> Result<Pending<PolicyAutoModel>, String> {
    let target = AutoModelTarget::from_definition(definition)?;
    let authority = PolicyProfileAuthority::new(definition);
    authority
        .require_current(&ClipboardPolicy::load())
        .map_err(Error::message)?;
    let permit = WorkerPermit::acquire_from(&AUTO_RESOLVERS, MAX_AUTO_RESOLVERS)
        .ok_or("Automatic model discovery workers are busy.")?;
    let connection = target.connection(AUTO_TIMEOUT);
    start_worker(connection.cancel_handle(), move || {
        let _permit = permit;
        let credential = authority.credential().map_err(Error::message)?;
        let model = policy_auto_model::resolve(
            &connection,
            credential.as_deref().map(String::as_str),
            None,
            || authority.require_current(&ClipboardPolicy::load()),
        )
        .map_err(Error::message)?;
        let PolicyProfileAuthority::Declared(definition) = authority else {
            unreachable!("discovery starts from the installed definition")
        };
        Ok(PolicyAutoModel {
            definition,
            target,
            model,
        })
    })
}

impl Profile {
    pub(crate) fn supports_policy_auto_model(definition: &PolicyAiProfile) -> bool {
        AutoModelTarget::from_definition(definition).is_ok()
    }

    pub(crate) fn policy_auto_model_uses_https(definition: &PolicyAiProfile) -> bool {
        AutoModelTarget::from_definition(definition).is_ok_and(|target| target.is_https())
    }

    pub(crate) fn policy_profile_destination(
        definition: &PolicyAiProfile,
    ) -> Result<String, String> {
        if let Ok(target) = AutoModelTarget::from_definition(definition) {
            return Ok(target.destination());
        }
        match definition.provider {
            PolicyAiProvider::EmbeddedLlama => Ok("Managed llama.cpp on this computer".into()),
            PolicyAiProvider::EmbeddedMlx => Ok("Managed Apple MLX on this computer".into()),
            _ => match http_target(definition)? {
                Target::External(address) => Ok(address.to_string()),
                Target::LmStudio(address) => Ok(format!("http://{address}/api/v1/chat")),
                Target::Remote(base, protocol) => {
                    Ok(format!("{base}{}", protocol.completion_path()))
                }
                _ => unreachable!("HTTP policy destination"),
            },
        }
    }

    pub(crate) fn policy_profile(
        definition: &PolicyAiProfile,
        profile_dir: &Path,
        options: ProfileOptions,
    ) -> Result<Self, String> {
        if options.credential_env.is_some() {
            return Err("A policy-provided AI profile uses its configured credential.".into());
        }
        let model = definition
            .model
            .as_deref()
            .ok_or("The policy-provided AI profile has no model configured.")?;
        let mut profile = match definition.provider {
            PolicyAiProvider::EmbeddedLlama => {
                Self::managed(profile_dir.to_owned(), model, options)?
            }
            PolicyAiProvider::EmbeddedMlx => {
                Self::managed_mlx(profile_dir.to_owned(), model, options)?
            }
            _ => {
                Self::validate_options(&options)?;
                if !valid_model(model) {
                    return Err("The policy-provided AI profile has an invalid model.".into());
                }
                Self {
                    target: http_target(definition)?,
                    model: model.into(),
                    options,
                    prompt_preset: super::PromptPreset::Generic,
                    usage_account: None,
                    policy_model: None,
                    policy_profile: None,
                }
            }
        };
        profile.policy_profile = Some(PolicyProfileAuthority::new(definition));
        Ok(profile)
    }

    pub(crate) fn policy_profile_with_auto_model(
        definition: &PolicyAiProfile,
        resolved: &PolicyAutoModel,
        options: ProfileOptions,
    ) -> Result<Self, String> {
        if options.credential_env.is_some() {
            return Err("A policy-provided AI profile uses its configured credential.".into());
        }
        if !resolved.matches_definition(definition) {
            return Err(CHANGED.into());
        }
        Self::validate_options(&options)?;
        Ok(Self {
            target: Target::PolicyAuto(resolved.target.clone()),
            model: resolved.model.clone(),
            options,
            prompt_preset: super::PromptPreset::Generic,
            usage_account: None,
            policy_model: None,
            policy_profile: Some(PolicyProfileAuthority::Resolved(Arc::new(resolved.clone()))),
        })
    }

    pub(crate) fn policy_authority_fingerprint(&self) -> Option<[u8; 32]> {
        let profile = self
            .policy_profile
            .as_ref()
            .map(PolicyProfileAuthority::fingerprint);
        let model = self.policy_model_fingerprint();
        match (profile, model) {
            (Some(profile), Some(model)) => {
                let mut bytes = b"kortty policy profile and model v1\0".to_vec();
                bytes.extend_from_slice(&profile);
                bytes.extend_from_slice(&model);
                Some(
                    HashAlgorithm::Sha256
                        .digest(&bytes)
                        .expect("SHA-256 is supported")
                        .try_into()
                        .expect("SHA-256 has 32 bytes"),
                )
            }
            (profile, model) => profile.or(model),
        }
    }

    pub(crate) fn require_current_policy_authority(&self) -> Result<(), String> {
        if let Some(authority) = &self.policy_profile {
            authority
                .require_current(&ClipboardPolicy::load())
                .map_err(Error::message)?;
        }
        self.require_current_policy_model()
    }

    pub(super) fn policy_credential(&self) -> Result<Option<Zeroizing<String>>, Error> {
        if self.starts_sidecar() {
            return Ok(None);
        }
        self.policy_profile
            .as_ref()
            .map_or(Ok(None), PolicyProfileAuthority::credential)
    }
}

pub(super) fn http_target(definition: &PolicyAiProfile) -> Result<Target, String> {
    let endpoint = || {
        definition
            .endpoint
            .as_deref()
            .ok_or_else(|| "The policy-provided AI profile has no endpoint configured.".to_owned())
    };
    match definition.provider {
        PolicyAiProvider::Anthropic => remote_target(
            definition.endpoint.as_deref().unwrap_or(ANTHROPIC_ENDPOINT),
            RemoteProtocol::Anthropic,
        ),
        PolicyAiProvider::OpenAiCompatible => {
            let endpoint = endpoint()?;
            if strip_ascii_prefix(endpoint, "https://").is_some() {
                remote_target(endpoint, RemoteProtocol::OpenAi)
            } else {
                let endpoint = endpoint.trim_end_matches('/');
                let endpoint = endpoint
                    .strip_suffix("/v1/chat/completions")
                    .unwrap_or(endpoint);
                loopback(endpoint, super::numeric_loopback_endpoint).map(Target::External)
            }
        }
        PolicyAiProvider::LmStudio => {
            loopback(endpoint()?, super::lm_studio_endpoint).map(Target::LmStudio)
        }
        PolicyAiProvider::EmbeddedLlama | PolicyAiProvider::EmbeddedMlx => {
            Err("An embedded policy profile uses its configured model without discovery.".into())
        }
    }
}

fn remote_target(endpoint: &str, protocol: RemoteProtocol) -> Result<Target, String> {
    let normalized = strip_ascii_prefix(endpoint, "https://")
        .filter(|_| !endpoint.starts_with("https://"))
        .map(|rest| format!("https://{rest}"));
    super::providers::remote_api_base(normalized.as_deref().unwrap_or(endpoint), protocol)
        .map(|base| Target::Remote(base, protocol))
}

fn loopback(
    endpoint: &str,
    parse: impl FnOnce(&str) -> Result<SocketAddr, String>,
) -> Result<SocketAddr, String> {
    let endpoint = strip_ascii_prefix(endpoint, "http://").unwrap_or(endpoint);
    if let Some(rest) = strip_ascii_prefix(endpoint, "localhost:") {
        parse(&format!("127.0.0.1:{rest}"))
    } else {
        parse(endpoint)
    }
}

fn strip_ascii_prefix<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let (start, rest) = value.split_at_checked(prefix.len())?;
    start.eq_ignore_ascii_case(prefix).then_some(rest)
}

const MASK: &str = "█";

pub(super) fn redact_echo(answer: String, credential: Option<&str>) -> String {
    match credential.filter(|value| !value.is_empty() && answer.contains(value)) {
        Some(value) => Zeroizing::new(answer).replace(value, MASK),
        None => answer,
    }
}

/// Hold the longest possible prefix of the credential across SSE events.
pub(super) struct EchoMask(Zeroizing<String>);

impl Default for EchoMask {
    fn default() -> Self {
        Self(Zeroizing::new(String::new()))
    }
}

impl EchoMask {
    pub(super) fn push(&mut self, text: String, credential: Option<&str>) -> String {
        let Some(credential) = credential.filter(|value| !value.is_empty()) else {
            return text;
        };
        self.0.push_str(&Zeroizing::new(text));
        let mut remaining = self.0.as_str();
        let mut safe = String::new();
        while let Some(position) = remaining.find(credential) {
            safe.push_str(&remaining[..position]);
            safe.push_str(MASK);
            remaining = &remaining[position + credential.len()..];
        }
        let bytes = remaining.as_bytes();
        let candidate = bytes.len().min(credential.len().saturating_sub(1));
        let keep = (1..=candidate)
            .rev()
            .find(|length| {
                credential
                    .as_bytes()
                    .starts_with(&bytes[bytes.len() - length..])
            })
            .unwrap_or(0);
        let (ready, pending) = remaining.split_at(remaining.len() - keep);
        safe.push_str(ready);
        self.0 = Zeroizing::new(pending.to_owned());
        safe
    }

    pub(super) fn finish(self) -> String {
        self.0.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use kortty_control::policy_value::encrypt;

    use super::*;
    use crate::local_ai::{Client, remote_header};

    fn installed(provider: &str, endpoint: Option<&str>, sealed: Option<&str>) -> ClipboardPolicy {
        let endpoint = endpoint
            .map(|value| format!("endpoint = {value:?}\n"))
            .unwrap_or_default();
        let sealed = sealed
            .map(|value| format!("api-key-encrypted = {value:?}\n"))
            .unwrap_or_default();
        crate::clipboard_policy::parse(&format!(
            "[meta]\nschema-version = 1\n\n[[ai-profile]]\nid = 'policy-fixture'\nname = 'Policy Fixture'\nprovider = '{provider}'\nmodel = 'fixture-model'\n{endpoint}{sealed}"
        ))
        .unwrap()
    }

    fn automatic(provider: &str, endpoint: &str) -> ClipboardPolicy {
        crate::clipboard_policy::parse(&format!(
            "[meta]\nschema-version = 1\n\n[[ai-profile]]\nid = 'policy-auto-fixture'\nname = 'Policy Auto Fixture'\nprovider = '{provider}'\nendpoint = {endpoint:?}"
        ))
        .unwrap()
    }

    #[test]
    fn automatic_policy_routes_preserve_scheme_host_and_chat_protocol() {
        for (provider, endpoint, expected, native, https) in [
            (
                "openai-compatible",
                "http://127.0.0.1:1234",
                "http://127.0.0.1:1234/v1/chat/completions",
                false,
                false,
            ),
            (
                "lm-studio",
                "https://localhost:1234/v1",
                "https://localhost:1234/v1/chat/completions",
                false,
                true,
            ),
            (
                "openai-compatible",
                "HTTPS://[::1]:1234/api/v1/chat",
                "https://[::1]:1234/api/v1/chat",
                true,
                true,
            ),
            (
                "lm-studio",
                "http://localhost:1234/api/v1/chat/",
                "http://localhost:1234/api/v1/chat",
                true,
                false,
            ),
        ] {
            let policy = automatic(provider, endpoint);
            let definition = &policy.ai_profiles()[0];
            let target = AutoModelTarget::from_definition(definition).unwrap();
            assert!(Profile::supports_policy_auto_model(definition));
            assert_eq!(Profile::policy_auto_model_uses_https(definition), https);
            assert_eq!(
                Profile::policy_profile_destination(definition).unwrap(),
                expected
            );
            assert_eq!(target.uses_native_protocol(), native);
        }
        for endpoint in [
            "https://example.test/v1",
            "http://localhost.example.test/v1",
            "http://@localhost:1234",
            "http://localhost:1234/v1?query=1",
            "http://localhost:1234/path/../v1",
        ] {
            let policy = automatic("openai-compatible", endpoint);
            assert!(!Profile::supports_policy_auto_model(
                &policy.ai_profiles()[0]
            ));
        }
        let anthropic = automatic("anthropic", "https://localhost:1234/v1/messages");
        assert!(!Profile::supports_policy_auto_model(
            &anthropic.ai_profiles()[0]
        ));
    }

    #[test]
    fn automatic_policy_fingerprint_and_request_body_use_the_reviewed_model() {
        for path in ["/v1", "/api/v1/chat"] {
            let policy = automatic("lm-studio", &format!("http://127.0.0.1:1234{path}"));
            let definition = &policy.ai_profiles()[0];
            let mut resolved = PolicyAutoModel {
                definition: Arc::new(definition.clone()),
                target: AutoModelTarget::from_definition(definition).unwrap(),
                model: "reviewed-local-model".into(),
            };
            let profile = Profile::policy_profile_with_auto_model(
                definition,
                &resolved,
                ProfileOptions::default(),
            )
            .unwrap();
            assert_eq!(profile.model, resolved.model());
            let fingerprint = profile.policy_authority_fingerprint();
            assert_ne!(fingerprint, Some(definition.fingerprint()));
            let client = Client::new(profile);
            let messages = super::super::chat_messages("system", &[], "request");
            let body = if resolved.target.uses_native_protocol() {
                super::super::providers::lm_request(&client, &messages, false, false)
            } else {
                client.openai_request(&messages, false, false)
            }
            .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["model"], "reviewed-local-model");
            resolved.model = "different-loaded-model".into();
            let replacement = Profile::policy_profile_with_auto_model(
                definition,
                &resolved,
                ProfileOptions::default(),
            )
            .unwrap();
            assert_ne!(fingerprint, replacement.policy_authority_fingerprint());
            let mut changed = definition.clone();
            changed.name = "Changed".into();
            assert!(
                Profile::policy_profile_with_auto_model(
                    &changed,
                    &resolved,
                    ProfileOptions::default()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn automatic_policy_dispatch_rechecks_the_live_loaded_model() {
        use crate::local_ai::test_support::{
            accept, fixture_http_response, listener, wire_request,
        };

        let (listener, _) = listener();
        let policy = automatic(
            "lm-studio",
            &format!("http://{}/api/v1/chat", listener.local_addr().unwrap()),
        );
        let definition = &policy.ai_profiles()[0];
        let resolved = PolicyAutoModel {
            definition: Arc::new(definition.clone()),
            target: AutoModelTarget::from_definition(definition).unwrap(),
            model: "reviewed-local-model".into(),
        };
        let authority = PolicyProfileAuthority::new(definition);
        let worker = std::thread::spawn(move || {
            for model in ["reviewed-local-model", "different-loaded-model"] {
                let mut stream = accept(&listener);
                wire_request(&mut stream, "GET /api/v1/models HTTP/1.1");
                let body = serde_json::json!({"models": [{"type": "llm", "key": model, "loaded_instances": [{"id": "loaded"}]}]});
                stream
                    .get_mut()
                    .write_all(fixture_http_response("200 OK", "", &body.to_string()).as_bytes())
                    .unwrap();
            }
        });
        let connection = resolved.target.connection(AUTO_TIMEOUT);
        let authorize = || authority.require_current(&policy);
        resolved
            .require_current_model(&connection, None, None, authorize)
            .unwrap();
        assert!(
            matches!(resolved.require_current_model(&connection, None, None, authorize), Err(Error::Review(reason)) if reason == AUTO_CHANGED)
        );
        worker.join().unwrap();
    }

    #[test]
    fn reviewed_automatic_https_credential_is_checked_before_metadata_and_completion() {
        use crate::local_ai::test_support::listener;

        let (listener, _) = listener();
        let policy = automatic(
            "lm-studio",
            &format!("https://{}/api/v1/chat", listener.local_addr().unwrap()),
        );
        let definition = &policy.ai_profiles()[0];
        let resolved = PolicyAutoModel {
            definition: Arc::new(definition.clone()),
            target: AutoModelTarget::from_definition(definition).unwrap(),
            model: "reviewed-local-model".into(),
        };
        let profile = Profile::policy_profile_with_auto_model(
            definition,
            &resolved,
            ProfileOptions::default(),
        )
        .unwrap();
        assert!(profile.is_remote());
        let client = Client::new(profile);
        let deadline = Some(Instant::now() + AUTO_TIMEOUT);
        let transport = client.transport_with_policy(deadline, || policy).unwrap();

        client
            .with_completion(|attempt| {
                attempt.reviewed_https_credential =
                    Some(Some(Zeroizing::new("reviewed-credential".into())));
                let preflight =
                    client.before_completion_dispatch(transport, deadline, || Ok(()), attempt);
                assert!(
                    matches!(preflight, Err(Error::Review(message)) if message.contains("credential changed"))
                );
                let completion = client.completion_response(transport, b"{}", deadline, attempt);
                assert!(
                    matches!(completion, Err(Error::Review(message)) if message.contains("credential changed"))
                );
                assert!(!attempt.dispatched);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn installed_profile_authority_rechecks_cached_transport_and_binds_key_rotation() {
        let endpoint = Some("HTTP://LOCALHOST:1234/v1/chat/completions");
        let key = encrypt("policy-credential-fixture").unwrap();
        let replacement_key = encrypt("replacement-credential-fixture").unwrap();
        let original = || installed("openai-compatible", endpoint, Some(&key));
        let replaced = || installed("openai-compatible", endpoint, Some(&replacement_key));
        let initial = original();
        let definition = &initial.ai_profiles()[0];
        let profile =
            Profile::policy_profile(definition, Path::new("."), ProfileOptions::default()).unwrap();
        assert_eq!(
            Profile::policy_profile_destination(definition).unwrap(),
            "127.0.0.1:1234"
        );
        assert_eq!(
            profile.policy_authority_fingerprint(),
            Some(definition.fingerprint())
        );
        assert_ne!(
            profile.policy_authority_fingerprint(),
            Some(replaced().ai_profiles()[0].fingerprint())
        );
        let credential = profile.policy_credential().unwrap().unwrap();
        assert_eq!(credential.as_str(), "policy-credential-fixture");
        let client = Client::new(profile);
        let transport = client.transport_with_policy(None, original).unwrap();
        let header = client
            .request_authorization(transport, Some(&credential))
            .unwrap()
            .unwrap();
        assert!(header.is_sensitive());
        assert_eq!(header, "Bearer policy-credential-fixture");
        assert!(matches!(
            client.transport_with_policy(None, replaced),
            Err(Error::Review(_))
        ));
        assert!(matches!(
            client.transport_with_policy(None, ClipboardPolicy::default),
            Err(Error::Review(_))
        ));
    }

    #[test]
    fn installed_profile_routes_preserve_protocol_and_refuse_credential_overrides() {
        let sealed = encrypt("synthetic-anthropic-key").unwrap();
        let policy = installed("anthropic", None, Some(&sealed));
        let definition = &policy.ai_profiles()[0];
        assert_eq!(
            Profile::policy_profile_destination(definition).unwrap(),
            ANTHROPIC_ENDPOINT
        );
        let profile =
            Profile::policy_profile(definition, Path::new("."), ProfileOptions::default()).unwrap();
        assert!(profile.is_remote());
        let credential = profile.policy_credential().unwrap().unwrap();
        let (name, header) = remote_header(&credential, RemoteProtocol::Anthropic).unwrap();
        assert_eq!(name, "x-api-key");
        assert_eq!(header, "synthetic-anthropic-key");
        assert!(header.is_sensitive());
        assert!(
            Profile::policy_profile(
                definition,
                Path::new("."),
                ProfileOptions {
                    credential_env: Some("TEST_CREDENTIAL".into()),
                    ..ProfileOptions::default()
                }
            )
            .is_err()
        );
        let plain_remote = installed(
            "openai-compatible",
            Some("http://models.example.invalid/v1"),
            None,
        );
        assert!(Profile::policy_profile_destination(&plain_remote.ai_profiles()[0]).is_err());
        let absent = installed("openai-compatible", None, None);
        assert!(Profile::policy_profile_destination(&absent.ai_profiles()[0]).is_err());
        let https = installed(
            "openai-compatible",
            Some("HTTPS://models.example.invalid/v1"),
            None,
        );
        assert_eq!(
            Profile::policy_profile_destination(&https.ai_profiles()[0]).unwrap(),
            "https://models.example.invalid/v1/chat/completions"
        );
    }

    #[test]
    fn installed_profile_masks_credential_echoes_across_provider_events() {
        let credential = Some("abcab");
        let mut output = EchoMask::default();
        assert_eq!(output.push("€ abc".into(), credential), "€ ");
        assert_eq!(output.push("ab / ab".into(), credential), "█ / ");
        assert_eq!(
            output.push("c€ abcab & abc".into(), credential),
            "abc€ █ & "
        );
        assert_eq!(output.finish(), "abc");
        assert_eq!(redact_echo("abcab abcab".into(), credential), "█ █");
    }

    #[test]
    fn installed_profile_drops_echoed_provider_usage_identifiers_but_keeps_counts() {
        let client = Client::new(Profile::new("127.0.0.1:1234", "fixture").unwrap());
        let key = "policy-credential-fixture";
        for echoed_header in [false, true] {
            client
                .with_completion(|attempt| {
                    attempt.credential = Some(Zeroizing::new(key.into()));
                    if echoed_header {
                        let mut headers = ureq::http::HeaderMap::new();
                        headers.insert("x-request-id", ureq::http::HeaderValue::from_static(key));
                        attempt.usage.observe_headers(&headers);
                    }
                    attempt.usage.observe_openai(
                        Some(&serde_json::json!({"prompt_tokens": 7, "completion_tokens": 3})),
                        Some(&serde_json::json!(format!("echo-{key}"))),
                        Some(&serde_json::json!(key)),
                    );
                    Ok(())
                })
                .unwrap();
            let usage = client.take_usage().unwrap();
            assert_eq!(
                (usage.input_tokens, usage.output_tokens),
                (Some(7), Some(3))
            );
            assert!(usage.model.is_none() && usage.request_id.is_none());
        }
        client
            .with_completion(|attempt| {
                attempt.credential = Some(Zeroizing::new(key.into()));
                attempt.usage.observe_openai(
                    None,
                    Some(&serde_json::json!("reported-model")),
                    Some(&serde_json::json!("reported-id")),
                );
                Ok(())
            })
            .unwrap();
        let usage = client.take_usage().unwrap();
        assert_eq!(usage.model.as_deref(), Some("reported-model"));
        assert_eq!(usage.request_id.as_deref(), Some("reported-id"));
    }
}
