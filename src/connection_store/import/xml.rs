use std::io;

use roxmltree::{Document, Node, ParsingOptions};
use zeroize::Zeroizing;

use super::{Format, ImportedProfile, Preview, invalid, kortty, ssh_profile};

const MAX_NODES: u32 = 32_768;

pub(super) fn parse(bytes: &[u8]) -> io::Result<Preview> {
    let utf16 = bytes
        .strip_prefix(&[0xff, 0xfe])
        .map(|bytes| (bytes, u16::from_le_bytes as fn([u8; 2]) -> u16))
        .or_else(|| {
            bytes
                .strip_prefix(&[0xfe, 0xff])
                .map(|bytes| (bytes, u16::from_be_bytes as _))
        });
    if let Some((bytes, decode)) = utf16 {
        let (units, remainder) = bytes.as_chunks::<2>();
        if !remainder.is_empty() {
            return Err(invalid("incomplete UTF-16 code unit in connection XML"));
        }
        let units: Zeroizing<Vec<u16>> =
            Zeroizing::new(units.iter().copied().map(decode).collect());
        let xml = Zeroizing::new(
            String::from_utf16(&units).map_err(|_| invalid("invalid UTF-16 in connection XML"))?,
        );
        return parse_xml(&xml);
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let xml = std::str::from_utf8(bytes)
        .map_err(|_| invalid("connection XML must use UTF-8 or UTF-16 with a byte-order mark"))?;
    parse_xml(xml)
}

fn parse_xml(xml: &str) -> io::Result<Preview> {
    let document = parse_document(xml)
        .map_err(|_| invalid("invalid connection XML or XML node limit exceeded"))?;
    let root = document.root_element();
    if has_unqualified_tag(root, "connections") {
        return kortty::parse(root);
    }
    let mut servers = root
        .descendants()
        .skip(1)
        .filter(|node| has_unqualified_tag(*node, "Server"))
        .peekable();
    if !has_unqualified_tag(root, "Servers") && servers.peek().is_none() {
        return Err(invalid(
            "expected a Servers document with MTPuTTY Server entries",
        ));
    }
    let mut preview = Preview {
        format: Format::Mtputty,
        ..Preview::default()
    };
    for server in servers {
        match parse_server(server) {
            Ok((connection, group)) => {
                preview.push(ImportedProfile::new(connection, group))?;
            }
            Err(_) => preview.invalid += 1,
        }
    }
    Ok(preview)
}

pub(super) fn parse_document(xml: &str) -> Result<Document<'_>, roxmltree::Error> {
    Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_NODES,
            entity_resolver: None,
        },
    )
}

fn parse_server(server: Node<'_, '_>) -> io::Result<(super::Connection, String)> {
    let host = scalar(server, "ServerName")?.unwrap_or_default();
    let name = scalar(server, "DisplayName")?;
    let port = scalar(server, "Port")?;
    let username = scalar(server, "UserName")?;
    let folder = scalar(server, "Folder")?;

    // The Java importer defaults only an absent username and never imports Password.
    ssh_profile(
        name.as_deref().unwrap_or(&host),
        &host,
        port.as_deref().unwrap_or(""),
        username.as_deref().unwrap_or("root"),
        folder.as_deref().unwrap_or(""),
        Vec::new(),
    )
}

fn scalar(server: Node<'_, '_>, field: &str) -> io::Result<Option<String>> {
    Ok(raw_scalar(server, field)?.map(|value| value.trim().to_owned()))
}

pub(super) fn raw_scalar(server: Node<'_, '_>, field: &str) -> io::Result<Option<String>> {
    let Some(node) = optional_child(server, field)? else {
        return Ok(None);
    };
    if node.children().any(|child| child.is_element()) {
        return Err(invalid("connection XML contains a non-scalar field"));
    }
    Ok(Some(
        node.children()
            .filter(|child| child.is_text())
            .filter_map(|text| text.text())
            .collect(),
    ))
}

pub(super) fn optional_child<'a, 'input>(
    server: Node<'a, 'input>,
    field: &str,
) -> io::Result<Option<Node<'a, 'input>>> {
    let mut nodes = server
        .children()
        .filter(|node| has_unqualified_tag(*node, field));
    let Some(node) = nodes.next() else {
        return Ok(None);
    };
    if nodes.next().is_some() {
        return Err(invalid("connection XML contains an ambiguous field"));
    }
    Ok(Some(node))
}

pub(super) fn has_unqualified_tag(node: Node<'_, '_>, name: &str) -> bool {
    node.tag_name().namespace().is_none() && node.has_tag_name(name)
}
