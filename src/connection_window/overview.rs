//! Allowlisted inventory of saved connections. A profile is never serialized directly.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

use jiff::Timestamp;
use serde::{Serialize, Serializer, ser::SerializeMap};

use crate::connection_store::custom_os_badges::Catalog;
use crate::connection_store::{Auth, Connection, SavedLogin, Store, TunnelConfig, TunnelTarget};
use crate::session_security::IsolationLevel;

const GENERATOR: &str = "kortty";
const NOTE: &str = "kortty connection overview for inventory use.";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Format {
    #[default]
    Json,
    Yaml,
    Toml,
    Xml,
}

impl Format {
    pub(crate) const ALL: [Self; 4] = [Self::Json, Self::Yaml, Self::Toml, Self::Xml];

    pub(crate) const fn suffix(self) -> &'static str {
        match self {
            Self::Json => ".json",
            Self::Yaml => ".yaml",
            Self::Toml => ".toml",
            Self::Xml => ".xml",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Json => "JSON",
            Self::Yaml => "YAML",
            Self::Toml => "TOML",
            Self::Xml => "XML",
        })
    }
}

/// Optional sections. Every row has the connection name and SSH endpoint.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Part {
    Folder,
    Authentication,
    JumpServer,
    Tunnels,
    Terminal,
    Security,
    Usage,
    Source,
    OsBadge,
}

impl Part {
    pub(crate) const ALL: [Self; 9] = [
        Self::Folder,
        Self::Authentication,
        Self::JumpServer,
        Self::Tunnels,
        Self::Terminal,
        Self::Security,
        Self::Usage,
        Self::Source,
        Self::OsBadge,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Folder => "Folder, tag, tab color, favorite",
            Self::Authentication => "Sign-in method and references",
            Self::JumpServer => "Jump server",
            Self::Tunnels => "Tunnels",
            Self::Terminal => "Terminal settings",
            Self::Security => "Isolation, incognito, host key check",
            Self::Usage => "Last used",
            Self::Source => "Source",
            Self::OsBadge => "OS symbol",
        }
    }

    pub(crate) fn defaults() -> BTreeSet<Self> {
        Self::ALL
            .into_iter()
            .filter(|part| !matches!(part, Self::Terminal | Self::Usage))
            .collect()
    }
}

/// Captured when the export opens so that preview and file use the same metadata.
pub(crate) struct Snapshot {
    exported_at: Timestamp,
    entries: BTreeMap<u64, Entry>,
}

impl Snapshot {
    pub(crate) fn capture(
        store: &Store,
        custom_badges: &Catalog,
        credential_name: impl Fn(&str) -> Option<String>,
        ssh_key_name: impl Fn(u64) -> Option<String>,
    ) -> Self {
        let used_at: BTreeMap<_, _> = store
            .recent_since(0)
            .filter_map(|(connection, millis)| {
                let millis = i64::try_from(millis).ok().filter(|&value| value > 0)?;
                Some((connection.id, Timestamp::from_millisecond(millis).ok()?))
            })
            .collect();
        Self {
            exported_at: Timestamp::now(),
            entries: store
                .connections()
                .iter()
                .map(|connection| {
                    (
                        connection.id,
                        Entry::capture(
                            connection,
                            store,
                            custom_badges,
                            &credential_name,
                            &ssh_key_name,
                            used_at.get(&connection.id).copied(),
                        ),
                    )
                })
                .collect(),
        }
    }

    /// Renders entries in the order given by the caller.
    pub(crate) fn render(
        &self,
        ids: &[u64],
        parts: &BTreeSet<Part>,
        format: Format,
    ) -> Result<String, serde_json::Error> {
        let entries: Vec<_> = ids
            .iter()
            .filter_map(|id| self.entries.get(id))
            .map(|entry| entry.fields(parts))
            .collect();
        let mut document = Fields(vec![
            ("generator", Value::text(GENERATOR)),
            ("version", Value::text(env!("CARGO_PKG_VERSION"))),
            ("exportedAt", Value::Time(self.exported_at)),
            ("count", Value::Number(entries.len() as u64)),
        ]);
        let rendered = match format {
            Format::Json => {
                document.0.push(("connections", Value::List(entries)));
                let mut json = serde_json::to_string_pretty(&document)?;
                json.push('\n');
                json
            }
            Format::Yaml => {
                document.0.push(("connections", Value::List(entries)));
                let mut out = format!("# {NOTE}\n");
                yaml_mapping(&mut out, &document, "", None);
                out
            }
            Format::Toml => toml_document(&document, &entries),
            Format::Xml => xml_document(&document, &entries),
        };
        Ok(rendered)
    }
}

struct Entry {
    basics: Fields,
    parts: [(Part, Fields); Part::ALL.len()],
}

impl Entry {
    fn capture(
        connection: &Connection,
        store: &Store,
        custom_badges: &Catalog,
        credential_name: &impl Fn(&str) -> Option<String>,
        ssh_key_name: &impl Fn(u64) -> Option<String>,
        used_at: Option<Timestamp>,
    ) -> Self {
        let login = &connection.login;
        let mut basics = Fields(vec![
            ("name", Value::text(connection.name.clone())),
            ("protocol", Value::text("sshTcp")),
        ]);
        basics.extend(endpoint(login));
        let mut folder = Fields::default();
        folder.text("folder", store.group_of(connection.id));
        folder.text("tag", Some(&connection.tag));
        if let Some(color) = connection.tab_color {
            folder.0.push(("tabColor", Value::text(color.to_string())));
        }
        if store.is_favorite(connection.id) {
            folder.0.push(("favorite", Value::Flag(true)));
        }

        let mut authentication = Fields(vec![("method", Value::text(auth_method(login)))]);
        match &login.auth {
            Auth::VaultPassword(id) => {
                let name = credential_name(id).filter(|value| !value.trim().is_empty());
                authentication.text("credential", name.as_deref().or(Some(id)));
            }
            Auth::KeyFile { path, .. } if path.is_absolute() => {
                authentication.text("privateKeyPath", path.to_str());
            }
            Auth::StoredKey(id) => {
                let name = ssh_key_name(*id).filter(|value| !value.trim().is_empty());
                authentication.0.push((
                    "sshKey",
                    Value::text(name.unwrap_or_else(|| id.to_string())),
                ));
            }
            Auth::Configured {
                credential, key, ..
            } => {
                if let Some(id) = credential {
                    let name = credential_name(id).filter(|value| !value.trim().is_empty());
                    authentication.text("credential", name.as_deref().or(Some(id)));
                }
                if let Some(id) = key {
                    let name = ssh_key_name(*id).filter(|value| !value.trim().is_empty());
                    authentication.0.push((
                        "sshKey",
                        Value::text(name.unwrap_or_else(|| id.to_string())),
                    ));
                }
            }
            _ => {}
        }

        let mut jump = Fields::default();
        if let Some(login) = &connection.jump {
            let mut fields = endpoint(login);
            fields.0.push(("method", Value::text(auth_method(login))));
            jump.0.push(("jumpServer", Value::Map(fields)));
        }

        let tunnels: Vec<_> = connection
            .tunnels
            .iter()
            .filter(|tunnel| tunnel.enabled)
            .map(tunnel_fields)
            .collect();
        let tunnels = if tunnels.is_empty() {
            Fields::default()
        } else {
            Fields(vec![("tunnels", Value::List(tunnels))])
        };

        let mut terminal = Fields(vec![(
            "emulation",
            Value::text(connection.emulation.clone()),
        )]);
        if let Some(effect) = connection.terminal_effect
            && let Ok(serde_json::Value::String(id)) = serde_json::to_value(effect.effect)
        {
            terminal.0.push(("terminalEffectPluginId", Value::text(id)));
        }
        terminal.0.extend([
            (
                "connectionTimeoutSeconds",
                Value::Number(connection.startup.seconds().into()),
            ),
            (
                "retryCount",
                Value::Number(connection.startup.attempts().max(1).into()),
            ),
        ]);

        let security = Fields(vec![
            (
                "isolationLevel",
                Value::text(match connection.isolation {
                    IsolationLevel::None => "none",
                    IsolationLevel::Process => "process",
                    IsolationLevel::Sandbox => "sandbox",
                }),
            ),
            ("incognito", Value::Flag(connection.incognito)),
            ("hostKeyCheck", Value::text("default")),
        ]);

        let usage = used_at.map_or_else(Fields::default, |used_at| {
            Fields(vec![(
                "usage",
                Value::Map(Fields(vec![("lastUsed", Value::Time(used_at))])),
            )])
        });
        let mut os_badge = Fields::default();
        let name = store
            .os_badge(connection.id)
            .map(|badge| badge.name())
            .or_else(|| {
                let reference = store.os_badge_id(connection.id)?;
                custom_badges.find(reference).map(|badge| badge.name())
            });
        os_badge.text("osBadge", name);

        Self {
            basics,
            parts: [
                (Part::Folder, folder),
                (
                    Part::Authentication,
                    Fields(vec![("authentication", Value::Map(authentication))]),
                ),
                (Part::JumpServer, jump),
                (Part::Tunnels, tunnels),
                (
                    Part::Terminal,
                    Fields(vec![("terminal", Value::Map(terminal))]),
                ),
                (
                    Part::Security,
                    Fields(vec![("security", Value::Map(security))]),
                ),
                (Part::Usage, usage),
                (
                    Part::Source,
                    Fields(vec![(
                        "source",
                        Value::Map(Fields(vec![("origin", Value::text("local"))])),
                    )]),
                ),
                (Part::OsBadge, os_badge),
            ],
        }
    }

    fn fields(&self, parts: &BTreeSet<Part>) -> Fields {
        let mut result = self.basics.clone();
        for (part, fields) in &self.parts {
            if parts.contains(part) {
                result.extend(fields.clone());
            }
        }
        result
    }
}

fn auth_method(login: &SavedLogin) -> &'static str {
    match login.auth.method() {
        crate::connection_store::AuthMethod::Password => "password",
        crate::connection_store::AuthMethod::KeyboardInteractive => "keyboardInteractive",
        crate::connection_store::AuthMethod::PublicKey => "publicKey",
    }
}

fn endpoint(login: &SavedLogin) -> Fields {
    let mut fields = Fields::default();
    fields.text("host", Some(&login.endpoint.host));
    fields
        .0
        .push(("port", Value::Number(login.endpoint.port.into())));
    fields.text("username", Some(&login.endpoint.username));
    fields
}

fn tunnel_fields(tunnel: &TunnelConfig) -> Fields {
    let bind = tunnel.rule.bind;
    let bind_host = bind.ip().to_string();
    let (kind, local_host, local_port, remote) = match &tunnel.rule.target {
        TunnelTarget::Local { host, port } => (
            "local",
            bind_host.as_str(),
            bind.port(),
            Some((host.as_str(), *port)),
        ),
        TunnelTarget::Remote { host, port } => (
            "remote",
            host.as_str(),
            *port,
            Some((bind_host.as_str(), bind.port())),
        ),
        TunnelTarget::Dynamic => ("dynamic", bind_host.as_str(), bind.port(), None),
    };
    let mut fields = Fields(vec![
        ("type", Value::text(kind)),
        ("localHost", Value::text(local_host)),
        ("localPort", Value::Number(local_port.into())),
    ]);
    if let Some((host, port)) = remote {
        fields.text("remoteHost", Some(host));
        fields.0.push(("remotePort", Value::Number(port.into())));
    }
    fields.text("description", Some(&tunnel.description));
    fields
}

// A small ordered tree for the fixed inventory schema shared by all four writers.
#[derive(Clone, Default)]
struct Fields(Vec<(&'static str, Value)>);

impl Fields {
    fn text(&mut self, key: &'static str, text: Option<&str>) {
        if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
            self.0.push((key, Value::text(text)));
        }
    }

    fn extend(&mut self, fields: Self) {
        self.0.extend(fields.0);
    }
}

impl Serialize for Fields {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

#[derive(Clone)]
enum Value {
    Text(String),
    Number(u64),
    Flag(bool),
    Time(Timestamp),
    Map(Fields),
    List(Vec<Fields>),
}

impl Value {
    fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Text(text) => serializer.serialize_str(text),
            Self::Number(number) => serializer.serialize_u64(*number),
            Self::Flag(flag) => serializer.serialize_bool(*flag),
            Self::Time(time) => serializer.collect_str(time),
            Self::Map(map) => map.serialize(serializer),
            Self::List(list) => list.serialize(serializer),
        }
    }
}

fn yaml_mapping(out: &mut String, fields: &Fields, indent: &str, first: Option<&str>) {
    for (index, (key, value)) in fields.0.iter().enumerate() {
        let lead = if index == 0 {
            first.unwrap_or(indent)
        } else {
            indent
        };
        let _ = write!(out, "{lead}{key}:");
        match value {
            Value::Map(fields) => {
                out.push('\n');
                yaml_mapping(out, fields, &format!("{indent}  "), None);
            }
            Value::List(items) if !items.is_empty() => {
                out.push('\n');
                let nested = format!("{indent}    ");
                let first = format!("{indent}  - ");
                for item in items {
                    yaml_mapping(out, item, &nested, Some(&first));
                }
            }
            _ => {
                out.push(' ');
                match value {
                    Value::Text(text) => yaml_text(out, text),
                    Value::Time(time) => yaml_text(out, &time.to_string()),
                    _ => scalar(out, value),
                }
                out.push('\n');
            }
        }
    }
}

fn yaml_text(out: &mut String, text: &str) {
    let plain = text
        .chars()
        .next()
        .is_some_and(|first| !"-?:,[]{}#&*!|>'\"%@`+.".contains(first) && !first.is_numeric())
        && text.trim() == text
        && !matches!(
            text.to_ascii_lowercase().as_str(),
            "~" | "null" | "true" | "false" | "yes" | "no" | "on" | "off" | "y" | "n" | "=" | "<<"
        )
        && !text
            .chars()
            .any(|ch| matches!(ch, ':' | '#') || yaml_unsafe(ch));
    if plain {
        out.push_str(text);
    } else {
        quoted(out, text, yaml_unsafe);
    }
}

fn yaml_unsafe(ch: char) -> bool {
    ch.is_control()
        || matches!(
            ch,
            '\u{2028}' | '\u{2029}' | '\u{FEFF}' | '\u{FFFE}' | '\u{FFFF}'
        )
}

fn toml_document(header: &Fields, entries: &[Fields]) -> String {
    let mut out = format!("# {NOTE}\n");
    toml_table(&mut out, "", header);
    if entries.is_empty() {
        out.push_str("connections = []\n");
    }
    for entry in entries {
        out.push_str("\n[[connections]]\n");
        toml_table(&mut out, "connections", entry);
    }
    out
}

fn toml_table(out: &mut String, path: &str, fields: &Fields) {
    for (key, value) in &fields.0 {
        if !matches!(value, Value::Map(_) | Value::List(_)) {
            let _ = write!(out, "{key} = ");
            if let Value::Text(text) = value {
                quoted(out, text, |ch| ch < ' ' || ch == '\u{7F}');
            } else {
                scalar(out, value);
            }
            out.push('\n');
        }
    }
    for (key, value) in &fields.0 {
        match value {
            Value::Map(fields) => {
                let child = format!("{path}.{key}");
                let _ = writeln!(out, "\n[{child}]");
                toml_table(out, &child, fields);
            }
            Value::List(items) => {
                let child = format!("{path}.{key}");
                for item in items {
                    let _ = writeln!(out, "\n[[{child}]]");
                    toml_table(out, &child, item);
                }
            }
            _ => {}
        }
    }
}

fn quoted(out: &mut String, text: &str, escape: impl Fn(char) -> bool) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{C}' => out.push_str("\\f"),
            _ if escape(ch) => {
                let _ = write!(out, "\\u{:04X}", u32::from(ch));
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
}

fn scalar(out: &mut String, value: &Value) {
    match value {
        Value::Text(text) => out.push_str(text),
        Value::Number(number) => {
            let _ = write!(out, "{number}");
        }
        Value::Flag(flag) => {
            let _ = write!(out, "{flag}");
        }
        Value::Time(time) => {
            let _ = write!(out, "{time}");
        }
        Value::List(_) => out.push_str("[]"),
        Value::Map(_) => {}
    }
}

fn xml_document(header: &Fields, entries: &[Fields]) -> String {
    let mut out =
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- {NOTE} -->\n<korttyOverview");
    for (key, value) in &header.0 {
        let _ = write!(out, " {key}=\"");
        xml_scalar(&mut out, value, true);
        out.push('"');
    }
    if entries.is_empty() {
        out.push_str("/>\n");
        return out;
    }
    out.push_str(">\n");
    for entry in entries {
        xml_map(&mut out, "connection", entry, 1);
    }
    out.push_str("</korttyOverview>\n");
    out
}

fn xml_map(out: &mut String, key: &str, fields: &Fields, depth: usize) {
    let indent = "  ".repeat(depth);
    let child_indent = format!("{indent}  ");
    let _ = writeln!(out, "{indent}<{key}>");
    for (name, value) in &fields.0 {
        match value {
            Value::Map(fields) => xml_map(out, name, fields, depth + 1),
            Value::List(items) => {
                let _ = writeln!(out, "{child_indent}<{name}>");
                let item_name = name.strip_suffix('s').unwrap_or(name);
                for item in items {
                    xml_map(out, item_name, item, depth + 2);
                }
                let _ = writeln!(out, "{child_indent}</{name}>");
            }
            _ => {
                let _ = write!(out, "{child_indent}<{name}>");
                xml_scalar(out, value, false);
                let _ = writeln!(out, "</{name}>");
            }
        }
    }
    let _ = writeln!(out, "{indent}</{key}>");
}

fn xml_scalar(out: &mut String, value: &Value, attribute: bool) {
    let Value::Text(text) = value else {
        scalar(out, value);
        return;
    };
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            '\n' if attribute => out.push_str("&#10;"),
            '\t' if attribute => out.push_str("&#9;"),
            '\u{0}'..='\u{8}'
            | '\u{B}'..='\u{C}'
            | '\u{E}'..='\u{1F}'
            | '\u{FFFE}'
            | '\u{FFFF}' => {
                out.push('\u{FFFD}');
            }
            _ => out.push(ch),
        }
    }
}

#[cfg(test)]
mod tests;
