use std::fmt::Write as _;
use std::io;

use serde::Serialize;
use zeroize::Zeroizing;

use super::super::super::metadata::xml_character;
use super::{Document, LogFormat};

pub(super) fn footer(format: LogFormat) -> &'static [u8] {
    match format {
        LogFormat::Xml => b"</session-log>\n",
        LogFormat::Json => b"{\"type\":\"end\"}\n",
        LogFormat::Yaml => b"end: true\n",
    }
}

pub(super) fn header(format: LogFormat, document: &Document, number: u32) -> io::Result<Vec<u8>> {
    let metadata = &document.metadata;
    let id = &document.id;
    let app_version = env!("CARGO_PKG_VERSION");
    let port = metadata.port.unwrap_or(0);
    let started_at = document.started.to_string();
    if format == LogFormat::Xml {
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<session-log formatVersion=\"1\"",
        );
        attribute(&mut xml, "journalId", id);
        let _ = write!(xml, " part=\"{number}\">\n<meta");
        attribute(&mut xml, "host", &metadata.host);
        let _ = write!(xml, " port=\"{port}\"");
        attribute(&mut xml, "username", &metadata.username);
        attribute(&mut xml, "connectionName", &metadata.connection_name);
        attribute(&mut xml, "appVersion", app_version);
        attribute(&mut xml, "startedAt", &started_at);
        attribute(&mut xml, "tabSessionId", id);
        xml.push_str("/>\n");
        return Ok(xml.into_bytes());
    }
    let mut bytes = Vec::new();
    if format == LogFormat::Yaml {
        bytes.extend_from_slice(b"formatVersion: 1\njournalId: ");
        serde_json::to_writer(&mut bytes, id).map_err(io::Error::other)?;
        let prefix = format!("\npart: {number}\nmeta: ");
        bytes.extend_from_slice(prefix.as_bytes());
    }
    serde_json::to_writer(
        &mut bytes,
        &serde_json::json!({
            "type": "meta",
            "formatVersion": 1,
            "journalId": id,
            "part": number,
            "host": &metadata.host,
            "port": port,
            "username": &metadata.username,
            "connectionName": &metadata.connection_name,
            "appVersion": app_version,
            "startedAt": &started_at,
            "tabSessionId": id,
        }),
    )
    .map_err(io::Error::other)?;
    bytes.push(b'\n');
    if format == LogFormat::Yaml {
        bytes.extend_from_slice(b"entries:\n");
    }
    Ok(bytes)
}

#[derive(Serialize)]
pub(super) struct Record<'a> {
    pub(super) seq: u64,
    pub(super) t: String,
    pub(super) k: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub(super) x: &'a str,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) partial: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) file: Option<&'a str>,
}

impl Record<'_> {
    pub(super) fn encode(&self, format: LogFormat) -> io::Result<Zeroizing<Vec<u8>>> {
        if format == LogFormat::Xml {
            return Ok(self.xml());
        }
        let mut bytes = Zeroizing::new(Vec::new());
        if format == LogFormat::Yaml {
            bytes.extend_from_slice(b"- ");
        }
        serde_json::to_writer(&mut *bytes, self).map_err(io::Error::other)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn xml(&self) -> Zeroizing<Vec<u8>> {
        let mut xml = Zeroizing::new(String::with_capacity(96 + self.x.len()));
        let _ = write!(xml, "<{} seq=\"{}\"", self.k, self.seq);
        attribute(&mut xml, "t", &self.t);
        if self.partial {
            xml.push_str(" partial=\"true\"");
        }
        if let Some(file) = self.file {
            attribute(&mut xml, "file", file);
        }
        if self.x.is_empty() {
            xml.push_str("/>\n");
        } else {
            xml.push('>');
            escaped(&mut xml, self.x);
            let _ = writeln!(xml, "</{}>", self.k);
        }
        Zeroizing::new(std::mem::take(&mut *xml).into_bytes())
    }
}

fn attribute(xml: &mut String, name: &str, value: &str) {
    let _ = write!(xml, " {name}=\"");
    escaped(xml, value);
    xml.push('"');
}

// Capture readers split physical lines before parsing XML. Character references preserve
// editable multiline notes without creating an extra record or XML attribute normalization.
fn escaped(xml: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => xml.push_str("&amp;"),
            '<' => xml.push_str("&lt;"),
            '>' => xml.push_str("&gt;"),
            '"' => xml.push_str("&quot;"),
            '\n' => xml.push_str("&#10;"),
            '\r' => xml.push_str("&#13;"),
            '\t' => xml.push_str("&#9;"),
            character if xml_character(character) => xml.push(character),
            _ => {}
        }
    }
}
