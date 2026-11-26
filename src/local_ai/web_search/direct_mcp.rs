//! Bounded, explicitly authorized page reads over Streamable HTTP MCP.

use std::io::{self, BufRead, BufReader, Read};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use ureq::http::{HeaderValue, Response, StatusCode, header};
use zeroize::Zeroizing;

use super::super::{MAX_RESPONSE_BYTES, credential_echo, transport_error};
use super::{
    Activity, Cancel, Connection, Error, Hit, PageUrl, Pending, WorkerPermit, bearer_header,
    connection_agent, connection_config, credential_value, require_policy, response_bytes,
    start_worker,
};

const VERSION: &str = "2025-06-18";
const NOVEMBER_VERSION: &str = "2025-11-25";
const TIMEOUT: Duration = Duration::from_secs(60);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
const ACCEPT: &str = "application/json, text/event-stream";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";
const SESSION_HEADER: &str = "mcp-session-id";
pub(super) const INVALID: Error = Error::Invalid("invalid MCP tool response");

#[derive(Default)]
struct Session {
    protocol_header: Option<&'static str>,
    id: Option<HeaderValue>,
}

impl Session {
    fn negotiate(version: Option<&str>, id: Option<HeaderValue>) -> Result<Self, Error> {
        let protocol_header = match version {
            Some(VERSION) => Some(VERSION),
            Some(NOVEMBER_VERSION) => Some(NOVEMBER_VERSION),
            Some("2025-03-26") => None,
            _ => return Err(Error::Invalid("unsupported MCP protocol")),
        };
        Ok(Self {
            protocol_header,
            id,
        })
    }
}

pub(super) enum Authentication {
    Query(&'static str, Option<Zeroizing<String>>),
    Bearer(Option<String>),
}

impl Authentication {
    fn env(&self) -> Option<&str> {
        match self {
            Self::Query(name, None) => Some(name),
            Self::Query(_, Some(_)) => None,
            Self::Bearer(name) => name.as_deref(),
        }
    }
}

pub(super) struct ReadPage {
    pub(super) label: &'static str,
    pub(super) endpoint: String,
    pub(super) auth: Authentication,
    pub(super) tool: &'static str,
}

impl ReadPage {
    pub(super) fn start<T: Send + 'static>(
        self,
        url: PageUrl,
        arguments: Value,
        before_dispatch: impl Fn() -> Result<(), String> + Send + 'static,
        read: impl FnOnce(Value, &PageUrl, &dyn Fn(&str) -> bool) -> Result<Vec<Hit>, Error>
        + Send
        + 'static,
        finish: impl FnOnce(Vec<Hit>) -> T + Send + 'static,
    ) -> Result<Pending<T>, String> {
        let permit = WorkerPermit::acquire().ok_or("AI workers are busy.")?;
        let activity = Arc::new(Activity::default());
        let cancel = Cancel {
            activity: Arc::clone(&activity),
        };
        start_worker(cancel, move || {
            let _permit = permit;
            require_policy()?;
            let key = self
                .auth
                .env()
                .map(|name| {
                    credential_value(name).map_err(|_| format!("Set {name} to use {}.", self.label))
                })
                .transpose()?;
            let agent = connection_agent(connection_config(TIMEOUT));
            let connection = Connection::attach(self.endpoint, agent, activity)
                .map_err(|error| message(self.label, error))?;
            #[cfg(target_os = "macos")]
            let _power = kortty_macos::AppNapActivity::begin();
            let key = match &self.auth {
                Authentication::Query(_, Some(saved)) => Some(saved.as_str()),
                _ => key.as_deref().map(String::as_str),
            };
            let call = ToolCall {
                auth: &self.auth,
                key,
                url: &url,
                tool: self.tool,
                arguments,
            };
            let result = call
                .fetch(
                    &connection,
                    || {
                        require_policy()?;
                        if let Some(name) = self.auth.env()
                            && !credential_value(name)
                                .is_ok_and(|current| Some(current.as_str()) == key)
                        {
                            return Err(format!(
                                "The {} credential changed. Review the page again.",
                                self.label
                            ));
                        }
                        before_dispatch()
                    },
                    read,
                )
                .map_err(|error| message(self.label, error))?;
            Ok(finish(result))
        })
    }
}

pub(super) struct ToolCall<'a> {
    pub(super) auth: &'a Authentication,
    pub(super) key: Option<&'a str>,
    pub(super) url: &'a PageUrl,
    pub(super) tool: &'static str,
    pub(super) arguments: Value,
}

impl ToolCall<'_> {
    pub(super) fn fetch(
        self,
        connection: &Connection,
        authorize: impl Fn() -> Result<(), String>,
        read: impl FnOnce(Value, &PageUrl, &dyn Fn(&str) -> bool) -> Result<Vec<Hit>, Error>,
    ) -> Result<Vec<Hit>, Error> {
        let key_secret = self.key.map(secret_checker);
        let has_secret = |text: &str| key_secret.as_ref().is_some_and(|contains| contains(text));
        if self.key == Some("")
            || has_secret(self.url.as_str())
            || has_secret(&self.url.decoded().map_err(Error::Invalid)?)
        {
            return Err(Error::Invalid("invalid page address or credential"));
        }
        let endpoint = match self.auth {
            Authentication::Query(..) => {
                let mut endpoint = url::Url::parse(&connection.base).map_err(|_| Error::Service)?;
                endpoint
                    .query_pairs_mut()
                    .append_pair("token", self.key.ok_or(Error::Credential)?)
                    .append_pair("tools", self.tool);
                Zeroizing::new(String::from(endpoint))
            }
            Authentication::Bearer(_) => Zeroizing::new(connection.base.clone()),
        };
        let bearer = match self.auth {
            Authentication::Query(..) => None,
            Authentication::Bearer(_) => self.key.map(bearer_header).transpose()?,
        };
        let service = Service {
            connection,
            endpoint: &endpoint,
            bearer: bearer.as_ref(),
            deadline: Instant::now() + TIMEOUT,
        };
        let authorize = || {
            require_active(connection)?;
            authorize().map_err(Error::Review)?;
            require_active(connection)
        };

        authorize()?;
        let mut response = service.post(
            &Session::default(),
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":VERSION,"capabilities":{},"clientInfo":{
                    "name":"kortty","version":env!("CARGO_PKG_VERSION")
                }
            }}),
        )?;
        let id = session_header(&response)?;
        let initialized = rpc_result(&mut response, 1, connection)?;
        let session = Session::negotiate(initialized["protocolVersion"].as_str(), id)?;
        let result = (|| {
            if !initialized["capabilities"]["tools"].is_object() {
                return Err(Error::Invalid("the server did not offer MCP tools"));
            }
            authorize()?;
            let response = service.post(
                &session,
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            )?;
            if response.status() != StatusCode::ACCEPTED {
                return Err(INVALID);
            }
            authorize()?;
            let mut response = service.post(
                &session,
                &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":self.tool,"arguments":self.arguments
                }}),
            )?;
            rpc_result(&mut response, 2, connection)
        })();
        if !matches!(&result, Err(Error::Review(_) | Error::Cancelled))
            && session.id.is_some()
            && remaining(service.deadline).is_ok()
        {
            authorize()?;
            service.close(&session);
        }
        let result = result?;
        authorize()?;
        require_tool_success(&result)?;
        let session_secret = session
            .id
            .as_ref()
            .and_then(|id| id.to_str().ok())
            .map(secret_checker);
        read(result, self.url, &|text| {
            has_secret(text)
                || session_secret
                    .as_ref()
                    .is_some_and(|contains| contains(text))
        })
    }
}

pub(super) fn text_result(result: &Value) -> Result<&str, Error> {
    let [entry] = result["content"].as_array().ok_or(INVALID)?.as_slice() else {
        return Err(INVALID);
    };
    if entry["type"].as_str() != Some("text") {
        return Err(INVALID);
    }
    entry["text"].as_str().ok_or(INVALID)
}

pub(super) fn require_tool_success(result: &Value) -> Result<(), Error> {
    if !matches!(
        result.as_object().ok_or(INVALID)?.get("isError"),
        None | Some(Value::Bool(false))
    ) {
        return Err(INVALID);
    }
    Ok(())
}

pub(super) fn secret_checker(secret: &str) -> impl Fn(&str) -> bool + '_ {
    let encoded =
        Zeroizing::new(url::form_urlencoded::byte_serialize(secret.as_bytes()).collect::<String>());
    move |text| {
        credential_echo(text, secret)
            || credential_echo(text, &encoded)
            || percent_encoding::percent_decode_str(text)
                .decode_utf8()
                .is_ok_and(|decoded| decoded.contains(secret))
    }
}

fn require_active(connection: &Connection) -> Result<(), Error> {
    if connection.active() {
        Ok(())
    } else {
        Err(Error::Cancelled)
    }
}

fn remaining(deadline: Instant) -> Result<Duration, Error> {
    let duration = deadline.saturating_duration_since(Instant::now());
    if duration.is_zero() {
        Err(Error::TimedOut)
    } else {
        Ok(duration)
    }
}

struct Service<'a> {
    connection: &'a Connection,
    endpoint: &'a str,
    bearer: Option<&'a HeaderValue>,
    deadline: Instant,
}

impl Service<'_> {
    fn post(&self, session: &Session, body: &Value) -> Result<Response<ureq::Body>, Error> {
        let body = Zeroizing::new(serde_json::to_vec(body).map_err(|_| Error::Service)?);
        let request = self
            .connection
            .agent
            .post(self.endpoint)
            .header(header::ACCEPT, ACCEPT)
            .header(header::CONTENT_TYPE, "application/json");
        let response = self
            .add_headers(request, session)
            .config()
            .http_status_as_error(false)
            .timeout_global(Some(remaining(self.deadline)?))
            .build()
            .send(body.as_slice());
        require_active(self.connection)?;
        let response = response.map_err(|error| transport_error(error, true))?;
        if !response.status().is_success() {
            return Err(Error::Status(response.status().as_u16()));
        }
        Ok(response)
    }

    fn close(&self, session: &Session) {
        if let Ok(remaining) = remaining(self.deadline)
            && self.connection.active()
        {
            let request = self.connection.agent.delete(self.endpoint);
            let _ = self
                .add_headers(request, session)
                .config()
                .timeout_global(Some(remaining.min(CLOSE_TIMEOUT)))
                .build()
                .call();
        }
    }

    fn add_headers<B>(
        &self,
        mut request: ureq::RequestBuilder<B>,
        session: &Session,
    ) -> ureq::RequestBuilder<B> {
        if let Some(version) = session.protocol_header {
            request = request.header(PROTOCOL_HEADER, version);
        }
        if let Some(id) = &session.id {
            request = request.header(SESSION_HEADER, id.clone());
        }
        if let Some(bearer) = self.bearer {
            request = request.header(header::AUTHORIZATION, bearer.clone());
        }
        request
    }
}

fn session_header(response: &Response<ureq::Body>) -> Result<Option<HeaderValue>, Error> {
    let headers = response.headers().get_all(SESSION_HEADER);
    let mut headers = headers.iter();
    let Some(value) = headers.next() else {
        return Ok(None);
    };
    if headers.next().is_some()
        || value.is_empty()
        || value.len() > 1024
        || !value.as_bytes().iter().all(|byte| byte.is_ascii_graphic())
    {
        return Err(INVALID);
    }
    let mut session = value.clone();
    session.set_sensitive(true);
    Ok(Some(session))
}

fn rpc_result(
    response: &mut Response<ureq::Body>,
    id: u64,
    connection: &Connection,
) -> Result<Value, Error> {
    let media_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|header| header.to_str().ok())
        .map(|value| value.split(';').next().unwrap_or_default().trim())
        .ok_or(INVALID)?;
    let mut envelope = if media_type.eq_ignore_ascii_case("application/json") {
        let bytes = response_bytes(response, true)?;
        serde_json::from_slice::<Value>(&bytes).map_err(|_| INVALID)?
    } else if media_type.eq_ignore_ascii_case("text/event-stream") {
        sse_result(response.body_mut().as_reader(), id, || connection.active())?
    } else {
        return Err(INVALID);
    };
    require_active(connection)?;
    if envelope["jsonrpc"].as_str() != Some("2.0")
        || envelope["id"].as_u64() != Some(id)
        || envelope.get("error").is_some()
    {
        return Err(INVALID);
    }
    envelope
        .as_object_mut()
        .and_then(|object| object.remove("result"))
        .ok_or(INVALID)
}

fn sse_result(reader: impl Read, id: u64, active: impl Fn() -> bool) -> Result<Value, Error> {
    let mut reader = BufReader::new(reader.take(MAX_RESPONSE_BYTES + 1));
    let mut line = Zeroizing::new(String::new());
    let mut data = Zeroizing::new(String::new());
    let mut bytes = 0;
    loop {
        if !active() {
            return Err(Error::Cancelled);
        }
        line.clear();
        let read = reader.read_line(&mut line).map_err(|error| {
            if !active() {
                Error::Cancelled
            } else if error.kind() == io::ErrorKind::TimedOut {
                Error::TimedOut
            } else {
                INVALID
            }
        })?;
        bytes += read as u64;
        if read == 0 || bytes > MAX_RESPONSE_BYTES {
            return Err(INVALID);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() && !data.is_empty() {
            let event: Value = serde_json::from_str(&data).map_err(|_| INVALID)?;
            if event["id"].as_u64() == Some(id) {
                return Ok(event);
            }
            if event.get("id").is_some()
                || event["jsonrpc"].as_str() != Some("2.0")
                || !event["method"]
                    .as_str()
                    .is_some_and(|method| method.starts_with("notifications/"))
            {
                return Err(INVALID);
            }
            data.clear();
        } else if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
        }
    }
}

fn message(label: &str, error: Error) -> String {
    match error {
        Error::Status(status) => format!("{label} MCP returned HTTP {status}."),
        Error::TimedOut => format!("The {label} page request timed out."),
        Error::Cancelled => format!("The {label} page request was canceled."),
        Error::Invalid(reason) => format!("The {label} page request was rejected ({reason})."),
        Error::Review(reason) => reason,
        _ => format!("Could not complete the {label} page request."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_follows_negotiated_protocol_header_rules() {
        for (version, header) in [
            ("2025-03-26", None),
            (VERSION, Some(VERSION)),
            (NOVEMBER_VERSION, Some(NOVEMBER_VERSION)),
        ] {
            let id = HeaderValue::from_static("fixture-session");
            let session = Session::negotiate(Some(version), Some(id.clone())).unwrap();
            assert_eq!(session.protocol_header, header);
            assert_eq!(session.id, Some(id));
        }
        assert!(Session::negotiate(None, None).is_err());
        assert!(Session::negotiate(Some("2026-07-28"), None).is_err());
    }

    #[test]
    fn sse_rejects_oversized_events_and_unrelated_result_ids() {
        let long = vec![b'a'; MAX_RESPONSE_BYTES as usize + 1];
        assert!(matches!(
            sse_result(long.as_slice(), 2, || true),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            sse_result(
                b"data: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}\n\n".as_slice(),
                2,
                || true,
            ),
            Err(Error::Invalid(_))
        ));
    }
}
