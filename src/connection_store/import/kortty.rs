use std::collections::HashSet;
use std::io;
use std::net::{IpAddr, Ipv4Addr};

use kortty_ssh::ConnectOptions;
use roxmltree::Node;

use super::xml::{has_unqualified_tag, optional_child, raw_scalar};
use super::{
    Auth, Format, ImportedProfile, LoginTarget, MAX_TUNNELS, Preview, SavedLogin, TerminalProfile,
    TunnelConfig, TunnelRule, TunnelTarget, invalid, normalize_host, port, ssh_profile,
};
use crate::clipboard_policy::PasteWarningMode;
use crate::connection_store::{AuthMethod, TabColor, valid_os_badge_reference};
use crate::session_security::IsolationLevel;
use crate::terminal_effects::{Effect, Selection};

pub(super) fn parse(root: Node<'_, '_>) -> io::Result<Preview> {
    let schema_version = root.attribute("schemaVersion");
    if schema_version.is_some_and(|version| !matches!(version, "1" | "2")) {
        return Err(invalid("unsupported kortty connection XML schema version"));
    }
    let mut preview = Preview {
        format: Format::Kortty,
        ..Preview::default()
    };
    for entry in root
        .children()
        .filter(|node| has_unqualified_tag(*node, "connection"))
    {
        let protocol = raw_scalar(entry, "protocol");
        match protocol
            .as_ref()
            .map(|field| field.as_deref().map(str::trim))
        {
            Ok(None | Some("" | "SSH_TCP")) => {}
            Ok(Some(_)) => {
                preview.unsupported += 1;
                continue;
            }
            Err(_) => {
                preview.invalid += 1;
                continue;
            }
        }
        match connection(entry, schema_version == Some("2")) {
            Ok((entry, skipped)) => {
                if preview.push(entry)? {
                    preview.skipped_tunnels += skipped;
                }
            }
            Err(_) => preview.invalid += 1,
        }
    }
    Ok(preview)
}

fn connection(
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
    if supports_own_settings
        && let Some(settings) = optional_child(node, "settings")?
        && matches!(
            raw_scalar(settings, "useGlobalSettings")?
                .as_deref()
                .map(str::trim),
            Some("false" | "0")
        )
    {
        connection.command_timestamps = Some(boolean(settings, "commandTimestampsEnabled")?);
    }
    let timeout = number(node, "connectionTimeoutSeconds")?.unwrap_or(connection.startup.seconds());
    let attempts = number(node, "retryCount")?.unwrap_or(connection.startup.attempts());
    connection.startup = ConnectOptions::new(timeout, attempts)?;
    connection.validate()?;
    let mut entry = ImportedProfile::new(connection, group);
    entry.badge = raw_scalar(node, "osBadge")?.filter(|value| valid_os_badge_reference(value));
    entry.has_password = has_embedded_password(node);
    entry.jump_has_password = jump_has_password;
    Ok((entry, skipped))
}

fn terminal_effect(node: Node<'_, '_>) -> io::Result<Option<Selection>> {
    let Some(id) = raw_scalar(node, "terminalEffectPluginId")? else {
        return Ok(None);
    };
    let Ok(effect) = serde_json::from_value::<Effect>(id.trim().into()) else {
        return Ok(None);
    };
    let speed = number::<f64>(node, "terminalEffectAnimationSpeed")?
        .filter(|speed| speed.is_finite())
        .unwrap_or(1.0);
    // Both apps store a multiplier from 1 to 99. Java supports fractional speeds.
    let speed = speed.clamp(1.0, 99.0).round() as u8;
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
