use std::fmt::{Display, Write as _};
use std::io;

use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::clipboard_policy::PasteWarningMode;
use crate::connection_store::{
    Auth, AuthMethod, JAVA_ANSI_FIELDS, TerminalOverrides, ThemeOverride, TunnelConfig,
    TunnelTarget, valid_os_badge_reference,
};
use crate::session_security::IsolationLevel;
use crate::terminal_effects::Speed;
use crate::terminal_theme::{CursorStyle, Id, Themes};

use super::{Entry, LocalEntry, Options, SerialEntry, java_emulation, java_local_profile};

pub(super) fn kortty(
    entries: &[Entry<'_>],
    locals: &[LocalEntry<'_>],
    serials: &[SerialEntry<'_>],
    options: Options,
    themes: Option<&Themes>,
) -> io::Result<String> {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<connections schemaVersion=\"2\">\n",
    );
    for Entry {
        connection,
        group,
        os_badge,
    } in entries
    {
        // Java's JAXB constructor supplies a new UUID when the XML omits id.
        xml.push_str("  <connection>\n");
        let login = &connection.login;
        let endpoint = &login.endpoint;
        field(&mut xml, "    ", "name", &connection.name);
        field(&mut xml, "    ", "host", &endpoint.host);
        field(&mut xml, "    ", "port", endpoint.port);
        field(
            &mut xml,
            "    ",
            "username",
            options.username(&endpoint.username),
        );
        field(&mut xml, "    ", "authMethod", auth(&login.auth));
        field(&mut xml, "    ", "protocol", "SSH_TCP");
        if let Some(selection) = connection.terminal_effect
            && let Ok(serde_json::Value::String(id)) = serde_json::to_value(selection.effect)
        {
            field(&mut xml, "    ", "terminalEffectPluginId", id);
            if selection.speed != Speed::DEFAULT {
                field(
                    &mut xml,
                    "    ",
                    "terminalEffectAnimationSpeed",
                    selection.speed,
                );
            }
        }
        if let Some(profile) = connection.profile() {
            field(
                &mut xml,
                "    ",
                "terminalEmulationType",
                java_emulation(profile),
            );
        }
        if !group.is_empty() {
            field(&mut xml, "    ", "group", group);
        }
        if !connection.tag.is_empty() {
            field(&mut xml, "    ", "tag", &connection.tag);
        }
        if let Some(color) = connection.tab_color {
            field(&mut xml, "    ", "tabColor", color);
        }
        if let Some(badge) = os_badge.filter(|value| valid_os_badge_reference(value)) {
            field(&mut xml, "    ", "osBadge", badge);
        }
        if let Some(mode) = connection.paste_warning {
            let mode = match mode {
                PasteWarningMode::Off => "off",
                PasteWarningMode::UnlessBracketed => "unless-bracketed",
                PasteWarningMode::Always => "always",
            };
            field(&mut xml, "    ", "pasteWarningMode", mode);
        }
        let isolation = match connection.isolation {
            IsolationLevel::None => "none",
            IsolationLevel::Process => "process",
            IsolationLevel::Sandbox => "sandbox",
        };
        field(&mut xml, "    ", "isolationMode", isolation);
        field(&mut xml, "    ", "incognito", connection.incognito);
        terminal_settings(
            &mut xml,
            &connection.terminal,
            connection.command_timestamps,
            connection.startup.keepalive(),
            themes,
        );
        for tunnel in options.tunnels(connection) {
            tunnel_xml(&mut xml, tunnel, false);
        }
        if let Some(jump) = &connection.jump
            && options.include_jump_server
        {
            xml.push_str("    <jumpServer>\n");
            field(&mut xml, "      ", "enabled", true);
            field(&mut xml, "      ", "host", &jump.endpoint.host);
            field(&mut xml, "      ", "port", jump.endpoint.port);
            field(
                &mut xml,
                "      ",
                "username",
                options.username(&jump.endpoint.username),
            );
            field(&mut xml, "      ", "authMethod", auth(&jump.auth));
            xml.push_str("    </jumpServer>\n");
        }
        field(
            &mut xml,
            "    ",
            "connectionTimeoutSeconds",
            connection.startup.seconds(),
        );
        field(
            &mut xml,
            "    ",
            "retryCount",
            connection.startup.attempts().max(1),
        );
        xml.push_str("  </connection>\n");
    }
    for LocalEntry { profile, group } in locals {
        // Java skips this separate element when its command tokenizer cannot
        // represent the exact launch or when the profile uses a Rust preset.
        let compatible = java_local_profile(profile);
        let node = if compatible {
            "connection"
        } else {
            "rustLocalConnection"
        };
        let _ = writeln!(xml, "  <{node}>");
        field(&mut xml, "    ", "name", &profile.name);
        if compatible {
            field(&mut xml, "    ", "protocol", "LOCAL_SHELL");
            if let Some(command) = &profile.command {
                let words = command
                    .iter()
                    .map(|word| format!("\"{word}\""))
                    .collect::<Vec<_>>();
                field(&mut xml, "    ", "localShellCommand", words.join(" "));
            }
        } else {
            if !profile.shell.is_default() {
                let preset = match profile.shell {
                    kortty_pty::LocalShell::PowerShell => "power-shell",
                    kortty_pty::LocalShell::CommandPrompt => "command-prompt",
                    kortty_pty::LocalShell::Default => "default",
                };
                field(&mut xml, "    ", "rustLocalShell", preset);
            }
            if let Some(command) = &profile.command {
                field(
                    &mut xml,
                    "    ",
                    "rustLocalArgv",
                    STANDARD.encode(serde_json::to_vec(command)?),
                );
            }
        }
        if let Some(directory) = &profile.working_directory {
            field(
                &mut xml,
                "    ",
                "localShellWorkingDirectory",
                directory.display(),
            );
        }
        if profile.shell_integration {
            field(
                &mut xml,
                "    ",
                if compatible {
                    "shellIntegrationAutoInject"
                } else {
                    "rustLocalShellIntegration"
                },
                true,
            );
        }
        if let Some(emulation) = profile.profile() {
            field(
                &mut xml,
                "    ",
                "terminalEmulationType",
                java_emulation(emulation),
            );
        }
        if !group.is_empty() {
            field(&mut xml, "    ", "group", group);
        }
        terminal_settings(&mut xml, &profile.terminal, None, None, themes);
        let _ = writeln!(xml, "  </{node}>");
    }
    for entry in serials {
        serial_profile(&mut xml, entry);
    }
    xml.push_str("</connections>\n");
    Ok(xml)
}

fn serial_profile(xml: &mut String, entry: &SerialEntry<'_>) {
    use crate::session::{SerialFlowControl, SerialLineEnding, SerialStopBits};
    let profile = entry.profile;
    let connection = &profile.connection;
    let settings = connection.settings;
    xml.push_str("  <connection>\n");
    field(xml, "    ", "name", &profile.name);
    field(xml, "    ", "protocol", "SERIAL");
    if !profile.tag.is_empty() {
        field(xml, "    ", "tag", &profile.tag);
    }
    if !entry.group.is_empty() {
        field(xml, "    ", "group", entry.group);
    }
    if let Some(emulation) = profile.profile() {
        field(
            xml,
            "    ",
            "terminalEmulationType",
            java_emulation(emulation),
        );
    }
    xml.push_str("    <serial>\n");
    let indent = "      ";
    field(xml, indent, "portName", connection.device.display());
    field(xml, indent, "baudRate", settings.baud_rate);
    field(xml, indent, "dataBits", settings.data_bits);
    field(
        xml,
        indent,
        "parity",
        format!("{:?}", settings.parity).to_ascii_uppercase(),
    );
    field(
        xml,
        indent,
        "stopBits",
        match settings.stop_bits {
            SerialStopBits::One => "ONE",
            SerialStopBits::OnePointFive => "ONE_POINT_FIVE",
            SerialStopBits::Two => "TWO",
        },
    );
    field(
        xml,
        indent,
        "flowControl",
        match settings.flow_control {
            SerialFlowControl::None => "NONE",
            SerialFlowControl::RtsCts => "RTS_CTS",
            SerialFlowControl::XonXoff => "XON_XOFF",
            SerialFlowControl::DsrDtr => "DSR_DTR",
        },
    );
    field(xml, indent, "assertDtr", settings.assert_dtr);
    field(xml, indent, "assertRts", settings.assert_rts);
    field(xml, indent, "localEcho", connection.local_echo);
    field(
        xml,
        indent,
        "lineEnding",
        match connection.line_ending {
            SerialLineEnding::Cr => "CR",
            SerialLineEnding::Lf => "LF",
            SerialLineEnding::CrLf => "CRLF",
        },
    );
    xml.push_str("    </serial>\n");
    terminal_settings(
        xml,
        &TerminalOverrides {
            encoding: connection.encoding,
            journal: profile.journal,
            paste_line_delay_ms: profile.paste_line_delay_ms,
            strict_terminal_mode: profile.strict_terminal_mode,
            ..TerminalOverrides::default()
        },
        None,
        None,
        None,
    );
    xml.push_str("  </connection>\n");
}

fn terminal_settings(
    xml: &mut String,
    settings: &TerminalOverrides,
    timestamps: Option<bool>,
    keepalive: Option<kortty_ssh::KeepAlive>,
    themes: Option<&Themes>,
) {
    if settings.ai_agent_disabled {
        field(xml, "    ", "aiAgentDisabled", true);
    }
    if let Some(delay) = settings.paste_line_delay_ms {
        field(xml, "    ", "pasteLineDelayMs", delay);
    }
    if let Some(strict) = settings.strict_terminal_mode {
        field(xml, "    ", "strictTerminalMode", strict);
    }
    let journal = settings.journal;
    if !journal.is_default() {
        xml.push_str("    <sessionJournalConfig>\n");
        let indent = "      ";
        field(xml, indent, "enabled", journal.auto_start);
        field(xml, indent, "captureInput", journal.capture_input);
        field(xml, indent, "aiSummariesEnabled", journal.ai_summaries);
        field(
            xml,
            indent,
            "summaryIntervalMinutes",
            journal.summary_interval_minutes,
        );
        field(xml, indent, "maxLogSizeMB", journal.max_log_size_mib);
        field(xml, indent, "maxLogParts", journal.max_log_parts);
        xml.push_str("    </sessionJournalConfig>\n");
    }
    if let Some(id) = &settings.highlight_set {
        field(xml, "    ", "highlightRuleSetId", id);
    }
    let mut presentation = settings.clone();
    presentation.highlight_set = None;
    presentation.ai_profile = None;
    presentation.ai_agent_disabled = false;
    presentation.ai_agent_design = None;
    presentation.ai_skills.clear();
    presentation.journal = Default::default();
    presentation.terminal_columns = None;
    presentation.terminal_rows = None;
    presentation.paste_line_delay_ms = None;
    presentation.strict_terminal_mode = None;
    let theme_id = match &settings.theme {
        Some(ThemeOverride::BuiltIn(preset)) => Some(preset.java_id()),
        Some(ThemeOverride::Java { java }) => Some(java.as_str()),
        Some(ThemeOverride::Custom { custom }) => {
            themes.and_then(|themes| themes.java_id(Id::Custom(*custom)))
        }
        None => None,
    };
    if theme_id.is_none() {
        presentation.theme = None;
        // Older custom themes can still export their colors before an exchange ID is assigned.
        if let Some(ThemeOverride::Custom { custom }) = &settings.theme
            && let Some(theme) = themes.and_then(|themes| themes.find(Id::Custom(*custom)))
        {
            let scheme = theme.scheme;
            presentation.foreground.get_or_insert(scheme.foreground);
            presentation.background.get_or_insert(scheme.background);
            presentation.cursor.get_or_insert(scheme.cursor);
            presentation.cursor_style.get_or_insert(scheme.cursor_style);
            presentation
                .font_family
                .get_or_insert_with(|| scheme.family.label().into());
            presentation.font_size.get_or_insert(scheme.font_size);
        }
    }
    let use_global = presentation.is_default() && timestamps.is_none();
    if use_global
        && keepalive.is_none()
        && settings.terminal_columns.is_none()
        && settings.terminal_rows.is_none()
    {
        return;
    }
    xml.push_str("    <settings>\n");
    let indent = "      ";
    field(xml, indent, "useGlobalSettings", use_global);
    if let Some(keepalive) = keepalive {
        field(xml, indent, "sshKeepAliveEnabled", keepalive.enabled());
        field(xml, indent, "sshKeepAliveInterval", keepalive.seconds());
    }
    macro_rules! optional {
        ($tag:literal, $value:expr) => {
            if let Some(value) = $value {
                field(xml, indent, $tag, value);
            }
        };
    }
    optional!("themeId", theme_id);
    optional!("fontFamily", presentation.font_family.as_deref());
    optional!("fontSize", presentation.font_size);
    optional!("foregroundColor", presentation.foreground);
    optional!("backgroundColor", presentation.background);
    optional!("cursorColor", presentation.cursor);
    let cursor_style = match presentation.cursor_style {
        Some(CursorStyle::BlinkBar) => Some("BLINK_VERTICAL_BAR".to_owned()),
        Some(CursorStyle::SteadyBar) => Some("STEADY_VERTICAL_BAR".to_owned()),
        style => java_enum_id(style),
    };
    optional!("cursorStyle", cursor_style);
    optional!("selectionColor", settings.selection);
    optional!("boldTextMode", java_enum_id(settings.bold));
    optional!("terminalColorsEnabled", settings.sgr_enabled);
    optional!("encoding", settings.encoding);
    optional!("terminalColumns", settings.terminal_columns);
    optional!("terminalRows", settings.terminal_rows);
    optional!("scrollbackLines", settings.scrollback_lines);
    optional!("scrollbackMemoryMb", settings.scrollback_memory_mib);
    optional!("scrollbackCompression", settings.scrollback_compression);
    optional!(
        "closeWithoutConfirmation",
        settings.confirm_close.map(|confirm| !confirm)
    );
    optional!("commandTimestampsEnabled", timestamps);
    if settings.selection.is_some() || settings.ansi.iter().any(Option::is_some) {
        field(xml, indent, "ansiPaletteCustomized", true);
        for (tag, color) in JAVA_ANSI_FIELDS.into_iter().zip(settings.ansi) {
            if let Some(color) = color {
                field(xml, indent, tag, color);
            }
        }
    }
    xml.push_str("    </settings>\n");
}

fn java_enum_id(value: Option<impl serde::Serialize>) -> Option<String> {
    let serde_json::Value::String(id) = serde_json::to_value(value?).ok()? else {
        return None;
    };
    Some(id.to_ascii_uppercase().replace('-', "_"))
}

pub(super) fn mtputty(entries: &[Entry<'_>], options: Options) -> String {
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Servers>\n");
    for Entry {
        connection, group, ..
    } in entries
    {
        let endpoint = &connection.login.endpoint;
        xml.push_str("  <Server>\n");
        field(&mut xml, "    ", "DisplayName", &connection.name);
        field(&mut xml, "    ", "ServerName", &endpoint.host);
        field(&mut xml, "    ", "Port", endpoint.port);
        field(
            &mut xml,
            "    ",
            "UserName",
            options.username(&endpoint.username),
        );
        if !group.is_empty() {
            field(&mut xml, "    ", "Folder", group);
        }
        let tunnels = options.tunnels(connection);
        if !tunnels.is_empty() {
            xml.push_str("    <SSHTunnels>\n");
            for tunnel in tunnels {
                tunnel_xml(&mut xml, tunnel, true);
            }
            xml.push_str("    </SSHTunnels>\n");
        }
        xml.push_str("  </Server>\n");
    }
    xml.push_str("</Servers>\n");
    xml
}

fn auth(auth: &Auth) -> &'static str {
    match auth.method() {
        AuthMethod::Password => "PASSWORD",
        AuthMethod::PublicKey => "PUBLIC_KEY",
        AuthMethod::KeyboardInteractive => "KEYBOARD_INTERACTIVE",
    }
}

fn tunnel_xml(xml: &mut String, tunnel: &TunnelConfig, mtputty: bool) {
    let bind = tunnel.rule.bind;
    let bind_host = bind.ip().to_string();
    let (kind, local_host, local_port, remote) = match &tunnel.rule.target {
        TunnelTarget::Local { host, port } => (
            "LOCAL",
            bind_host.as_str(),
            bind.port(),
            Some((host.as_str(), *port)),
        ),
        TunnelTarget::Remote { host, port } => (
            "REMOTE",
            host.as_str(),
            *port,
            Some((bind_host.as_str(), bind.port())),
        ),
        TunnelTarget::Dynamic => ("DYNAMIC", bind_host.as_str(), bind.port(), None),
    };
    let (indent, node, fields) = if mtputty {
        (
            "      ",
            "SSHTunnel",
            [
                "Enabled",
                "Type",
                "LocalHost",
                "LocalPort",
                "RemoteHost",
                "RemotePort",
                "Description",
            ],
        )
    } else {
        (
            "    ",
            "sshTunnels",
            [
                "enabled",
                "type",
                "localHost",
                "localPort",
                "remoteHost",
                "remotePort",
                "description",
            ],
        )
    };
    let _ = writeln!(xml, "{indent}<{node}>");
    let child_indent = if mtputty { "        " } else { "      " };
    field(xml, child_indent, fields[0], tunnel.enabled);
    field(xml, child_indent, fields[1], kind);
    field(xml, child_indent, fields[2], local_host);
    field(xml, child_indent, fields[3], local_port);
    if let Some((host, port)) = remote {
        field(xml, child_indent, fields[4], host);
        field(xml, child_indent, fields[5], port);
    }
    if !tunnel.description.is_empty() {
        field(xml, child_indent, fields[6], &tunnel.description);
    }
    let _ = writeln!(xml, "{indent}</{node}>");
}

fn field(xml: &mut String, indent: &str, name: &str, value: impl Display) {
    let _ = write!(xml, "{indent}<{name}>");
    for ch in value.to_string().chars() {
        match ch {
            '&' => xml.push_str("&amp;"),
            '<' => xml.push_str("&lt;"),
            '>' => xml.push_str("&gt;"),
            _ => xml.push(ch),
        }
    }
    let _ = writeln!(xml, "</{name}>");
}
