use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::{CONSENT_SECONDS, TOOLS, Tool, display_client};
use crate::client::McpSocket;
use crate::endpoint::Endpoint;
use crate::server::read_frame;
use crate::{Client, ClientError, ErrorCode, Fault, Method, Params};

const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// Serve MCP newline-delimited JSON-RPC on standard input and output.
pub fn serve_stdio(root: &Path, timeout_ms: Option<u64>) -> io::Result<()> {
    serve(root, io::stdin().lock(), io::stdout(), timeout_ms)
}

struct Call {
    id: Value,
    socket: Option<McpSocket>,
    cancelled: bool,
}

impl Call {
    fn cancel(&mut self) {
        self.cancelled = true;
        if let Some(socket) = self.socket.take() {
            socket.close();
        }
    }
}

enum Operation {
    List,
    Tool(Tool, Params),
}

impl Operation {
    fn discovers_unscoped(&self) -> bool {
        matches!(self, Self::Tool(tool, params) if matches!(tool.method, Method::Panes | Method::Tabs | Method::Agents)
            && !params.0.contains_key("window") && !params.0.contains_key("tab"))
    }

    fn discovers_panes(&self) -> bool {
        self.discovers_unscoped()
            && matches!(self, Self::Tool(tool, _) if tool.method == Method::Panes)
    }

    fn targets_selection(&self) -> bool {
        matches!(self, Self::Tool(tool, params) if tool.method.mutates()
            || matches!(tool.method, Method::Read | Method::Wait)
            || params.0.contains_key("window") || params.0.contains_key("tab"))
    }

    fn can_retry(&self, error: &ClientError) -> bool {
        let lost = lost_channel(error);
        match self {
            Self::List => {
                lost || matches!(error, ClientError::Server { code, .. } if code == "stale_instance")
            }
            Self::Tool(..) => lost && self.discovers_unscoped(),
        }
    }
}

#[derive(Default)]
struct Bridge {
    client: Option<Client>,
    instance: Option<String>,
    pane_list_required: bool,
}

fn serve(
    root: &Path,
    mut input: impl BufRead,
    output: impl Write + Send,
    timeout_ms: Option<u64>,
) -> io::Result<()> {
    let output = Mutex::new(output);
    let bridge = Mutex::new(Bridge::default());
    let pending = Mutex::new(None::<Call>);
    let mut name = None;
    let mut initialized = false;
    thread::scope(|scope| {
        let mut worker = None;
        let result = (|| {
            loop {
                let frame = match read_frame(&mut input, &AtomicBool::new(true), None) {
                    Ok(Some(frame)) => Zeroizing::new(frame),
                    Ok(None) => return Ok(()),
                    Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                        emit(
                            &output,
                            &error_response(Value::Null, -32600, "MCP request exceeds 1 MiB"),
                        )?;
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                };
                let Ok(message) = serde_json::from_slice::<Value>(&frame) else {
                    emit(
                        &output,
                        &error_response(Value::Null, -32700, "Invalid JSON"),
                    )?;
                    continue;
                };
                let id = message.get("id");
                let Some(method) = message
                    .get("method")
                    .and_then(Value::as_str)
                    .filter(|_| message["jsonrpc"] == "2.0")
                else {
                    emit(
                        &output,
                        &error_response(Value::Null, -32600, "Invalid JSON-RPC request"),
                    )?;
                    continue;
                };
                let params = &message["params"];
                let Some(id) = id else {
                    if method == "notifications/initialized" && name.is_some() {
                        initialized = true;
                    } else if method == "notifications/cancelled" {
                        let cancelled = {
                            let mut pending = pending.lock().expect("MCP request state");
                            pending
                                .as_mut()
                                .filter(|call| params.get("requestId") == Some(&call.id))
                                .is_some_and(|call| {
                                    call.cancel();
                                    true
                                })
                        };
                        if cancelled && let Some(worker) = worker.take() {
                            finish(worker)?;
                        }
                    }
                    continue;
                };
                if !id.as_str().is_some_and(|id| id.len() <= 256) && !id.is_i64() && !id.is_u64() {
                    emit(
                        &output,
                        &error_response(Value::Null, -32600, "Invalid MCP request ID"),
                    )?;
                    continue;
                }
                let id = id.clone();
                if !params.is_null() && !params.is_object() {
                    emit(
                        &output,
                        &error_response(id, -32602, "MCP parameters must be an object"),
                    )?;
                    continue;
                }
                let operation = match method {
                    "initialize" if name.is_none() => {
                        let version = params["protocolVersion"]
                            .as_str()
                            .filter(|version| VERSIONS.contains(version))
                            .unwrap_or(VERSIONS[0]);
                        name = Some(display_client(
                            params["clientInfo"]["name"].as_str().unwrap_or_default(),
                        ));
                        emit(
                            &output,
                            &response(
                                id,
                                json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"kortty","title":"kortty","version":env!("CARGO_PKG_VERSION")}}),
                            ),
                        )?;
                        continue;
                    }
                    "ping" => {
                        emit(&output, &response(id, json!({})))?;
                        continue;
                    }
                    _ if !initialized => {
                        emit(
                            &output,
                            &error_response(id, -32000, "MCP initialization is incomplete"),
                        )?;
                        continue;
                    }
                    "tools/list" => Operation::List,
                    "tools/call" => {
                        let Some(tool) = TOOLS
                            .iter()
                            .copied()
                            .find(|tool| params["name"] == tool.name)
                        else {
                            emit(&output, &error_response(id, -32602, "Unknown MCP tool"))?;
                            continue;
                        };
                        let arguments = match params.get("arguments") {
                            Some(Value::Object(arguments)) => Params(arguments.clone()),
                            None => Params::default(),
                            _ => {
                                emit(
                                    &output,
                                    &error_response(id, -32602, "Tool arguments must be an object"),
                                )?;
                                continue;
                            }
                        };
                        if let Err(fault) = tool.validate(&arguments, false) {
                            emit(
                                &output,
                                &json!({"jsonrpc":"2.0","id":id,"error":fault.wire()}),
                            )?;
                            continue;
                        }
                        Operation::Tool(tool, arguments)
                    }
                    _ => {
                        emit(&output, &error_response(id, -32601, "Method not found"))?;
                        continue;
                    }
                };
                {
                    let mut call = pending.lock().expect("MCP request state");
                    if call.is_some() {
                        emit(
                            &output,
                            &error_response(
                                id,
                                -32000,
                                "An MCP tool request is already in progress",
                            ),
                        )?;
                        continue;
                    }
                    *call = Some(Call {
                        id: id.clone(),
                        socket: None,
                        cancelled: false,
                    });
                }
                if let Some(previous) = worker.take() {
                    finish(previous)?;
                }
                let name = name.as_deref().expect("initialized MCP client").to_owned();
                let (output, bridge, pending) = (&output, &bridge, &pending);
                worker = Some(scope.spawn(move || {
                    let discovers = operation.discovers_panes();
                    let result = execute(root, &name, operation, timeout_ms, bridge, pending);
                    let reply = pending
                        .lock()
                        .expect("MCP request state")
                        .take()
                        .is_some_and(|call| !call.cancelled);
                    if reply {
                        let selected = discovers && result["isError"] == false;
                        emit(output, &response(id, result))?;
                        // Cancelled or undelivered lists cannot refresh the client's selection.
                        if selected {
                            bridge.lock().expect("MCP connection").pane_list_required = false;
                        }
                    }
                    Ok(())
                }));
            }
        })();
        if let Some(call) = pending.lock().expect("MCP request state").as_mut() {
            call.cancel();
        }
        let completed = worker.take().map_or(Ok(()), finish);
        result.and(completed)
    })
}

fn finish(worker: thread::ScopedJoinHandle<'_, io::Result<()>>) -> io::Result<()> {
    worker
        .join()
        .map_err(|_| io::Error::other("MCP request worker stopped"))?
}

fn execute(
    root: &Path,
    name: &str,
    operation: Operation,
    timeout_ms: Option<u64>,
    bridge: &Mutex<Bridge>,
    pending: &Mutex<Option<Call>>,
) -> Value {
    let timeout = timeout_ms.unwrap_or_else(|| match &operation {
        Operation::Tool(tool, args) if tool.method == Method::Wait => {
            args.number("timeout_ms", 30_000).unwrap_or(30_000) + 5_000
        }
        Operation::Tool(tool, _) if tool.method.mutates() => CONSENT_SECONDS * 1_000 + 5_000,
        _ => 5_000,
    });
    let deadline = Instant::now().checked_add(Duration::from_millis(timeout));
    let mut bridge = bridge.lock().expect("MCP connection");
    let run = |bridge: &mut Bridge| -> Result<Value, ClientError> {
        let deadline = deadline.ok_or(ClientError::Protocol("Invalid client deadline"))?;
        if pending
            .lock()
            .expect("MCP request state")
            .as_ref()
            .is_some_and(|call| call.cancelled)
        {
            return Err(ClientError::Protocol("MCP request was cancelled"));
        }
        // Reconnect before sending when the profile has published a new listener.
        if bridge.client.as_ref().is_some_and(|client| {
            Endpoint::read_profile(root)
                .is_ok_and(|endpoint| endpoint.instance_id != client.instance())
        }) {
            bridge.client.take();
        }
        if bridge.client.is_none() {
            let connect_by = deadline.min(Instant::now() + Duration::from_secs(5));
            let client = Client::connect_mcp_before(root, connect_by, name)?;
            if bridge
                .instance
                .as_deref()
                .is_some_and(|instance| instance != client.instance())
            {
                bridge.pane_list_required = true;
            }
            bridge.instance = Some(client.instance().to_owned());
            bridge.client = Some(client);
        }
        if bridge.pane_list_required && operation.targets_selection() {
            return Err(fault_error(
                ErrorCode::Stale,
                "The control instance changed. Call pane_list again before selecting a pane.",
            ));
        }
        let client = bridge.client.as_mut().expect("connected MCP client");
        client.set_deadline(deadline)?;
        {
            let mut call = pending.lock().expect("MCP request state");
            let call = call.as_mut().expect("running MCP request");
            if call.cancelled {
                return Err(ClientError::Protocol("MCP request was cancelled"));
            }
            call.socket = Some(client.mcp_socket()?);
        }
        match &operation {
            Operation::List => client.call("mcp.capabilities", &json!({})),
            Operation::Tool(tool, args) => {
                let mut args = args.0.clone();
                if tool.method.mutates() {
                    args.insert("instance".into(), json!(client.instance()));
                }
                client.call(tool.method.name(), &Value::Object(args))
            }
        }
    };
    let mut result = run(&mut bridge);
    let local_selection_refusal = bridge.pane_list_required && operation.targets_selection();
    if !local_selection_refusal && result.as_ref().is_err_and(reset_connection) {
        let established = bridge.client.take().is_some();
        if established
            && result
                .as_ref()
                .is_err_and(|error| operation.can_retry(error))
            && deadline.is_some_and(|deadline| Instant::now() < deadline)
        {
            result = run(&mut bridge);
            if result.as_ref().is_err_and(reset_connection) {
                bridge.client.take();
            }
        }
    }
    match operation {
        Operation::List => {
            let writes = result
                .ok()
                .is_some_and(|value| value["mcp_write_tools"] == true);
            let tools: Vec<_> = TOOLS
                .iter()
                .copied()
                .filter(|tool| writes || !tool.method.mutates())
                .map(Tool::schema)
                .collect();
            json!({"tools":tools})
        }
        Operation::Tool(_, _) => match result {
            Ok(value) => tool_result(value, false),
            Err(ClientError::Server { rpc, .. }) => tool_result(json!({"error":rpc}), true),
            Err(error) => tool_result(json!({"error":{"message":error.to_string()}}), true),
        },
    }
}

fn fault_error(code: ErrorCode, message: &'static str) -> ClientError {
    let rpc = Fault::new(code, message).wire();
    let exit = rpc["data"]["exit"]
        .as_u64()
        .and_then(|value| u8::try_from(value).ok())
        .expect("control fault exit code");
    ClientError::Server {
        code: code.wire().into(),
        message: message.into(),
        exit,
        rpc,
    }
}

fn lost_channel(error: &ClientError) -> bool {
    match error {
        ClientError::Io(error) => matches!(
            error.kind(),
            io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::NotConnected
                | io::ErrorKind::UnexpectedEof
        ),
        ClientError::Protocol("Control connection closed") => true,
        _ => false,
    }
}

fn reset_connection(error: &ClientError) -> bool {
    match error {
        ClientError::Server { code, .. } => matches!(
            code.as_str(),
            "stale_instance" | "mcp_server_disabled" | "control_api_disabled" | "blocked_by_policy"
        ),
        ClientError::Connect(_) => false,
        _ => true,
    }
}

fn tool_result(value: Value, error: bool) -> Value {
    json!({"content":[{"type":"text","text":value.to_string()}],"isError":error})
}

fn response(id: Value, value: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":value})
}

fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

fn emit(output: &Mutex<impl Write>, value: &Value) -> io::Result<()> {
    let mut output = output.lock().expect("MCP output");
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(method: Method, arguments: Value) -> Operation {
        Operation::Tool(
            *TOOLS.iter().find(|tool| tool.method == method).unwrap(),
            Params(arguments.as_object().unwrap().clone()),
        )
    }

    #[test]
    fn only_unscoped_discovery_and_catalogue_replay_after_channel_loss() {
        let lost = ClientError::Protocol("Control connection closed");
        let reset = ClientError::Io(io::ErrorKind::ConnectionReset.into());
        let stale = fault_error(ErrorCode::Stale, "Retired MCP authorization channel");
        let listing = tool(Method::Panes, json!({"local_shell_only":true}));
        assert!(listing.discovers_panes());
        assert!(!listing.targets_selection());
        assert!(listing.can_retry(&lost));
        assert!(listing.can_retry(&reset));
        assert!(!listing.can_retry(&stale));
        assert!(Operation::List.can_retry(&lost));
        assert!(Operation::List.can_retry(&stale));
        for operation in [
            tool(Method::Tabs, json!({})),
            tool(Method::Agents, json!({"kind":"codex","state":"working"})),
        ] {
            assert!(!operation.targets_selection());
            assert!(!operation.discovers_panes());
            assert!(operation.can_retry(&lost));
            assert!(!operation.can_retry(&stale));
        }

        for operation in [
            tool(Method::Panes, json!({"window":"w1"})),
            tool(Method::Panes, json!({"tab":"t1"})),
            tool(Method::Tabs, json!({"window":"w1"})),
            tool(Method::Agents, json!({"tab":"t1"})),
            tool(Method::Read, json!({"pane":"p1"})),
            tool(Method::Wait, json!({"pane":"@focused"})),
            tool(Method::Run, json!({"pane":"p1","command":"echo fixture"})),
        ] {
            assert!(operation.targets_selection());
            assert!(!operation.discovers_panes());
            assert!(!operation.can_retry(&lost));
        }
        for error in [
            ClientError::Connect(io::ErrorKind::ConnectionRefused.into()),
            ClientError::Io(io::ErrorKind::TimedOut.into()),
            ClientError::Protocol("Invalid control response"),
            fault_error(ErrorCode::Policy, "Managed policy blocks MCP"),
            fault_error(ErrorCode::McpDisabled, "MCP is disabled"),
        ] {
            assert!(!listing.can_retry(&error));
            assert!(!Operation::List.can_retry(&error));
        }
    }
}
