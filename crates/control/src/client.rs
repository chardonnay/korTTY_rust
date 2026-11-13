use std::collections::VecDeque;
use std::fmt;
use std::io::{self, BufReader, Write};
use std::net::Shutdown;
#[cfg(windows)]
use std::net::{Ipv4Addr, SocketAddr, TcpStream as Stream};
#[cfg(unix)]
use std::os::unix::net::UnixStream as Stream;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::endpoint::Endpoint;
use crate::events::DEPTH;
use crate::protocol::{API, MAX_LINE, PROTOCOL_VERSION};
use crate::server::{epoch_millis, read_frame};

#[derive(Debug)]
pub enum ClientError {
    Connect(io::Error),
    Io(io::Error),
    Protocol(&'static str),
    Server {
        code: String,
        message: String,
        exit: u8,
        rpc: Value,
    },
}

impl ClientError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Connect(_) => 3,
            Self::Io(error) if error.kind() == io::ErrorKind::TimedOut => 4,
            Self::Server { exit, .. } => *exit,
            _ => 1,
        }
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(error) if error.kind() == io::ErrorKind::NotFound => {
                f.write_str("Control API is off or the profile is unavailable")
            }
            Self::Connect(error) => write!(f, "Cannot connect to local control API ({error})"),
            Self::Io(error) => write!(f, "Control API I/O failed ({error})"),
            Self::Protocol(error) => f.write_str(error),
            Self::Server { code, message, .. } => write!(f, "{code}: {message}"),
        }
    }
}

impl std::error::Error for ClientError {}

pub struct Client {
    events: EventBacklog,
    stream: BufReader<Stream>,
    deadline: Instant,
    next_id: u64,
    instance: String,
    #[cfg(target_os = "linux")]
    endpoint_pid: u32,
}

#[derive(Default)]
struct EventBacklog {
    pending: VecDeque<QueuedEvent>,
    ordinary_count: usize,
    overflow_count: usize,
    loss: Option<ClientLoss>,
}

enum QueuedEvent {
    Ordinary(Value),
    ServerOverflow(ServerOverflow),
}

struct ServerOverflow {
    subscription: u64,
    at_millis: u64,
    dropped: u64,
}

#[derive(Default)]
struct ClientLoss {
    index: usize,
    at_millis: u64,
    dropped: u64,
    server_markers_dropped: u64,
}

impl EventBacklog {
    fn push(&mut self, value: Value) {
        let event = ServerOverflow::parse(&value)
            .map_or_else(|| QueuedEvent::Ordinary(value), QueuedEvent::ServerOverflow);
        let overflow = event.is_server_overflow();
        let count = if overflow {
            &mut self.overflow_count
        } else {
            &mut self.ordinary_count
        };
        // Ordinary traffic cannot displace a server marker. The separate, compact
        // marker budget also bounds connections that never drain their backlog.
        if *count == DEPTH {
            let index = self
                .pending
                .iter()
                .position(|event| event.is_server_overflow() == overflow)
                .expect("Full event queue contains the evicted event kind");
            self.pending.remove(index);
            let loss = self.loss.get_or_insert_with(|| ClientLoss {
                index,
                at_millis: epoch_millis(),
                ..ClientLoss::default()
            });
            loss.index = loss.index.min(index);
            let dropped = if overflow {
                &mut loss.server_markers_dropped
            } else {
                &mut loss.dropped
            };
            *dropped = dropped.saturating_add(1);
        } else {
            *count += 1;
        }
        self.pending.push_back(event);
    }

    fn pop(&mut self) -> Option<Value> {
        // Place the local summary at the first missing position, preserving the
        // wire order of the retained server markers and ordinary events.
        if self.loss.as_ref().is_some_and(|loss| loss.index == 0) {
            return self.loss.take().map(ClientLoss::into_event);
        }
        let event = self.pending.pop_front()?;
        if let Some(loss) = self.loss.as_mut() {
            loss.index -= 1;
        }
        match event {
            QueuedEvent::Ordinary(value) => {
                self.ordinary_count -= 1;
                Some(value)
            }
            QueuedEvent::ServerOverflow(overflow) => {
                self.overflow_count -= 1;
                Some(overflow.into_event())
            }
        }
    }
}

impl QueuedEvent {
    fn is_server_overflow(&self) -> bool {
        matches!(self, Self::ServerOverflow(_))
    }
}

impl ServerOverflow {
    fn parse(value: &Value) -> Option<Self> {
        let params = value.get("params")?.as_object()?;
        if value["method"] != "event"
            || params.get("kind")?.as_str()? != "events.overflow"
            || value.as_object()?.len() != 3
            || params.len() != 4
        {
            return None;
        }
        let id = params.get("subscription_id")?.as_str()?.strip_prefix('s')?;
        if id.starts_with('0') || !id.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let dropped = params.get("dropped")?.as_u64()?;
        if dropped == 0 {
            return None;
        }
        Some(Self {
            subscription: id.parse().ok()?,
            at_millis: params.get("at_millis")?.as_u64()?,
            dropped,
        })
    }

    fn into_event(self) -> Value {
        json!({"jsonrpc":"2.0","method":"event","params":{
            "kind":"events.overflow","at_millis":self.at_millis,
            "subscription_id":format!("s{}", self.subscription),"dropped":self.dropped
        }})
    }
}

impl ClientLoss {
    fn into_event(self) -> Value {
        let mut params = json!({
            "kind":"events.overflow", "at_millis":self.at_millis,
            "source":"client", "dropped":self.dropped
        });
        // A lost marker is distinct from both its server count and a locally
        // dropped ordinary event. Never attribute either to a subscription.
        if self.server_markers_dropped != 0 {
            params["server_markers_dropped"] = json!(self.server_markers_dropped);
        }
        json!({"jsonrpc":"2.0","method":"event","params":params})
    }
}

#[derive(Serialize)]
struct Call<'a, P> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: &'a P,
}

#[derive(Serialize)]
struct Credentials<'a> {
    token: &'a str,
    client: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_kind: Option<&'static str>,
}

pub(crate) struct McpSocket(Stream);

impl McpSocket {
    pub(crate) fn close(self) {
        let _ = self.0.shutdown(Shutdown::Both);
    }
}

impl Client {
    pub fn connect(root: &Path, timeout: Duration) -> Result<Self, ClientError> {
        Self::connect_before(root, deadline_after(timeout)?, "kortty-cli", None)
    }

    #[cfg(test)]
    pub(crate) fn connect_mcp(
        root: &Path,
        timeout: Duration,
        name: &str,
    ) -> Result<Self, ClientError> {
        Self::connect_mcp_before(root, deadline_after(timeout)?, name)
    }

    pub(crate) fn connect_mcp_before(
        root: &Path,
        deadline: Instant,
        name: &str,
    ) -> Result<Self, ClientError> {
        Self::connect_before(root, deadline, name, Some("mcp"))
    }

    fn connect_before(
        root: &Path,
        deadline: Instant,
        name: &str,
        kind: Option<&'static str>,
    ) -> Result<Self, ClientError> {
        let endpoint = Endpoint::read_profile(root).map_err(ClientError::Connect)?;
        #[cfg(unix)]
        let stream = {
            let _ = remaining(deadline)?;
            Stream::connect(
                endpoint
                    .path
                    .as_ref()
                    .ok_or(ClientError::Protocol("Missing local socket path"))?,
            )
            .map_err(ClientError::Connect)?
        };
        #[cfg(windows)]
        let stream = Stream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port)),
            remaining(deadline)?,
        )
        .map_err(ClientError::Connect)?;
        stream
            .set_read_timeout(Some(Duration::from_millis(100).min(remaining(deadline)?)))
            .map_err(ClientError::Connect)?;
        let mut client = Self {
            events: EventBacklog::default(),
            stream: BufReader::new(stream),
            deadline,
            next_id: 0,
            instance: endpoint.instance_id.clone(),
            #[cfg(target_os = "linux")]
            endpoint_pid: endpoint.pid,
        };
        let hello = client.call_encoded(
            "auth",
            &Credentials {
                token: &endpoint.token,
                client: name,
                client_kind: kind,
            },
        )?;
        if hello.get("api").and_then(Value::as_str) != Some(API)
            || hello.get("protocol_version").and_then(Value::as_u64)
                != Some(u64::from(PROTOCOL_VERSION))
            || hello.get("instance_id").and_then(Value::as_str) != Some(client.instance.as_str())
            || (kind == Some("mcp") && hello["mcp_write_tools"].as_bool().is_none())
        {
            return Err(ClientError::Protocol(
                "The listener does not match the local control endpoint",
            ));
        }
        Ok(client)
    }

    pub(crate) fn set_deadline(&mut self, deadline: Instant) -> Result<(), ClientError> {
        let timeout = remaining(deadline)?;
        self.stream
            .get_ref()
            .set_read_timeout(Some(timeout.min(Duration::from_millis(100))))
            .map_err(ClientError::Io)?;
        self.deadline = deadline;
        Ok(())
    }

    pub(crate) fn mcp_socket(&self) -> Result<McpSocket, ClientError> {
        self.stream
            .get_ref()
            .try_clone()
            .map(McpSocket)
            .map_err(ClientError::Io)
    }

    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// Resolves the pane owning this process or an OS-observed ancestor.
    /// A caller outside a local shell receives no pane.
    pub fn resolve_current_pane(&mut self) -> Result<Option<String>, ClientError> {
        let pids = crate::process::ancestors();
        if pids.is_empty() {
            return Ok(None);
        }
        #[cfg(target_os = "linux")]
        if !crate::process::same_pid_namespace(self.stream.get_ref(), self.endpoint_pid) {
            return Ok(None);
        }
        let response = self.call("pane.resolve", &json!({ "pids": pids }))?;
        let (pane, matched) = match (response.get("pane"), response.get("matched_pid")) {
            (Some(Value::Null), Some(Value::Null)) => return Ok(None),
            (Some(Value::Object(pane)), Some(matched)) => (pane, matched),
            _ => return Err(ClientError::Protocol("Invalid pane resolution response")),
        };
        let matched = matched
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| pids.contains(pid))
            .ok_or(ClientError::Protocol(
                "Pane resolution returned an unexpected process ID",
            ))?;
        if pane.get("shell_pid").and_then(Value::as_u64) != Some(u64::from(matched)) {
            return Err(ClientError::Protocol(
                "Pane resolution returned inconsistent process IDs",
            ));
        }
        if !crate::process::ancestors().contains(&matched) {
            return Ok(None);
        }
        let pane_id = pane
            .get("pane_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or(ClientError::Protocol("Missing resolved pane ID"))?;
        Ok(Some(pane_id.to_owned()))
    }

    pub fn call(&mut self, method: &str, params: &Value) -> Result<Value, ClientError> {
        self.call_encoded(method, params)
    }

    /// Reads the next subscription event within the client's configured deadline.
    pub fn next_event(&mut self) -> Result<Value, ClientError> {
        if let Some(event) = self.events.pop() {
            return Ok(event);
        }
        let event = self.read_message()?;
        if event.get("id").is_some() || event.get("method").and_then(Value::as_str).is_none() {
            return Err(ClientError::Protocol(
                "Expected a control subscription event",
            ));
        }
        Ok(event)
    }

    fn read_message(&mut self) -> Result<Value, ClientError> {
        let bytes = read_frame(
            &mut self.stream,
            &AtomicBool::new(true),
            Some(self.deadline),
        )
        .map_err(ClientError::Io)?
        .ok_or(ClientError::Protocol("Control connection closed"))?;
        let response: Value = serde_json::from_slice(&bytes)
            .map_err(|_| ClientError::Protocol("Invalid control response"))?;
        if response.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(ClientError::Protocol("Invalid control response version"));
        }
        Ok(response)
    }

    fn call_encoded(
        &mut self,
        method: &str,
        params: &impl Serialize,
    ) -> Result<Value, ClientError> {
        let remaining = remaining(self.deadline)?;
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let mut bytes = Zeroizing::new(
            serde_json::to_vec(&Call {
                jsonrpc: "2.0",
                id,
                method,
                params,
            })
            .map_err(|_| ClientError::Protocol("Invalid control request"))?,
        );
        if bytes.len() > MAX_LINE {
            return Err(ClientError::Protocol("Control request exceeds 1 MiB"));
        }
        bytes.push(b'\n');
        let stream = self.stream.get_mut();
        stream
            .set_write_timeout(Some(remaining))
            .map_err(ClientError::Io)?;
        stream.write_all(&bytes).map_err(ClientError::Io)?;
        let mut response = loop {
            let response = self.read_message()?;
            if response.get("id").is_none()
                && response.get("method").and_then(Value::as_str).is_some()
            {
                self.events.push(response);
                continue;
            }
            break response;
        };
        if let Some(error) = response
            .as_object_mut()
            .and_then(|object| object.remove("error"))
        {
            let data = error.get("data");
            let code = data
                .and_then(|data| data.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("control_error");
            if response.get("id").and_then(Value::as_u64) != Some(id)
                && !(response.get("id") == Some(&Value::Null) && code == "too_many_connections")
            {
                return Err(ClientError::Protocol(
                    "Control response has the wrong request id",
                ));
            }
            let exit = data
                .and_then(|data| data.get("exit"))
                .and_then(Value::as_u64)
                .filter(|exit| (1..=4).contains(exit))
                .map_or(1, |exit| exit as u8);
            return Err(ClientError::Server {
                code: code.into(),
                exit,
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Control request failed")
                    .into(),
                rpc: error,
            });
        }
        if response.get("id").and_then(Value::as_u64) != Some(id) {
            return Err(ClientError::Protocol(
                "Control response has the wrong request id",
            ));
        }
        response
            .as_object_mut()
            .and_then(|object| object.remove("result"))
            .ok_or(ClientError::Protocol("Missing control result"))
    }
}

fn deadline_after(timeout: Duration) -> Result<Instant, ClientError> {
    Instant::now()
        .checked_add(timeout)
        .ok_or(ClientError::Protocol("Invalid client deadline"))
}

fn remaining(deadline: Instant) -> Result<Duration, ClientError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| ClientError::Io(io::ErrorKind::TimedOut.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_backlog_bounds_server_markers_without_merging_subscriptions() {
        let mut backlog = EventBacklog::default();
        for index in 0..=DEPTH {
            backlog.push(
                ServerOverflow {
                    subscription: 1 + (index % 2) as u64,
                    at_millis: index as u64,
                    dropped: 100 + index as u64,
                }
                .into_event(),
            );
        }

        let loss = backlog.pop().unwrap();
        assert_eq!(loss["params"]["source"], "client");
        assert_eq!(loss["params"]["dropped"], 0);
        assert_eq!(loss["params"]["server_markers_dropped"], 1);
        assert!(loss["params"].get("subscription_id").is_none());
        for index in 1..=DEPTH {
            let marker = backlog.pop().unwrap();
            assert_eq!(
                marker["params"]["subscription_id"],
                format!("s{}", 1 + index % 2)
            );
            assert_eq!(marker["params"]["at_millis"], index);
            assert_eq!(marker["params"]["dropped"], 100 + index);
        }
        assert!(backlog.pop().is_none());
    }
}
