use std::fmt;
#[cfg(unix)]
use std::fs;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
#[cfg(windows)]
use std::net::{TcpListener as Listener, TcpStream as Stream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener as Listener, UnixStream as Stream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_channel::{Receiver, Sender};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq as _;
use zeroize::Zeroizing;

use crate::endpoint::{self, Endpoint};
use crate::mcp::{self, Mode};
use crate::protocol::{
    AGENT_START_WAIT_DEFAULT_MS, AGENT_WAIT_DEFAULT_MS, API, ErrorCode, Fault, MAX_CLIENTS,
    MAX_LINE, MAX_RESULT, MAX_WAIT_MS, Method, PROTOCOL_VERSION, Params, ReadMode, TabCreateParams,
    decode_request, schema,
};

const POLL: Duration = Duration::from_millis(100);
const UI_TIMEOUT: Duration = Duration::from_secs(2);
const SPLIT_TIMEOUT: Duration = Duration::from_secs(10);
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
const NOTIFY_INTERVAL: Duration = Duration::from_secs(5);
const AUDIT_LIMIT: u64 = 4 * 1024 * 1024;

type ResultValue = Result<Value, Fault>;

#[derive(Clone)]
pub struct Reply {
    sender: mpsc::SyncSender<ResultValue>,
    active: Arc<AtomicBool>,
    deadline: Instant,
    peer: Option<Arc<McpPeer>>,
}

impl Reply {
    pub fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
            && self
                .peer
                .as_ref()
                .is_none_or(|peer| peer.writes_unchanged())
            && Instant::now() < self.deadline
    }

    pub fn mcp_client(&self) -> Option<(&str, &str)> {
        self.peer
            .as_ref()
            .map(|peer| (peer.id.as_str(), peer.client.as_str()))
    }

    pub fn send(&self, result: ResultValue) {
        if self.active() {
            let _ = self.sender.try_send(result);
        }
    }
}

/// A bounded request dispatched to the desktop. Debug omits terminal input.
#[derive(Clone)]
pub struct Request {
    pub method: Method,
    pub params: Params,
    pub instance: Arc<str>,
    pub reply: Reply,
}

impl fmt::Debug for Request {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControlRequest")
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

pub struct Server {
    events: crate::Events,
    instance: Arc<str>,
    active: Arc<AtomicBool>,
    mcp: Arc<McpGate>,
    requests: Receiver<Request>,
    worker: Option<JoinHandle<()>>,
    address: String,
}

impl Server {
    /// Starts the private listener and publishes its token after binding.
    pub fn start(root: &Path) -> io::Result<Self> {
        let directory = endpoint::directory(root, true)?;
        let lock = endpoint::lock(&directory)?;
        let mut entropy = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut *entropy).map_err(io::Error::other)?;
        let token = URL_SAFE_NO_PAD.encode(entropy.as_slice());
        let instance_id = uuid()?;
        let audit = endpoint::new_audit(&directory, &instance_id)?;
        let mut bound = Bound::open(&directory, lock)?;
        let (transport, path, host, port) = bound.address()?;
        let address = path.as_ref().map_or_else(
            || format!("127.0.0.1:{port}"),
            |path| path.display().to_string(),
        );
        let endpoint = Endpoint {
            transport: transport.to_owned(),
            path,
            host,
            port,
            token,
            pid: std::process::id(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: PROTOCOL_VERSION,
            instance_id,
            started_at_millis: epoch_millis(),
        };
        endpoint.publish(&directory)?;
        bound.published = true;
        let active = Arc::new(AtomicBool::new(true));
        let mcp = Arc::new(McpGate::default());
        let (ui, requests) = async_channel::bounded(MAX_CLIENTS);
        let events = crate::Events::default();
        let instance: Arc<str> = Arc::from(endpoint.instance_id.as_str());
        let context = Arc::new(Context {
            events: events.clone(),
            instance: Arc::from(endpoint.instance_id.as_str()),
            endpoint,
            active: Arc::clone(&active),
            mcp: Arc::clone(&mcp),
            ui,
            started: Instant::now(),
            audit: Mutex::new(audit),
        });
        let worker = thread::Builder::new()
            .name("kortty-control".into())
            .spawn(move || accept(bound, context))?;
        Ok(Self {
            events,
            instance,
            active,
            mcp,
            requests,
            worker: Some(worker),
            address,
        })
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn events(&self) -> crate::Events {
        self.events.clone()
    }

    pub fn instance(&self) -> &str {
        &self.instance
    }

    pub fn requests(&self) -> Receiver<Request> {
        self.requests.clone()
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub fn set_mcp_mode(&self, mode: Mode) {
        self.mcp.set(mode);
    }

    /// Immediately revokes requests, then waits for socket and descriptor cleanup.
    pub fn stop(mut self) {
        self.revoke();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    /// Stops accepting and dispatching requests without waiting for listener cleanup.
    pub fn revoke(&self) {
        self.active.store(false, Ordering::Release);
        self.requests.close();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.revoke();
    }
}

struct Bound {
    listener: Listener,
    directory: PathBuf,
    _lock: File,
    published: bool,
}

impl Bound {
    fn open(directory: &Path, lock: File) -> io::Result<Self> {
        #[cfg(unix)]
        let listener = {
            use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
            let path = directory.join(endpoint::SOCKET);
            match fs::symlink_metadata(&path) {
                Ok(metadata)
                    if metadata.file_type().is_socket()
                        && metadata.uid() == rustix::process::geteuid().as_raw() =>
                {
                    match Stream::connect(&path) {
                        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                            fs::remove_file(&path)?
                        }
                        _ => {
                            return Err(io::Error::new(
                                io::ErrorKind::AddrInUse,
                                "Control socket is already in use",
                            ));
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "Unexpected file at control socket path",
                    ));
                }
                Err(error) => return Err(error),
            }
            Listener::bind(path)?
        };
        #[cfg(windows)]
        let listener = endpoint::bind_loopback_exclusive()?;
        let bound = Self {
            listener,
            directory: directory.to_owned(),
            _lock: lock,
            published: false,
        };
        bound.listener.set_nonblocking(true)?;
        Ok(bound)
    }

    fn address(&self) -> io::Result<(&'static str, Option<PathBuf>, Option<String>, u16)> {
        #[cfg(unix)]
        {
            Ok(("unix", Some(self.directory.join(endpoint::SOCKET)), None, 0))
        }
        #[cfg(windows)]
        {
            Ok((
                "loopback",
                None,
                Some("127.0.0.1".into()),
                self.listener.local_addr()?.port(),
            ))
        }
    }
}

impl Drop for Bound {
    fn drop(&mut self) {
        if self.published {
            endpoint::remove_descriptor(&self.directory);
        }
        #[cfg(unix)]
        let _ = fs::remove_file(self.directory.join(endpoint::SOCKET));
        let _ = self._lock.unlock();
    }
}

struct Context {
    events: crate::Events,
    endpoint: Endpoint,
    instance: Arc<str>,
    active: Arc<AtomicBool>,
    mcp: Arc<McpGate>,
    ui: Sender<Request>,
    started: Instant,
    audit: Mutex<File>,
}

#[derive(Default)]
struct McpGate(AtomicU64);

impl McpGate {
    fn mode(state: u64) -> Mode {
        match state & 3 {
            1 => Mode::PolicyDenied,
            2 => Mode::Read,
            3 => Mode::Write,
            _ => Mode::Off,
        }
    }

    fn set(&self, mode: Mode) {
        let _ = self
            .0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (Self::mode(state) != mode).then(|| (state.wrapping_add(4) & !3) | mode as u64)
            });
    }
}

struct McpPeer {
    id: String,
    client: String,
    gate: Arc<McpGate>,
    state: u64,
    connected: AtomicBool,
}

impl McpPeer {
    fn access(&self) -> Result<bool, Fault> {
        if !self.connected.load(Ordering::Acquire) {
            return Err(disabled());
        }
        let state = self.gate.0.load(Ordering::Acquire);
        let writes = McpGate::mode(state).access()?;
        if state != self.state {
            return Err(Fault::new(
                ErrorCode::Stale,
                "MCP settings changed. Reconnect to local control.",
            ));
        }
        Ok(writes)
    }

    fn writes_unchanged(&self) -> bool {
        self.connected.load(Ordering::Acquire) && self.gate.0.load(Ordering::Acquire) == self.state
    }
}

fn accept(bound: Bound, context: Arc<Context>) {
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    while context.active.load(Ordering::Acquire) {
        workers.retain(|worker| !worker.is_finished());
        match bound.listener.accept() {
            Ok((mut stream, _)) if workers.len() >= MAX_CLIENTS => {
                let _ = stream.set_write_timeout(Some(POLL));
                let _ = respond(
                    &mut stream,
                    Value::Null,
                    Err(Fault::new(
                        ErrorCode::TooMany,
                        "Too many control connections",
                    )),
                );
            }
            Ok((stream, _)) => {
                let context = Arc::clone(&context);
                if let Ok(worker) = thread::Builder::new()
                    .name("kortty-control-client".into())
                    .spawn(move || {
                        let _ = serve(stream, &context);
                    })
                {
                    workers.push(worker);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    context.active.store(false, Ordering::Release);
    context.ui.close();
    for worker in workers {
        let _ = worker.join();
    }
}

fn serve(stream: Stream, context: &Context) -> io::Result<()> {
    // macOS accept inherits the listener's nonblocking mode.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(POLL))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let mut stream = BufReader::new(stream);
    let mut authorized = false;
    let mut subscriptions: Vec<crate::events::Subscription> = Vec::new();
    let mut notified: Option<Instant> = None;
    let deadline = Instant::now() + AUTH_TIMEOUT;
    loop {
        let frame = match read_frame_poll(
            &mut stream,
            &context.active,
            (!authorized).then_some(deadline),
            |reader| {
                for subscription in &subscriptions {
                    subscription.flush(reader.get_mut())?;
                }
                Ok(())
            },
        ) {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                return respond(
                    stream.get_mut(),
                    Value::Null,
                    Err(Fault::new(
                        ErrorCode::TooLarge,
                        "Control request exceeds 1 MiB",
                    )),
                );
            }
            Err(error) => return Err(error),
        };
        let frame = Zeroizing::new(frame);
        let (id, name, mut params) = match decode_request(&frame) {
            Ok(request) => request,
            Err((id, fault)) => {
                respond(stream.get_mut(), id, Err(fault))?;
                if !authorized {
                    return Ok(());
                }
                continue;
            }
        };
        if !authorized {
            let result = authorize(context, &name, &mut params);
            authorized = result.is_ok();
            match result {
                Ok((hello, peer)) => {
                    respond(stream.get_mut(), id, Ok(hello))?;
                    if let Some(peer) = peer {
                        return serve_mcp(stream, context, peer);
                    }
                }
                Err(fault) => return respond(stream.get_mut(), id, Err(fault)),
            }
            continue;
        }
        let result = match Method::from_name(&name) {
            Some(Method::EventsSubscribe) => (|| {
                if !context.active.load(Ordering::Acquire) {
                    return Err(disabled());
                }
                validate_params(Method::EventsSubscribe, &params)?;
                if subscriptions.len() == crate::events::MAX_SUBSCRIPTIONS {
                    return Err(Fault::new(
                        ErrorCode::Busy,
                        "Too many subscriptions on this connection",
                    ));
                }
                audit(context, Method::EventsSubscribe)?;
                context.events.subscribe(&params).map(|next| {
                    let info = next.info();
                    subscriptions.push(next);
                    info
                })
            })(),
            Some(Method::EventsUnsubscribe) => (|| {
                if !context.active.load(Ordering::Acquire) {
                    return Err(disabled());
                }
                validate_params(Method::EventsUnsubscribe, &params)?;
                audit(context, Method::EventsUnsubscribe)?;
                let wanted = params.string("subscription_id")?;
                subscriptions
                    .retain(|subscription| wanted.is_some_and(|id| subscription.id() != id));
                Ok(
                    json!({"subscribed":subscriptions.iter().map(crate::events::Subscription::id).collect::<Vec<_>>()}),
                )
            })(),
            Some(Method::Notify) => (|| {
                if !context.active.load(Ordering::Acquire) {
                    return Err(disabled());
                }
                normalize_notification(&mut params)?;
                let now = Instant::now();
                if let Some(retry) = notified
                    .and_then(|at| NOTIFY_INTERVAL.checked_sub(now.duration_since(at)))
                    .filter(|retry| !retry.is_zero())
                {
                    return Err(Fault::new(
                        ErrorCode::Busy,
                        "Only one notification every five seconds is allowed",
                    )
                    .with(
                        "retry_after_millis",
                        json!(retry.as_nanos().div_ceil(1_000_000)),
                    ));
                }
                let result = dispatch(context, &name, params);
                if result.is_ok() {
                    notified = Some(now);
                }
                result
            })(),
            _ => dispatch(context, &name, params),
        };
        respond(stream.get_mut(), id, result)?;
    }
}

fn normalize_notification(params: &mut Params) -> Result<(), Fault> {
    validate_params(Method::Notify, params)?;
    let title = notification_text(params.required("title")?, 80);
    let body = notification_text(params.required("body")?, 200);
    if title.is_empty() && body.is_empty() {
        return Err(Fault::new(
            ErrorCode::InvalidParams,
            "A notification needs a title or a body",
        )
        .with("param", json!("title"))
        .with("known", json!(["title", "body"])));
    }
    params
        .0
        .insert("title".into(), json!(format!("korTTY · {title}")));
    params.0.insert("body".into(), json!(body));
    Ok(())
}

fn notification_text(text: &str, limit: usize) -> String {
    fn visible(character: char) -> bool {
        !character.is_control()
            && !matches!(
                character,
                '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
                    | '\u{2060}'..='\u{206f}' | '\u{feff}'
            )
    }

    let mut bounded = String::new();
    let mut units = 0;
    for character in text
        .trim_matches(|character: char| character.is_whitespace() || !visible(character))
        .chars()
        .filter(|&character| visible(character))
    {
        units += character.len_utf16();
        if units > limit {
            break;
        }
        bounded.push(character);
    }
    bounded
}

fn serve_mcp(
    mut reader: BufReader<Stream>,
    context: &Context,
    peer: Arc<McpPeer>,
) -> io::Result<()> {
    use std::net::Shutdown;

    let mut writer = reader.get_ref().try_clone()?;
    let (send, receive) = mpsc::sync_channel(1);
    thread::scope(|scope| {
        let peer_read = Arc::clone(&peer);
        let input = thread::Builder::new()
            .name("kortty-control-mcp-reader".into())
            .spawn_scoped(scope, move || {
                while let Ok(Some(frame)) = read_frame(&mut reader, &context.active, None) {
                    if send.try_send(Zeroizing::new(frame)).is_err() {
                        break;
                    }
                }
                peer_read.connected.store(false, Ordering::Release);
            })?;
        let result = (|| {
            while let Ok(frame) = receive.recv() {
                if !peer.connected.load(Ordering::Acquire) {
                    break;
                }
                let (id, method, mut params) = match decode_request(&frame) {
                    Ok(request) => request,
                    Err((id, fault)) => {
                        respond(&mut writer, id, Err(fault))?;
                        continue;
                    }
                };
                let result = (|| {
                    let writes = peer.access()?;
                    if method == "mcp.capabilities" && params.0.is_empty() {
                        return Ok(json!({"mcp_write_tools": writes}));
                    }
                    let method = Method::from_name(&method).ok_or_else(|| {
                        Fault::new(
                            ErrorCode::McpMethod,
                            "This method is outside the MCP allowlist",
                        )
                    })?;
                    mcp::check(method, &mut params, writes)?;
                    if params
                        .string("instance")?
                        .is_some_and(|instance| instance != context.instance.as_ref())
                    {
                        return Err(Fault::new(
                            ErrorCode::Stale,
                            "The control instance has changed",
                        ));
                    }
                    audit(context, method)?;
                    if method == Method::Wait {
                        wait_output_for(context, &params, Some(&peer))
                    } else {
                        let timeout = if method.mutates() {
                            Duration::from_secs(mcp::CONSENT_SECONDS)
                        } else {
                            UI_TIMEOUT
                        };
                        call_ui_for(
                            context,
                            method,
                            params,
                            Instant::now() + timeout,
                            Some(&peer),
                        )
                        .and_then(|result| {
                            if !method.mutates() && result["masked_count"].as_u64().is_none() {
                                return Err(Fault::new(
                                    ErrorCode::Internal,
                                    "Masked MCP output is unavailable",
                                ));
                            }
                            Ok(result)
                        })
                    }
                })();
                if peer.connected.load(Ordering::Acquire) {
                    respond(&mut writer, id, result)?;
                }
            }
            Ok(())
        })();
        peer.connected.store(false, Ordering::Release);
        let _ = writer.shutdown(Shutdown::Both);
        let _ = input.join();
        result
    })
}

fn authorize(
    context: &Context,
    method: &str,
    params: &mut Params,
) -> Result<(Value, Option<Arc<McpPeer>>), Fault> {
    let token = params.0.remove("token");
    let token = match token {
        Some(Value::String(token)) => Zeroizing::new(token),
        _ => Zeroizing::new(String::new()),
    };
    if method != Method::Auth.name()
        || token.len() > 256
        || !bool::from(token.as_bytes().ct_eq(context.endpoint.token.as_bytes()))
    {
        return Err(Fault::new(
            ErrorCode::Unauthorized,
            "Control authentication failed",
        ));
    }
    let peer = match params.string("client_kind")? {
        None | Some("cli") => None,
        Some("mcp") => {
            let state = context.mcp.0.load(Ordering::Acquire);
            McpGate::mode(state).access()?;
            let client = mcp::display_client(params.string("client")?.unwrap_or_default());
            Some(Arc::new(McpPeer {
                id: uuid().map_err(|_| {
                    Fault::new(ErrorCode::Internal, "Could not identify MCP connection")
                })?,
                client,
                gate: Arc::clone(&context.mcp),
                state,
                connected: AtomicBool::new(true),
            }))
        }
        _ => return Err(Fault::param("client_kind")),
    };
    let mut hello = json!({"api": API, "protocol_version": PROTOCOL_VERSION, "app_version": context.endpoint.app_version,
        "pid": context.endpoint.pid, "transport": context.endpoint.transport, "instance_id": context.instance.as_ref(),
        "server_time_millis": epoch_millis(), "ids_survive_restart": false,
        "methods": Method::ALL.iter().map(|method| method.name()).collect::<Vec<_>>(), "capabilities": ["split", "agents", "events", "notifications"]});
    if let Some(peer) = &peer {
        let writes = peer.access()?;
        hello["methods"] = json!(
            Method::ALL
                .iter()
                .filter(|method| mcp::allowed(**method) && (writes || !method.mutates()))
                .map(|method| method.name())
                .collect::<Vec<_>>()
        );
        hello["capabilities"] = json!(["mcp"]);
        hello["mcp_write_tools"] = json!(writes);
    }
    Ok((hello, peer))
}

fn dispatch(context: &Context, name: &str, params: Params) -> ResultValue {
    let method = Method::from_name(name).ok_or_else(|| {
        if matches!(name, "tab.close" | "tab.rename") {
            Fault::new(ErrorCode::Unsupported, "Reserved control method")
        } else {
            Fault::new(ErrorCode::UnknownMethod, "Unknown control method")
        }
    })?;
    if !context.active.load(Ordering::Acquire) {
        return Err(disabled());
    }
    validate_params(method, &params)?;
    if method.mutates() {
        let requested = if method == Method::CreateTab {
            params.nullable_string("instance")?
        } else {
            params.string("instance")?
        };
        if let Some(requested) = requested
            && requested != context.instance.as_ref()
        {
            let fault = Fault::new(ErrorCode::Stale, "The control instance has changed");
            return Err(if method == Method::CreateTab {
                fault
                    .with("instance", json!(context.instance.as_ref()))
                    .with("requested", json!(requested))
            } else {
                fault
            });
        }
    }
    if method == Method::CreateTab {
        TabCreateParams::parse(&params)?;
    }
    let ui_timeout = match method {
        Method::Split => SPLIT_TIMEOUT,
        Method::AgentWait => {
            Duration::from_millis(
                params
                    .number("timeout_ms", AGENT_WAIT_DEFAULT_MS)?
                    .clamp(1, MAX_WAIT_MS),
            ) + UI_TIMEOUT
        }
        Method::AgentPrompt => {
            let wait = params
                .number("timeout_ms", AGENT_WAIT_DEFAULT_MS)?
                .clamp(1, MAX_WAIT_MS);
            if params.string("wait_until")?.is_some() {
                Duration::from_millis(wait) + UI_TIMEOUT
            } else {
                UI_TIMEOUT
            }
        }
        _ => UI_TIMEOUT,
    };
    audit(context, method)?;
    match method {
        Method::Auth => Err(Fault::new(
            ErrorCode::InvalidParams,
            "This connection is already authenticated",
        )),
        Method::Ping => Ok(
            json!({"pong": true, "instance": context.instance.as_ref(), "protocol_version": PROTOCOL_VERSION,
            "app_version": context.endpoint.app_version, "uptime_millis": context.started.elapsed().as_millis()}),
        ),
        Method::Schema => schema(&params),
        Method::AgentStart => start_agent(context, params),
        Method::Wait => wait_output(context, &params),
        method => call_ui(context, method, params, Instant::now() + ui_timeout),
    }
}

fn validate_params(method: Method, params: &Params) -> Result<(), Fault> {
    if params
        .0
        .keys()
        .any(|name| !method.params().contains(&name.as_str()))
    {
        return Err(Fault::new(
            ErrorCode::InvalidParams,
            "Unexpected control parameter",
        ));
    }
    Ok(())
}

fn start_agent(context: &Context, mut params: Params) -> ResultValue {
    let pane = params
        .string("pane")?
        .map(str::trim)
        .filter(|pane| !pane.is_empty())
        .map(str::to_owned);
    let split = params
        .string("split_from")?
        .map(str::trim)
        .filter(|pane| !pane.is_empty())
        .map(str::to_owned);
    if pane.is_some() == split.is_some() {
        return Err(Fault::param("pane"));
    }
    let kind = params.required("kind")?.trim().to_ascii_lowercase();
    let executable = match kind.as_str() {
        "aider" => "aider",
        "amp" => "amp",
        "antigravity" => "agy",
        "claude-code" => "claude",
        "cline" => "cline",
        "codex" => "codex",
        "continue" => "cn",
        "copilot" => "copilot",
        "crush" => "crush",
        "cursor-agent" => "cursor-agent",
        "gemini-cli" => "gemini",
        "goose" => "goose",
        "grok" => "grok",
        "grok-build" => "grok-linux-aarch64",
        "jules" => "jules",
        "kilo" => "kilo",
        "kiro" => "kiro-cli",
        "minimax-code" => "mcode",
        "opencode" => "opencode",
        "qwen-code" => "qwen",
        _ => return Err(Fault::param("kind")),
    };
    let command = params
        .strings("command", 128)?
        .filter(|parts| !parts.is_empty())
        .unwrap_or_else(|| vec![executable]);
    if command
        .iter()
        .any(|part| part.is_empty() || part.contains(['\r', '\n', '\0']))
    {
        return Err(Fault::param("command"));
    }
    let command: Vec<String> = command.into_iter().map(str::to_owned).collect();
    let wait = params.flag("wait", false)?;
    let until = params
        .string("until")?
        .unwrap_or("done")
        .trim()
        .to_ascii_lowercase();
    let until = if until.is_empty() {
        "any".to_owned()
    } else {
        until
    };
    if !matches!(
        until.as_str(),
        "blocked" | "done" | "idle" | "working" | "any"
    ) {
        return Err(Fault::param("until"));
    }
    let prompt = params.string("prompt")?.map(str::to_owned);
    if prompt
        .as_ref()
        .is_some_and(|prompt| prompt.len() > 64 * 1024)
    {
        return Err(Fault::param("prompt"));
    }
    let ready = Duration::from_millis(
        params
            .number("ready_timeout_ms", AGENT_WAIT_DEFAULT_MS)?
            .clamp(1, MAX_WAIT_MS),
    );
    let wait_ms = params
        .number("timeout_ms", AGENT_START_WAIT_DEFAULT_MS)?
        .clamp(1, MAX_WAIT_MS);
    let pane = if let Some(pane) = pane {
        pane
    } else {
        let split=Params(json!({"pane":split,"orientation":params.string("orientation")?.unwrap_or("horizontal"),"focus":false}).as_object().cloned().unwrap_or_default());
        call_ui(
            context,
            Method::Split,
            split,
            Instant::now() + SPLIT_TIMEOUT,
        )?["pane"]["pane_id"]
            .as_str()
            .ok_or_else(|| Fault::new(ErrorCode::Internal, "Split returned no pane"))?
            .to_owned()
    };
    // Resolve the movable selector once before connect, launch and detection waits.
    let mut info = call_ui(
        context,
        Method::Pane,
        Params(
            json!({"pane":pane})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ),
        Instant::now() + UI_TIMEOUT,
    )?;
    let pane = info["pane"]["pane_id"]
        .as_str()
        .ok_or_else(|| Fault::new(ErrorCode::Internal, "Pane returned no id"))?
        .to_owned();
    let stream = info["pane"]["stream_generation"].clone();
    let deadline = Instant::now() + ready;
    while info["pane"]["connected"] != true {
        if Instant::now() >= deadline {
            return Err(timeout("connect"));
        }
        thread::sleep(POLL);
        info = call_ui(
            context,
            Method::Pane,
            Params(
                json!({"pane":pane})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            ),
            Instant::now() + UI_TIMEOUT,
        )?;
        if info["pane"]["stream_generation"] != stream {
            return Err(Fault::new(ErrorCode::Stale, "The terminal was replaced"));
        }
    }
    params.0.insert("pane".into(), json!(pane));
    params.0.insert("command".into(), json!(command));
    params.0.insert("stream_generation".into(), stream.clone());
    call_ui(
        context,
        Method::AgentStart,
        params,
        Instant::now() + UI_TIMEOUT,
    )?;
    let deadline = Instant::now() + ready;
    let mut agent = loop {
        let result = call_ui(
            context,
            Method::Pane,
            Params(
                json!({"pane":pane})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            ),
            Instant::now() + UI_TIMEOUT,
        );
        match result {
            Ok(value) if value["pane"]["stream_generation"] != stream => {
                return Err(Fault::new(ErrorCode::Stale, "The terminal was replaced"));
            }
            Ok(value)
                if value["pane"]["agent"]["detected"] == true
                    && value["pane"]["agent"]["kind"] == kind =>
            {
                break value["pane"]["agent"].clone();
            }
            Ok(_) => {}
            Err(error) if error.code == ErrorCode::AgentNotFound => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(timeout("detect"));
        }
        if !context.active.load(Ordering::Acquire) {
            return Err(disabled());
        }
        thread::sleep(POLL);
    };
    if let Some(prompt) = prompt.filter(|prompt| !prompt.trim().is_empty()) {
        let mut prompt_params = Params(
            json!({"pane":pane,"text":prompt,"timeout_ms":wait_ms,"agent_instance":agent["agent_instance"],"stream_generation":stream})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        );
        if wait {
            prompt_params.0.insert("wait_until".into(), json!(until));
        }
        call_ui(
            context,
            Method::AgentPrompt,
            prompt_params,
            Instant::now() + Duration::from_millis(wait_ms) + UI_TIMEOUT,
        )?;
        agent = call_ui(
            context,
            Method::Agent,
            Params(
                json!({"pane":pane})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            ),
            Instant::now() + UI_TIMEOUT,
        )?["agent"]
            .clone();
    }
    info = call_ui(
        context,
        Method::Pane,
        Params(
            json!({"pane":pane})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ),
        Instant::now() + UI_TIMEOUT,
    )?;
    Ok(json!({"pane":info["pane"],"agent":agent,"command":command}))
}

fn audit(context: &Context, method: Method) -> Result<(), Fault> {
    let mut file = context
        .audit
        .lock()
        .map_err(|_| Fault::new(ErrorCode::Internal, "Control audit is unavailable"))?;
    if file
        .metadata()
        .map_or(true, |metadata| metadata.len() >= AUDIT_LIMIT)
        || writeln!(&mut *file, "{} {}", epoch_millis(), method.name()).is_err()
    {
        return Err(Fault::new(
            ErrorCode::Internal,
            "Could not write control audit",
        ));
    }
    Ok(())
}

fn call_ui(context: &Context, method: Method, params: Params, deadline: Instant) -> ResultValue {
    call_ui_for(context, method, params, deadline, None)
}

fn call_ui_for(
    context: &Context,
    method: Method,
    params: Params,
    deadline: Instant,
    peer: Option<&Arc<McpPeer>>,
) -> ResultValue {
    let expired = || {
        if peer.is_some() && method.mutates() {
            Fault::new(
                ErrorCode::McpWriteDenied,
                "Nobody approved the MCP write in time",
            )
            .with("reason", json!("timeout"))
        } else {
            timeout("ui")
        }
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    let reply = Reply {
        sender,
        active: Arc::clone(&context.active),
        deadline,
        peer: peer.cloned(),
    };
    if !reply.active() {
        return Err(if context.active.load(Ordering::Acquire) {
            expired()
        } else {
            disabled()
        });
    }
    context
        .ui
        .try_send(Request {
            method,
            params,
            instance: Arc::clone(&context.instance),
            reply,
        })
        .map_err(|error| {
            if error.is_closed() {
                disabled()
            } else {
                Fault::new(ErrorCode::Busy, "Control request queue is full")
            }
        })?;
    loop {
        if !context.active.load(Ordering::Acquire) {
            return Err(disabled());
        }
        if let Some(peer) = peer {
            peer.access()?;
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(expired());
        };
        let response = receiver.recv_timeout(remaining.min(POLL));
        if !context.active.load(Ordering::Acquire) {
            return Err(disabled());
        }
        if let Some(peer) = peer {
            peer.access()?;
        }
        if peer.is_some() && method.mutates() && Instant::now() >= deadline {
            return Err(expired());
        }
        match response {
            Ok(_) if Instant::now() >= deadline => return Err(timeout("ui")),
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Fault::new(ErrorCode::Unavailable, "Desktop is unavailable"));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn wait_output(context: &Context, params: &Params) -> ResultValue {
    wait_output_for(context, params, None)
}

fn wait_output_for(context: &Context, params: &Params, peer: Option<&Arc<McpPeer>>) -> ResultValue {
    let pane = params.required("pane")?;
    let mode = ReadMode::parse(params, ReadMode::Recent)?;
    let lines = ReadMode::lines(params)?;
    let pattern = match (params.string("contains")?, params.string("regex")?) {
        (Some(text), None) if !text.is_empty() && text.chars().count() <= 512 => {
            Pattern::Literal(text)
        }
        (None, Some(regex)) if !regex.is_empty() && regex.chars().count() <= 512 => Pattern::Regex(
            regex::RegexBuilder::new(regex)
                .size_limit(256 * 1024)
                .build()
                .map_err(|_| {
                    Fault::new(
                        ErrorCode::InvalidRegex,
                        "Invalid or unsupported regular expression",
                    )
                })?,
        ),
        _ => return Err(Fault::param("contains")),
    };
    let started = Instant::now();
    let wait = params.number("timeout_ms", 30_000)?.clamp(1, MAX_WAIT_MS);
    let interval = Duration::from_millis(params.number("poll_ms", 150)?.clamp(50, 60_000));
    let deadline = started + Duration::from_millis(wait);
    let mut selector = pane.to_owned();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(timeout("wait_output"));
        }
        let read = Params(
            json!({"pane": selector, "mode": mode.name(), "lines": lines})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        );
        let result = call_ui_for(
            context,
            Method::Read,
            read,
            deadline.min(Instant::now() + UI_TIMEOUT),
            peer,
        )?;
        let masked_count = result.get("masked_count").cloned();
        if peer.is_some() && masked_count.as_ref().and_then(Value::as_u64).is_none() {
            return Err(Fault::new(
                ErrorCode::Internal,
                "Masked terminal output is unavailable",
            ));
        }
        let screen: crate::protocol::Screen = serde_json::from_value(result)
            .map_err(|_| Fault::new(ErrorCode::Internal, "Invalid screen response"))?;
        // Resolve a movable selector just once so a wait cannot follow the user's changing focus.
        selector.clone_from(&screen.pane_id);
        for (index, line) in screen.lines.iter().enumerate() {
            if let Some(matched) = pattern.find(line) {
                let mut value = json!({"pane_id": screen.pane_id, "matched": true, "match": matched, "line": line,
                    "line_index": index, "waited_millis": started.elapsed().as_millis(), "screen": screen});
                if let Some(masked_count) = masked_count {
                    value["masked_count"] = masked_count;
                }
                return Ok(value);
            }
        }
        let wake = (Instant::now() + interval).min(deadline);
        while let Some(remaining) = wake.checked_duration_since(Instant::now()) {
            if !context.active.load(Ordering::Acquire) {
                return Err(disabled());
            }
            if let Some(peer) = peer {
                peer.access()?;
            }
            thread::sleep(remaining.min(POLL));
        }
    }
}

enum Pattern<'a> {
    Literal(&'a str),
    Regex(regex::Regex),
}

impl Pattern<'_> {
    fn find<'a>(&self, line: &'a str) -> Option<&'a str> {
        match self {
            Self::Literal(text) => line
                .find(text)
                .map(|offset| &line[offset..offset + text.len()]),
            Self::Regex(regex) => regex.find(line).map(|hit| hit.as_str()),
        }
    }
}

fn timeout(stage: &'static str) -> Fault {
    Fault::new(ErrorCode::Timeout, "Control operation timed out").with("stage", json!(stage))
}
fn disabled() -> Fault {
    Fault::new(ErrorCode::Disabled, "Control API is off")
}

#[derive(Serialize)]
struct Response<'a> {
    jsonrpc: &'static str,
    id: &'a Value,
    #[serde(flatten)]
    outcome: &'a Outcome,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum Outcome {
    Result(Value),
    Error(Value),
}

#[derive(Default)]
struct ResponseBuffer(Vec<u8>);

impl Write for ResponseBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_RESULT - self.0.len() {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        self.0.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl ResponseBuffer {
    fn serialize(&mut self, id: &Value, outcome: &Outcome) -> io::Result<bool> {
        self.0.clear();
        let frame = Response {
            jsonrpc: "2.0",
            id,
            outcome,
        };
        match serde_json::to_writer(&mut *self, &frame) {
            Ok(()) => Ok(true),
            Err(error) if error.io_error_kind() == Some(io::ErrorKind::FileTooLarge) => Ok(false),
            Err(error) => Err(io::Error::other(error)),
        }
    }
}

fn respond(stream: &mut impl Write, id: Value, result: ResultValue) -> io::Result<()> {
    let outcome = match result {
        Ok(result) => Outcome::Result(result),
        Err(fault) => Outcome::Error(fault.wire()),
    };
    let mut buffer = ResponseBuffer::default();
    if !buffer.serialize(&id, &outcome)? {
        let fault = Fault::new(
            ErrorCode::TooLarge,
            "Control response exceeds its size limit",
        );
        let error = Outcome::Error(fault.wire());
        if !buffer.serialize(&id, &error)? && !buffer.serialize(&Value::Null, &error)? {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
    }
    stream.write_all(&buffer.0)?;
    stream.write_all(b"\n")
}

pub(crate) fn read_frame(
    reader: &mut impl BufRead,
    active: &AtomicBool,
    deadline: Option<Instant>,
) -> io::Result<Option<Vec<u8>>> {
    read_frame_poll(reader, active, deadline, |_| Ok(()))
}

fn read_frame_poll<R: BufRead>(
    reader: &mut R,
    active: &AtomicBool,
    deadline: Option<Instant>,
    mut poll: impl FnMut(&mut R) -> io::Result<()>,
) -> io::Result<Option<Vec<u8>>> {
    let may_read = || {
        if !active.load(Ordering::Acquire) {
            return Ok(false);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        Ok(true)
    };
    let mut bytes = Vec::new();
    loop {
        if !may_read()? {
            return Ok(None);
        }
        poll(reader)?;
        let buffer = reader.fill_buf();
        if !may_read()? {
            return Ok(None);
        }
        let buffer = match buffer {
            Ok(buffer) => buffer,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        if buffer.is_empty() {
            return Ok(None);
        }
        let newline = buffer.iter().position(|&byte| byte == b'\n');
        let count = newline.unwrap_or(buffer.len());
        if count > MAX_LINE - bytes.len() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        bytes.extend_from_slice(&buffer[..count]);
        reader.consume(count + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(Some(bytes));
        }
    }
}

pub(crate) fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_millis() as u64)
}

fn uuid() -> io::Result<String> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let n = u128::from_be_bytes(bytes);
    Ok(format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        n >> 96,
        (n >> 80) & 0xffff,
        (n >> 64) & 0xffff,
        (n >> 48) & 0xffff,
        n & 0xffff_ffff_ffff
    ))
}

#[cfg(test)]
mod tests;
