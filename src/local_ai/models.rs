//! Model discovery via the OpenAI-compatible list and embeddings via Ollama.

use std::time::Duration;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use zeroize::Zeroizing;

use super::{
    Client, Connection, Error, MAX_RESPONSE_BYTES, Pending, WorkerPermit, authorization,
    numeric_loopback_endpoint, start_worker, transport_error, valid_model, validate_credential,
};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_EMBEDDING_INPUTS: usize = 64;
const MAX_EMBEDDING_INPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_EMBEDDING_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_EMBEDDING_DIMENSIONS: usize = 65_536;

pub(crate) fn discover(endpoint: &str) -> Result<Pending<Vec<String>>, String> {
    start_discovery(endpoint, None)
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
    let mut models = response
        .data
        .into_iter()
        .map(|entry| entry.id)
        .filter(|model| valid_model(model))
        .collect::<Vec<_>>();
    models.sort_unstable();
    models.dedup();
    Ok(models)
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<Model>,
}

#[derive(Deserialize)]
struct Model {
    id: String,
}

impl Client {
    /// Requests a vector for each input from an explicitly selected local Ollama model.
    pub(crate) fn embed(&self, input: &[&str]) -> Result<Vec<Vec<f32>>, String> {
        self.embeddings(input).map_err(Error::message)
    }

    fn embeddings(&self, input: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
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
        let endpoint = self.connection.url("/api/embed");
        let mut request = self
            .connection
            .agent
            .post(&endpoint)
            .content_type("application/json");
        if let Some(header) = authorization(self.profile.options.credential_env.as_deref())? {
            request = request.header("authorization", header);
        }
        let body = Zeroizing::new(
            serde_json::to_vec(&EmbeddingRequest {
                model: self.model(),
                input,
                truncate: false,
            })
            .map_err(|_| Error::Service)?,
        );
        let response: EmbeddingResponse =
            self.connection
                .read_json(MAX_EMBEDDING_RESPONSE_BYTES, || {
                    request
                        .config()
                        .http_status_as_error(false)
                        .build()
                        .send(body.as_slice())
                })?;
        let dimensions = response.embeddings.first().map_or(0, Vec::len);
        if response.embeddings.len() != input.len()
            || !(1..=MAX_EMBEDDING_DIMENSIONS).contains(&dimensions)
            || response.embeddings.iter().any(|embedding| {
                embedding.len() != dimensions || embedding.iter().any(|value| !value.is_finite())
            })
        {
            return Err(Error::Invalid("invalid embeddings"));
        }
        Ok(response.embeddings)
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
    truncate: bool,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    embeddings: Vec<Vec<f32>>,
}

#[cfg(test)]
mod tests;
