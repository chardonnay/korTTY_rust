use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use kortty_ssh::ConnectOptions;
use kortty_terminal::TerminalProfile;
use serde::{Deserialize, Serialize};

use crate::clipboard_policy::PasteWarningMode;
use crate::profile;
use crate::session_security::IsolationLevel;
use crate::ssh_dialog::{LoginTarget, normalize_host, unsafe_text};
use crate::ssh_tunnel::Destination;
use crate::terminal_effects::Selection;

mod color;
pub(crate) mod custom_os_badges;
pub(crate) mod import;
mod library;
mod os_badge;
pub(crate) use color::TabColor;
use library::Library;
pub(crate) use library::{MAX_GROUP_PATH_BYTES, Search, group_suffix};
pub(crate) use os_badge::OsBadge;

const VERSION: u32 = 1;
const MAX_BYTES: u64 = 1024 * 1024;
const MAX_PROFILES: usize = 512;
pub const MAX_TUNNELS: usize = 16;

pub(crate) fn valid_os_badge_reference(value: &str) -> bool {
    OsBadge::find(value).is_some() || custom_os_badges::is_reference(value)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub id: u64,
    pub name: String,
    pub tag: String,
    pub login: SavedLogin,
    pub jump: Option<SavedLogin>,
    pub emulation: String,
    pub tunnels: Vec<TunnelConfig>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub incognito: bool,
    #[serde(default, skip_serializing_if = "IsolationLevel::is_none")]
    pub isolation: IsolationLevel,
    #[serde(
        default,
        with = "startup_options",
        skip_serializing_if = "startup_options::is_default"
    )]
    pub startup: ConnectOptions,
    #[serde(
        default,
        skip_serializing_if = "crate::terminal_logging::Config::is_default"
    )]
    pub(crate) logging: crate::terminal_logging::Config,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) terminal_effect: Option<Selection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) command_timestamps: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tab_color: Option<TabColor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) paste_warning: Option<PasteWarningMode>,
}

mod startup_options {
    use kortty_ssh::ConnectOptions;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Stored {
        timeout_seconds: u16,
        attempts: u8,
    }

    pub(super) fn is_default(options: &ConnectOptions) -> bool {
        *options == ConnectOptions::default()
    }

    pub(super) fn serialize<S: Serializer>(
        options: &ConnectOptions,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        Stored {
            timeout_seconds: options.seconds(),
            attempts: options.attempts(),
        }
        .serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ConnectOptions, D::Error> {
        let options = Stored::deserialize(deserializer)?;
        ConnectOptions::new(options.timeout_seconds, options.attempts)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedLogin {
    pub endpoint: LoginTarget,
    pub auth: Auth,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keyboard_interactive: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum Auth {
    #[default]
    Prompt,
    KeyboardInteractive,
    VaultPassword(String),
    KeyFile {
        path: PathBuf,
        passphrase: Option<String>,
    },
    StoredKey(u64),
    Configured {
        method: AuthMethod,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMethod {
    Password,
    PublicKey,
    KeyboardInteractive,
}

pub(crate) enum ActiveAuth<'a> {
    Prompt,
    KeyboardInteractive,
    VaultPassword(&'a str),
    KeyFile {
        path: &'a Path,
        passphrase: Option<&'a str>,
    },
    StoredKey(u64),
}

impl Auth {
    pub(crate) fn method(&self) -> AuthMethod {
        match self {
            Self::Prompt | Self::VaultPassword(_) => AuthMethod::Password,
            Self::KeyboardInteractive => AuthMethod::KeyboardInteractive,
            Self::KeyFile { .. } | Self::StoredKey(_) => AuthMethod::PublicKey,
            Self::Configured { method, .. } => *method,
        }
    }

    pub(crate) fn active(&self) -> ActiveAuth<'_> {
        match self {
            Self::Prompt => ActiveAuth::Prompt,
            Self::KeyboardInteractive => ActiveAuth::KeyboardInteractive,
            Self::VaultPassword(id) => ActiveAuth::VaultPassword(id),
            Self::KeyFile { path, passphrase } => ActiveAuth::KeyFile {
                path,
                passphrase: passphrase.as_deref(),
            },
            Self::StoredKey(id) => ActiveAuth::StoredKey(*id),
            Self::Configured {
                method: AuthMethod::Password,
                credential: Some(id),
                ..
            } => ActiveAuth::VaultPassword(id),
            Self::Configured {
                method: AuthMethod::PublicKey,
                key: Some(id),
                ..
            } => ActiveAuth::StoredKey(*id),
            Self::Configured {
                method: AuthMethod::KeyboardInteractive,
                ..
            } => ActiveAuth::KeyboardInteractive,
            Self::Configured { .. } => ActiveAuth::Prompt,
        }
    }

    pub(crate) fn requires_interactive(&self) -> bool {
        matches!(
            self.active(),
            ActiveAuth::Prompt | ActiveAuth::KeyboardInteractive
        )
    }

    pub(crate) fn registered_key(&self) -> Option<u64> {
        match self {
            Self::StoredKey(id) => Some(*id),
            Self::Configured { key, .. } => *key,
            _ => None,
        }
    }

    pub(crate) fn credential_reference(&self) -> Option<&str> {
        match self {
            Self::VaultPassword(id) => Some(id),
            Self::Configured { credential, .. } => credential.as_deref(),
            _ => None,
        }
    }

    pub(crate) fn has_vault_reference(&self) -> bool {
        self.credential_reference().is_some()
            || matches!(
                self,
                Self::KeyFile {
                    passphrase: Some(_),
                    ..
                }
            )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TunnelConfig {
    pub enabled: bool,
    pub description: String,
    pub rule: TunnelRule,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TunnelRule {
    pub bind: SocketAddr,
    pub target: TunnelTarget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum TunnelTarget {
    Local { host: String, port: u16 },
    Remote { host: String, port: u16 },
    Dynamic,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TunnelPlan {
    pub login: LoginTarget,
    pub jump: Option<LoginTarget>,
    pub rules: Vec<TunnelRule>,
}

impl Connection {
    pub fn profile(&self) -> Option<TerminalProfile> {
        TerminalProfile::from_term_name(&self.emulation)
    }

    pub fn matches_route(&self, login: &LoginTarget, jump: Option<&LoginTarget>) -> bool {
        self.login.endpoint == *login && self.jump.as_ref().map(|hop| &hop.endpoint) == jump
    }

    pub(crate) fn missing_username(&self) -> Option<&'static str> {
        if self.login.endpoint.username.is_empty() {
            return Some("The saved SSH connection requires a username.");
        }
        self.jump
            .as_ref()
            .filter(|jump| jump.endpoint.username.is_empty())
            .map(|_| "The saved SSH jump host requires a username.")
    }

    pub fn plan(&self) -> TunnelPlan {
        TunnelPlan {
            login: self.login.endpoint.clone(),
            jump: self.jump.as_ref().map(|login| login.endpoint.clone()),
            rules: self
                .tunnels
                .iter()
                .filter(|tunnel| tunnel.enabled)
                .map(|tunnel| tunnel.rule.clone())
                .collect(),
        }
    }

    pub fn validate(&self) -> io::Result<()> {
        if !valid_text(&self.name, 160, false)
            || !valid_text(&self.tag, 96, true)
            || self.profile().is_none()
            || self.tunnels.len() > MAX_TUNNELS
        {
            return Err(invalid(
                "invalid saved connection name, tag, emulation, or tunnel count",
            ));
        }
        self.login.validate()?;
        self.logging.validate()?;
        if self
            .terminal_effect
            .is_some_and(|selection| selection.speed != selection.speed())
        {
            return Err(invalid(
                "terminal effect animation speed must be between 1 and 99",
            ));
        }
        if let Some(jump) = &self.jump {
            jump.validate()?;
        }
        for tunnel in &self.tunnels {
            tunnel.validate()?;
        }
        Ok(())
    }
}

impl SavedLogin {
    fn validate(&self) -> io::Result<()> {
        self.endpoint
            .validate_saved()
            .map_err(|_| invalid("invalid SSH endpoint"))?;
        if normalize_host(&self.endpoint.host).as_deref() != Ok(self.endpoint.host.as_str()) {
            return Err(invalid("saved SSH hostname must be normalized"));
        }
        match &self.auth {
            Auth::Prompt => {}
            Auth::KeyboardInteractive => {}
            Auth::VaultPassword(id) => validate_credential_id(id)?,
            Auth::StoredKey(0) => return Err(invalid("invalid registered SSH key ID")),
            Auth::StoredKey(_) => {}
            Auth::KeyFile { path, passphrase } => {
                if !path.is_absolute()
                    || !path
                        .to_str()
                        .is_some_and(|value| valid_text(value, 4096, false))
                {
                    return Err(invalid("invalid absolute SSH private key path"));
                }
                if let Some(id) = passphrase {
                    validate_credential_id(id)?;
                }
            }
            Auth::Configured {
                credential, key, ..
            } => {
                if let Some(id) = credential {
                    validate_credential_id(id)?;
                }
                if *key == Some(0) {
                    return Err(invalid("invalid registered SSH key ID"));
                }
            }
        }
        if self.keyboard_interactive && self.auth.method() == AuthMethod::KeyboardInteractive {
            return Err(invalid(
                "keyboard-interactive login cannot also be a follow-up",
            ));
        }
        Ok(())
    }
}

impl TunnelConfig {
    pub fn validate(&self) -> io::Result<()> {
        if !valid_text(&self.description, 160, true) {
            return Err(invalid("invalid tunnel description"));
        }
        self.rule.destination()?;
        Ok(())
    }
}

impl TunnelRule {
    pub fn destination(&self) -> io::Result<Destination> {
        if self.bind.port() == 0
            || matches!(self.bind, SocketAddr::V6(address) if address.scope_id() != 0 || address.flowinfo() != 0)
        {
            return Err(invalid("invalid tunnel listen address or port"));
        }
        match &self.target {
            TunnelTarget::Local { host, port } => {
                Ok(Destination::Fixed(tunnel_endpoint(host, *port)?))
            }
            TunnelTarget::Remote { host, port } => {
                Ok(Destination::Remote(tunnel_endpoint(host, *port)?))
            }
            TunnelTarget::Dynamic => Ok(Destination::Socks),
        }
    }

    pub fn label(&self) -> String {
        let bind = if matches!(self.target, TunnelTarget::Remote { .. }) {
            format!("{} on SSH server", self.bind)
        } else {
            self.bind.to_string()
        };
        let exposure = if self.bind.ip().is_loopback() {
            ""
        } else {
            " · reachable from other devices"
        };
        match self.destination() {
            Ok(destination) => format!("{bind} → {destination}{exposure}"),
            Err(_) => "Invalid tunnel".into(),
        }
    }
}

fn tunnel_endpoint(host: &str, port: u16) -> io::Result<kortty_ssh::Endpoint> {
    if normalize_host(host).as_deref() != Ok(host) {
        return Err(invalid("invalid tunnel service hostname"));
    }
    kortty_ssh::Endpoint::new(host, port)
}

fn validate_credential_id(id: &str) -> io::Result<()> {
    if id.is_empty()
        || id.len() > 96
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(invalid("invalid vault credential ID"));
    }
    Ok(())
}

fn valid_text(value: &str, max: usize, empty: bool) -> bool {
    value.len() <= max && (empty || !value.trim().is_empty()) && !unsafe_text(value)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    next_id: u64,
    connections: Vec<Connection>,
    approvals: BTreeMap<u64, TunnelPlan>,
    #[serde(default, skip_serializing_if = "Library::is_empty")]
    library: Library,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: VERSION,
            next_id: 1,
            connections: Vec::new(),
            approvals: BTreeMap::new(),
            library: Library::default(),
        }
    }
}

impl Document {
    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION || self.next_id == 0 || self.connections.len() > MAX_PROFILES {
            return Err(invalid(
                "unsupported connection store version or profile count",
            ));
        }
        let mut seen = BTreeSet::new();
        for connection in &self.connections {
            connection.validate()?;
            if connection.id == 0 || connection.id >= self.next_id || !seen.insert(connection.id) {
                return Err(invalid("invalid or duplicate saved connection ID"));
            }
        }
        self.library.validate(&seen)?;
        if self.approvals.len() > self.connections.len()
            || self.approvals.iter().any(|(id, approval)| {
                approval.rules.is_empty()
                    || !self
                        .connections
                        .iter()
                        .any(|connection| connection.id == *id && connection.plan() == *approval)
            })
        {
            return Err(invalid(
                "connection store contains an invalid tunnel approval",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Store {
    path: PathBuf,
    document: Document,
}

impl Store {
    pub fn open(path: PathBuf) -> io::Result<Self> {
        let document = read_document(&path)?;
        Ok(Self { path, document })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn connections(&self) -> &[Connection] {
        &self.document.connections
    }

    pub fn get(&self, id: u64) -> Option<&Connection> {
        self.connections()
            .iter()
            .find(|connection| connection.id == id)
    }

    pub fn search<'a>(&'a self, query: &str) -> impl Iterator<Item = &'a Connection> + use<'a> {
        let query = Search::new(query);
        self.connections()
            .iter()
            .filter(move |connection| query.quick(connection))
    }

    pub fn group_of(&self, id: u64) -> Option<&str> {
        self.document
            .library
            .membership
            .get(&id)
            .map(String::as_str)
    }

    pub(crate) fn os_badge_id(&self, id: u64) -> Option<&str> {
        self.document.library.os_badges.get(&id).map(String::as_str)
    }

    pub(crate) fn os_badge(&self, id: u64) -> Option<OsBadge> {
        self.os_badge_id(id).and_then(OsBadge::find)
    }

    pub(crate) fn set_os_badge_id(&mut self, id: u64, badge: Option<&str>) -> io::Result<()> {
        self.set_os_badges(&[id], badge)
    }

    pub(crate) fn set_os_badges(&mut self, ids: &[u64], badge: Option<&str>) -> io::Result<()> {
        let ids = self.require_connections(ids)?;
        if badge.is_some_and(|value| !valid_os_badge_reference(value)) {
            return Err(invalid("invalid OS symbol reference"));
        }
        let mut candidate = self.document.clone();
        for id in ids {
            candidate.library.set_os_badge(id, badge);
        }
        self.commit(candidate)
    }

    pub(crate) fn group_color(&self, path: &str) -> Option<TabColor> {
        self.document.library.group_colors.get(path).copied()
    }

    pub(crate) fn inherited_group_color(&self, mut path: &str) -> Option<(&str, TabColor)> {
        loop {
            if let Some((source, color)) = self.document.library.group_colors.get_key_value(path) {
                return Some((source.as_str(), *color));
            }
            path = path.rsplit_once('/')?.0;
        }
    }

    pub(crate) fn ancestor_group_color(&self, path: &str) -> Option<(&str, TabColor)> {
        self.inherited_group_color(path.rsplit_once('/')?.0)
    }

    pub(crate) fn effective_tab_color(
        &self,
        connection: &Connection,
    ) -> Option<(TabColor, Option<&str>)> {
        if let Some(color) = connection.tab_color {
            return Some((color, None));
        }
        let (group, color) = self.inherited_group_color(self.group_of(connection.id)?)?;
        Some((color, Some(group)))
    }

    pub fn groups(&self) -> impl Iterator<Item = &str> {
        self.document.library.groups.iter().map(String::as_str)
    }

    pub(crate) fn has_group(&self, path: &str) -> bool {
        self.document.library.groups.contains(path)
    }

    pub fn in_group<'a>(&'a self, group: &'a str) -> impl Iterator<Item = &'a Connection> {
        self.connections()
            .iter()
            .filter(move |entry| self.is_in_group(entry.id, group))
    }

    pub(crate) fn is_in_group(&self, id: u64, group: &str) -> bool {
        self.group_of(id)
            .is_some_and(|path| group_suffix(path, group).is_some())
    }

    pub fn is_favorite(&self, id: u64) -> bool {
        self.document.library.favorites.contains(&id)
    }

    pub fn recent(&self) -> impl Iterator<Item = &Connection> {
        self.document
            .library
            .recent
            .iter()
            .filter_map(|id| self.get(*id))
    }

    pub(crate) fn recent_since(&self, cutoff: u64) -> impl Iterator<Item = (&Connection, u64)> {
        self.recent()
            .filter(|connection| !connection.incognito)
            .filter_map(move |connection| {
                let used_at = self
                    .document
                    .library
                    .used_at
                    .get(&connection.id)
                    .copied()
                    .unwrap_or(0);
                (cutoff == 0 || used_at > cutoff).then_some((connection, used_at))
            })
    }

    #[cfg(test)]
    pub fn record_used(&mut self, id: u64) -> io::Result<()> {
        self.record_used_at(id, use_timestamp())
    }

    pub(crate) fn record_used_at(&mut self, id: u64, time: u64) -> io::Result<()> {
        let original = self
            .get(id)
            .ok_or_else(|| invalid("saved connection no longer exists"))?;
        let _lock = StoreLock::acquire(&self.path)?;
        let mut candidate = read_document(&self.path)?;
        let Some(current) = candidate.connections.iter().find(|entry| entry.id == id) else {
            self.document = candidate;
            return Ok(());
        };
        if original.incognito
            || current.incognito
            || !current.matches_route(
                &original.login.endpoint,
                original.jump.as_ref().map(|login| &login.endpoint),
            )
        {
            self.document = candidate;
            return Ok(());
        }
        if candidate.library.record_use(id, time.max(1)) {
            self.write(&candidate)?;
        }
        self.document = candidate;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_favorite(&mut self, id: u64, favorite: bool) -> io::Result<()> {
        self.set_favorites(self.require_connections(&[id])?, favorite)
    }

    pub(crate) fn toggle_favorites(&mut self, ids: &[u64]) -> io::Result<bool> {
        let ids = self.require_connections(ids)?;
        let favorite = !ids.is_subset(&self.document.library.favorites);
        self.set_favorites(ids, favorite)?;
        Ok(favorite)
    }

    fn set_favorites(&mut self, ids: BTreeSet<u64>, favorite: bool) -> io::Result<()> {
        let mut candidate = self.document.clone();
        if favorite {
            candidate.library.favorites.extend(ids);
        } else {
            candidate.library.favorites.retain(|id| !ids.contains(id));
        }
        self.commit(candidate)
    }

    pub(crate) fn set_connection_tags(&mut self, ids: &[u64], tag: &str) -> io::Result<()> {
        let ids = self.require_connections(ids)?;
        if !valid_text(tag, 96, true) {
            return Err(invalid("invalid saved connection tag"));
        }
        let mut candidate = self.document.clone();
        for entry in &mut candidate.connections {
            if ids.contains(&entry.id) {
                tag.clone_into(&mut entry.tag);
            }
        }
        self.commit(candidate)
    }

    pub(crate) fn create_group(&mut self, parent: Option<&str>, name: &str) -> io::Result<String> {
        let mut candidate = self.document.clone();
        let path = candidate.library.create_group(parent, name)?;
        self.commit(candidate)?;
        Ok(path)
    }

    pub(crate) fn set_group_color(
        &mut self,
        path: &str,
        color: Option<TabColor>,
    ) -> io::Result<()> {
        let mut candidate = self.document.clone();
        candidate.library.require_group(Some(path))?;
        if let Some(color) = color {
            candidate
                .library
                .group_colors
                .insert(path.to_owned(), color);
        } else {
            candidate.library.group_colors.remove(path);
        }
        self.commit(candidate)
    }

    pub(crate) fn rename_group(&mut self, path: &str, name: &str) -> io::Result<String> {
        let mut candidate = self.document.clone();
        let path = candidate.library.rename_group(path, name)?;
        self.commit(candidate)?;
        Ok(path)
    }

    pub(crate) fn delete_group(&mut self, path: &str) -> io::Result<()> {
        let mut candidate = self.document.clone();
        let ids = candidate.library.remove_group(path)?;
        candidate
            .connections
            .retain(|entry| !ids.contains(&entry.id));
        candidate.approvals.retain(|id, _| !ids.contains(id));
        self.commit(candidate)
    }

    pub fn approved(&self, id: u64, plan: &TunnelPlan) -> bool {
        self.document.approvals.get(&id) == Some(plan)
    }

    pub fn save(&mut self, connection: Connection) -> io::Result<u64> {
        let group = self.group_of(connection.id).map(str::to_owned);
        self.save_in_group(connection, group.as_deref())
    }

    pub(crate) fn save_in_group(
        &mut self,
        connection: Connection,
        group: Option<&str>,
    ) -> io::Result<u64> {
        self.save_entry(connection, group, |_, _| {})
    }

    pub(crate) fn save_in_group_with_badge(
        &mut self,
        connection: Connection,
        group: Option<&str>,
        badge: Option<&str>,
    ) -> io::Result<u64> {
        self.save_entry(connection, group, |library, id| {
            library.set_os_badge(id, badge);
        })
    }

    fn save_entry(
        &mut self,
        mut connection: Connection,
        group: Option<&str>,
        update_library: impl FnOnce(&mut Library, u64),
    ) -> io::Result<u64> {
        connection.validate()?;
        let mut candidate = self.document.clone();
        candidate.library.require_group(group)?;
        if connection.id == 0 {
            connection.id = candidate.next_id;
            candidate.next_id = candidate
                .next_id
                .checked_add(1)
                .ok_or_else(|| invalid("connection ID space exhausted"))?;
            candidate.connections.push(connection.clone());
        } else {
            let original = candidate
                .connections
                .iter_mut()
                .find(|entry| entry.id == connection.id)
                .ok_or_else(|| invalid("saved connection no longer exists"))?;
            if original.plan() != connection.plan() {
                candidate.approvals.remove(&connection.id);
            }
            *original = connection.clone();
        }
        if let Some(group) = group {
            candidate
                .library
                .membership
                .insert(connection.id, group.to_owned());
        } else {
            candidate.library.membership.remove(&connection.id);
        }
        update_library(&mut candidate.library, connection.id);
        self.commit(candidate)?;
        Ok(connection.id)
    }

    #[cfg(test)]
    pub fn delete(&mut self, id: u64) -> io::Result<()> {
        self.delete_connections(&[id])
    }

    pub(crate) fn delete_connections(&mut self, ids: &[u64]) -> io::Result<()> {
        let ids = self.require_connections(ids)?;
        let mut candidate = self.document.clone();
        candidate
            .connections
            .retain(|entry| !ids.contains(&entry.id));
        candidate.approvals.retain(|id, _| !ids.contains(id));
        candidate.library.remove_connections(&ids);
        self.commit(candidate)
    }

    fn require_connections(&self, ids: &[u64]) -> io::Result<BTreeSet<u64>> {
        let ids: BTreeSet<_> = ids.iter().copied().collect();
        if ids.is_empty() {
            return Err(invalid("no saved connections selected"));
        }
        let found = self
            .connections()
            .iter()
            .filter(|entry| ids.contains(&entry.id))
            .count();
        if found != ids.len() {
            return Err(invalid("saved connection no longer exists"));
        }
        Ok(ids)
    }

    pub fn approve(&mut self, id: u64, plan: TunnelPlan) -> io::Result<()> {
        if plan.rules.is_empty() || self.get(id).is_none_or(|profile| profile.plan() != plan) {
            return Err(invalid("saved tunnel plan changed before approval"));
        }
        let mut candidate = self.document.clone();
        candidate.approvals.insert(id, plan);
        self.commit(candidate)
    }

    fn commit(&mut self, mut candidate: Document) -> io::Result<()> {
        let _lock = StoreLock::acquire(&self.path)?;
        let mut disk = read_document(&self.path)?;
        let recent = std::mem::replace(
            &mut disk.library.recent,
            self.document.library.recent.clone(),
        );
        let used_at = std::mem::replace(
            &mut disk.library.used_at,
            self.document.library.used_at.clone(),
        );
        if disk != self.document {
            return Err(io::Error::other(
                "saved connections changed in another instance; reload before retrying",
            ));
        }
        candidate.library.recent = recent
            .into_iter()
            .filter(|id| candidate.connections.iter().any(|entry| entry.id == *id))
            .collect();
        candidate.library.used_at = used_at
            .into_iter()
            .filter(|(id, _)| candidate.library.recent.contains(id))
            .collect();
        self.write(&candidate)?;
        self.document = candidate;
        Ok(())
    }

    fn write(&self, candidate: &Document) -> io::Result<()> {
        candidate.validate()?;
        let bytes = serde_json::to_vec_pretty(candidate).map_err(invalid)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("connection store exceeds its size limit"));
        }
        profile::save_atomic(&self.path, &bytes)
    }
}

pub(crate) fn use_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |elapsed| {
            elapsed.as_millis().try_into().unwrap_or(u64::MAX)
        })
        .max(1)
}

struct StoreLock(File);

impl StoreLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        let directory = path
            .parent()
            .ok_or_else(|| invalid("connection store has no directory"))?;
        fs::create_dir_all(directory)?;
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock_path = PathBuf::from(lock_path);
        if fs::symlink_metadata(&lock_path).is_ok_and(|metadata| !metadata.is_file()) {
            return Err(invalid("connection lock must be a regular file"));
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(lock_path)?;
        if !file.metadata()?.is_file() {
            return Err(invalid("connection lock must be a regular file"));
        }
        file.try_lock().map_err(|error| match error {
            fs::TryLockError::WouldBlock => {
                io::Error::other("connection store is being saved by another instance")
            }
            fs::TryLockError::Error(error) => error,
        })?;
        Ok(Self(file))
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn read_document(path: &Path) -> io::Result<Document> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(invalid("connection store must be a regular file"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Document::default()),
        Err(error) => return Err(error),
    }
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("connection store must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    decode_document(&bytes)
}

fn decode_document(bytes: &[u8]) -> io::Result<Document> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("connection store exceeds its size limit"));
    }
    let document: Document = serde_json::from_slice(bytes).map_err(invalid)?;
    document.validate()?;
    Ok(document)
}

pub(crate) fn portable_backup(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut document = decode_document(bytes)?;
    document.approvals.clear();
    serde_json::to_vec(&document).map_err(invalid)
}

pub(crate) fn validate_backup(bytes: &[u8]) -> io::Result<()> {
    if !decode_document(bytes)?.approvals.is_empty() {
        return Err(invalid("profile backup contains local tunnel approvals"));
    }
    Ok(())
}

fn invalid(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests;
