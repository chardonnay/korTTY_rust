//! Bounded requests to an explicitly chosen local model server.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use async_channel::Receiver;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use ureq::{Agent, config::Config};
use zeroize::Zeroizing;

mod models;
pub(crate) mod reply;
pub(crate) use models::{discover, discover_with_credential};

pub(crate) const DEFAULT_ENDPOINT: &str = "127.0.0.1:11434";
pub(crate) const MAX_MODEL_BYTES: usize = 256;
const MAX_RESPONSE_BYTES: u64 = 256 * 1024;
const MAX_ERROR_RESPONSE_BYTES: u64 = 4 * 1024;
const MAX_WORKERS: usize = 2;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
static WORKERS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct ProfileOptions {
    pub(crate) max_output_tokens: Option<u32>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) credential_env: Option<String>,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Profile {
    address: SocketAddr,
    model: String,
    options: ProfileOptions,
}

impl Profile {
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
        validate_credential(options.credential_env.as_deref())?;
        Self::from_address(numeric_loopback_endpoint(endpoint)?, model, options)
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
            address,
            model: model.into(),
            options,
        })
    }
}

fn valid_model(model: &str) -> bool {
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

fn parse_loopback(endpoint: &str) -> Result<SocketAddr, String> {
    endpoint
        .parse::<SocketAddr>()
        .ok()
        .filter(|address| address.ip().is_loopback() && address.port() != 0)
        .ok_or_else(|| "Enter a numeric loopback address and port".into())
}

fn validate_credential(name: Option<&str>) -> Result<(), String> {
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
    let Some(name) = name else {
        return Ok(None);
    };
    let value = Zeroizing::new(std::env::var(name).map_err(|_| Error::Credential)?);
    if value.is_empty() || value.len() > 4096 || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(Error::Credential);
    }
    let bearer = Zeroizing::new(format!("Bearer {}", value.as_str()));
    let mut header = ureq::http::HeaderValue::from_str(&bearer).map_err(|_| Error::Credential)?;
    header.set_sensitive(true);
    Ok(Some(header))
}

pub(crate) struct Pending<T> {
    pub(crate) reply: Receiver<Result<T, String>>,
    pub(crate) cancel: Cancel,
}

pub(crate) struct Cancel {
    activity: Arc<Activity>,
    agent: Option<Agent>,
}

impl Drop for Cancel {
    fn drop(&mut self) {
        *self
            .activity
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = false;
        if let Some(agent) = self.agent.take() {
            agent.shutdown();
        }
        self.activity.changed.notify_all();
    }
}

struct Activity {
    active: Mutex<bool>,
    changed: Condvar,
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
        let cancel = client.connection.cancel_handle();
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
}

pub(crate) fn start<T: Send + 'static>(
    profile: Profile,
    run: impl FnOnce(&Client) -> Result<T, String> + Send + 'static,
) -> Result<Pending<T>, String> {
    let (session, cancel) = Session::begin(profile).ok_or("Local AI workers are busy")?;
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

struct WorkerPermit;

impl WorkerPermit {
    fn acquire() -> Option<Self> {
        WORKERS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < MAX_WORKERS).then_some(current + 1)
            })
            .ok()
            .map(|_| Self)
    }
}

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
pub(crate) enum Error {
    Service,
    Credential,
    Status(u16),
    Invalid(&'static str),
    Cancelled,
    TimedOut,
    UnsupportedResponseFormat,
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
            Self::Cancelled => "The local AI request was cancelled".into(),
            Self::TimedOut => "The local AI request timed out".into(),
            Self::UnsupportedResponseFormat => {
                "The local AI server rejected JSON response mode".into()
            }
        }
    }
}

struct Connection {
    address: SocketAddr,
    agent: Agent,
    activity: Arc<Activity>,
}

impl Connection {
    fn new(address: SocketAddr, timeout: Duration) -> Self {
        let agent = Agent::new_with_config(
            Config::builder()
                .proxy(None)
                .max_redirects(0)
                .max_response_header_size(16 * 1024)
                .timeout_connect(Some(Duration::from_secs(3)))
                .timeout_global(Some(timeout))
                .build(),
        );
        Self {
            address,
            agent,
            activity: Arc::new(Activity {
                active: Mutex::new(true),
                changed: Condvar::new(),
            }),
        }
    }

    fn active(&self) -> bool {
        *self
            .activity
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn cancel_handle(&self) -> Cancel {
        Cancel {
            activity: Arc::clone(&self.activity),
            agent: Some(self.agent.clone()),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }
}

pub(crate) struct Client {
    profile: Profile,
    connection: Connection,
}

impl Client {
    pub(crate) fn new(profile: Profile) -> Self {
        let connection = Connection::new(
            profile.address,
            profile.options.timeout.unwrap_or(DEFAULT_TIMEOUT),
        );
        Self {
            profile,
            connection,
        }
    }

    pub(crate) fn active(&self) -> bool {
        self.connection.active()
    }

    pub(crate) fn complete(&self, system: &str, input: &str) -> Result<String, Error> {
        self.complete_request(system, RequestContent::Text(input), true, None, false)
    }

    pub(crate) fn complete_before(
        &self,
        system: &str,
        input: &str,
        response_format: bool,
        deadline: Instant,
    ) -> Result<String, Error> {
        self.complete_request(
            system,
            RequestContent::Text(input),
            response_format,
            Some(deadline),
            false,
        )
    }

    /// Returns a complete text response for workflows that replace user content.
    pub(crate) fn complete_text_before(
        &self,
        system: &str,
        input: &str,
        response_format: bool,
        deadline: Instant,
    ) -> Result<String, Error> {
        self.complete_request(
            system,
            RequestContent::Text(input),
            response_format,
            Some(deadline),
            true,
        )
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
        if !self.active() {
            return Err(Error::Cancelled);
        }
        let endpoint = self.connection.url("/v1/chat/completions");
        let body = Request {
            model: self.model(),
            stream: false,
            temperature: 0,
            max_tokens: self.profile.options.max_output_tokens.unwrap_or(4096),
            reasoning_effort: self.profile.options.reasoning_effort.as_deref(),
            messages: [
                RequestMessage {
                    role: "system",
                    content: RequestContent::Text(system),
                },
                RequestMessage {
                    role: "user",
                    content: input,
                },
            ],
            response_format: response_format.then_some(ResponseFormat {
                kind: "json_object",
            }),
        };
        let body = Zeroizing::new(serde_json::to_vec(&body).map_err(|_| Error::Service)?);
        let result = self.read_completion(
            &endpoint,
            &body,
            response_format,
            deadline,
            require_complete,
        );
        if !self.active() {
            return Err(Error::Cancelled);
        }
        result
    }

    fn read_completion(
        &self,
        endpoint: &str,
        body: &[u8],
        response_format: bool,
        deadline: Option<Instant>,
        require_complete: bool,
    ) -> Result<String, Error> {
        let mut request = self
            .connection
            .agent
            .post(endpoint)
            .content_type("application/json");
        if let Some(header) = authorization(self.profile.options.credential_env.as_deref())? {
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
        let mut response = request
            .build()
            .send(body)
            .map_err(|error| transport_error(error, bounded))?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            if deadline.is_some() && response_format && matches!(status, 400 | 422) {
                let error = response
                    .body_mut()
                    .with_config()
                    .limit(MAX_ERROR_RESPONSE_BYTES)
                    .read_to_vec();
                if error.as_deref().is_ok_and(unsupported_response_format) {
                    return Err(Error::UnsupportedResponseFormat);
                }
            }
            return Err(Error::Status(status));
        }
        let bytes = Zeroizing::new(
            response
                .body_mut()
                .with_config()
                .limit(MAX_RESPONSE_BYTES)
                .read_to_vec()
                .map_err(|error| match error {
                    ureq::Error::BodyExceedsLimit(_) => Error::Invalid("response exceeds 256 KiB"),
                    error => transport_error(error, bounded),
                })?,
        );
        if !require_complete {
            return first_choice::<String>(&bytes)?
                .message
                .content
                .ok_or(Error::Invalid("missing message content"));
        }
        let choice = first_choice::<Value>(&bytes)?;
        if choice.finish_reason.as_ref().and_then(Value::as_str) == Some("length") {
            return Err(Error::Invalid("model output reached its token limit"));
        }
        match choice.message.content {
            Some(Value::String(text)) => Ok(text),
            Some(Value::Array(parts)) => Ok(parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")),
            Some(_) => Err(Error::Invalid("invalid chat response")),
            None => Err(Error::Invalid("missing message content")),
        }
    }

    /// Waits between requests without keeping a socket open or delaying a UI abort.
    pub(crate) fn wait_until(&self, wake_at: Instant, cancelled: &dyn Fn() -> bool) -> bool {
        loop {
            if cancelled() {
                return false;
            }
            let active = self
                .connection
                .activity
                .active
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if !*active {
                return false;
            }
            let remaining = wake_at.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return true;
            }
            // Dropping Cancel wakes immediately. The short ceiling also polls callers
            // whose cancellation flag may change independently of the socket handle.
            drop(
                self.connection
                    .activity
                    .changed
                    .wait_timeout(active, remaining.min(Duration::from_millis(50))),
            );
        }
    }
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
    if error.code.as_deref() == Some("unsupported_response_format")
        || (error.param.as_deref() == Some("response_format")
            && matches!(
                error.code.as_deref(),
                Some("unsupported_value" | "unsupported_parameter")
            ))
    {
        return true;
    }
    error.message.is_some_and(|message| {
        let message = message.to_ascii_lowercase();
        [
            "response_format is not supported",
            "response_format is unsupported",
            "unsupported parameter: response_format",
            "unsupported parameter: 'response_format'",
            "unsupported parameter: \"response_format\"",
        ]
        .iter()
        .any(|evidence| message.contains(evidence))
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

fn first_choice<T: DeserializeOwned>(bytes: &[u8]) -> Result<Choice<T>, Error> {
    let completion: Completion<T> =
        serde_json::from_slice(bytes).map_err(|_| Error::Invalid("invalid chat response"))?;
    completion
        .choices
        .into_iter()
        .next()
        .ok_or(Error::Invalid("missing message content"))
}

#[derive(Deserialize)]
struct Completion<T> {
    choices: Vec<Choice<T>>,
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
    model: &'a str,
    stream: bool,
    temperature: u8,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
    messages: [RequestMessage<'a>; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
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
