//! Portable, credential-free exports of selected saved connection profiles.

use std::fmt::{self, Write as _};
use std::io;

use crate::connection_store::{
    Connection, LocalProfile, MAX_GROUP_PATH_BYTES, SerialProfile, TunnelConfig, TunnelTarget,
};
use crate::ssh_dialog::unsafe_text;
use crate::terminal_theme::Themes;

pub(crate) mod protection;
mod xml;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Format {
    #[default]
    Kortty,
    Mtputty,
    Mobaxterm,
    PuttyCm,
}

impl Format {
    pub(crate) const ALL: [Self; 4] = [Self::Kortty, Self::Mtputty, Self::Mobaxterm, Self::PuttyCm];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Kortty => "kortty XML",
            Self::Mtputty => "MTPuTTY XML",
            Self::Mobaxterm => "MobaXterm sessions",
            Self::PuttyCm => "PuTTY Connection Manager CSV",
        }
    }

    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Kortty | Self::Mtputty => "xml",
            Self::Mobaxterm => "mxtsessions",
            Self::PuttyCm => "csv",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

pub(crate) struct Entry<'a> {
    pub(crate) connection: &'a Connection,
    pub(crate) group: &'a str,
    pub(crate) os_badge: Option<&'a str>,
}

pub(crate) struct LocalEntry<'a> {
    pub(crate) profile: &'a LocalProfile,
    pub(crate) group: &'a str,
}

pub(crate) struct SerialEntry<'a> {
    pub(crate) profile: &'a SerialProfile,
    pub(crate) group: &'a str,
}

pub(crate) fn java_local_profile(profile: &LocalProfile) -> bool {
    profile.shell.is_default()
        && profile.command.as_ref().is_none_or(|command| {
            command.iter().all(|word| {
                !word.is_empty()
                    && !word.contains(['"', '\u{fffe}', '\u{ffff}'])
                    && !word.chars().any(char::is_control)
            })
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Options {
    pub(crate) include_username: bool,
    pub(crate) include_tunnels: bool,
    pub(crate) include_jump_server: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            include_username: true,
            include_tunnels: true,
            include_jump_server: true,
        }
    }
}

impl Options {
    fn username(self, username: &str) -> &str {
        if self.include_username { username } else { "" }
    }

    fn tunnels(self, connection: &Connection) -> &[TunnelConfig] {
        if self.include_tunnels {
            &connection.tunnels
        } else {
            &[]
        }
    }
}

#[cfg(test)]
fn encode(format: Format, entries: &[Entry<'_>]) -> io::Result<Vec<u8>> {
    encode_with_options(format, entries, Options::default())
}

#[cfg(test)]
fn encode_with_options(
    format: Format,
    entries: &[Entry<'_>],
    options: Options,
) -> io::Result<Vec<u8>> {
    encode_profiles(format, entries, &[], &[], options, None)
}

/// Serializes the requested profiles without consulting the vault or filesystem.
pub(crate) fn encode_profiles(
    format: Format,
    entries: &[Entry<'_>],
    locals: &[LocalEntry<'_>],
    serials: &[SerialEntry<'_>],
    options: Options,
    themes: Option<&Themes>,
) -> io::Result<Vec<u8>> {
    if !(1..=512).contains(&(entries.len() + locals.len() + serials.len())) {
        return Err(invalid("Select between 1 and 512 connections to export."));
    }
    if !locals.is_empty() && format != Format::Kortty {
        return Err(invalid(
            "Local shell profiles require the kortty XML format.",
        ));
    }
    if !serials.is_empty() && format != Format::Kortty {
        return Err(invalid("Serial profiles require the kortty XML format."));
    }
    for entry in entries {
        entry.connection.validate()?;
    }
    for entry in locals {
        entry.profile.validate()?;
    }
    for entry in serials {
        entry.profile.validate()?;
    }
    for group in entries
        .iter()
        .map(|entry| entry.group)
        .chain(locals.iter().map(|entry| entry.group))
        .chain(serials.iter().map(|entry| entry.group))
    {
        if group.len() > MAX_GROUP_PATH_BYTES || unsafe_text(group) {
            return Err(invalid("An exported connection has an invalid folder."));
        }
    }
    let result = match format {
        Format::Kortty => xml::kortty(entries, locals, serials, options, themes)?,
        Format::Mtputty => xml::mtputty(entries, options),
        Format::Mobaxterm => mobaxterm(entries, options)?,
        Format::PuttyCm => putty_cm(entries, options)?,
    };
    if result.len() > 1024 * 1024 {
        return Err(invalid(
            "Connection export exceeds 1 MiB. Select fewer connections.",
        ));
    }
    if matches!(format, Format::Kortty | Format::Mtputty) {
        if result.contains(['\u{fffe}', '\u{ffff}']) {
            return Err(invalid(
                "An exported connection contains an invalid XML character.",
            ));
        }
        crate::connection_store::import::check_xml_export(&result)?;
    }
    Ok(result.into_bytes())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn putty_cm(entries: &[Entry<'_>], options: Options) -> io::Result<String> {
    let mut output = String::from(
        "\u{feff}Name,Protocol,Host,Port,Username,Group,LocalTunnels,RemoteTunnels,DynamicTunnels,Comment\r\n",
    );
    for Entry {
        connection, group, ..
    } in entries
    {
        let endpoint = &connection.login.endpoint;
        let mut forwards: [Vec<String>; 3] = Default::default();
        for tunnel in options
            .tunnels(connection)
            .iter()
            .filter(|tunnel| tunnel.enabled)
        {
            let listen = tunnel.rule.bind.port();
            match &tunnel.rule.target {
                TunnelTarget::Local { host, port } | TunnelTarget::Remote { host, port }
                    if !host.contains(':') =>
                {
                    let column =
                        usize::from(matches!(tunnel.rule.target, TunnelTarget::Remote { .. }));
                    forwards[column].push(format!("{listen}:{host}:{port}"));
                }
                TunnelTarget::Dynamic => forwards[2].push(listen.to_string()),
                _ => {}
            }
        }
        let port = endpoint.port.to_string();
        let group = group.replace('/', "\\");
        let [local, remote, dynamic] = forwards.map(|rules| rules.join(";"));
        let fields = [
            connection.name.as_str(),
            "SSH",
            endpoint.host.as_str(),
            &port,
            options.username(&endpoint.username),
            &group,
            &local,
            &remote,
            &dynamic,
            "Exported from kortty",
        ];
        if fields
            .iter()
            .any(|field| field.trim_start().starts_with(['=', '+', '-', '@']))
        {
            return Err(invalid(
                "A field starts with a spreadsheet formula marker. Export these connections as XML.",
            ));
        }
        for (index, field) in fields.into_iter().enumerate() {
            if index != 0 {
                output.push(',');
            }
            if field.contains([',', '"']) {
                output.push('"');
                output.push_str(&field.replace('"', "\"\""));
                output.push('"');
            } else {
                output.push_str(field);
            }
        }
        output.push_str("\r\n");
    }
    Ok(output)
}

fn mobaxterm(entries: &[Entry<'_>], options: Options) -> io::Result<String> {
    use std::collections::HashSet;

    const SSH_SETTINGS: &str = "%-1%-1%-1%-1%0%0%%%-1%0%0%0%%1080%%0%0%1";
    const APPEARANCE: &str = "#MobaFont%10%0%0%-1%15%236,236,236%30,30,30%180,180,192%0%-1%0%%xterm%-1%-1%_Std_Colors_0_%80%24%0%1%-1%<none>%%0%1%-1%-1#0#";

    let mut groups: Vec<(&str, Vec<&Connection>)> = Vec::new();
    for Entry {
        connection, group, ..
    } in entries
    {
        if let Some((_, members)) = groups.iter_mut().find(|(folder, _)| folder == group) {
            members.push(connection);
        } else {
            groups.push((group, vec![connection]));
        }
    }
    let mut output = String::new();
    for (index, (group, connections)) in groups.into_iter().enumerate() {
        if index == 0 {
            output.push_str("[Bookmarks]\r\n");
        } else {
            let _ = write!(output, "[Bookmarks_{index}]\r\n");
        }
        let _ = write!(output, "SubRep={group}\r\nImgNum=41\r\n");
        let mut names = HashSet::new();
        for connection in connections {
            let endpoint = &connection.login.endpoint;
            let username = options.username(&endpoint.username);
            let name = connection.name.trim().replace(['=', '%', '#'], "_");
            let key = name.to_lowercase();
            if name.starts_with([';', '['])
                || matches!(key.as_str(), "subrep" | "imgnum")
                || !names.insert(key)
                || username.contains(['%', '#'])
            {
                return Err(invalid(
                    "A connection name or username cannot be represented in MobaXterm sessions. Export these connections as XML.",
                ));
            }
            let _ = write!(
                output,
                "{name}=#109#0%{}%{}%{}{SSH_SETTINGS}{APPEARANCE}\r\n",
                endpoint.host, endpoint.port, username,
            );
            for tunnel in options
                .tunnels(connection)
                .iter()
                .filter(|tunnel| tunnel.enabled)
            {
                let bind = tunnel.rule.bind;
                output.push_str("# Tunnel: ");
                match &tunnel.rule.target {
                    TunnelTarget::Local { host, port } => {
                        let _ = write!(output, "LOCAL {bind} -> {host}:{port}");
                    }
                    TunnelTarget::Remote { host, port } => {
                        let _ = write!(output, "REMOTE {bind} -> {host}:{port}");
                    }
                    TunnelTarget::Dynamic => {
                        let _ = write!(output, "DYNAMIC (SOCKS) {bind}");
                    }
                }
                if !tunnel.description.is_empty() {
                    // The Java importer tries to parse even comment lines as
                    // bookmarks if they contain an ASCII key/value delimiter.
                    let description = tunnel.description.replace('=', "＝");
                    let _ = write!(output, " - {description}");
                }
                output.push_str("\r\n");
            }
        }
        output.push_str("\r\n");
    }
    Ok(output)
}

pub(crate) fn java_emulation(profile: kortty_terminal::TerminalProfile) -> String {
    use kortty_terminal::TerminalProfile::{Hp70092, SunCde, Xterm};

    match profile {
        Xterm => "XTERM".into(),
        SunCde => "SUN_CDE".into(),
        Hp70092 => "HP700_92".into(),
        other => other.term_name().to_ascii_uppercase(),
    }
}

#[cfg(test)]
mod tests;
