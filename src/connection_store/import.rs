//! Bounded previews and atomic imports of local connection files.

use std::collections::{BTreeSet, HashSet};
use std::io::{self, Read as _};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use kortty_vault::Secret;
use zeroize::Zeroizing;

use super::{
    Auth, Connection, LocalProfile, LoginTarget, MAX_BYTES, MAX_PROFILES, MAX_TUNNELS, SavedLogin,
    SerialProfile, Store, TerminalProfile, TunnelConfig, TunnelRule, TunnelTarget, invalid,
    library, normalize_host,
};
use crate::private_files::FileStamp;

mod archive;
mod java_bindings;
mod kortty;
mod mobaxterm;
mod options;
mod teamwork;
mod xml;

pub(crate) use java_bindings::{
    JavaConnectionBindings, JavaImportedRoute, JavaManagedKeyBinding, JavaPasswordBinding,
    check_java_connection_routes, check_java_imported_routes, prepare_java_connection_import,
};
pub(crate) use options::{CredentialReplacement, Destination, Options, Prepared, SourceGroup};
pub(crate) use teamwork::{
    MAX_TEAMWORK_BYTES, TeamworkEntry, read_teamwork, read_teamwork_bytes, valid_teamwork_id,
};

const COLUMNS: [&str; 10] = [
    "Name",
    "Protocol",
    "Host",
    "Port",
    "Username",
    "Group",
    "LocalTunnels",
    "RemoteTunnels",
    "DynamicTunnels",
    "Comment",
];

#[derive(Default)]
pub(crate) struct Preview {
    format: Format,
    entries: Vec<ImportedProfile>,
    local_entries: Vec<ImportedLocal>,
    serial_entries: Vec<ImportedSerial>,
    pub(crate) unsupported: usize,
    pub(crate) invalid: usize,
    pub(crate) duplicates: usize,
    pub(crate) skipped_tunnels: usize,
    java_key_references: usize,
}

#[derive(Clone)]
struct ImportedProfile {
    connection: Connection,
    java_id: Option<String>,
    java_key_id: Option<String>,
    java_route: Option<(LoginTarget, Option<LoginTarget>)>,
    group: String,
    badge: Option<String>,
    has_password: bool,
    jump_has_password: bool,
}

#[derive(Clone)]
struct ImportedLocal {
    profile: LocalProfile,
    group: String,
}

#[derive(Clone)]
struct ImportedSerial {
    profile: SerialProfile,
    group: String,
}

impl ImportedProfile {
    fn new(connection: Connection, group: String) -> Self {
        Self {
            connection,
            java_id: None,
            java_key_id: None,
            java_route: None,
            group,
            badge: None,
            has_password: false,
            jump_has_password: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Format {
    #[default]
    PuttyCm,
    Mtputty,
    Kortty,
    Mobaxterm,
}

impl Format {
    pub(crate) fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?;
        if extension.eq_ignore_ascii_case("csv") {
            Some(Self::PuttyCm)
        } else if extension.eq_ignore_ascii_case("xml") {
            Some(Self::Mtputty)
        } else if extension.eq_ignore_ascii_case("mxtsessions")
            || extension.eq_ignore_ascii_case("ini")
        {
            Some(Self::Mobaxterm)
        } else {
            None
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::PuttyCm => "PuTTY Connection Manager CSV",
            Self::Mtputty => "MTPuTTY XML",
            Self::Kortty => "kortty XML",
            Self::Mobaxterm => "MobaXterm sessions",
        }
    }
}

/// Unlock material supplied explicitly for the chosen import file.
pub(crate) enum Access<'a> {
    Plain,
    ZipPassword(&'a Secret),
    OpenPgp {
        private_key: &'a Path,
        passphrase: &'a Secret,
    },
}

#[derive(Debug)]
pub(crate) enum ReadOutcome {
    Preview(Preview),
    ZipPasswordRequired,
    OpenPgpKeyRequired,
}

pub(crate) fn supports_path(path: &Path) -> bool {
    Format::from_path(path).is_some() || archive::Kind::from_path(path).is_some()
}

impl std::fmt::Debug for Preview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionImportPreview")
            .field(
                "entries",
                &(self.entries.len() + self.local_entries.len() + self.serial_entries.len()),
            )
            .finish_non_exhaustive()
    }
}

/// Checks that generated XML fits the same parser limits used on reimport.
pub(crate) fn check_xml_export(content: &str) -> io::Result<()> {
    xml::parse_document(content).map(|_| ()).map_err(|error| {
        if matches!(error, roxmltree::Error::NodesLimitReached) {
            invalid("Connection XML exceeds the import node limit. Select fewer connections and export them in separate files.")
        } else {
            invalid("The connection export contains invalid XML.")
        }
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) imported: usize,
    pub(crate) already_saved: usize,
}

impl Preview {
    pub(crate) fn java_backup(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("connection import exceeds its size limit"));
        }
        let preview = Self::parse_bytes(Format::Kortty, bytes)?;
        if preview.format != Format::Kortty {
            return Err(invalid("expected Java connections.xml"));
        }
        Ok(preview)
    }

    pub(crate) fn portable_java_backup(&self) -> io::Result<(Vec<u8>, usize)> {
        options::portable_java_backup(self)
    }

    pub(crate) fn merge_java_backup(
        local: Option<&[u8]>,
        incoming: &[u8],
    ) -> io::Result<Option<Vec<u8>>> {
        options::merge_java_backup(local, incoming)
    }

    #[cfg(test)]
    pub(crate) fn read(path: &Path) -> io::Result<Self> {
        match Self::read_with(path, Access::Plain)? {
            ReadOutcome::Preview(preview) => Ok(preview),
            ReadOutcome::ZipPasswordRequired => Err(invalid("the connection ZIP needs a password")),
            ReadOutcome::OpenPgpKeyRequired => Err(invalid(
                "the OpenPGP connection archive needs a private key",
            )),
        }
    }

    #[cfg(test)]
    pub(crate) fn read_with(path: &Path, access: Access<'_>) -> io::Result<ReadOutcome> {
        Self::read_verified(path, access, None).map(|(outcome, _)| outcome)
    }

    pub(crate) fn read_verified(
        path: &Path,
        access: Access<'_>,
        expected: Option<&FileStamp>,
    ) -> io::Result<(ReadOutcome, FileStamp)> {
        if !path.is_absolute() {
            return Err(invalid("choose an absolute path to a connection file"));
        }
        if let Some(kind) = archive::Kind::from_path(path) {
            let (content, stamp) = read_stable(path, kind.size_limit(), expected)?;
            return archive::read(&content, kind, access).map(|outcome| (outcome, stamp));
        }
        let format = Format::from_path(path).ok_or_else(|| {
            invalid("choose a connection XML, CSV, MobaXterm sessions file, ZIP or OpenPGP archive")
        })?;
        let (content, stamp) = read_stable(path, MAX_BYTES, expected)?;
        Self::parse_bytes(format, &content).map(|preview| (ReadOutcome::Preview(preview), stamp))
    }

    fn parse_bytes(format: Format, content: &[u8]) -> io::Result<Self> {
        match format {
            Format::PuttyCm => Self::parse(
                std::str::from_utf8(content).map_err(|_| invalid("PuTTY CM CSV must use UTF-8"))?,
            ),
            Format::Mtputty | Format::Kortty => xml::parse(content),
            Format::Mobaxterm => mobaxterm::parse(content),
        }
    }

    pub(crate) fn format(&self) -> Format {
        self.format
    }

    pub(crate) fn entries(&self) -> impl ExactSizeIterator<Item = (&Connection, &str)> {
        self.entries
            .iter()
            .map(|entry| (&entry.connection, entry.group.as_str()))
    }

    pub(crate) fn source_groups(&self) -> BTreeSet<SourceGroup> {
        self.entries
            .iter()
            .map(|entry| &entry.group)
            .chain(self.local_entries.iter().map(|entry| &entry.group))
            .chain(self.serial_entries.iter().map(|entry| &entry.group))
            .map(|group| SourceGroup::from_path(group))
            .collect()
    }

    pub(crate) fn prepare(&self, options: &Options, store: &Store) -> io::Result<Prepared> {
        Prepared::new(self, options, store)
    }

    fn parse(content: &str) -> io::Result<Self> {
        let mut lines = content.trim_start_matches('\u{feff}').lines();
        let header = lines.next().and_then(fields).ok_or_else(bad_header)?;
        if header.len() < 4
            || header
                .iter()
                .zip(COLUMNS)
                .any(|(name, expected)| !name.trim().eq_ignore_ascii_case(expected))
        {
            return Err(bad_header());
        }
        let mut result = Self::default();
        for line in lines.filter(|line| !line.trim().is_empty()) {
            let Some(row) = fields(line).filter(|row| (4..=header.len()).contains(&row.len()))
            else {
                result.invalid += 1;
                continue;
            };
            if row[1].trim().eq_ignore_ascii_case("serial") {
                let rate = row[3].trim().parse().ok().filter(|&rate| rate != 22);
                let group = row.get(5).map_or("", |value| value.trim());
                match serial_bookmark(row[0].trim(), row[2].trim(), rate, group) {
                    Ok(entry) => result.push_serial(entry)?,
                    Err(_) => result.invalid += 1,
                }
                continue;
            }
            if !row[1].trim().eq_ignore_ascii_case("ssh") {
                result.unsupported += 1;
                continue;
            }
            let Ok((entry, group, skipped_tunnels)) = csv_entry(&row) else {
                result.invalid += 1;
                continue;
            };
            if result.push(ImportedProfile::new(entry, group))? {
                result.skipped_tunnels += skipped_tunnels;
            }
        }
        Ok(result)
    }

    fn push(&mut self, entry: ImportedProfile) -> io::Result<bool> {
        if let Some(previous) = self.entries.iter_mut().find(|previous| {
            same_profile(
                &previous.connection,
                &previous.group,
                &entry.connection,
                &entry.group,
            )
        }) {
            previous.java_route = None;
            previous.java_key_id = None;
            self.duplicates += 1;
            return Ok(false);
        }
        self.check_capacity()?;
        self.entries.push(entry);
        Ok(true)
    }

    fn push_local(&mut self, entry: ImportedLocal) -> io::Result<()> {
        if self
            .local_entries
            .iter()
            .any(|previous| previous.profile == entry.profile && previous.group == entry.group)
        {
            self.duplicates += 1;
        } else {
            self.check_capacity()?;
            self.local_entries.push(entry);
        }
        Ok(())
    }

    fn check_capacity(&self) -> io::Result<()> {
        if self.entries.len() + self.local_entries.len() + self.serial_entries.len() >= MAX_PROFILES
        {
            return Err(invalid(
                "connection import contains more than 512 distinct profiles",
            ));
        }
        Ok(())
    }

    fn push_serial(&mut self, entry: ImportedSerial) -> io::Result<()> {
        if self
            .serial_entries
            .iter()
            .any(|previous| previous.profile == entry.profile && previous.group == entry.group)
        {
            self.duplicates += 1;
        } else {
            self.check_capacity()?;
            self.serial_entries.push(entry);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn apply(&self, store: &mut Store) -> io::Result<Outcome> {
        self.prepare(&Options::default(), store)?.apply(store)
    }
}

fn read_stable(
    path: &Path,
    limit: u64,
    expected: Option<&FileStamp>,
) -> io::Result<(Zeroizing<Vec<u8>>, FileStamp)> {
    let mut file = crate::profile::open_preference(path)?;
    let original = FileStamp::read(&file)?;
    if expected.is_some_and(|expected| expected != &original) {
        return Err(invalid(
            "the connection file changed. Preview it again before unlocking it",
        ));
    }
    let size = file.metadata()?.len();
    if size > limit {
        return Err(invalid("connection import exceeds its size limit"));
    }
    let mut content = Zeroizing::new(Vec::with_capacity(size as usize));
    (&mut file).take(limit + 1).read_to_end(&mut content)?;
    original.require_unchanged(&file)?;
    original.require_unchanged(&crate::profile::open_preference(path)?)?;
    if content.len() as u64 > limit {
        return Err(invalid("connection import exceeds its size limit"));
    }
    Ok((content, original))
}

fn bad_header() -> io::Error {
    invalid("expected a PuTTY CM CSV with Name, Protocol, Host and Port columns")
}

fn serial_bookmark(
    name: &str,
    device: &str,
    baud_rate: Option<u32>,
    group: &str,
) -> io::Result<ImportedSerial> {
    let mut connection = crate::session::SerialConnection {
        device: PathBuf::from(device),
        ..Default::default()
    };
    if let Some(baud_rate) = baud_rate.filter(|rate| (1..=20_000_000).contains(rate)) {
        connection.settings.baud_rate = baud_rate;
    }
    let profile = SerialProfile::new(name, connection, TerminalProfile::default());
    profile.validate()?;
    Ok(ImportedSerial {
        profile,
        group: import_group(group)?,
    })
}

fn import_group(group: &str) -> io::Result<String> {
    let group = group.replace('\\', "/");
    if !group.is_empty() && !library::valid_path(&group) {
        return Err(invalid("invalid imported connection folder"));
    }
    Ok(group)
}

// PuTTY CM writes one physical line per profile. Reject malformed quotes and
// extra fields so commas can never shift the host, user or forwarding columns.
fn fields(mut line: &str) -> Option<Vec<String>> {
    let mut result = Vec::new();
    loop {
        if result.len() == COLUMNS.len() {
            return None;
        }
        line = line.trim_start_matches([' ', '\t']);
        if let Some(mut quoted) = line.strip_prefix('"') {
            let mut field = String::new();
            loop {
                let (part, rest) = quoted.split_once('"')?;
                field.push_str(part);
                if let Some(rest) = rest.strip_prefix('"') {
                    field.push('"');
                    quoted = rest;
                } else {
                    line = rest.trim_start_matches([' ', '\t']);
                    break;
                }
            }
            result.push(field);
        } else {
            let split = line.find(',').unwrap_or(line.len());
            let (field, rest) = line.split_at(split);
            if field.contains('"') {
                return None;
            }
            result.push(field.to_owned());
            line = rest;
        }
        if line.is_empty() {
            return Some(result);
        }
        line = line.strip_prefix(',')?;
    }
}

fn csv_entry(row: &[String]) -> io::Result<(Connection, String, usize)> {
    let value = |index: usize| row.get(index).map_or("", |text| text.trim());
    let mut tunnels = Vec::new();
    let mut skipped = 0;
    let mut bindings = HashSet::new();
    for column in 6..=8 {
        for spec in value(column)
            .split(';')
            .map(str::trim)
            .filter(|spec| !spec.is_empty())
        {
            match tunnel(column, spec) {
                Ok(tunnel)
                    if tunnels.len() < MAX_TUNNELS
                        && bindings.insert((column == 7, tunnel.rule.bind)) =>
                {
                    tunnels.push(tunnel);
                }
                _ => skipped += 1,
            }
        }
    }
    let (connection, group) =
        ssh_profile(value(0), value(2), value(3), value(4), value(5), tunnels)?;
    Ok((connection, group, skipped))
}

fn ssh_profile(
    name: &str,
    host: &str,
    port_field: &str,
    username: &str,
    group: &str,
    tunnels: Vec<TunnelConfig>,
) -> io::Result<(Connection, String)> {
    let host = normalize_host(host).map_err(|_| invalid("invalid SSH host"))?;
    let port = if port_field.is_empty() {
        22
    } else {
        port(port_field)?
    };
    let group = import_group(group)?;
    let connection = Connection {
        id: 0,
        name: name.to_owned(),
        tag: String::new(),
        login: SavedLogin {
            endpoint: LoginTarget {
                host,
                port,
                username: username.to_owned(),
            },
            auth: Auth::Prompt,
            keyboard_interactive: false,
        },
        jump: None,
        emulation: TerminalProfile::Xterm.term_name().into(),
        tunnels,
        incognito: false,
        isolation: Default::default(),
        startup: Default::default(),
        logging: Default::default(),
        terminal_effect: None,
        command_timestamps: None,
        tab_color: None,
        paste_warning: None,
        window_geometry: None,
        terminal: Default::default(),
    };
    connection.validate()?;
    Ok((connection, group))
}

fn tunnel(column: usize, spec: &str) -> io::Result<TunnelConfig> {
    let (bind_port, target) = if column == 8 {
        (port(spec)?, TunnelTarget::Dynamic)
    } else {
        let mut parts = spec.split(':').map(str::trim);
        let (Some(bind), Some(host), Some(target_port), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(invalid("invalid PuTTY CM forwarding rule"));
        };
        let host = if host.is_empty() { "localhost" } else { host };
        let host = normalize_host(host).map_err(|_| invalid("invalid forwarding host"))?;
        let port = port(target_port)?;
        let target = if column == 6 {
            TunnelTarget::Local { host, port }
        } else {
            TunnelTarget::Remote { host, port }
        };
        (self::port(bind)?, target)
    };
    let tunnel = TunnelConfig {
        enabled: false,
        description: String::new(),
        rule: TunnelRule {
            bind: (Ipv4Addr::LOCALHOST, bind_port).into(),
            target,
        },
    };
    tunnel.validate()?;
    Ok(tunnel)
}

fn port(value: &str) -> io::Result<u16> {
    value
        .parse()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| invalid("invalid SSH port"))
}

fn same_profile(a: &Connection, a_group: &str, b: &Connection, b_group: &str) -> bool {
    a.name == b.name && a_group == b_group && a.login.endpoint == b.login.endpoint
}

fn ensure_group(library: &mut library::Library, group: &str) -> io::Result<()> {
    let mut parent = None;
    for (index, name) in group.split('/').scan(0, |offset, name| {
        *offset += name.len() + 1;
        Some((*offset - 1, name))
    }) {
        let path = &group[..index];
        if !library.groups.contains(path) {
            library.create_group(parent, name)?;
        }
        parent = Some(path);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
