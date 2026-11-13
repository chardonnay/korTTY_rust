use std::collections::HashSet;
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::LazyLock;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use kortty_pty::LocalShell;
use kortty_ssh::{ConnectOptions, KeepAlive};
use kortty_terminal::{ScrollbackConfig, TextEncoding};
use regex::Regex;
use roxmltree::Node;

use super::xml::{has_unqualified_tag, optional_child, raw_scalar};
use super::{
    Auth, Format, ImportedLocal, ImportedProfile, ImportedSerial, LocalProfile, LoginTarget,
    MAX_PROFILES, MAX_TUNNELS, Preview, SavedLogin, SerialProfile, TerminalProfile, TunnelConfig,
    TunnelRule, TunnelTarget, invalid, normalize_host, port, ssh_profile,
};
use crate::clipboard_policy::PasteWarningMode;
use crate::connection_store::{
    AuthMethod, JAVA_ANSI_FIELDS, TabColor, TerminalOverrides, ThemeOverride, WindowGeometry,
    valid_os_badge_reference,
};
use crate::session_journal::ConnectionConfig;
use crate::session_security::IsolationLevel;
use crate::terminal_effects::{Effect, Selection, Speed};
use crate::terminal_theme::Rgb;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    Ordinary,
    RouteLineage,
    CredentialReview,
}

pub(super) fn parse(root: Node<'_, '_>, source: Source) -> io::Result<Preview> {
    let supports_own_settings = own_settings_supported(root)?;
    let mut preview = Preview {
        format: Format::Kortty,
        ..Preview::default()
    };
    let mut source_ids = HashSet::new();
    let mut duplicate_ids = HashSet::new();
    let mut source_entries = 0;
    for entry in root.children().filter(|node| node.is_element()) {
        let rust_local = has_unqualified_tag(entry, "rustLocalConnection");
        if rust_local && !supports_own_settings {
            preview.unsupported += 1;
            continue;
        }
        if !rust_local && !has_unqualified_tag(entry, "connection") {
            continue;
        }
        let source_id = if source != Source::Ordinary && !rust_local {
            source_entries += 1;
            if source_entries > MAX_PROFILES && source == Source::CredentialReview {
                return Err(invalid("Java connections.xml exceeds its entry limit."));
            }
            entry
                .attribute("id")
                .filter(|id| super::valid_teamwork_id(id))
        } else {
            None
        };
        if let Some(id) = source_id
            && !source_ids.insert(id)
        {
            if source == Source::CredentialReview {
                return Err(invalid(
                    "Java connections.xml contains duplicate connection IDs.",
                ));
            }
            duplicate_ids.insert(id);
        }
        let key_reference = source == Source::CredentialReview
            && binding_field(entry, "authMethod")
                .is_ok_and(|field| field.map(str::trim) == Some("PUBLIC_KEY"))
            && entry
                .children()
                .any(|child| has_unqualified_tag(child, "sshKeyId"));
        if key_reference {
            preview.java_key_references += 1;
        }
        if rust_local
            || raw_scalar(entry, "protocol").is_ok_and(|value| {
                value
                    .as_deref()
                    .is_some_and(|value| value.trim() == "LOCAL_SHELL")
            })
        {
            match local_profile(entry, rust_local, supports_own_settings) {
                Ok(local) => preview.push_local(local)?,
                Err(_) => preview.invalid += 1,
            }
            continue;
        }
        if raw_scalar(entry, "protocol").is_ok_and(|value| {
            value
                .as_deref()
                .is_some_and(|value| value.trim() == "SERIAL")
        }) {
            match serial_profile(entry, supports_own_settings) {
                Ok(serial) => preview.push_serial(serial)?,
                Err(_) => preview.invalid += 1,
            }
            continue;
        }
        match is_ssh(entry) {
            Ok(true) => {}
            Ok(false) => {
                preview.unsupported += 1;
                continue;
            }
            Err(_) => {
                preview.invalid += 1;
                continue;
            }
        }
        match connection(entry, supports_own_settings).and_then(|(mut profile, skipped)| {
            profile.connection.window_geometry = window_geometry(entry)?;
            profile.java_id = source_id.map(str::to_owned);
            if source_id.is_some() {
                profile.java_route = declared_route(entry, &profile.connection).ok().flatten();
                if key_reference && profile.java_route.is_some() {
                    profile.java_key_id = binding_field(entry, "sshKeyId")
                        .ok()
                        .flatten()
                        .filter(|id| super::valid_teamwork_id(id))
                        .map(str::to_owned);
                }
            }
            Ok((profile, skipped))
        }) {
            Ok((entry, skipped)) => {
                if preview.push(entry)? {
                    preview.skipped_tunnels += skipped;
                }
            }
            Err(_) => preview.invalid += 1,
        }
    }
    if !duplicate_ids.is_empty() {
        for entry in &mut preview.entries {
            if entry
                .java_id
                .as_deref()
                .is_some_and(|id| duplicate_ids.contains(id))
            {
                entry.java_route = None;
            }
        }
    }
    Ok(preview)
}

fn binding_field<'a, 'input: 'a>(
    connection: Node<'a, 'input>,
    field: &str,
) -> io::Result<Option<&'a str>> {
    let Some(node) = optional_child(connection, field)? else {
        return Ok(None);
    };
    let mut children = node.children();
    let value = children.next();
    if children.next().is_some() || value.is_some_and(|child| !child.is_text()) {
        return Err(invalid(
            "Java connection contains an ambiguous binding field",
        ));
    }
    Ok(Some(
        value.and_then(|child| child.text()).unwrap_or_default(),
    ))
}

fn declared_route(
    node: Node<'_, '_>,
    connection: &super::Connection,
) -> io::Result<Option<(LoginTarget, Option<LoginTarget>)>> {
    if !matches!(
        binding_field(node, "connectionSource")?.map(str::trim),
        None | Some("LOCAL")
    ) || !matches!(
        binding_field(node, "protocol")?.map(str::trim),
        None | Some("SSH_TCP")
    ) || temporary_identity(node)?
    {
        return Ok(None);
    }
    let Some(target) = binding_endpoint(node)? else {
        return Ok(None);
    };
    let jump = if let Some(jump) = optional_child(node, "jumpServer")?
        && boolean(jump, "enabled")?
    {
        if temporary_identity(jump)? {
            return Ok(None);
        }
        let Some(endpoint) = binding_endpoint(jump)? else {
            return Ok(None);
        };
        Some(endpoint)
    } else {
        None
    };
    Ok(connection
        .matches_route(&target, jump.as_ref())
        .then_some((target, jump)))
}

fn temporary_identity(node: Node<'_, '_>) -> io::Result<bool> {
    Ok(
        binding_field(node, "temporaryKeyContent")?.is_some_and(|value| !value.trim().is_empty())
            || binding_field(node, "privateKeyPath")?
                .is_some_and(|value| value.trim().starts_with("TEMPORARY:")),
    )
}

fn binding_endpoint(node: Node<'_, '_>) -> io::Result<Option<LoginTarget>> {
    let (Some(host), Some(username)) = (
        binding_field(node, "host")?,
        binding_field(node, "username")?,
    ) else {
        return Ok(None);
    };
    let host = normalize_host(host.trim()).map_err(|_| invalid("invalid Java SSH binding host"))?;
    let port = match binding_field(node, "port")?.map(str::trim) {
        None => 22,
        Some(value) => port(value)?,
    };
    let endpoint = LoginTarget {
        host,
        port,
        username: username.to_owned(),
    };
    endpoint.validate().map_err(invalid)?;
    Ok(Some(endpoint))
}

fn local_profile(
    node: Node<'_, '_>,
    rust_local: bool,
    supports_own_settings: bool,
) -> io::Result<ImportedLocal> {
    let group = raw_scalar(node, "group")?.unwrap_or_default();
    if !group.is_empty() && !super::library::valid_path(&group) {
        return Err(invalid("invalid kortty connection folder"));
    }
    let (shell, command) = if rust_local {
        let shell = match raw_scalar(node, "rustLocalShell")?.as_deref() {
            None | Some("default") => LocalShell::Default,
            Some("power-shell") => LocalShell::PowerShell,
            Some("command-prompt") => LocalShell::CommandPrompt,
            _ => return Err(invalid("unsupported local shell preset")),
        };
        let command = raw_scalar(node, "rustLocalArgv")?
            .map(|encoded| {
                let json = STANDARD
                    .decode(encoded)
                    .map_err(|_| invalid("invalid native local command encoding"))?;
                Ok::<Vec<String>, io::Error>(serde_json::from_slice(&json)?)
            })
            .transpose()?;
        (shell, command)
    } else {
        let command = raw_scalar(node, "localShellCommand")?
            .filter(|line| !line.trim().is_empty())
            .map(|line| java_local_arguments(&line))
            .filter(|words| !words.is_empty());
        (LocalShell::Default, command)
    };
    let name = raw_scalar(node, "name")?
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| {
            command
                .as_ref()
                .and_then(|words| words.first())
                .and_then(|program| program.rsplit(['/', '\\']).next())
                .filter(|name| !name.is_empty())
                .unwrap_or("Local Shell")
                .to_owned()
        });
    let profile = LocalProfile {
        id: 0,
        name,
        shell,
        command,
        working_directory: raw_scalar(node, "localShellWorkingDirectory")?
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from),
        shell_integration: local_shell_integration(node)?,
        emulation: terminal_profile(node)?.term_name().into(),
        terminal: terminal_overrides(node, supports_own_settings)?.0,
    };
    profile.validate()?;
    Ok(ImportedLocal { profile, group })
}

fn local_shell_integration(node: Node<'_, '_>) -> io::Result<bool> {
    let java = optional_child(node, "shellIntegrationAutoInject")?
        .map(|_| boolean(node, "shellIntegrationAutoInject"))
        .transpose()?;
    let native = optional_child(node, "rustLocalShellIntegration")?
        .map(|_| boolean(node, "rustLocalShellIntegration"))
        .transpose()?;
    if java
        .zip(native)
        .is_some_and(|(java, native)| java != native)
    {
        return Err(invalid("conflicting local shell integration settings"));
    }
    Ok(java.or(native).unwrap_or(false))
}

fn terminal_profile(node: Node<'_, '_>) -> io::Result<TerminalProfile> {
    let emulation = raw_scalar(node, "terminalEmulationType")?.unwrap_or_default();
    let emulation = emulation.trim();
    if emulation.is_empty() {
        return Ok(TerminalProfile::default());
    }
    TerminalProfile::ALL
        .into_iter()
        .find(|profile| {
            profile.term_name().eq_ignore_ascii_case(emulation)
                || crate::connection_export::java_emulation(*profile)
                    .eq_ignore_ascii_case(emulation)
        })
        .ok_or_else(|| invalid("unsupported terminal emulation"))
}

fn serial_profile(node: Node<'_, '_>, supported: bool) -> io::Result<ImportedSerial> {
    use crate::session::{SerialConnection, SerialSettings};
    let serial =
        optional_child(node, "serial")?.ok_or_else(|| invalid("missing serial settings"))?;
    let device = raw_scalar(serial, "portName")?.unwrap_or_default();
    let defaults = SerialSettings::default();
    let settings = SerialSettings {
        baud_rate: number::<u32>(serial, "baudRate")?.unwrap_or(defaults.baud_rate),
        data_bits: number::<u8>(serial, "dataBits")?.unwrap_or(defaults.data_bits),
        parity: serial_enum(serial, "parity")?.unwrap_or(defaults.parity),
        stop_bits: serial_enum(serial, "stopBits")?.unwrap_or(defaults.stop_bits),
        flow_control: serial_enum(serial, "flowControl")?.unwrap_or(defaults.flow_control),
        assert_dtr: optional_boolean(serial, "assertDtr")?.unwrap_or(defaults.assert_dtr),
        assert_rts: optional_boolean(serial, "assertRts")?.unwrap_or(defaults.assert_rts),
    };
    let terminal = terminal_overrides(node, supported)?.0;
    let connection = SerialConnection {
        device: PathBuf::from(device.trim()),
        settings,
        line_ending: serial_enum(serial, "lineEnding")?.unwrap_or_default(),
        local_echo: boolean(serial, "localEcho")?,
        encoding: terminal.encoding,
    };
    let mut profile = SerialProfile::new(
        &raw_scalar(node, "name")?.unwrap_or_default(),
        connection,
        terminal_profile(node)?,
    );
    profile.journal = terminal.journal;
    profile.paste_line_delay_ms = terminal.paste_line_delay_ms;
    profile.strict_terminal_mode = terminal.strict_terminal_mode;
    profile.tag = raw_scalar(node, "tag")?
        .unwrap_or_default()
        .trim()
        .to_owned();
    profile.validate()?;
    let group = raw_scalar(node, "group")?.unwrap_or_default();
    if !group.is_empty() && !super::library::valid_path(&group) {
        return Err(invalid("invalid kortty connection folder"));
    }
    Ok(ImportedSerial { profile, group })
}

fn serial_enum<T: serde::de::DeserializeOwned>(
    node: Node<'_, '_>,
    field: &str,
) -> io::Result<Option<T>> {
    raw_scalar(node, field)?
        .map(|value| {
            let value = value.trim();
            let mut name = String::new();
            if value == "CRLF" {
                name.push_str("CrLf");
            } else {
                for part in value.split('_') {
                    let mut characters = part.chars();
                    if let Some(first) = characters.next() {
                        name.push(first.to_ascii_uppercase());
                        name.extend(characters.map(|character| character.to_ascii_lowercase()));
                    }
                }
            }
            serde_json::from_value(name.into())
                .map_err(|_| invalid("invalid serial settings in kortty connection XML"))
        })
        .transpose()
}

fn java_local_arguments(command: &str) -> Vec<String> {
    // Match the pinned Java tokenizer's quoted tokens and ASCII whitespace.
    static WORD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#""([^"]*)"|([^ \t\n\x0B\x0C\r]+)"#).unwrap());
    WORD.captures_iter(command.trim_matches(|ch| ch <= ' '))
        .filter_map(|word| word.get(1).or_else(|| word.get(2)))
        .map(|word| word.as_str())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(super) fn own_settings_supported(root: Node<'_, '_>) -> io::Result<bool> {
    match root.attribute("schemaVersion") {
        Some("2") => Ok(true),
        None | Some("1") => Ok(false),
        Some(_) => Err(invalid("unsupported kortty connection XML schema version")),
    }
}

pub(super) fn is_ssh(node: Node<'_, '_>) -> io::Result<bool> {
    Ok(matches!(
        raw_scalar(node, "protocol")?.as_deref().map(str::trim),
        None | Some("" | "SSH_TCP")
    ))
}

pub(super) fn connection(
    node: Node<'_, '_>,
    supports_own_settings: bool,
) -> io::Result<(ImportedProfile, usize)> {
    let host = raw_scalar(node, "host")?.unwrap_or_default();
    let name = raw_scalar(node, "name")?.unwrap_or_else(|| host.clone());
    let connection_port = raw_scalar(node, "port")?.unwrap_or_default();
    let username = raw_scalar(node, "username")?.unwrap_or_default();
    let group = raw_scalar(node, "group")?.unwrap_or_default();
    if !group.is_empty() && !super::library::valid_path(&group) {
        return Err(invalid("invalid kortty connection folder"));
    }
    let mut tunnels = Vec::new();
    let mut skipped = 0;
    let mut binds = HashSet::new();
    for child in node
        .children()
        .filter(|child| has_unqualified_tag(*child, "sshTunnels"))
    {
        match tunnel(child) {
            Ok(tunnel)
                if tunnels.len() < MAX_TUNNELS
                    && binds.insert((
                        matches!(tunnel.rule.target, TunnelTarget::Remote { .. }),
                        tunnel.rule.bind,
                    )) =>
            {
                tunnels.push(tunnel);
            }
            _ => skipped += 1,
        }
    }
    let (mut connection, _) = ssh_profile(
        &name,
        host.trim(),
        connection_port.trim(),
        username.trim(),
        "",
        tunnels,
    )?;
    connection.login.auth = auth(node)?;
    let jump = jump(node)?;
    let jump_has_password = jump.as_ref().is_some_and(|(_, has_password)| *has_password);
    connection.jump = jump.map(|(login, _)| login);
    if let Some(emulation) = raw_scalar(node, "terminalEmulationType")? {
        let emulation = emulation.trim();
        if !emulation.is_empty() {
            let profile = TerminalProfile::ALL
                .into_iter()
                .find(|profile| {
                    profile.term_name().eq_ignore_ascii_case(emulation)
                        || crate::connection_export::java_emulation(*profile)
                            .eq_ignore_ascii_case(emulation)
                })
                .ok_or_else(|| {
                    invalid("unsupported terminal emulation in kortty connection XML")
                })?;
            connection.emulation = profile.term_name().into();
        }
    }
    connection.tag = raw_scalar(node, "tag")?.unwrap_or_default();
    connection.tab_color = raw_scalar(node, "tabColor")?
        .as_deref()
        .and_then(TabColor::parse);
    connection.paste_warning = match raw_scalar(node, "pasteWarningMode")?
        .as_deref()
        .map(str::trim)
    {
        Some("off") => Some(PasteWarningMode::Off),
        Some("unless-bracketed") => Some(PasteWarningMode::UnlessBracketed),
        Some("always") => Some(PasteWarningMode::Always),
        _ => None,
    };
    if let Some(value) = raw_scalar(node, "isolationMode")?
        && !value.trim().is_empty()
    {
        connection.isolation = IsolationLevel::from_id(value.trim())
            .ok_or_else(|| invalid("unknown isolation level in kortty connection XML"))?;
    }
    connection.incognito = boolean(node, "incognito")?;
    connection.terminal_effect = terminal_effect(node)?;
    (connection.terminal, connection.command_timestamps) =
        terminal_overrides(node, supports_own_settings)?;
    let timeout = number(node, "connectionTimeoutSeconds")?.unwrap_or(connection.startup.seconds());
    let attempts = number(node, "retryCount")?.unwrap_or(connection.startup.attempts());
    connection.startup =
        ConnectOptions::new(timeout, attempts)?.with_keepalive(ssh_keepalive(node)?);
    connection.validate()?;
    let mut entry = ImportedProfile::new(connection, group);
    entry.badge = raw_scalar(node, "osBadge")?.filter(|value| valid_os_badge_reference(value));
    entry.has_password = has_embedded_password(node);
    entry.jump_has_password = jump_has_password;
    Ok((entry, skipped))
}

fn ssh_keepalive(node: Node<'_, '_>) -> io::Result<Option<KeepAlive>> {
    let Some(settings) = optional_child(node, "settings")? else {
        return Ok(None);
    };
    let enabled = optional_boolean(settings, "sshKeepAliveEnabled")?;
    let seconds = number::<i32>(settings, "sshKeepAliveInterval")?;
    if enabled.is_none() && seconds.is_none() {
        return Ok(None);
    }
    let defaults = KeepAlive::default();
    let seconds = seconds.map_or(defaults.seconds(), |seconds| {
        seconds.clamp(
            i32::from(KeepAlive::MIN_SECONDS),
            i32::from(KeepAlive::MAX_SECONDS),
        ) as u16
    });
    KeepAlive::new(enabled.unwrap_or(defaults.enabled()), seconds).map(Some)
}

fn window_geometry(node: Node<'_, '_>) -> io::Result<Option<WindowGeometry>> {
    let Some(geometry) = optional_child(node, "windowGeometry")? else {
        return Ok(None);
    };
    // JAXB's model defaults differ from the editor defaults for a new custom preference.
    let coordinate = |field, default, bounds: std::ops::RangeInclusive<u16>| {
        let value = number::<f64>(geometry, field)?.unwrap_or(default);
        if !value.is_finite()
            || value < f64::from(*bounds.start())
            || value > f64::from(*bounds.end())
        {
            return Err(invalid(
                "invalid saved window geometry in kortty connection XML",
            ));
        }
        Ok(value.trunc() as u16)
    };
    Ok(Some(WindowGeometry {
        width: coordinate("width", 800.0, WindowGeometry::WIDTH)?,
        height: coordinate("height", 600.0, WindowGeometry::HEIGHT)?,
        x: coordinate("x", 0.0, WindowGeometry::X)?,
        y: coordinate("y", 0.0, WindowGeometry::Y)?,
        maximized: boolean(geometry, "maximized")?,
    }))
}

fn terminal_overrides(
    node: Node<'_, '_>,
    supported: bool,
) -> io::Result<(TerminalOverrides, Option<bool>)> {
    let mut inherited = TerminalOverrides {
        paste_line_delay_ms: number::<i32>(node, "pasteLineDelayMs")?.map(|delay| {
            delay.clamp(
                0,
                i32::from(crate::connection_store::MAX_PASTE_LINE_DELAY_MS),
            ) as u16
        }),
        strict_terminal_mode: optional_boolean(node, "strictTerminalMode")?,
        ai_agent_disabled: optional_boolean(node, "aiAgentDisabled")?.unwrap_or(false),
        journal: journal_config(node)?,
        highlight_set: raw_scalar(node, "highlightRuleSetId")?
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty()),
        ..TerminalOverrides::default()
    };
    let settings = if supported {
        optional_child(node, "settings")?
    } else {
        None
    };
    if let Some(settings) = settings {
        // The Java shell connectors read these directly from the connection's settings.
        inherited.terminal_columns = number(settings, "terminalColumns")?;
        inherited.terminal_rows = number(settings, "terminalRows")?;
    }
    inherited.validate()?;
    let Some(settings) = settings else {
        return Ok((inherited, None));
    };
    if optional_boolean(settings, "useGlobalSettings")? != Some(false) {
        return Ok((inherited, None));
    }
    let color = |field| -> io::Result<Option<Rgb>> {
        Ok(raw_scalar(settings, field)?.as_deref().and_then(Rgb::parse))
    };
    let customized = optional_boolean(settings, "ansiPaletteCustomized")? == Some(true);
    let mut ansi = [None; 16];
    if customized {
        for (field, value) in JAVA_ANSI_FIELDS.into_iter().zip(&mut ansi) {
            *value = color(field)?;
        }
    }
    let options = TerminalOverrides {
        theme: raw_scalar(settings, "themeId")?.and_then(ThemeOverride::from_java),
        font_family: raw_scalar(settings, "fontFamily")?
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty()),
        font_size: number::<u8>(settings, "fontSize")?,
        foreground: color("foregroundColor")?,
        background: color("backgroundColor")?,
        cursor: color("cursorColor")?,
        cursor_style: enum_field(settings, "cursorStyle")?,
        selection: if customized {
            color("selectionColor")?
        } else {
            None
        },
        ansi,
        bold: enum_field(settings, "boldTextMode")?,
        sgr_enabled: optional_boolean(settings, "terminalColorsEnabled")?,
        encoding: raw_scalar(settings, "encoding")?
            .as_deref()
            .and_then(TextEncoding::from_name),
        scrollback_lines: number::<u32>(settings, "scrollbackLines")?,
        scrollback_memory_mib: number::<i32>(settings, "scrollbackMemoryMb")?.map(|mib| {
            if mib <= 0 {
                ScrollbackConfig::DEFAULT_MEMORY_MIB
            } else {
                mib.min(i32::from(ScrollbackConfig::MAX_MEMORY_MIB)) as u8
            }
        }),
        scrollback_compression: optional_boolean(settings, "scrollbackCompression")?,
        confirm_close: optional_boolean(settings, "closeWithoutConfirmation")?.map(|value| !value),
        ..inherited
    };
    options.validate()?;
    Ok((
        options,
        optional_boolean(settings, "commandTimestampsEnabled")?,
    ))
}

fn journal_config(node: Node<'_, '_>) -> io::Result<ConnectionConfig> {
    let Some(node) = optional_child(node, "sessionJournalConfig")? else {
        return Ok(ConnectionConfig::default());
    };
    let defaults = ConnectionConfig::default();
    let settings = ConnectionConfig {
        auto_start: boolean(node, "enabled")?,
        capture_input: boolean(node, "captureInput")?,
        ai_summaries: optional_boolean(node, "aiSummariesEnabled")?
            .unwrap_or(defaults.ai_summaries),
        summary_interval_minutes: number(node, "summaryIntervalMinutes")?.unwrap_or(0),
        max_log_size_mib: number(node, "maxLogSizeMB")?
            .filter(|value| *value != 0)
            .unwrap_or(defaults.max_log_size_mib),
        max_log_parts: number(node, "maxLogParts")?
            .filter(|value| *value != 0)
            .unwrap_or(defaults.max_log_parts),
    };
    settings.validate()?;
    Ok(settings)
}

fn enum_field<T: serde::de::DeserializeOwned>(
    node: Node<'_, '_>,
    field: &str,
) -> io::Result<Option<T>> {
    Ok(raw_scalar(node, field)?.and_then(|value| {
        let id = value.trim().to_ascii_lowercase().replace('_', "-");
        serde_json::from_value(id.into()).ok()
    }))
}

fn optional_boolean(node: Node<'_, '_>, field: &str) -> io::Result<Option<bool>> {
    match raw_scalar(node, field)?.as_deref().map(str::trim) {
        None => Ok(None),
        Some("false" | "0") => Ok(Some(false)),
        Some("true" | "1") => Ok(Some(true)),
        _ => Err(invalid("invalid boolean field in kortty connection XML")),
    }
}

fn terminal_effect(node: Node<'_, '_>) -> io::Result<Option<Selection>> {
    let Some(id) = raw_scalar(node, "terminalEffectPluginId")? else {
        return Ok(None);
    };
    let Ok(effect) = serde_json::from_value::<Effect>(id.trim().into()) else {
        return Ok(None);
    };
    let speed = number::<f64>(node, "terminalEffectAnimationSpeed")?
        .map_or(Speed::DEFAULT, Speed::normalized);
    Ok(Some(Selection { effect, speed }))
}

fn jump(node: Node<'_, '_>) -> io::Result<Option<(SavedLogin, bool)>> {
    let Some(jump) = optional_child(node, "jumpServer")? else {
        return Ok(None);
    };
    if !boolean(jump, "enabled")? {
        return Ok(None);
    }
    let host = raw_scalar(jump, "host")?.unwrap_or_default();
    let host = normalize_host(host.trim()).map_err(|_| invalid("invalid SSH jump hostname"))?;
    let endpoint = LoginTarget {
        host,
        port: number(jump, "port")?.unwrap_or(22),
        username: raw_scalar(jump, "username")?
            .unwrap_or_default()
            .trim()
            .to_owned(),
    };
    Ok(Some((
        SavedLogin {
            endpoint,
            auth: auth(jump)?,
            keyboard_interactive: false,
        },
        has_embedded_password(jump),
    )))
}

fn auth(node: Node<'_, '_>) -> io::Result<Auth> {
    Ok(
        match raw_scalar(node, "authMethod")?.as_deref().map(str::trim) {
            Some("KEYBOARD_INTERACTIVE") => Auth::KeyboardInteractive,
            Some("PUBLIC_KEY") => Auth::Configured {
                method: AuthMethod::PublicKey,
                credential: None,
                key: None,
            },
            _ => Auth::Prompt,
        },
    )
}

fn has_embedded_password(node: Node<'_, '_>) -> bool {
    node.children()
        .filter(|child| has_unqualified_tag(*child, "encryptedPassword"))
        .flat_map(|field| field.children())
        .any(|text| text.is_text() && text.text().is_some_and(|value| !value.trim().is_empty()))
}

fn tunnel(node: Node<'_, '_>) -> io::Result<TunnelConfig> {
    let local_host = raw_scalar(node, "localHost")?.unwrap_or_else(|| "localhost".into());
    let local_port = raw_scalar(node, "localPort")?.unwrap_or_default();
    let remote_host = raw_scalar(node, "remoteHost")?.unwrap_or_else(|| "localhost".into());
    let remote_port = raw_scalar(node, "remotePort")?.unwrap_or_default();
    let kind = raw_scalar(node, "type")?;
    let target_host = |host: &str| {
        normalize_host(host.trim()).map_err(|_| invalid("invalid SSH forwarding destination"))
    };
    let (bind_host, bind_port, target) = match kind.as_deref().map(str::trim) {
        None | Some("LOCAL") => (
            local_host,
            local_port,
            TunnelTarget::Local {
                host: target_host(&remote_host)?,
                port: port(remote_port.trim())?,
            },
        ),
        Some("REMOTE") => (
            remote_host,
            remote_port,
            TunnelTarget::Remote {
                host: target_host(&local_host)?,
                port: port(local_port.trim())?,
            },
        ),
        Some("DYNAMIC") => (local_host, local_port, TunnelTarget::Dynamic),
        _ => return Err(invalid("unsupported SSH forwarding type")),
    };
    let bind_host = bind_host.trim();
    let ip = if bind_host.eq_ignore_ascii_case("localhost") {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        bind_host
            .parse::<IpAddr>()
            .map_err(|_| invalid("invalid SSH forwarding bind address"))?
    };
    let tunnel = TunnelConfig {
        enabled: boolean(node, "enabled")?,
        description: raw_scalar(node, "description")?.unwrap_or_default(),
        rule: TunnelRule {
            bind: (ip, port(bind_port.trim())?).into(),
            target,
        },
    };
    tunnel.validate()?;
    Ok(tunnel)
}

fn boolean(node: Node<'_, '_>, field: &str) -> io::Result<bool> {
    match raw_scalar(node, field)?.as_deref().map(str::trim) {
        None | Some("false" | "0") => Ok(false),
        Some("true" | "1") => Ok(true),
        _ => Err(invalid("invalid boolean field in kortty connection XML")),
    }
}

fn number<T: std::str::FromStr>(node: Node<'_, '_>, field: &str) -> io::Result<Option<T>> {
    raw_scalar(node, field)?
        .map(|value| {
            value
                .trim()
                .parse()
                .map_err(|_| invalid("invalid number in kortty connection XML"))
        })
        .transpose()
}
