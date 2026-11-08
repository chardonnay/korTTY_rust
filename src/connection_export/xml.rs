use std::fmt::{Display, Write as _};

use crate::clipboard_policy::PasteWarningMode;
use crate::connection_store::{
    Auth, AuthMethod, TunnelConfig, TunnelTarget, valid_os_badge_reference,
};
use crate::session_security::IsolationLevel;

use super::{Entry, Options, java_emulation};

pub(super) fn kortty(entries: &[Entry<'_>], options: Options) -> String {
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
            if selection.speed() != 1 {
                field(
                    &mut xml,
                    "    ",
                    "terminalEffectAnimationSpeed",
                    selection.speed(),
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
    xml.push_str("</connections>\n");
    xml
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
