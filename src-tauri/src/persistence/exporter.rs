use crate::model::connection::ConnectionSettings;
use crate::model::tunnel::{TunnelConfig, TunnelType};
use anyhow::Result;
use serde::Serialize;
use std::fmt::Write;
use std::path::Path;

#[derive(Debug, Clone, Copy)]
pub enum ExportFormat {
    KorTTY,
    MobaXterm,
    MTPuTTY,
    PuTTYConnectionManager,
}

#[derive(Debug, Clone)]
pub struct ConnectionExportOptions {
    pub include_username: bool,
    pub include_password: bool,
    pub include_tunnels: bool,
    pub include_jump_server: bool,
}

pub fn export_connections(
    connections: &[ConnectionSettings],
    path: &Path,
    format: ExportFormat,
    options: &ConnectionExportOptions,
) -> Result<()> {
    let sanitized = sanitize_connections(connections, options);
    match format {
        ExportFormat::KorTTY => export_kortty(&sanitized, path),
        ExportFormat::MobaXterm => export_mobaxterm(&sanitized, path),
        ExportFormat::MTPuTTY => export_mtputty(&sanitized, path),
        ExportFormat::PuTTYConnectionManager => export_putty_cm(&sanitized, path),
    }
}

fn sanitize_connections(
    connections: &[ConnectionSettings],
    options: &ConnectionExportOptions,
) -> Vec<ConnectionSettings> {
    connections
        .iter()
        .cloned()
        .map(|mut connection| {
            if !options.include_username {
                connection.username.clear();
                connection.credential_id = None;
            }
            if !options.include_password {
                connection.password = None;
                connection.private_key_passphrase = None;
            }
            if !options.include_tunnels {
                connection.tunnels.clear();
            }
            if !options.include_jump_server {
                connection.jump_server = None;
            }
            connection
        })
        .collect()
}

#[derive(Serialize)]
#[serde(rename = "connections")]
struct ConnectionExportWrapper<'a> {
    #[serde(rename = "connection")]
    connections: &'a [ConnectionSettings],
}

fn export_kortty(connections: &[ConnectionSettings], path: &Path) -> Result<()> {
    let wrapper = ConnectionExportWrapper { connections };
    let xml = quick_xml::se::to_string(&wrapper)?;
    let formatted = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{xml}");
    std::fs::write(path, formatted)?;
    Ok(())
}

fn export_mtputty(connections: &[ConnectionSettings], path: &Path) -> Result<()> {
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Servers>\n");

    for connection in connections {
        xml.push_str("  <Server>\n");
        write_xml_tag(
            &mut xml,
            4,
            "DisplayName",
            &export_mtputty_name(&connection.name),
        )?;
        write_xml_tag(&mut xml, 4, "ServerName", &connection.host)?;
        write_xml_tag(&mut xml, 4, "Port", &connection.port.to_string())?;
        if !connection.username.is_empty() {
            write_xml_tag(&mut xml, 4, "UserName", &connection.username)?;
        }
        if let Some(group) = connection
            .group
            .as_deref()
            .filter(|group| !group.is_empty())
        {
            write_xml_tag(&mut xml, 4, "Folder", group)?;
        }

        if !connection.tunnels.is_empty() {
            xml.push_str("    <SSHTunnels>\n");
            for tunnel in connection.tunnels.iter().filter(|tunnel| tunnel.enabled) {
                xml.push_str("      <SSHTunnel>\n");
                write_xml_tag(&mut xml, 8, "Enabled", "true")?;
                write_xml_tag(&mut xml, 8, "Type", tunnel_type_name(&tunnel.tunnel_type))?;
                write_xml_tag(
                    &mut xml,
                    8,
                    "LocalHost",
                    default_if_empty(&tunnel.local_host, "localhost"),
                )?;
                write_xml_tag(&mut xml, 8, "LocalPort", &tunnel.local_port.to_string())?;
                if !matches!(tunnel.tunnel_type, TunnelType::Dynamic) {
                    write_xml_tag(
                        &mut xml,
                        8,
                        "RemoteHost",
                        default_if_empty(&tunnel.remote_host, "localhost"),
                    )?;
                    write_xml_tag(&mut xml, 8, "RemotePort", &tunnel.remote_port.to_string())?;
                }
                if let Some(description) = tunnel
                    .description
                    .as_deref()
                    .filter(|desc| !desc.is_empty())
                {
                    write_xml_tag(&mut xml, 8, "Description", description)?;
                }
                xml.push_str("      </SSHTunnel>\n");
            }
            xml.push_str("    </SSHTunnels>\n");
        }

        xml.push_str("  </Server>\n");
    }

    xml.push_str("</Servers>\n");
    std::fs::write(path, xml)?;
    Ok(())
}

fn export_mobaxterm(connections: &[ConnectionSettings], path: &Path) -> Result<()> {
    let mut groups: std::collections::BTreeMap<String, Vec<&ConnectionSettings>> =
        std::collections::BTreeMap::new();

    for connection in connections {
        groups
            .entry(connection.group.clone().unwrap_or_default())
            .or_default()
            .push(connection);
    }

    let mut output = String::new();
    for (index, (group_name, group_connections)) in groups.into_iter().enumerate() {
        if index == 0 {
            output.push_str("[Bookmarks]\n");
        } else {
            writeln!(output, "[Bookmarks_{index}]")?;
        }
        writeln!(output, "SubRep={group_name}")?;
        output.push_str("ImgNum=41\n");

        for connection in group_connections {
            writeln!(
                output,
                "{}=#109#0%{}%{}%{}%-1%-1%-1%-1%0%0%%%-1%0%0%0%%1080%%0%0%1#MobaFont%10%0%0%-1%15%236,236,236%30,30,30%180,180,192%0%-1%0%%xterm%-1%-1%_Std_Colors_0_%80%24%0%1%-1%<none>%%0%1%-1%-1#0#",
                sanitize_mobaxterm_name(&connection.name),
                connection.host,
                connection.port,
                connection.username
            )?;

            for tunnel in connection.tunnels.iter().filter(|tunnel| tunnel.enabled) {
                output.push_str("# Tunnel: ");
                match tunnel.tunnel_type {
                    TunnelType::Local => {
                        write!(
                            output,
                            "LOCAL {}:{} -> {}:{}",
                            default_if_empty(&tunnel.local_host, "localhost"),
                            tunnel.local_port,
                            default_if_empty(&tunnel.remote_host, "localhost"),
                            tunnel.remote_port
                        )?;
                    }
                    TunnelType::Remote => {
                        write!(
                            output,
                            "REMOTE {}:{} -> {}:{}",
                            default_if_empty(&tunnel.remote_host, "localhost"),
                            tunnel.remote_port,
                            default_if_empty(&tunnel.local_host, "localhost"),
                            tunnel.local_port
                        )?;
                    }
                    TunnelType::Dynamic => {
                        write!(
                            output,
                            "DYNAMIC (SOCKS) {}:{}",
                            default_if_empty(&tunnel.local_host, "localhost"),
                            tunnel.local_port
                        )?;
                    }
                }
                if let Some(description) = tunnel
                    .description
                    .as_deref()
                    .filter(|desc| !desc.is_empty())
                {
                    write!(output, " - {description}")?;
                }
                output.push('\n');
            }
        }

        output.push('\n');
    }

    std::fs::write(path, output)?;
    Ok(())
}

fn export_putty_cm(connections: &[ConnectionSettings], path: &Path) -> Result<()> {
    let mut output = String::from("\u{feff}Name,Protocol,Host,Port,Username,Group,LocalTunnels,RemoteTunnels,DynamicTunnels,Comment\n");

    for connection in connections {
        let (local_tunnels, remote_tunnels, dynamic_tunnels) =
            collect_putty_tunnels(&connection.tunnels);
        let group = connection
            .group
            .as_deref()
            .unwrap_or_default()
            .replace('/', "\\");
        let comment = format!("Exported from KorTTY (ID: {})", connection.id);

        writeln!(
            output,
            "{},SSH,{},{},{},{},{},{},{},{}",
            escape_csv(&connection.name),
            escape_csv(&connection.host),
            connection.port,
            escape_csv(&connection.username),
            escape_csv(&group),
            escape_csv(&local_tunnels),
            escape_csv(&remote_tunnels),
            escape_csv(&dynamic_tunnels),
            escape_csv(&comment)
        )?;
    }

    std::fs::write(path, output)?;
    Ok(())
}

fn sanitize_mobaxterm_name(name: &str) -> String {
    name.replace(['=', '%', '#'], "_")
}

fn export_mtputty_name(name: &str) -> String {
    if name.is_empty() {
        "Unnamed".to_string()
    } else {
        name.to_string()
    }
}

fn default_if_empty<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() {
        fallback
    } else {
        value
    }
}

fn tunnel_type_name(tunnel_type: &TunnelType) -> &'static str {
    match tunnel_type {
        TunnelType::Local => "LOCAL",
        TunnelType::Remote => "REMOTE",
        TunnelType::Dynamic => "DYNAMIC",
    }
}

fn collect_putty_tunnels(tunnels: &[TunnelConfig]) -> (String, String, String) {
    let mut local = Vec::new();
    let mut remote = Vec::new();
    let mut dynamic = Vec::new();

    for tunnel in tunnels.iter().filter(|tunnel| tunnel.enabled) {
        match tunnel.tunnel_type {
            TunnelType::Local => local.push(format!(
                "{}:{}:{}",
                tunnel.local_port,
                default_if_empty(&tunnel.remote_host, "localhost"),
                tunnel.remote_port
            )),
            TunnelType::Remote => remote.push(format!(
                "{}:{}:{}",
                tunnel.remote_port,
                default_if_empty(&tunnel.local_host, "localhost"),
                tunnel.local_port
            )),
            TunnelType::Dynamic => dynamic.push(tunnel.local_port.to_string()),
        }
    }

    (local.join(";"), remote.join(";"), dynamic.join(";"))
}

fn write_xml_tag(output: &mut String, indent: usize, name: &str, value: &str) -> Result<()> {
    writeln!(
        output,
        "{space}<{name}>{value}</{name}>",
        space = " ".repeat(indent),
        value = escape_xml(value)
    )?;
    Ok(())
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn escape_csv(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        return format!("\"{}\"", value.replace('"', "\"\""));
    }
    value.to_string()
}
