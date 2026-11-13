use std::io;
use std::path::PathBuf;

use kortty_terminal::{TerminalProfile, TextEncoding};
use roxmltree::Node;

use crate::profile::{InitialTerminalSize, PaneLayout, invalid};
use crate::session::{
    SerialConnection, SerialFlowControl, SerialLineEnding, SerialParity, SerialSettings,
    SerialStopBits,
};
use crate::session_security::{IsolationLevel, LocalSettings};
use crate::ssh_dialog::{LoginTarget, normalize_host, unsafe_text};
use crate::workspace::SshTarget;

use super::{Note, appearance::clamp_font, boolean, child, flag, has_any, number, tag, value};

const DEFAULT_FONT: u8 = 14;
pub(super) const JAVA_DEFAULT_INITIAL_SIZE: InitialTerminalSize = InitialTerminalSize {
    columns: Some(80),
    rows: Some(24),
};

pub(super) struct Tab<'a> {
    pub(super) connection: Option<&'a str>,
    pub(super) local: bool,
    pub(super) font: Option<u8>,
}

impl<'a> Tab<'a> {
    pub(super) fn read(node: Node<'a, '_>) -> io::Result<Self> {
        Ok(Self {
            connection: value(node, "connectionId")?,
            local: flag(node, "localShell")?,
            font: number::<i32>(node, "fontSizeOverride")?
                .filter(|&size| size > 0)
                .map(clamp_font),
        })
    }

    pub(super) fn local_pane(&self, focused: bool) -> PaneLayout {
        local(
            TerminalProfile::Xterm,
            self.font.unwrap_or(DEFAULT_FONT),
            focused,
            IsolationLevel::None,
            false,
        )
    }
}

pub(super) fn convert(
    tab: &Tab<'_>,
    connection: Node<'_, '_>,
    focused: bool,
) -> io::Result<Result<(PaneLayout, bool), Note>> {
    if flag(connection, "incognito")? {
        return Ok(Err(Note::PrivateConnection));
    }
    let protocol = value(connection, "protocol")?.unwrap_or("SSH_TCP");
    let encoding = value(connection, "encoding")?;
    if matches!(protocol, "MOSH" | "MOSH_CLIENT") {
        return Ok(Err(Note::Mosh));
    }
    let Some(profile) =
        terminal_profile(value(connection, "terminalEmulationType")?.unwrap_or("XTERM"))
    else {
        return Ok(Err(Note::UnsupportedConnection));
    };
    let Some(isolation) =
        IsolationLevel::from_id(value(connection, "isolationMode")?.unwrap_or("none"))
    else {
        return Ok(Err(Note::UnsupportedConnection));
    };
    let font = tab.font.unwrap_or(DEFAULT_FONT);
    let term = profile.term_name().into();
    let pane = match protocol {
        "LOCAL_SHELL" if isolation != IsolationLevel::Sandbox => {
            let integration = flag(connection, "shellIntegrationAutoInject")?;
            // Portable layouts cannot retain arbitrary launch commands or working directories.
            if value(connection, "localShellCommand")?.is_some()
                || value(connection, "localShellWorkingDirectory")?.is_some()
                || (integration && value(connection, "connectionSource")? == Some("TEAMWORK"))
            {
                return Ok(Err(Note::UnsupportedConnection));
            }
            local(profile, font, focused, isolation, integration)
        }
        "SSH_TCP" => {
            let target = ssh(connection, isolation);
            match target {
                Ok(target) => PaneLayout::Ssh {
                    font,
                    focused,
                    term,
                    target,
                    initial_size: InitialTerminalSize::default(),
                },
                Err(_) => return Ok(Err(Note::UnsupportedConnection)),
            }
        }
        "SERIAL" if isolation == IsolationLevel::None => {
            let settings =
                child(connection, "serial")?.ok_or_else(|| invalid("missing Java serial settings"));
            match settings
                .and_then(|settings| serial(settings, encoding.and_then(TextEncoding::from_name)))
            {
                Ok(connection) => PaneLayout::Serial {
                    font,
                    focused,
                    term,
                    connection,
                },
                Err(_) => return Ok(Err(Note::UnsupportedConnection)),
            }
        }
        _ => return Ok(Err(Note::UnsupportedConnection)),
    };
    let omitted = has_any(
        connection,
        &["terminalEffectPluginId", "tabColor", "highlightRuleSetId"],
    ) || encoding.is_some_and(|value| encoding_omitted(protocol, value));
    Ok(Ok((pane, omitted)))
}

pub(super) fn catalogue_pane(connection: Node<'_, '_>) -> io::Result<Option<PaneLayout>> {
    let protocol = value(connection, "protocol")?.unwrap_or("SSH_TCP");
    // Existing Java settings can change effective routes or introduce side effects on opening.
    if value(connection, "connectionSource")? == Some("TEAMWORK")
        || flag(connection, "disableHostKeyCheck")?
        || (protocol != "LOCAL_SHELL" && flag(connection, "shellIntegrationAutoInject")?)
        || value(connection, "localShellCommand")?.is_some()
        || value(connection, "localShellWorkingDirectory")?.is_some()
        || value(connection, "encoding")?.is_some_and(|value| encoding_omitted(protocol, value))
    {
        return Ok(None);
    }
    if !unpadded(connection, "host")? || !unpadded(connection, "username")? {
        return Ok(None);
    }
    if let Some(jump) = child(connection, "jumpServer")?
        && flag(jump, "enabled")?
        && (value(jump, "autoCommand")?.is_some()
            || !unpadded(jump, "host")?
            || !unpadded(jump, "username")?)
    {
        return Ok(None);
    }
    if let Some(serial) = child(connection, "serial")?
        && !unpadded(serial, "portName")?
    {
        return Ok(None);
    }
    for tunnel in connection
        .children()
        .filter(|node| tag(*node, "sshTunnels"))
    {
        if flag(tunnel, "enabled")? {
            return Ok(None);
        }
    }
    let probe = Tab {
        connection: None,
        local: false,
        font: Some(DEFAULT_FONT),
    };
    Ok(convert(&probe, connection, false)?
        .ok()
        .map(|(pane, _)| pane))
}

pub(super) fn catalogue_initial_size(connection: Node<'_, '_>) -> io::Result<InitialTerminalSize> {
    let Some(settings) = child(connection, "settings")? else {
        return Ok(JAVA_DEFAULT_INITIAL_SIZE);
    };
    // Java starts the shell with the connection's raw dimensions even when appearance inherits.
    let size = InitialTerminalSize {
        columns: number(settings, "terminalColumns")?.or(JAVA_DEFAULT_INITIAL_SIZE.columns),
        rows: number(settings, "terminalRows")?.or(JAVA_DEFAULT_INITIAL_SIZE.rows),
    };
    size.validate()?;
    Ok(size)
}

pub(super) fn remote_tool(connection: Node<'_, '_>) -> io::Result<Result<SshTarget, Note>> {
    if flag(connection, "incognito")? {
        return Ok(Err(Note::PrivateConnection));
    }
    if !matches!(value(connection, "protocol")?, None | Some("SSH_TCP")) {
        return Ok(Err(Note::UnsupportedConnection));
    }
    let Some(isolation) =
        IsolationLevel::from_id(value(connection, "isolationMode")?.unwrap_or("none"))
    else {
        return Ok(Err(Note::UnsupportedConnection));
    };
    Ok(ssh(connection, isolation).map_err(|_| Note::UnsupportedConnection))
}

fn encoding_omitted(protocol: &str, value: &str) -> bool {
    match TextEncoding::from_name(value) {
        Some(TextEncoding::Utf8) => false,
        Some(_) => protocol != "SERIAL",
        None => true,
    }
}

fn unpadded(node: Node<'_, '_>, name: &str) -> io::Result<bool> {
    // JAXB retains whitespace in strings, while the offline import trims fields.
    let Some(field) = child(node, name)? else {
        return Ok(true);
    };
    Ok(field
        .children()
        .filter(Node::is_text)
        .filter_map(|text| text.text())
        .all(|text| text.trim() == text))
}

fn local(
    profile: TerminalProfile,
    font: u8,
    focused: bool,
    isolation: IsolationLevel,
    shell_integration: bool,
) -> PaneLayout {
    PaneLayout::Terminal {
        font,
        focused,
        initial_size: InitialTerminalSize::default(),
        vt100: profile == TerminalProfile::Vt100,
        term: (!matches!(profile, TerminalProfile::Xterm | TerminalProfile::Vt100))
            .then(|| profile.term_name().into()),
        security: (isolation != IsolationLevel::None || shell_integration).then_some(
            LocalSettings {
                isolation,
                shell_integration,
                ..LocalSettings::default()
            },
        ),
    }
}

fn terminal_profile(value: &str) -> Option<TerminalProfile> {
    match value.to_ascii_uppercase().as_str() {
        "XTERM" => Some(TerminalProfile::Xterm),
        "SUN_CDE" => Some(TerminalProfile::SunCde),
        "HP700_92" => Some(TerminalProfile::Hp70092),
        name => TerminalProfile::from_term_name(&name.to_ascii_lowercase()),
    }
}

fn ssh(connection: Node<'_, '_>, isolation: IsolationLevel) -> io::Result<SshTarget> {
    let jump = match child(connection, "jumpServer")? {
        Some(jump) if flag(jump, "enabled")? => Some(login(jump)?),
        _ => None,
    };
    SshTarget::from_imported_route(login(connection)?, jump, isolation)
}

fn login(node: Node<'_, '_>) -> io::Result<LoginTarget> {
    let host = value(node, "host")?.ok_or_else(|| invalid("missing Java SSH host"))?;
    let username = value(node, "username")?.ok_or_else(|| invalid("missing Java SSH user"))?;
    Ok(LoginTarget {
        host: normalize_host(host).map_err(invalid)?,
        port: number::<u16>(node, "port")?.unwrap_or(22),
        username: username.into(),
    })
}

fn serial(node: Node<'_, '_>, encoding: Option<TextEncoding>) -> io::Result<SerialConnection> {
    let device = value(node, "portName")?
        .filter(|path| !unsafe_text(path))
        .map(PathBuf::from)
        .ok_or_else(|| invalid("missing or unsafe Java serial port name"))?;
    #[cfg(unix)]
    if !device.starts_with("/dev")
        || device
            .components()
            .any(|part| part == std::path::Component::ParentDir)
    {
        return Err(invalid("Java serial port must name a device under /dev"));
    }
    let parity = match value(node, "parity")?.unwrap_or("NONE") {
        "NONE" => SerialParity::None,
        "ODD" => SerialParity::Odd,
        "EVEN" => SerialParity::Even,
        "MARK" => SerialParity::Mark,
        "SPACE" => SerialParity::Space,
        _ => return Err(invalid("unknown Java serial parity")),
    };
    let stop_bits = match value(node, "stopBits")?.unwrap_or("ONE") {
        "ONE" => SerialStopBits::One,
        "ONE_POINT_FIVE" => SerialStopBits::OnePointFive,
        "TWO" => SerialStopBits::Two,
        _ => return Err(invalid("unknown Java serial stop bits")),
    };
    let flow_control = match value(node, "flowControl")?.unwrap_or("NONE") {
        "NONE" => SerialFlowControl::None,
        "RTS_CTS" => SerialFlowControl::RtsCts,
        "XON_XOFF" => SerialFlowControl::XonXoff,
        "DSR_DTR" => SerialFlowControl::DsrDtr,
        _ => return Err(invalid("unknown Java serial flow control")),
    };
    let line_ending = match value(node, "lineEnding")?.unwrap_or("CR") {
        "CR" => SerialLineEnding::Cr,
        "LF" => SerialLineEnding::Lf,
        "CRLF" => SerialLineEnding::CrLf,
        _ => return Err(invalid("unknown Java serial line ending")),
    };
    let connection = SerialConnection {
        device,
        settings: SerialSettings {
            baud_rate: number(node, "baudRate")?.unwrap_or(9600),
            data_bits: number(node, "dataBits")?.unwrap_or(8),
            parity,
            stop_bits,
            flow_control,
            assert_dtr: boolean(node, "assertDtr")?.unwrap_or(true),
            assert_rts: boolean(node, "assertRts")?.unwrap_or(true),
        },
        line_ending,
        local_echo: flag(node, "localEcho")?,
        encoding,
    };
    connection.validate()?;
    Ok(connection)
}
