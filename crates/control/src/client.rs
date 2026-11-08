use std::fmt;
use std::io::{self, BufReader, Write};
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

use crate::endpoint::{self, Endpoint};
use crate::protocol::{API, MAX_LINE, PROTOCOL_VERSION};
use crate::server::read_frame;

#[derive(Debug)]
pub enum ClientError {
    Connect(io::Error),
    Io(io::Error),
    Protocol(&'static str),
    Server {
        code: String,
        message: String,
        exit: u8,
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
    stream: BufReader<Stream>,
    deadline: Instant,
    next_id: u64,
    instance: String,
    #[cfg(target_os = "linux")]
    endpoint_pid: u32,
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
    client: &'static str,
}

impl Client {
    pub fn connect(root: &Path, timeout: Duration) -> Result<Self, ClientError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(ClientError::Protocol("Invalid client deadline"))?;
        let directory = endpoint::directory(root, false).map_err(ClientError::Connect)?;
        let endpoint = Endpoint::read(&directory).map_err(ClientError::Connect)?;
        #[cfg(unix)]
        let stream = Stream::connect(
            endpoint
                .path
                .as_ref()
                .ok_or(ClientError::Protocol("Missing local socket path"))?,
        )
        .map_err(ClientError::Connect)?;
        #[cfg(windows)]
        let stream = Stream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port)),
            timeout,
        )
        .map_err(ClientError::Connect)?;
        stream
            .set_read_timeout(Some(Duration::from_millis(100).min(timeout)))
            .map_err(ClientError::Connect)?;
        let mut client = Self {
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
                client: "kortty-cli",
            },
        )?;
        if hello.get("api").and_then(Value::as_str) != Some(API)
            || hello.get("protocol_version").and_then(Value::as_u64)
                != Some(u64::from(PROTOCOL_VERSION))
            || hello.get("instance_id").and_then(Value::as_str) != Some(client.instance.as_str())
        {
            return Err(ClientError::Protocol(
                "The listener does not match the local control endpoint",
            ));
        }
        Ok(client)
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

    fn call_encoded(
        &mut self,
        method: &str,
        params: &impl Serialize,
    ) -> Result<Value, ClientError> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| ClientError::Io(io::ErrorKind::TimedOut.into()))?;
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
        let bytes = read_frame(
            &mut self.stream,
            &AtomicBool::new(true),
            Some(self.deadline),
        )
        .map_err(ClientError::Io)?
        .ok_or(ClientError::Protocol("Control connection closed"))?;
        let mut response: Value = serde_json::from_slice(&bytes)
            .map_err(|_| ClientError::Protocol("Invalid control response"))?;
        if response.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(ClientError::Protocol("Invalid control response version"));
        }
        if let Some(error) = response.get("error") {
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
