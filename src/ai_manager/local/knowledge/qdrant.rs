//! Bounded transport for the vectors of one immutable knowledge index generation.
//!
//! On services that restrict unindexed payload filters, provision a `keyword`
//! index for `generation` and an `integer` index with lookup for `source` before
//! publishing, including on collections newly created by Qdrant Cloud.

use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use std::{cell::Cell, fmt};

use kortty_vault::{Secret, valid_credential_id};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ureq::http::{HeaderValue, Method, Request, StatusCode};
use url::{Host, Url};
use zeroize::Zeroizing;

use crate::local_ai::{self, Client};
use crate::vault_window::VaultWindow;

use super::{MAX_DIMENSIONS, MAX_INDEX_PASSAGES, MAX_SOURCES, sha256};

const MAX_ENDPOINT_BYTES: usize = 2048;
const MAX_COLLECTION_BYTES: usize = 128;
const MAX_BATCH_POINTS: usize = 128;
// The scalar budget also bounds batches for the largest supported dimensions.
const MAX_BATCH_VALUES: usize = 16 * MAX_DIMENSIONS;
const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
// Queries request only point IDs, scores, and the three metadata fields.
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
// Qdrant returns its f32 cosine dot product directly, including rounding overshoot.
const MAX_COSINE_MAGNITUDE: f64 = 1.001;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const OPERATION_TIMEOUT: Duration = Duration::from_secs(120);
const READ_CONSISTENCY: &str = "consistency=all";
const WRITE_ORDERING: &str = "wait=true&ordering=strong";

const INVALID_ENDPOINT: &str = "Use an HTTPS Qdrant endpoint or a numeric loopback HTTP URL without credentials, a query, or a fragment.";
const CREDENTIAL: &str = "The configured Qdrant API key is unavailable, invalid, or changed.";
const VAULT_CHANGED: &str =
    "The Qdrant vault credential changed or the vault was locked. Review the request again.";
const STANDING_VAULT_CHANGED: &str =
    "The Qdrant vault envelope changed. Retrieve and review the knowledge again.";
const NETWORK: &str = "Could not reach the reviewed Qdrant collection.";
const RESPONSE: &str = "Qdrant returned an invalid or unsuccessful response.";
const QUERY_DISABLED: &str = "The Qdrant collection's strict query limit is zero. Ask the collection administrator to allow search results.";
const GENERATION: &str = "Could not identify the Qdrant index generation.";
const TIMEOUT: &str = "The Qdrant operation timed out.";

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ai_manager::local) struct Settings {
    pub(in crate::ai_manager::local) endpoint: String,
    pub(in crate::ai_manager::local) collection: String,
    pub(in crate::ai_manager::local) dimensions: usize,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(in crate::ai_manager::local) credential_env: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::ai_manager::local) vault_key: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:6333".into(),
            collection: "kortty_rag".into(),
            dimensions: 768,
            credential_env: String::new(),
            vault_key: None,
        }
    }
}

impl fmt::Debug for Settings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QdrantSettings")
            .field("destination", &self.destination())
            .field("dimensions", &self.dimensions)
            .field(
                "has_credential",
                &(!self.credential_env.is_empty() || self.vault_key.is_some()),
            )
            .finish()
    }
}

impl Settings {
    pub(in crate::ai_manager::local) fn validate(&self) -> Result<(), String> {
        self.collection_url()?;
        if !(1..=MAX_DIMENSIONS).contains(&self.dimensions) {
            return Err(format!(
                "Qdrant requires 1 to {MAX_DIMENSIONS} embedding dimensions."
            ));
        }
        if let Some(id) = &self.vault_key {
            if !self.credential_env.is_empty() {
                return Err("Select either a Qdrant vault key or an environment variable.".into());
            }
            if !valid_credential_id(id) {
                return Err("Select a valid Qdrant vault credential.".into());
            }
        } else if !self.credential_env.is_empty() {
            local_ai::validate_credential(Some(&self.credential_env))?;
        }
        Ok(())
    }

    pub(in crate::ai_manager::local) fn resolve_vault_key(
        &self,
        vault: Option<&VaultWindow>,
        reviewed: Option<&Weak<()>>,
    ) -> Result<Option<Arc<VaultKey>>, String> {
        self.resolve_vault_key_for(vault, reviewed, false)
    }

    pub(super) fn resolve_standing_vault_key(
        &self,
        vault: Option<&VaultWindow>,
    ) -> Result<Option<Arc<VaultKey>>, String> {
        self.resolve_vault_key_for(vault, None, true)
    }

    fn resolve_vault_key_for(
        &self,
        vault: Option<&VaultWindow>,
        reviewed: Option<&Weak<()>>,
        standing: bool,
    ) -> Result<Option<Arc<VaultKey>>, String> {
        self.validate()?;
        let Some(id) = &self.vault_key else {
            return Ok(None);
        };
        let vault = vault.ok_or("Unlock the vault to use the selected Qdrant API key.")?;
        let revision = vault
            .credential_revision()
            .ok_or("Unlock the vault to use the selected Qdrant API key.")?;
        if reviewed.is_some_and(|reviewed| !Weak::ptr_eq(reviewed, &revision)) {
            return Err(VAULT_CHANGED.into());
        }
        let (secret, standing_envelope) = if standing {
            let (secret, envelope) = vault.qdrant_api_key_with_envelope(id)?;
            let digest = sha256(envelope).map_err(|_| STANDING_VAULT_CHANGED)?;
            (Secret::new(secret.to_owned()), Some(digest))
        } else {
            (vault.qdrant_api_key(id)?, None)
        };
        let binding = VaultKey {
            id: id.clone(),
            secret,
            revision,
            standing_envelope,
        };
        binding.require_current()?;
        Ok(Some(Arc::new(binding)))
    }

    pub(super) fn require_standing_envelope(
        &self,
        root: &Path,
        envelope: Option<[u8; 32]>,
    ) -> Result<(), String> {
        match (self.vault_key.as_ref(), envelope) {
            (None, None) if self.credential_env.is_empty() => Ok(()),
            (Some(_), Some(expected)) if self.credential_env.is_empty() => {
                let path = crate::profile::vault_path_at(root);
                let current = kortty_vault::Vault::encrypted_envelope(path)
                    .map_err(|_| STANDING_VAULT_CHANGED)?;
                if sha256(&current).map_err(|_| STANDING_VAULT_CHANGED)? != expected {
                    return Err(STANDING_VAULT_CHANGED.into());
                }
                Ok(())
            }
            _ => Err(STANDING_VAULT_CHANGED.into()),
        }
    }

    pub(in crate::ai_manager::local) fn require_vault_key(
        &self,
        binding: Option<&VaultKey>,
    ) -> Result<(), String> {
        match (&self.vault_key, binding) {
            (None, None) => Ok(()),
            (Some(id), Some(binding)) if id == &binding.id && self.credential_env.is_empty() => {
                binding.require_current()
            }
            _ => Err(VAULT_CHANGED.into()),
        }
    }

    pub(in crate::ai_manager::local) fn destination(&self) -> String {
        self.collection_url().map_or_else(
            |_| "Invalid Qdrant destination".into(),
            |url| url.to_string(),
        )
    }

    fn collection_url(&self) -> Result<Url, String> {
        if self.collection.is_empty()
            || self.collection.len() > MAX_COLLECTION_BYTES
            || matches!(self.collection.as_str(), "." | "..")
            || !self
                .collection
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err("Use a Qdrant collection name with up to 128 letters, digits, dots, underscores, or hyphens.".into());
        }
        let endpoint = &self.endpoint;
        let authority = endpoint
            .split_once("://")
            .and_then(|(_, value)| value.split(['/', '?', '#']).next());
        if endpoint.len() > MAX_ENDPOINT_BYTES
            || endpoint.contains('\\')
            || endpoint.chars().any(char::is_control)
            || endpoint.trim() != endpoint.as_str()
            || authority.is_none_or(|authority| authority.is_empty() || authority.contains('@'))
        {
            return Err(INVALID_ENDPOINT.into());
        }
        let mut url = Url::parse(endpoint).map_err(|_| INVALID_ENDPOINT)?;
        let loopback = match url.host() {
            Some(Host::Ipv4(address)) => address.is_loopback(),
            Some(Host::Ipv6(address)) => address.is_loopback(),
            _ => false,
        };
        if url.host().is_none()
            || (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.port() == Some(0)
        {
            return Err(INVALID_ENDPOINT.into());
        }
        url.path_segments_mut()
            .map_err(|_| INVALID_ENDPOINT)?
            .pop_if_empty()
            .push("collections")
            .push(&self.collection);
        Ok(url)
    }
}

/// An operation's secret and revocable proof of the unlocked vault that supplied it.
pub(in crate::ai_manager::local) struct VaultKey {
    id: String,
    secret: Secret,
    revision: Weak<()>,
    standing_envelope: Option<[u8; 32]>,
}

impl VaultKey {
    pub(in crate::ai_manager::local) fn require_current(&self) -> Result<(), String> {
        require_vault_revision(&self.revision)
    }

    pub(in crate::ai_manager::local) fn revision(&self) -> Weak<()> {
        self.revision.clone()
    }

    pub(super) fn standing_envelope(&self) -> Option<[u8; 32]> {
        self.standing_envelope
    }

    #[cfg(test)]
    pub(super) fn for_test(id: String, secret: &str, revision: Weak<()>) -> Arc<Self> {
        assert!(valid_credential_id(&id));
        assert!(kortty_vault::valid_qdrant_api_key(secret));
        Arc::new(Self {
            id,
            secret: Secret::new(secret.to_owned()),
            revision,
            standing_envelope: None,
        })
    }
}

pub(in crate::ai_manager::local) fn require_vault_revision(
    revision: &Weak<()>,
) -> Result<(), String> {
    if revision.strong_count() == 0 {
        Err(VAULT_CHANGED.into())
    } else {
        Ok(())
    }
}

pub(super) type Generation = [u8; 16];

pub(super) fn new_generation() -> Result<Generation, String> {
    let mut generation = [0; 16];
    getrandom::fill(&mut generation).map_err(|_| GENERATION)?;
    validate_generation(generation)?;
    Ok(generation)
}

fn validate_generation(generation: Generation) -> Result<(), String> {
    if generation == [0; 16] {
        Err(GENERATION.into())
    } else {
        Ok(())
    }
}

pub(super) struct Point<'a> {
    pub(super) source: usize,
    pub(super) position: usize,
    pub(super) vector: &'a [f32],
}

pub(super) struct Connection {
    collection: Url,
    dimensions: usize,
    credential: Option<Credential>,
    agent: ureq::Agent,
    staging_attempted: Cell<bool>,
}

enum Credential {
    Environment(EnvironmentKey),
    Vault(Arc<VaultKey>),
}

impl Credential {
    fn require_current(&self) -> Result<(), String> {
        match self {
            Self::Environment(key) => key.current().map(drop),
            Self::Vault(key) => key.require_current(),
        }
    }

    fn header(&self) -> Result<HeaderValue, String> {
        let mut header = match self {
            Self::Environment(key) => HeaderValue::from_bytes(key.current()?.as_bytes()),
            Self::Vault(key) => {
                key.require_current()?;
                HeaderValue::from_bytes(key.secret.as_bytes())
            }
        }
        .map_err(|_| CREDENTIAL)?;
        header.set_sensitive(true);
        Ok(header)
    }
}

struct EnvironmentKey {
    name: String,
    revision: [u8; 32],
}

impl EnvironmentKey {
    fn new(name: &str) -> Result<Self, String> {
        let value = local_ai::credential_value(name).map_err(|_| CREDENTIAL)?;
        Ok(Self {
            name: name.into(),
            revision: sha256(value.as_bytes()).map_err(|_| CREDENTIAL)?,
        })
    }

    fn current(&self) -> Result<Zeroizing<String>, String> {
        let value = local_ai::credential_value(&self.name).map_err(|_| CREDENTIAL)?;
        if sha256(value.as_bytes()).map_err(|_| CREDENTIAL)? != self.revision {
            return Err(CREDENTIAL.into());
        }
        Ok(value)
    }
}

struct Authority<F> {
    deadline: Instant,
    check: F,
}

impl<F: Fn() -> Result<(), String>> Authority<F> {
    fn new(check: F) -> Self {
        Self {
            deadline: Instant::now() + OPERATION_TIMEOUT,
            check,
        }
    }

    fn remaining(&self) -> Result<Duration, String> {
        (self.check)()?;
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            Err(TIMEOUT.into())
        } else {
            Ok(remaining.min(REQUEST_TIMEOUT))
        }
    }
}

#[derive(Serialize)]
struct Upsert<'a> {
    points: Vec<WirePoint<'a>>,
}

#[derive(Serialize)]
struct WirePoint<'a> {
    id: String,
    vector: &'a [f32],
    payload: PointPayload<&'a str>,
}

#[derive(Deserialize, Serialize)]
struct PointPayload<T> {
    generation: T,
    source: usize,
    position: usize,
}

#[derive(Deserialize)]
struct QueryResult {
    points: Vec<WireHit>,
}

#[derive(Debug)]
pub(super) struct QueryHits {
    pub(super) limit: usize,
    pub(super) hits: Vec<(f32, usize, usize)>,
}

#[derive(Deserialize)]
struct WireHit {
    id: String,
    score: f64,
    payload: PointPayload<String>,
}

#[derive(Deserialize)]
struct Response {
    status: String,
    result: Value,
}

impl Connection {
    #[cfg(test)]
    pub(super) fn new(settings: &Settings, client: Option<&Client>) -> Result<Self, String> {
        Self::new_with_vault(settings, client, None)
    }

    pub(super) fn new_with_vault(
        settings: &Settings,
        client: Option<&Client>,
        vault_key: Option<Arc<VaultKey>>,
    ) -> Result<Self, String> {
        settings.validate()?;
        settings.require_vault_key(vault_key.as_deref())?;
        let credential = if let Some(key) = vault_key {
            Some(Credential::Vault(key))
        } else {
            (!settings.credential_env.is_empty())
                .then(|| EnvironmentKey::new(&settings.credential_env).map(Credential::Environment))
                .transpose()?
        };
        Ok(Self {
            collection: settings.collection_url()?,
            dimensions: settings.dimensions,
            credential,
            agent: local_ai::service_agent(client, REQUEST_TIMEOUT)?,
            staging_attempted: Cell::new(false),
        })
    }

    pub(super) fn staging_attempted(&self) -> bool {
        self.staging_attempted.get()
    }

    pub(super) fn stage(
        &self,
        generation: Generation,
        points: &[Point<'_>],
        check: impl Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        self.staging_attempted.set(false);
        validate_generation(generation)?;
        let authority = Authority::new(check);
        authority.remaining()?;
        let mut seen = [false; MAX_INDEX_PASSAGES];
        if points.len() > MAX_INDEX_PASSAGES
            || points.iter().any(|point| {
                point.source >= MAX_SOURCES
                    || point.position >= MAX_INDEX_PASSAGES
                    || std::mem::replace(&mut seen[point.position], true)
                    || !valid_vector(point.vector, self.dimensions)
            })
        {
            return Err("The reviewed Qdrant points do not match the index limits.".into());
        }
        if points.is_empty() {
            return Ok(());
        }
        self.ensure_collection(&authority)?;
        let generation_text = generation_text(generation);
        let batch_size = (MAX_BATCH_VALUES / self.dimensions).min(MAX_BATCH_POINTS);
        let route = self.route(&["points"], Some(WRITE_ORDERING));
        for batch in points.chunks(batch_size) {
            let points = batch
                .iter()
                .map(|point| {
                    Ok(WirePoint {
                        id: point_id(generation, point.position)?,
                        vector: point.vector,
                        payload: PointPayload {
                            generation: generation_text.as_str(),
                            source: point.source,
                            position: point.position,
                        },
                    })
                })
                .collect::<Result<_, String>>()?;
            let result = self.request(
                Method::PUT,
                &route,
                Some(&Upsert { points }),
                &authority,
                Some(&self.staging_attempted),
            )?;
            require_completed(result)?;
        }
        Ok(())
    }

    pub(super) fn query(
        &self,
        generation: Generation,
        query: &[f32],
        enabled: &[bool],
        limit: usize,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<QueryHits, String> {
        validate_generation(generation)?;
        let authority = Authority::new(check);
        authority.remaining()?;
        if enabled.len() > MAX_SOURCES
            || limit > MAX_INDEX_PASSAGES
            || !valid_vector(query, self.dimensions)
        {
            return Err("The Qdrant query does not match the reviewed index limits.".into());
        }
        let sources: Vec<_> = enabled
            .iter()
            .enumerate()
            .filter_map(|(source, &enabled)| enabled.then_some(source))
            .collect();
        if limit == 0 || sources.is_empty() {
            return Ok(QueryHits {
                limit: 0,
                hits: Vec::new(),
            });
        }
        let collection = self.read_collection(&authority)?.ok_or(RESPONSE)?;
        let limit = query_limit(&collection, limit)?;
        let generation_text = generation_text(generation);
        let body = json!({
            "query": query,
            "limit": limit,
            "with_payload": ["generation", "source", "position"],
            "with_vector": false,
            "filter": {"must": [
                generation_condition(&generation_text),
                {"key": "source", "match": {"any": sources}},
            ]},
        });
        let url = self.route(&["points", "query"], Some(READ_CONSISTENCY));
        let result = self
            .request(Method::POST, &url, Some(&body), &authority, None)?
            .ok_or(RESPONSE)?;
        let result: QueryResult = serde_json::from_value(result).map_err(|_| RESPONSE)?;
        if result.points.len() > limit {
            return Err(RESPONSE.into());
        }
        let mut seen = [false; MAX_INDEX_PASSAGES];
        let mut hits = Vec::with_capacity(result.points.len());
        for WireHit { id, score, payload } in result.points {
            let PointPayload {
                generation: returned,
                source,
                position,
            } = payload;
            if returned != generation_text
                || enabled.get(source) != Some(&true)
                || position >= MAX_INDEX_PASSAGES
                || std::mem::replace(&mut seen[position], true)
                || !(-MAX_COSINE_MAGNITUDE..=MAX_COSINE_MAGNITUDE).contains(&score)
                || id != point_id(generation, position)?
            {
                return Err(RESPONSE.into());
            }
            hits.push((score.clamp(-1.0, 1.0) as f32, position, source));
        }
        hits.sort_unstable_by(|left, right| right.0.total_cmp(&left.0).then(left.1.cmp(&right.1)));
        Ok(QueryHits { limit, hits })
    }

    pub(super) fn delete(
        &self,
        generation: Generation,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        validate_generation(generation)?;
        let authority = Authority::new(check);
        let body =
            json!({"filter": {"must": [generation_condition(&generation_text(generation))]}});
        let url = self.route(&["points", "delete"], Some(WRITE_ORDERING));
        let result = self.request(Method::POST, &url, Some(&body), &authority, None)?;
        if result.is_none() {
            return Ok(());
        }
        require_completed(result)
    }

    fn ensure_collection(
        &self,
        authority: &Authority<impl Fn() -> Result<(), String>>,
    ) -> Result<(), String> {
        if let Some(current) = self.read_collection(authority)? {
            return self.validate_collection(&current);
        }
        let body = json!({"vectors": {"size": self.dimensions, "distance": "Cosine"}});
        let creation = self.request(Method::PUT, &self.collection, Some(&body), authority, None);
        // Inspect service defaults after creation, including after a concurrent create or lost reply.
        if let Some(current) = self.read_collection(authority)? {
            self.validate_collection(&current)
        } else {
            creation.and(Err(RESPONSE.into()))
        }
    }

    fn read_collection(
        &self,
        authority: &Authority<impl Fn() -> Result<(), String>>,
    ) -> Result<Option<Value>, String> {
        self.request(
            Method::GET,
            &self.collection,
            None::<&Value>,
            authority,
            None,
        )
    }

    fn validate_collection(&self, result: &Value) -> Result<(), String> {
        let vectors = &result["config"]["params"]["vectors"];
        if vectors["size"].as_u64() != Some(self.dimensions as u64)
            || !vectors["distance"]
                .as_str()
                .is_some_and(|distance| distance.eq_ignore_ascii_case("cosine"))
        {
            return Err(
                "The Qdrant collection does not use the reviewed cosine vector dimensions.".into(),
            );
        }
        let strict = &result["config"]["strict_mode_config"];
        if strict["enabled"].as_bool() == Some(true) {
            let restrict_reads = strict["unindexed_filtering_retrieve"].as_bool() == Some(false);
            let restrict_updates = strict["unindexed_filtering_update"].as_bool() == Some(false);
            let schema = &result["payload_schema"];
            if (restrict_reads || restrict_updates)
                && schema["generation"]["data_type"].as_str() != Some("keyword")
            {
                return Err("The Qdrant collection requires a keyword payload index for generation. Strict search also requires an integer payload index with lookup for source.".into());
            }
            let source = &schema["source"];
            if restrict_reads
                && (source["data_type"].as_str() != Some("integer")
                    || source["params"]["lookup"].as_bool() == Some(false))
            {
                return Err("The Qdrant collection requires an integer payload index with lookup for source.".into());
            }
        }
        Ok(())
    }

    fn route(&self, suffix: &[&str], query: Option<&str>) -> Url {
        let mut url = self.collection.clone();
        url.path_segments_mut()
            .expect("a validated Qdrant endpoint is hierarchical")
            .extend(suffix.iter().copied());
        url.set_query(query);
        url
    }

    fn request(
        &self,
        method: Method,
        url: &Url,
        body: Option<&impl Serialize>,
        authority: &Authority<impl Fn() -> Result<(), String>>,
        attempted: Option<&Cell<bool>>,
    ) -> Result<Option<Value>, String> {
        let mut request = Request::builder()
            .method(method)
            .uri(url.as_str())
            .header("accept", "application/json");
        let bytes = if let Some(body) = body {
            request = request.header("content-type", "application/json");
            serde_json::to_vec(body).map_err(|_| "Could not encode the reviewed Qdrant request.")?
        } else {
            Vec::new()
        };
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err("The reviewed Qdrant request exceeds its size limit.".into());
        }
        let bytes = Zeroizing::new(bytes);
        let remaining = authority.remaining()?;
        if let Some(credential) = &self.credential {
            request = request.header("api-key", credential.header()?);
        }
        let request = request.body(bytes.as_slice()).map_err(|_| NETWORK)?;
        if let Some(attempted) = attempted {
            attempted.set(true);
        }
        let request = self
            .agent
            .configure_request(request)
            .http_status_as_error(false)
            .timeout_global(Some(remaining))
            .build();
        let response = self.agent.run(request);
        if response.is_err() {
            self.require_credential_current()?;
        }
        let mut response = response.map_err(|_| NETWORK)?;
        if !response.status().is_success() {
            authority.remaining()?;
            self.require_credential_current()?;
            return if response.status() == StatusCode::NOT_FOUND {
                Ok(None)
            } else {
                Err(RESPONSE.into())
            };
        }
        let bytes = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_vec()
            .map(Zeroizing::new);
        authority.remaining()?;
        self.require_credential_current()?;
        let bytes = bytes.map_err(|_| RESPONSE)?;
        let response: Response = serde_json::from_slice(&bytes).map_err(|_| RESPONSE)?;
        if response.status != "ok" {
            return Err(RESPONSE.into());
        }
        Ok(Some(response.result))
    }

    pub(super) fn require_credential_current(&self) -> Result<(), String> {
        self.credential
            .as_ref()
            .map_or(Ok(()), Credential::require_current)
    }
}

fn query_limit(collection: &Value, requested: usize) -> Result<usize, String> {
    let config = collection["config"].as_object().ok_or(RESPONSE)?;
    let strict = match config.get("strict_mode_config") {
        None | Some(Value::Null) => return Ok(requested),
        Some(Value::Object(strict)) => strict,
        _ => return Err(RESPONSE.into()),
    };
    match strict.get("enabled") {
        None | Some(Value::Null | Value::Bool(false)) => return Ok(requested),
        Some(Value::Bool(true)) => {}
        _ => return Err(RESPONSE.into()),
    }
    match strict.get("max_query_limit") {
        None | Some(Value::Null) => Ok(requested),
        Some(value) => match value.as_u64().ok_or(RESPONSE)? {
            0 => Err(QUERY_DISABLED.into()),
            maximum => Ok(requested.min(usize::try_from(maximum).unwrap_or(usize::MAX))),
        },
    }
}

fn require_completed(result: Option<Value>) -> Result<(), String> {
    if result.as_ref().and_then(|value| value["status"].as_str()) != Some("completed") {
        return Err(RESPONSE.into());
    }
    Ok(())
}

fn valid_vector(vector: &[f32], dimensions: usize) -> bool {
    if vector.len() != dimensions {
        return false;
    }
    let squared: f64 = vector.iter().map(|&value| f64::from(value).powi(2)).sum();
    (0.999..=1.001).contains(&squared)
}

fn generation_text(generation: Generation) -> String {
    format!("{:032x}", u128::from_be_bytes(generation))
}

fn generation_condition(generation: &str) -> Value {
    json!({"key": "generation", "match": {"value": generation}})
}

fn point_id(generation: Generation, position: usize) -> Result<String, String> {
    let mut key = b"kortty-qdrant-point-v1\0".to_vec();
    key.extend(generation);
    key.extend((position as u64).to_be_bytes());
    let mut uuid: [u8; 16] = sha256(&key).map_err(|_| GENERATION)?[..16]
        .try_into()
        .map_err(|_| GENERATION)?;
    uuid[6] = (uuid[6] & 0x0f) | 0x80;
    uuid[8] = (uuid[8] & 0x3f) | 0x80;
    let hex = generation_text(uuid);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

#[cfg(test)]
mod tests;
