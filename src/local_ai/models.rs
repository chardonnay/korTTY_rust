//! Provider model discovery and bounded local embedding requests.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use zeroize::{Zeroize, Zeroizing};

use crate::clipboard_policy::{ClipboardPolicy, PolicyAiProfile};

use super::{
    ANTHROPIC_VERSION, Cancel, Client, Connection, DEFAULT_TIMEOUT, Error, MAX_RESPONSE_BYTES,
    Pending, PolicyProfileAuthority, Purpose, RemoteProtocol, Target, WorkerPermit, authorization,
    numeric_loopback_endpoint, policy_profile, providers, remote_credential, remote_header,
    start_worker, transport_error, valid_model, validate_credential,
};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
const LOCAL_SCAN_TIMEOUT: Duration = Duration::from_secs(3);
pub(crate) const MAX_LOCAL_SCAN_ENDPOINTS: usize = 8;
const MAX_EMBEDDING_INPUTS: usize = 64;
const MAX_EMBEDDING_INPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_EMBEDDING_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_EMBEDDING_DIMENSIONS: usize = 65_536;

pub(crate) fn discover(endpoint: &str) -> Result<Pending<Vec<String>>, String> {
    start_discovery(endpoint, None)
}

#[derive(Clone)]
pub(crate) struct LocalModelServer {
    pub(crate) endpoint: SocketAddr,
    pub(crate) models: Result<Vec<String>, String>,
}

pub(crate) fn scan_local_models(
    addresses: Vec<SocketAddr>,
) -> Result<Pending<Vec<LocalModelServer>>, String> {
    if addresses.is_empty()
        || addresses.len() > MAX_LOCAL_SCAN_ENDPOINTS
        || addresses
            .iter()
            .any(|address| !address.ip().is_loopback() || address.port() == 0)
    {
        return Err("Choose valid loopback model-server addresses.".into());
    }
    let permit = WorkerPermit::acquire().ok_or(super::WORKERS_BUSY)?;
    let agent = ureq::Agent::new_with_config(super::connection_config(LOCAL_SCAN_TIMEOUT));
    let activity = Arc::new(super::Activity::default());
    activity.attach(&agent).map_err(Error::message)?;
    let cancel = Cancel {
        activity: Arc::clone(&activity),
    };
    start_worker(cancel, move || {
        let _permit = permit;
        let mut results = Vec::with_capacity(addresses.len());
        for endpoint in addresses {
            let policy = ClipboardPolicy::load();
            if policy.problem.is_some() || !policy.ai_allowed {
                return Err("Managed policy blocks AI model discovery.".into());
            }
            let connection = Connection::attach(
                format!("http://{endpoint}"),
                agent.clone(),
                Arc::clone(&activity),
            )
            .map_err(Error::message)?;
            let models = discover_models(&connection, None)
                .map(|mut models| {
                    models.truncate(512);
                    models
                })
                .map_err(Error::message);
            if !connection.active() {
                return Err(Error::Cancelled.message());
            }
            results.push(LocalModelServer { endpoint, models });
        }
        Ok(results)
    })
}

pub(crate) fn discover_with_credential(
    endpoint: &str,
    credential_env: Option<String>,
) -> Result<Pending<Vec<String>>, String> {
    match credential_env {
        None => discover(endpoint),
        Some(name) => start_discovery(endpoint, Some(name)),
    }
}

pub(crate) fn discover_lm_studio(
    endpoint: &str,
    credential_env: Option<String>,
) -> Result<Pending<Vec<String>>, String> {
    let address = super::lm_studio_endpoint(endpoint)?;
    validate_credential(credential_env.as_deref())?;
    let permit = WorkerPermit::acquire().ok_or("Local AI workers are busy")?;
    let connection = Connection::new(address, DISCOVERY_TIMEOUT);
    start_worker(connection.cancel_handle(), move || {
        let mut request = connection.agent.get(connection.url("/api/v1/models"));
        if let Some(header) = authorization(credential_env.as_deref()).map_err(Error::message)? {
            request = request.header("authorization", header);
        }
        let response: NativeModels = connection
            .read_json(MAX_RESPONSE_BYTES, || {
                request.config().http_status_as_error(false).build().call()
            })
            .map_err(Error::message)?;
        drop(permit);
        Ok(response.names())
    })
}

pub(crate) fn discover_remote(
    endpoint: &str,
    protocol: RemoteProtocol,
    credential_env: Option<String>,
) -> Result<Pending<Vec<String>>, String> {
    let base = providers::remote_api_base(endpoint, protocol)?;
    validate_credential(credential_env.as_deref())?;
    let permit = WorkerPermit::acquire().ok_or("AI workers are busy")?;
    let connection = Connection::with_base(base, DISCOVERY_TIMEOUT);
    start_worker(connection.cancel_handle(), move || {
        let _permit = permit;
        let mut request = connection.agent.get(connection.url("/models"));
        if protocol == RemoteProtocol::Anthropic {
            request = request.header("anthropic-version", ANTHROPIC_VERSION);
        }
        if let Some((name, value)) =
            remote_credential(credential_env.as_deref(), protocol).map_err(Error::message)?
        {
            request = request.header(name, value);
        }
        connection
            .read_json::<ModelList>(MAX_RESPONSE_BYTES, || {
                request.config().http_status_as_error(false).build().call()
            })
            .map(ModelList::names)
            .map_err(Error::message)
    })
}

pub(crate) fn discover_policy_profile(
    definition: &PolicyAiProfile,
) -> Result<Pending<Vec<String>>, String> {
    let authority = PolicyProfileAuthority::new(definition);
    authority
        .require_current(&ClipboardPolicy::load())
        .map_err(Error::message)?;
    let target = policy_profile::http_target(definition)?;
    let (connection, path, protocol, lm_studio) = match target {
        Target::External(address) => (
            Connection::new(address, DISCOVERY_TIMEOUT),
            "/v1/models",
            RemoteProtocol::OpenAi,
            false,
        ),
        Target::LmStudio(address) => (
            Connection::new(address, DISCOVERY_TIMEOUT),
            "/api/v1/models",
            RemoteProtocol::OpenAi,
            true,
        ),
        Target::Remote(base, protocol) => (
            Connection::with_base(base, DISCOVERY_TIMEOUT),
            "/models",
            protocol,
            false,
        ),
        _ => unreachable!("HTTP policy discovery"),
    };
    let permit = WorkerPermit::acquire().ok_or("AI workers are busy")?;
    start_worker(connection.cancel_handle(), move || {
        let _permit = permit;
        let credential = authority.credential().map_err(Error::message)?;
        let mut request = connection.agent.get(connection.url(path));
        if protocol == RemoteProtocol::Anthropic {
            request = request.header("anthropic-version", ANTHROPIC_VERSION);
        }
        if let Some(value) = &credential {
            let (name, header) = remote_header(value, protocol).map_err(Error::message)?;
            request = request.header(name, header);
        }
        let send = || request.config().http_status_as_error(false).build().call();
        authority
            .require_current(&ClipboardPolicy::load())
            .map_err(Error::message)?;
        let mut names = if lm_studio {
            connection
                .read_json::<NativeModels>(MAX_RESPONSE_BYTES, send)
                .map(NativeModels::names)
        } else {
            connection
                .read_json::<ModelList>(MAX_RESPONSE_BYTES, send)
                .map(ModelList::names)
        }
        .map_err(Error::message)?;
        authority
            .require_current(&ClipboardPolicy::load())
            .map_err(Error::message)?;
        if let Some(value) = credential {
            names.retain_mut(|name| {
                if name.contains(value.as_str()) {
                    name.zeroize();
                    false
                } else {
                    true
                }
            });
        }
        Ok(names)
    })
}

#[derive(Deserialize)]
struct NativeModels {
    models: Vec<NativeModel>,
}

impl NativeModels {
    fn names(self) -> Vec<String> {
        let mut models: Vec<_> = self
            .models
            .into_iter()
            .filter(|model| model.kind == "llm" && valid_model(&model.key))
            .map(|model| model.key)
            .collect();
        models.sort_unstable();
        models.dedup();
        models
    }
}

#[derive(Deserialize)]
struct NativeModel {
    #[serde(rename = "type")]
    kind: String,
    key: String,
}

fn start_discovery(
    endpoint: &str,
    credential_env: Option<String>,
) -> Result<Pending<Vec<String>>, String> {
    let address = numeric_loopback_endpoint(endpoint)?;
    validate_credential(credential_env.as_deref())?;
    let permit = WorkerPermit::acquire().ok_or("Local AI workers are busy")?;
    let connection = Connection::new(address, DISCOVERY_TIMEOUT);
    start_worker(connection.cancel_handle(), move || {
        let result =
            discover_models(&connection, credential_env.as_deref()).map_err(Error::message);
        drop(permit);
        result
    })
}

fn discover_models(
    connection: &Connection,
    credential_env: Option<&str>,
) -> Result<Vec<String>, Error> {
    let endpoint = connection.url("/v1/models");
    let mut request = connection.agent.get(&endpoint);
    if let Some(header) = authorization(credential_env)? {
        request = request.header("authorization", header);
    }
    let response: ModelList = connection.read_json(MAX_RESPONSE_BYTES, || {
        request.config().http_status_as_error(false).build().call()
    })?;
    Ok(response.names())
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<Model>,
}

impl ModelList {
    fn names(self) -> Vec<String> {
        let mut models = self
            .data
            .into_iter()
            .map(|entry| entry.id)
            .filter(|model| valid_model(model))
            .collect::<Vec<_>>();
        models.sort_unstable();
        models.dedup();
        models
    }
}

#[derive(Deserialize)]
struct Model {
    id: String,
}

impl Client {
    /// Binds a local embedding cache to its model and configured route.
    pub(crate) fn embedding_revision(&self) -> Option<[u8; 32]> {
        let binding = match &self.profile.target {
            Target::External(address) => serde_json::to_vec(&("local", address, self.model())),
            Target::Managed(root, Purpose::Embedding, _) => serde_json::to_vec(&(
                "managed",
                root,
                self.model(),
                self.profile.policy_model_fingerprint(),
            )),
            _ => return None,
        }
        .ok()?;
        pgp::crypto::hash::HashAlgorithm::Sha256
            .digest(&binding)
            .ok()?
            .as_slice()
            .try_into()
            .ok()
    }

    /// Requests one vector per input from the selected local embedding model.
    pub(crate) fn embed(&self, input: &[&str]) -> Result<Vec<Vec<f32>>, String> {
        self.embed_reviewed(input, || Ok(()))
    }

    pub(crate) fn embed_reviewed(
        &self,
        input: &[&str],
        before_dispatch: impl FnOnce() -> Result<(), String>,
    ) -> Result<Vec<Vec<f32>>, String> {
        self.embeddings(input, before_dispatch)
            .map_err(Error::message)
    }

    fn embeddings(
        &self,
        input: &[&str],
        before_dispatch: impl FnOnce() -> Result<(), String>,
    ) -> Result<Vec<Vec<f32>>, Error> {
        if matches!(
            self.profile.target,
            Target::LmStudio(_) | Target::PolicyAuto(_) | Target::Remote(..)
        ) {
            return Err(Error::Invalid(
                "use a local embedding endpoint for embeddings",
            ));
        }
        if input.is_empty()
            || input.len() > MAX_EMBEDDING_INPUTS
            || input.iter().any(|text| text.trim().is_empty())
            || input
                .iter()
                .fold(0usize, |length, text| length.saturating_add(text.len()))
                > MAX_EMBEDDING_INPUT_BYTES
        {
            return Err(Error::Invalid(
                "embedding input must contain 1–64 texts up to 2 MiB total",
            ));
        }
        if matches!(
            self.profile.target,
            Target::Managed(_, Purpose::Chat, _) | Target::ManagedMlx(_)
        ) {
            return Err(Error::Invalid(
                "managed chat profiles do not support embeddings",
            ));
        }
        let managed = matches!(
            self.profile.target,
            Target::Managed(_, Purpose::Embedding, _)
        );
        #[cfg(target_os = "macos")]
        let _power = kortty_macos::AppNapActivity::begin();
        let deadline = managed
            .then(|| Instant::now() + self.profile.options.timeout.unwrap_or(DEFAULT_TIMEOUT));
        let transport = self.transport(deadline)?;
        let connection = &transport.connection;
        let endpoint = connection.url(if managed {
            "/v1/embeddings"
        } else {
            "/api/embed"
        });
        let mut request = connection
            .agent
            .post(&endpoint)
            .content_type("application/json");
        let credential = self.profile.policy_credential()?;
        if let Some(header) =
            self.request_authorization(transport, credential.as_deref().map(String::as_str))?
        {
            request = request.header("authorization", header);
        }
        let body = Zeroizing::new(
            serde_json::to_vec(&EmbeddingRequest {
                model: self.model(),
                input,
                truncate: (!managed).then_some(false),
                encoding_format: managed.then_some("float"),
            })
            .map_err(|_| Error::Service)?,
        );
        let mut request = request.config().http_status_as_error(false);
        if let Some(deadline) = deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::TimedOut);
            }
            request = request.timeout_global(Some(remaining));
        }
        self.require_transport_policy(ClipboardPolicy::load)?;
        before_dispatch().map_err(Error::Review)?;
        let send = || request.build().send(body.as_slice());
        let vectors = if managed {
            connection
                .read_json::<ManagedResponse>(MAX_EMBEDDING_RESPONSE_BYTES, send)
                .and_then(|response| response.ordered(input.len()))
        } else {
            connection
                .read_json::<EmbeddingResponse>(MAX_EMBEDDING_RESPONSE_BYTES, send)
                .map(|response| response.embeddings)
        };
        self.validate_transport(transport)?;
        let vectors = vectors?;
        let dimensions = vectors.first().map_or(0, Vec::len);
        if vectors.len() != input.len()
            || !(1..=MAX_EMBEDDING_DIMENSIONS).contains(&dimensions)
            || vectors.iter().any(|embedding| {
                embedding.len() != dimensions || embedding.iter().any(|value| !value.is_finite())
            })
        {
            return Err(Error::Invalid("invalid embeddings"));
        }
        Ok(vectors)
    }
}

impl Connection {
    fn read_json<T: DeserializeOwned>(
        &self,
        limit: u64,
        call: impl FnOnce() -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<T, Error> {
        if !self.active() {
            return Err(Error::Cancelled);
        }
        #[cfg(target_os = "macos")]
        let _power = kortty_macos::AppNapActivity::begin();
        let result = (|| {
            let mut response = call().map_err(|error| transport_error(error, true))?;
            if !response.status().is_success() {
                return Err(Error::Status(response.status().as_u16()));
            }
            let bytes = Zeroizing::new(
                response
                    .body_mut()
                    .with_config()
                    .limit(limit)
                    .read_to_vec()
                    .map_err(|error| match error {
                        ureq::Error::BodyExceedsLimit(_) => {
                            Error::Invalid("response exceeds its size limit")
                        }
                        error => transport_error(error, true),
                    })?,
            );
            serde_json::from_slice(&bytes).map_err(|_| Error::Invalid("invalid JSON response"))
        })();
        if !self.active() {
            return Err(Error::Cancelled);
        }
        result
    }
}

#[derive(Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: &'a [&'a str],
    #[serde(skip_serializing_if = "Option::is_none")]
    truncate: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    encoding_format: Option<&'static str>,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Deserialize)]
struct ManagedResponse {
    data: Vec<ManagedVector>,
}

#[derive(Deserialize)]
struct ManagedVector {
    index: usize,
    embedding: Vec<f32>,
}

impl ManagedResponse {
    fn ordered(mut self, inputs: usize) -> Result<Vec<Vec<f32>>, Error> {
        self.data.sort_unstable_by_key(|vector| vector.index);
        if self.data.len() != inputs
            || self
                .data
                .iter()
                .enumerate()
                .any(|(index, vector)| vector.index != index)
        {
            return Err(Error::Invalid("invalid embedding indices"));
        }
        Ok(self
            .data
            .into_iter()
            .map(|vector| vector.embedding)
            .collect())
    }
}

#[cfg(test)]
mod tests;
