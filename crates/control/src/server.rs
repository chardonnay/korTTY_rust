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
use std::sync::atomic::{AtomicBool, Ordering};
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
use crate::protocol::{
    API, ErrorCode, Fault, MAX_CLIENTS, MAX_LINE, MAX_RESULT, MAX_WAIT_MS, Method,
    PROTOCOL_VERSION, Params, ReadMode, decode_request, schema,
};

const POLL: Duration = Duration::from_millis(100);
const UI_TIMEOUT: Duration = Duration::from_secs(2);
const SPLIT_TIMEOUT: Duration = Duration::from_secs(10);
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
const AUDIT_LIMIT: u64 = 4 * 1024 * 1024;

type ResultValue = Result<Value, Fault>;

#[derive(Clone)]
pub struct Reply {
    sender: mpsc::SyncSender<ResultValue>,
    active: Arc<AtomicBool>,
    deadline: Instant,
}

impl Reply {
    pub fn active(&self) -> bool {
        self.active.load(Ordering::Acquire) && Instant::now() < self.deadline
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
    active: Arc<AtomicBool>,
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
        let (ui, requests) = async_channel::bounded(MAX_CLIENTS);
        let context = Arc::new(Context {
            instance: Arc::from(endpoint.instance_id.as_str()),
            endpoint,
            active: Arc::clone(&active),
            ui,
            started: Instant::now(),
            audit: Mutex::new(audit),
        });
        let worker = thread::Builder::new()
            .name("kortty-control".into())
            .spawn(move || accept(bound, context))?;
        Ok(Self {
            active,
            requests,
            worker: Some(worker),
            address,
        })
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn requests(&self) -> Receiver<Request> {
        self.requests.clone()
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
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
    endpoint: Endpoint,
    instance: Arc<str>,
    active: Arc<AtomicBool>,
    ui: Sender<Request>,
    started: Instant,
    audit: Mutex<File>,
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
    stream.set_read_timeout(Some(POLL))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let mut stream = BufReader::new(stream);
    let mut authorized = false;
    let deadline = Instant::now() + AUTH_TIMEOUT;
    loop {
        let frame = match read_frame(
            &mut stream,
            &context.active,
            (!authorized).then_some(deadline),
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
            respond(stream.get_mut(), id, result)?;
            if !authorized {
                return Ok(());
            }
            continue;
        }
        respond(stream.get_mut(), id, dispatch(context, &name, params))?;
    }
}

fn authorize(context: &Context, method: &str, params: &mut Params) -> ResultValue {
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
    match params.string("client_kind")? {
        None | Some("cli") => {}
        Some("mcp") => {
            return Err(Fault::new(
                ErrorCode::McpDisabled,
                "MCP control is disabled",
            ));
        }
        _ => return Err(Fault::param("client_kind")),
    }
    Ok(
        json!({"api": API, "protocol_version": PROTOCOL_VERSION, "app_version": context.endpoint.app_version,
        "pid": context.endpoint.pid, "transport": context.endpoint.transport, "instance_id": context.instance.as_ref(),
        "server_time_millis": epoch_millis(), "ids_survive_restart": false,
        "methods": Method::ALL.iter().map(|method| method.name()).collect::<Vec<_>>(), "capabilities": ["split"]}),
    )
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
    if method.mutates()
        && params
            .string("instance")?
            .is_some_and(|instance| instance != context.instance.as_ref())
    {
        return Err(Fault::new(
            ErrorCode::Stale,
            "The control instance has changed",
        ));
    }
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
        Method::Wait => wait_output(context, &params),
        method => call_ui(
            context,
            method,
            params,
            Instant::now()
                + if method == Method::Split {
                    SPLIT_TIMEOUT
                } else {
                    UI_TIMEOUT
                },
        ),
    }
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
    let (sender, receiver) = mpsc::sync_channel(1);
    let reply = Reply {
        sender,
        active: Arc::clone(&context.active),
        deadline,
    };
    if !reply.active() {
        return Err(if context.active.load(Ordering::Acquire) {
            timeout("ui")
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
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(timeout("ui"));
        };
        match receiver.recv_timeout(remaining.min(POLL)) {
            Ok(_) if !context.active.load(Ordering::Acquire) => return Err(disabled()),
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
        let result = call_ui(
            context,
            Method::Read,
            read,
            deadline.min(Instant::now() + UI_TIMEOUT),
        )?;
        let screen: crate::protocol::Screen = serde_json::from_value(result)
            .map_err(|_| Fault::new(ErrorCode::Internal, "Invalid screen response"))?;
        // Resolve a movable selector just once so a wait cannot follow the user's changing focus.
        selector.clone_from(&screen.pane_id);
        for (index, line) in screen.lines.iter().enumerate() {
            if let Some(matched) = pattern.find(line) {
                return Ok(
                    json!({"pane_id": screen.pane_id, "matched": true, "match": matched, "line": line,
                    "line_index": index, "waited_millis": started.elapsed().as_millis(), "screen": screen}),
                );
            }
        }
        let wake = (Instant::now() + interval).min(deadline);
        while let Some(remaining) = wake.checked_duration_since(Instant::now()) {
            if !context.active.load(Ordering::Acquire) {
                return Err(disabled());
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

fn epoch_millis() -> u64 {
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
