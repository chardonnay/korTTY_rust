use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const API: &str = "kortty-control";
pub const PROTOCOL_VERSION: u8 = 1;
pub(crate) const MAX_LINE: usize = 1_048_576;
pub(crate) const MAX_RESULT: usize = 524_288;
pub(crate) const MAX_CLIENTS: usize = 8;
pub(crate) const MAX_READ_LINES: usize = 10_000;
pub(crate) const MAX_RESOLVE_PIDS: usize = 32;
pub(crate) const MAX_WAIT_MS: u64 = 600_000;
pub const AGENT_WAIT_DEFAULT_MS: u64 = 60_000;
pub const AGENT_START_WAIT_DEFAULT_MS: u64 = 300_000;
const MAX_ID_BYTES: usize = 256;
const MAX_KEYS: usize = 32_768;

macro_rules! errors {
    ($($variant:ident, $wire:literal, $number:literal, $exit:literal, $retry:literal;)+) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum ErrorCode { $($variant),+ }
        impl ErrorCode {
            const ALL: &[Self] = &[$(Self::$variant),+];
            pub const fn wire(self) -> &'static str { match self { $(Self::$variant => $wire),+ } }
            const fn number(self) -> i32 { match self { $(Self::$variant => $number),+ } }
            const fn exit(self) -> u8 { match self { $(Self::$variant => $exit),+ } }
            const fn retryable(self) -> bool { match self { $(Self::$variant => $retry),+ } }
        }
    }
}

errors! {
    Parse, "parse_error", -32700, 2, false;
    InvalidRequest, "invalid_request", -32600, 2, false;
    UnknownMethod, "unknown_method", -32601, 2, false;
    InvalidParams, "invalid_params", -32602, 2, false;
    Internal, "internal_error", -32603, 1, false;
    TooLarge, "message_too_large", -32005, 2, false;
    Unauthorized, "unauthorized", -32001, 3, false;
    Disabled, "control_api_disabled", -32002, 3, false;
    Policy, "blocked_by_policy", -32003, 3, false;
    TooMany, "too_many_connections", -32006, 3, true;
    McpDisabled, "mcp_server_disabled", -32007, 3, false;
    McpMethod, "method_not_allowed_for_mcp", -32008, 3, false;
    McpWriteDenied, "mcp_write_denied", -32009, 3, false;
    WindowNotFound, "window_not_found", -32010, 1, false;
    TabNotFound, "tab_not_found", -32011, 1, false;
    PaneNotFound, "pane_not_found", -32012, 1, false;
    AgentNotFound, "agent_not_found", -32013, 1, false;
    AgentBlocked, "agent_blocked", -32023, 1, false;
    Ambiguous, "ambiguous_pane", -32014, 2, false;
    Stale, "stale_instance", -32015, 1, false;
    UnknownKey, "unknown_key", -32016, 2, false;
    InvalidRegex, "invalid_regex", -32017, 2, false;
    McpWriteBlocked, "mcp_write_refused", -32018, 1, false;
    ConnectionNotFound, "connection_not_found", -32019, 1, false;
    NotConnected, "not_connected", -32020, 1, true;
    WriteFailed, "write_failed", -32021, 1, true;
    EmptyInput, "empty_input", -32022, 2, false;
    HostShortcutConflict, "host_shortcut_conflict", -32024, 2, false;
    Busy, "busy", -32025, 1, true;
    AmbiguousConnection, "ambiguous_connection", -32026, 2, false;
    LastPane, "last_pane", -32030, 1, false;
    Unsupported, "unsupported", -32031, 1, false;
    SplitFailed, "split_failed", -32032, 1, true;
    Timeout, "timeout", -32040, 4, true;
    Unavailable, "ui_unavailable", -32041, 1, true;
}

#[derive(Clone, Debug)]
pub struct Fault {
    pub code: ErrorCode,
    message: &'static str,
    extra: Map<String, Value>,
}

impl Fault {
    pub fn new(code: ErrorCode, message: &'static str) -> Self {
        Self {
            code,
            message,
            extra: Map::new(),
        }
    }

    pub fn param(name: &'static str) -> Self {
        Self::new(ErrorCode::InvalidParams, "Invalid or missing parameter")
            .with("param", json!(name))
    }

    pub fn connection_not_found(connection: &str) -> Self {
        Self::new(
            ErrorCode::ConnectionNotFound,
            "No saved connection has that id or name",
        )
        .with("connection", json!(connection))
    }

    pub fn ambiguous_connection(connection: &str, candidates: Vec<String>) -> Self {
        Self::new(
            ErrorCode::AmbiguousConnection,
            "Multiple connections match. Pass the connection id.",
        )
        .with("connection", json!(connection))
        .with("candidates", json!(candidates))
    }

    pub fn with(mut self, key: &str, value: Value) -> Self {
        self.extra.insert(key.to_owned(), value);
        self
    }

    pub(crate) fn wire(mut self) -> Value {
        self.extra.insert("code".into(), json!(self.code.wire()));
        self.extra
            .insert("retryable".into(), json!(self.code.retryable()));
        self.extra.insert("exit".into(), json!(self.code.exit()));
        json!({"code": self.code.number(), "message": self.message, "data": self.extra})
    }
}

macro_rules! methods {
    ($($variant:ident, $name:literal, $cli:literal, $mutates:literal, $result:literal, [$($param:literal),*];)+) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum Method { $($variant),+ }
        impl Method {
            pub const ALL: &[Self] = &[$(Self::$variant),+];
            pub const fn name(self) -> &'static str { match self { $(Self::$variant => $name),+ } }
            pub const fn cli(self) -> &'static str { match self { $(Self::$variant => $cli),+ } }
            pub const fn mutates(self) -> bool { match self { $(Self::$variant => $mutates),+ } }
            pub const fn params(self) -> &'static [&'static str] { match self { $(Self::$variant => &[$($param),*]),+ } }
            const fn result(self) -> &'static str { match self { $(Self::$variant => $result),+ } }
        }
    }
}

methods! {
    Auth, "auth", "", false, "authentication and capabilities", ["token", "client", "client_kind", "mcp_session"];
    Ping, "ping", "ping", false, "{pong, instance, protocol_version, app_version, uptime_millis}", [];
    Schema, "api.schema", "schema", false, "method schema", ["method"];
    EventsSubscribe, "events.subscribe", "events", false, "{subscription_id,kinds,queue_depth}", ["kinds", "panes", "include_evidence"];
    EventsUnsubscribe, "events.unsubscribe", "events unsubscribe", false, "{subscribed:[string]}", ["subscription_id"];
    Agents, "agent.list", "agent list", false, "{agents,totals}", ["state", "kind", "tab", "window"];
    Agent, "agent.get", "agent get", false, "{agent}", ["pane"];
    AgentExplain, "agent.explain", "agent explain", false, "{explain}", ["pane"];
    AgentPrompt, "agent.prompt", "agent prompt", true, "WriteResult", ["pane", "text", "wait_until", "timeout_ms", "instance"];
    AgentKeys, "agent.send_keys", "agent send-keys", true, "WriteResult", ["pane", "keys", "instance"];
    AgentWait, "agent.wait", "agent wait", false, "AgentWaitResult", ["pane", "until", "timeout_ms"];
    AgentRename, "agent.rename", "agent rename", true, "{agent}", ["pane", "alias", "instance"];
    AgentStart, "agent.start", "agent start", true, "{pane,agent,command}", ["pane", "split_from", "orientation", "kind", "command", "prompt", "ready_timeout_ms", "wait", "until", "timeout_ms", "instance"];
    Notify, "notification.show", "notify", false, "{shown,supported}", ["title", "body"];
    Windows, "window.list", "window list", false, "{instance, windows}", [];
    Tabs, "tab.list", "tab list", false, "{instance, tabs}", ["window"];
    FocusTab, "tab.focus", "tab focus", true, "{ok, tab}", ["tab", "instance"];
    CreateTab, "tab.create", "tab create", true, "{instance, pending, tab:TabInfo|null}", ["connection", "window", "isolation", "incognito", "instance"];
    Panes, "pane.list", "pane list", false, "{instance, panes}", ["tab", "window", "local_shell_only"];
    Current, "pane.current", "pane current", false, "{pane}", [];
    Pane, "pane.get", "pane get", false, "{pane}", ["pane"];
    Resolve, "pane.resolve", "", false, "{pane:PaneInfo|null, matched_pid:int|null}", ["pids"];
    FocusPane, "pane.focus", "pane focus", true, "{ok, pane}", ["pane", "raise", "instance"];
    Read, "pane.read", "pane read", false, "PaneText", ["pane", "mode", "lines"];
    SendText, "pane.send_text", "pane send-text", true, "WriteResult", ["pane", "text", "submit", "bracketed", "allow_shortcut_conflict", "instance"];
    Run, "pane.run", "pane run", true, "WriteResult", ["pane", "command", "instance"];
    SendKeys, "pane.send_keys", "pane send-keys", true, "WriteResult", ["pane", "keys", "instance"];
    Wait, "pane.wait_output", "pane wait-output", false, "MatchResult", ["pane", "mode", "lines", "contains", "regex", "timeout_ms", "poll_ms"];
    Split, "pane.split", "pane split", true, "{pane}", ["pane", "orientation", "focus", "instance"];
    Close, "pane.close", "pane close", true, "{ok}", ["pane", "instance"];
}

impl Method {
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|method| method.name() == name)
    }

    fn schema_entry(self) -> Value {
        let params: Vec<_> = self
            .params()
            .iter()
            .map(|name| {
                let kind = match *name {
                    "keys" => "string[]|string",
                    "pids" => "int[]",
                    "kinds" | "panes" => "string[]",
                    "command" if self == Self::AgentStart => "string[]",
                    "raise"
                    | "focus"
                    | "submit"
                    | "allow_shortcut_conflict"
                    | "incognito"
                    | "wait"
                    | "include_evidence"
                    | "local_shell_only" => "bool",
                    "lines" | "timeout_ms" | "poll_ms" | "ready_timeout_ms" => "integer",
                    _ => "string",
                };
                let required = match *name {
                    "pane" => self != Self::AgentStart,
                    "tab" => self == Self::FocusTab,
                    "command" => self == Self::Run,
                    "kind" => self == Self::AgentStart,
                    "until" => self == Self::AgentWait,
                    "title" | "body" | "token" | "keys" | "text" | "pids" | "connection" => true,
                    _ => false,
                };
                let mut param = json!({"name": name, "type": kind, "required": required});
                if *name == "incognito" {
                    param["default"] = json!("false");
                }
                match (self, *name) {
                    (Self::AgentPrompt | Self::AgentWait, "timeout_ms")
                    | (Self::AgentStart, "ready_timeout_ms") => {
                        param["default"] = json!(AGENT_WAIT_DEFAULT_MS.to_string())
                    }
                    (Self::AgentStart, "timeout_ms") => {
                        param["default"] = json!(AGENT_START_WAIT_DEFAULT_MS.to_string())
                    }
                    (Self::AgentStart, "wait") | (Self::EventsSubscribe, "include_evidence") => {
                        param["default"] = json!("false")
                    }
                    (Self::AgentStart, "until") => param["default"] = json!("done"),
                    (Self::AgentStart, "orientation") => param["default"] = json!("horizontal"),
                    (Self::SendText, "allow_shortcut_conflict") => {
                        param["default"] = json!("false");
                        param["description"] =
                            json!("Write matching host AI shortcuts as literal terminal text.");
                    }
                    _ => {}
                }
                param
            })
            .collect();
        let cli = if self.cli().is_empty() {
            Value::Null
        } else {
            json!(format!("kortty-cli {}", self.cli()))
        };
        let mut entry = json!({"name": self.name(), "params": params, "result": self.result(), "mutating": self.mutates(), "cli": cli});
        if self == Self::CreateTab {
            entry["errors"] = json!([
                ErrorCode::ConnectionNotFound.wire(),
                ErrorCode::AmbiguousConnection.wire(),
                ErrorCode::WindowNotFound.wire(),
                ErrorCode::Policy.wire(),
                ErrorCode::Stale.wire(),
            ]);
        } else if matches!(
            self,
            Self::SendText | Self::Run | Self::AgentPrompt | Self::AgentStart
        ) {
            entry["errors"] = json!([ErrorCode::HostShortcutConflict.wire()]);
        }
        entry
    }
}

pub(crate) fn schema(params: &Params) -> Result<Value, Fault> {
    if let Some(name) = params.string("method")? {
        return Method::from_name(name)
            .map(Method::schema_entry)
            .ok_or_else(|| Fault::new(ErrorCode::UnknownMethod, "Unknown method"));
    }
    let methods: Vec<_> = Method::ALL
        .iter()
        .map(|method| method.schema_entry())
        .collect();
    let errors: Vec<_> = ErrorCode::ALL.iter().map(|code| json!({
        "code": code.wire(), "json_rpc": code.number(), "exit": code.exit(), "retryable": code.retryable(),
    })).collect();
    Ok(
        json!({"api": API, "protocol_version": PROTOCOL_VERSION, "app_version": env!("CARGO_PKG_VERSION"),
        "methods": methods, "errors": errors, "events": crate::events::KINDS,
        "reserved": [{"name": "tab.close", "reason": "Reserved"}, {"name": "tab.rename", "reason": "Reserved"}],
        "id_formats": {"window": "w<n>", "tab": "t<uuid>", "pane": "p<hex>", "qualified": "<w>:<t>:<p>", "aliases": ["@focused"]},
        "limits": {"max_line_bytes": MAX_LINE, "max_result_bytes": MAX_RESULT, "max_connections": MAX_CLIENTS,
            "max_read_lines": MAX_READ_LINES, "max_wait_ms": MAX_WAIT_MS, "max_regex_chars": 512,
            "max_request_id_bytes": MAX_ID_BYTES, "max_keys": MAX_KEYS, "max_resolve_pids": MAX_RESOLVE_PIDS,
            "max_event_subscriptions":crate::events::MAX_SUBSCRIPTIONS,"event_queue_depth":crate::events::DEPTH}}),
    )
}

#[derive(Clone, Default)]
pub struct Params(pub(crate) Map<String, Value>);

impl Params {
    pub fn insert(&mut self, name: &str, value: Value) {
        self.0.insert(name.to_owned(), value);
    }
    pub fn strings(&self, name: &'static str, limit: usize) -> Result<Option<Vec<&str>>, Fault> {
        self.0
            .get(name)
            .map(|value| {
                value
                    .as_array()
                    .filter(|values| values.len() <= limit)
                    .ok_or_else(|| Fault::param(name))?
                    .iter()
                    .map(|value| value.as_str().ok_or_else(|| Fault::param(name)))
                    .collect()
            })
            .transpose()
    }
    /// Returns viable OS PIDs in the caller's nearest-first order.
    pub fn resolve_pids(&self) -> Result<Vec<u32>, Fault> {
        let pids = self
            .0
            .get("pids")
            .and_then(Value::as_array)
            .ok_or_else(|| Fault::param("pids"))?;
        if pids.len() > MAX_RESOLVE_PIDS {
            return Err(Fault::param("pids")
                .with("max", json!(MAX_RESOLVE_PIDS))
                .with("given", json!(pids.len())));
        }
        let mut result = Vec::with_capacity(pids.len());
        for pid in pids {
            if let Some(pid) = pid.as_u64() {
                if let Ok(pid) = u32::try_from(pid)
                    && pid != 0
                {
                    result.push(pid);
                }
            } else if pid.as_i64().is_none() {
                return Err(Fault::param("pids"));
            }
        }
        Ok(result)
    }

    pub fn string(&self, name: &'static str) -> Result<Option<&str>, Fault> {
        self.0
            .get(name)
            .map(|value| value.as_str().ok_or_else(|| Fault::param(name)))
            .transpose()
    }

    fn present(&self, name: &'static str) -> Option<&Value> {
        self.0.get(name).filter(|value| !value.is_null())
    }

    pub(crate) fn nullable_string(&self, name: &'static str) -> Result<Option<&str>, Fault> {
        self.present(name)
            .map(|value| value.as_str().ok_or_else(|| Fault::param(name)))
            .transpose()
    }

    pub fn required(&self, name: &'static str) -> Result<&str, Fault> {
        self.string(name)?.ok_or_else(|| Fault::param(name))
    }

    pub fn flag(&self, name: &'static str, default: bool) -> Result<bool, Fault> {
        self.0.get(name).map_or(Ok(default), |value| {
            value.as_bool().ok_or_else(|| Fault::param(name))
        })
    }

    pub fn number(&self, name: &'static str, default: u64) -> Result<u64, Fault> {
        self.0.get(name).map_or(Ok(default), |value| {
            value.as_u64().ok_or_else(|| Fault::param(name))
        })
    }

    pub fn keys(&self) -> Result<Vec<&str>, Fault> {
        let keys = match self.0.get("keys") {
            Some(Value::String(value)) => value.split_whitespace().take(MAX_KEYS + 1).collect(),
            Some(Value::Array(keys)) if keys.len() > MAX_KEYS => return Err(too_many_keys()),
            Some(Value::Array(keys)) => keys
                .iter()
                .map(|key| key.as_str().ok_or_else(|| Fault::param("keys")))
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err(Fault::param("keys")),
        };
        if keys.len() > MAX_KEYS {
            return Err(too_many_keys());
        }
        Ok(keys)
    }
}

pub const TAB_CREATE_ISOLATIONS: [&str; 3] = ["none", "process", "sandbox"];

/// The validated selection and request-local overrides of a tab.create call.
#[derive(Debug, Eq, PartialEq)]
pub struct TabCreateParams<'a> {
    pub connection: &'a str,
    pub window: Option<&'a str>,
    pub isolation: Option<&'a str>,
    pub incognito: bool,
}

impl<'a> TabCreateParams<'a> {
    pub fn parse(params: &'a Params) -> Result<Self, Fault> {
        let connection = params.required("connection")?.trim_matches(java_whitespace);
        if connection.is_empty() {
            return Err(Fault::new(
                ErrorCode::InvalidParams,
                "connection must name a saved connection",
            ));
        }
        let window = params.nullable_string("window")?;
        let isolation = params.nullable_string("isolation")?;
        if let Some(value) = isolation
            && !TAB_CREATE_ISOLATIONS.contains(&value)
        {
            return Err(Fault::new(
                ErrorCode::InvalidParams,
                "isolation must be one of none, process, sandbox",
            )
            .with("isolation", json!(value))
            .with("known", json!(TAB_CREATE_ISOLATIONS)));
        }
        let incognito = params.present("incognito").map_or(Ok(false), |value| {
            value.as_bool().ok_or_else(|| Fault::param("incognito"))
        })?;
        Ok(Self {
            connection,
            window,
            isolation,
            incognito,
        })
    }
}

pub fn tab_create_result(instance: &str, tab: Option<Value>) -> Value {
    json!({"instance": instance, "pending": tab.is_none(), "tab": tab})
}

fn java_whitespace(character: char) -> bool {
    ('\u{1c}'..='\u{1f}').contains(&character)
        || (character.is_whitespace()
            && !matches!(character, '\u{85}' | '\u{a0}' | '\u{2007}' | '\u{202f}'))
}

fn too_many_keys() -> Fault {
    // Keep the write's canonical key list within the response budget even if all keys are named.
    Fault::new(ErrorCode::TooLarge, "Too many terminal keys").with("max_keys", json!(MAX_KEYS))
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum ReadMode {
    Visible,
    Recent,
}

impl ReadMode {
    pub fn parse(params: &Params, default: Self) -> Result<Self, Fault> {
        match params.string("mode")? {
            None => Ok(default),
            Some("visible") => Ok(Self::Visible),
            Some("recent") => Ok(Self::Recent),
            Some("detection") => Err(Fault::new(
                ErrorCode::Unsupported,
                "Coding-agent detection is unavailable",
            )),
            _ => Err(Fault::param("mode")),
        }
    }

    pub fn lines(params: &Params) -> Result<usize, Fault> {
        let lines = params.number("lines", 200)?;
        if !(1..=MAX_READ_LINES as u64).contains(&lines) {
            return Err(Fault::param("lines"));
        }
        Ok(lines as usize)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Visible => "visible",
            Self::Recent => "recent",
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Screen {
    pub pane_id: String,
    pub mode: String,
    pub lines: Vec<String>,
    pub columns: u16,
    pub rows: u16,
    pub alternate_screen: bool,
    pub osc_title: Option<String>,
    pub truncated: bool,
}

pub(crate) fn decode_request(bytes: &[u8]) -> Result<(Value, String, Params), (Value, Fault)> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| (Value::Null, Fault::new(ErrorCode::Parse, "Invalid JSON")))?;
    let Value::Object(mut request) = value else {
        return Err((
            Value::Null,
            Fault::new(ErrorCode::InvalidRequest, "Expected one JSON-RPC request"),
        ));
    };
    let id = request
        .remove("id")
        .filter(|id| id.is_string() || id.is_number())
        .ok_or_else(|| {
            (
                Value::Null,
                Fault::new(
                    ErrorCode::InvalidRequest,
                    "A string or number id is required",
                ),
            )
        })?;
    if id.as_str().is_some_and(|id| id.len() > MAX_ID_BYTES) {
        // An unbounded ID could make a small successful mutation impossible to acknowledge.
        return Err((
            Value::Null,
            Fault::new(
                ErrorCode::InvalidRequest,
                "Request ID exceeds its byte limit",
            ),
        ));
    }
    if request.remove("jsonrpc").as_ref().and_then(Value::as_str) != Some("2.0") {
        return Err((
            id,
            Fault::new(ErrorCode::InvalidRequest, "Expected JSON-RPC 2.0"),
        ));
    }
    let Some(Value::String(method)) = request.remove("method") else {
        return Err((
            id,
            Fault::new(ErrorCode::InvalidRequest, "A method name is required"),
        ));
    };
    let params = match request.remove("params") {
        None | Some(Value::Null) => Params::default(),
        Some(Value::Object(params)) => Params(params),
        _ => {
            return Err((
                id,
                Fault::new(ErrorCode::InvalidParams, "Expected a parameters object"),
            ));
        }
    };
    Ok((id, method, params))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_create_schema_and_nullable_options_follow_the_wire_contract() {
        let schema = Method::CreateTab.schema_entry();
        assert_eq!(schema["mutating"], true);
        assert_eq!(schema["result"], "{instance, pending, tab:TabInfo|null}");
        assert_eq!(
            schema["params"][0],
            json!({"name": "connection", "type": "string", "required": true})
        );
        assert_eq!(
            schema["params"][3],
            json!({"name": "incognito", "type": "bool", "required": false, "default": "false"})
        );
        let params = Params(
            json!({"connection": "\u{1f} web-01\u{3000}", "window": null, "isolation": null, "incognito": null})
                .as_object().unwrap().clone(),
        );
        assert_eq!(
            TabCreateParams::parse(&params).unwrap(),
            TabCreateParams {
                connection: "web-01",
                window: None,
                isolation: None,
                incognito: false,
            }
        );
        let tab = json!({"tab_id": "t123"});
        assert_eq!(
            tab_create_result("test", Some(tab.clone())),
            json!({"instance": "test", "pending": false, "tab": tab})
        );
    }

    #[test]
    fn resolve_pids_are_required_ordered_and_bounded() {
        let params = |pids| Params(Map::from_iter([("pids".into(), pids)]));
        assert_eq!(
            params(json!([31, 0, -1, 4_294_967_296_u64, 23]))
                .resolve_pids()
                .unwrap(),
            [31, 23]
        );
        assert!(params(json!([])).resolve_pids().unwrap().is_empty());
        for invalid in [json!(1), json!(["2"]), json!([2.5]), json!([null])] {
            assert_eq!(
                params(invalid).resolve_pids().unwrap_err().code,
                ErrorCode::InvalidParams
            );
        }
        assert_eq!(
            Params::default().resolve_pids().unwrap_err().code,
            ErrorCode::InvalidParams
        );
        let error = params(json!(vec![1; MAX_RESOLVE_PIDS + 1]))
            .resolve_pids()
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidParams);
    }

    #[test]
    fn bounds_request_ids_and_key_lists_before_dispatch() {
        let frame =
            json!({"jsonrpc": "2.0", "method": "pane.close", "id": "x".repeat(MAX_ID_BYTES + 1)});
        let Err((id, error)) = decode_request(&serde_json::to_vec(&frame).unwrap()) else {
            panic!("request ID beyond the limit was accepted");
        };
        assert!(id.is_null());
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        let params = Params(Map::from_iter([(
            "keys".into(),
            json!("a ".repeat(MAX_KEYS + 1)),
        )]));
        assert_eq!(params.keys().unwrap_err().code, ErrorCode::TooLarge);
    }
}
