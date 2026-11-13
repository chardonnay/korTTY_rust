//! Interchange formats used by the Java snippet variable manager.

use std::fmt::Write as _;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use roxmltree::{Document, Node, ParsingOptions};
use serde::Deserialize;
use serde_json::{json, value::RawValue};

use super::{MAX_VARIABLES, Variable, invalid, profile};

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

pub(super) fn read_java_backup(bytes: &[u8]) -> io::Result<Vec<Variable>> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(invalid("The variable file exceeds 8 MiB"));
    }
    let source =
        std::str::from_utf8(bytes).map_err(|_| invalid("Invalid variable XML encoding"))?;
    Format::Xml.decode(source)
}

#[derive(Clone, Copy)]
enum Format {
    Json,
    Xml,
    Yaml,
}

impl Format {
    fn from_path(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(invalid("Enter an absolute file path"));
        }
        match path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("json") => Ok(Self::Json),
            Some("xml") => Ok(Self::Xml),
            Some("yaml" | "yml") => Ok(Self::Yaml),
            _ => Err(invalid("Use a file ending in .json, .xml, .yaml, or .yml")),
        }
    }

    fn decode(self, source: &str) -> io::Result<Vec<Variable>> {
        let source = source.strip_prefix('\u{feff}').unwrap_or(source);
        let mut entries = match self {
            Self::Json => read_json(source)?,
            Self::Xml => read_xml(source)?,
            Self::Yaml => read_yaml(source)?,
        };
        entries.retain(|entry| !entry.name.trim().is_empty());
        for entry in &mut entries {
            entry.name = entry.name.trim().into();
            entry.validate()?;
        }
        Ok(entries)
    }

    fn encode(self, entries: &[Variable]) -> io::Result<String> {
        match self {
            Self::Json => {
                let variables: Vec<_> = entries
                    .iter()
                    .map(|entry| json!({ "name": entry.name, "value": entry.value }))
                    .collect();
                let mut output = serde_json::to_string_pretty(&json!({ "variables": variables }))
                    .map_err(|_| invalid("Could not encode variable JSON"))?;
                output.push('\n');
                Ok(output)
            }
            Self::Xml => write_xml(entries),
            Self::Yaml => write_yaml(entries),
        }
    }
}

#[derive(PartialEq, Eq)]
pub(crate) enum Action {
    Import(PathBuf),
    Export(PathBuf, Vec<Variable>),
}

#[derive(Clone)]
pub(crate) enum Outcome {
    Imported(Vec<Variable>),
    Exported(usize),
}

impl Action {
    pub(crate) fn run(self) -> io::Result<Outcome> {
        match self {
            Self::Import(path) => {
                let format = Format::from_path(&path)?;
                let file = profile::open_preference(&path)?;
                let metadata = file.metadata()?;
                if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
                    return Err(invalid("Choose a regular variable file of at most 8 MiB"));
                }
                let mut source = String::new();
                file.take(MAX_FILE_BYTES + 1).read_to_string(&mut source)?;
                if source.len() as u64 > MAX_FILE_BYTES {
                    return Err(invalid("The variable file exceeds 8 MiB"));
                }
                Ok(Outcome::Imported(format.decode(&source)?))
            }
            Self::Export(path, entries) => {
                let output = Format::from_path(&path)?.encode(&entries)?;
                profile::save_new_atomic(&path, output.as_bytes())?;
                Ok(Outcome::Exported(entries.len()))
            }
        }
    }
}

fn check_count(count: usize) -> io::Result<()> {
    if count > MAX_VARIABLES {
        return Err(invalid("The variable file contains more than 256 entries"));
    }
    Ok(())
}

fn read_json(source: &str) -> io::Result<Vec<Variable>> {
    #[derive(Deserialize)]
    struct Wrapper<'a> {
        #[serde(borrow)]
        variables: Vec<&'a RawValue>,
    }

    #[derive(Deserialize)]
    struct Entry<'a> {
        #[serde(borrow)]
        name: Option<&'a RawValue>,
        #[serde(borrow)]
        value: Option<&'a RawValue>,
    }

    let entries = if source.trim_start().starts_with('[') {
        serde_json::from_str::<Vec<&RawValue>>(source)
    } else {
        serde_json::from_str::<Wrapper<'_>>(source).map(|root| root.variables)
    }
    .map_err(|_| invalid("Expected a JSON variables array"))?;
    check_count(entries.len())?;
    entries
        .into_iter()
        .map(RawValue::get)
        .filter(|entry| entry.starts_with('{'))
        .map(|raw| {
            let entry: Entry<'_> =
                serde_json::from_str(raw).map_err(|_| invalid("Invalid JSON variable"))?;
            Ok(Variable {
                name: json_text(entry.name)?,
                value: json_text(entry.value)?,
            })
        })
        .collect()
}

fn json_text(value: Option<&RawValue>) -> io::Result<String> {
    let Some(value) = value.map(RawValue::get) else {
        return Ok(String::new());
    };
    match value.as_bytes().first() {
        Some(b'"') => serde_json::from_str(value).map_err(|_| invalid("Invalid JSON variable")),
        Some(b'-' | b'0'..=b'9' | b't' | b'f') => Ok(value.to_owned()),
        _ => Err(invalid("Expected a scalar variable name or value")),
    }
}

fn read_xml(source: &str) -> io::Result<Vec<Variable>> {
    let document = Document::parse_with_options(
        source,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 8192,
            ..ParsingOptions::default()
        },
    )
    .map_err(|_| invalid("Invalid variable XML"))?;
    let root = document.root_element();
    if !xml_is(root, "snippetVariables") {
        return Err(invalid("Expected an XML snippetVariables element"));
    }
    let mut entries = Vec::new();
    for child in xml_elements(root)? {
        if !xml_is(child, "variable") {
            return Err(invalid("Expected an XML variable element"));
        }
        check_count(entries.len() + 1)?;
        let mut name = None;
        let mut value = None;
        for field in xml_elements(child)? {
            let target = if xml_is(field, "name") {
                &mut name
            } else if xml_is(field, "value") {
                &mut value
            } else {
                return Err(invalid("Expected an XML variable name or value"));
            };
            if target.is_some()
                || field
                    .children()
                    .any(|node| node.is_element() || node.is_pi())
            {
                return Err(invalid("Invalid XML variable field"));
            }
            *target = Some(
                field
                    .children()
                    .filter(Node::is_text)
                    .filter_map(|n| n.text())
                    .collect(),
            );
        }
        entries.push(Variable {
            name: name.unwrap_or_default(),
            value: value.unwrap_or_default(),
        });
    }
    Ok(entries)
}

pub(super) fn xml_is(node: Node<'_, '_>, name: &str) -> bool {
    node.has_tag_name(name) && node.tag_name().namespace().is_none() && node.attributes().len() == 0
}

pub(super) fn xml_elements<'a, 'input>(
    parent: Node<'a, 'input>,
) -> io::Result<impl Iterator<Item = Node<'a, 'input>>> {
    if parent.children().any(|child| {
        child.is_pi()
            || (child.is_text() && child.text().is_some_and(|text| !text.trim().is_empty()))
    }) {
        return Err(invalid(
            "Unexpected text or processing instruction inside an XML element",
        ));
    }
    Ok(parent.children().filter(Node::is_element))
}

fn write_xml(entries: &[Variable]) -> io::Result<String> {
    let mut output = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<snippetVariables>\n",
    );
    for entry in entries {
        output.push_str("    <variable>\n        <name>");
        escape_xml(&mut output, &entry.name)?;
        output.push_str("</name>\n        <value>");
        escape_xml(&mut output, &entry.value)?;
        output.push_str("</value>\n    </variable>\n");
    }
    output.push_str("</snippetVariables>\n");
    Ok(output)
}

pub(super) fn escape_xml(output: &mut String, input: &str) -> io::Result<()> {
    for ch in input.chars() {
        match ch {
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '&' => output.push_str("&amp;"),
            '\r' => output.push_str("&#13;"),
            '\t'
            | '\n'
            | ' '..='\u{d7ff}'
            | '\u{e000}'..='\u{fffd}'
            | '\u{10000}'..='\u{10ffff}' => {
                output.push(ch);
            }
            _ => {
                return Err(invalid(
                    "The text contains a character XML 1.0 cannot represent",
                ));
            }
        }
    }
    Ok(())
}

fn read_yaml(source: &str) -> io::Result<Vec<Variable>> {
    let mut found_root = false;
    let mut empty_list = false;
    let mut entries: Vec<Variable> = Vec::new();
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line == "---" {
            continue;
        }
        let item = line
            .strip_prefix("- ")
            .or_else(|| (line == "-").then_some(""));
        if item.is_none() && raw == raw.trim_start() {
            let rest = line
                .strip_prefix("variables:")
                .filter(|_| !found_root)
                .ok_or_else(|| invalid("Expected a YAML variables list"))?;
            match plain_yaml(rest.trim()) {
                "" => {}
                "[]" => empty_list = true,
                _ => return Err(invalid("Expected a YAML variables list")),
            }
            found_root = true;
            continue;
        }
        if !found_root || empty_list {
            return Err(invalid("Expected a YAML variables list"));
        }
        if item.is_some() {
            check_count(entries.len() + 1)?;
            entries.push(Variable {
                name: String::new(),
                value: String::new(),
            });
        }
        let entry = entries
            .last_mut()
            .ok_or_else(|| invalid("Expected a YAML variable entry"))?;
        let field = item.unwrap_or(line).trim();
        if field.is_empty() {
            continue;
        }
        let (key, scalar) = field
            .split_once(':')
            .ok_or_else(|| invalid("Expected a YAML variable name or value"))?;
        match key.trim() {
            "name" => entry.name = yaml_text(scalar.trim())?,
            "value" => entry.value = yaml_text(scalar.trim())?,
            _ => return Err(invalid("Expected a YAML variable name or value")),
        }
    }
    if !found_root {
        return Err(invalid("Expected a YAML variables list"));
    }
    Ok(entries)
}

fn plain_yaml(scalar: &str) -> &str {
    if scalar.starts_with('#') {
        return "";
    }
    scalar
        .split_once(" #")
        .map_or(scalar, |(text, _)| text.trim_end())
}

pub(super) fn yaml_text(scalar: &str) -> io::Result<String> {
    if scalar.starts_with('"') {
        let end = quoted_yaml_end(scalar, '"')?;
        return serde_json::from_str(&scalar[..end])
            .map_err(|_| invalid("Invalid quoted YAML scalar"));
    }
    if scalar.starts_with('\'') {
        let end = quoted_yaml_end(scalar, '\'')?;
        return Ok(scalar[1..end - 1].replace("''", "'"));
    }
    Ok(plain_yaml(scalar).into())
}

fn quoted_yaml_end(scalar: &str, quote: char) -> io::Result<usize> {
    let mut chars = scalar.char_indices().skip(1).peekable();
    while let Some((offset, ch)) = chars.next() {
        if (quote == '"' && ch == '\\')
            || (quote == '\''
                && ch == quote
                && chars.peek().is_some_and(|&(_, next)| next == quote))
        {
            chars.next();
        } else if ch == quote {
            if plain_yaml(scalar[offset + 1..].trim_start()).is_empty() {
                return Ok(offset + 1);
            }
            break;
        }
    }
    Err(invalid("Invalid quoted YAML scalar"))
}

fn write_yaml(entries: &[Variable]) -> io::Result<String> {
    if entries.is_empty() {
        return Ok("variables: []\n".into());
    }
    let mut output = String::from("variables:\n");
    for entry in entries {
        let name = serde_json::to_string(&entry.name)
            .map_err(|_| invalid("Could not encode variable name"))?;
        let value = serde_json::to_string(&entry.value)
            .map_err(|_| invalid("Could not encode variable value"))?;
        let _ = writeln!(output, "  - name: {name}\n    value: {value}");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::tests::Profile;
    use std::fs;

    // Produced by SnippetVariableExchange at Java reference commit 1ebbe724ee84.
    const FIXTURES: [(Format, &str, &str); 3] = [
        (
            Format::Json,
            "variables.json",
            include_str!("fixtures/variables.json"),
        ),
        (
            Format::Xml,
            "variables.xml",
            include_str!("fixtures/variables.xml"),
        ),
        (
            Format::Yaml,
            "variables.yaml",
            include_str!("fixtures/variables.yaml"),
        ),
    ];

    #[test]
    fn variable_exchange_matches_pinned_java_fixtures() {
        let profile = Profile::new();
        for (format, file_name, fixture) in FIXTURES {
            let fixture = fixture.replace("\r\n", "\n");
            let source = profile.0.join(format!("java-{file_name}"));
            fs::write(&source, &fixture).unwrap();
            let Outcome::Imported(entries) = Action::Import(source).run().unwrap() else {
                panic!("expected imported variables");
            };
            assert_eq!(entries.len(), 3);
            assert_eq!(
                (&*entries[0].name, &*entries[0].value),
                ("host", "東京 & <db>")
            );
            assert_eq!(entries[1].value, "one\r\ntwo: \"quoted\" # café \\");
            assert!(entries[2].default_value().is_none());
            assert_eq!(format.encode(&entries).unwrap(), fixture);
            let destination = profile.0.join(file_name);
            assert!(matches!(
                Action::Export(destination.clone(), entries.clone()).run(),
                Ok(Outcome::Exported(3))
            ));
            assert_eq!(fs::read_to_string(&destination).unwrap(), fixture);
            assert_eq!(
                Action::Export(destination, entries)
                    .run()
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::AlreadyExists
            );
        }
    }

    #[test]
    fn variable_exchange_rejects_unsafe_and_oversized_input() {
        for source in [
            r#"<!DOCTYPE snippetVariables SYSTEM "file:///etc/passwd"><snippetVariables/>"#,
            r#"<!DOCTYPE snippetVariables [<!ENTITY inject "data">]><snippetVariables><variable><name>&inject;</name></variable></snippetVariables>"#,
            "<snippetVariables><variable><name><markup/></name></variable></snippetVariables>",
        ] {
            assert!(Format::Xml.decode(source).is_err());
        }
        assert!(Format::Json.decode(r#"{"other": []}"#).is_err());
        assert!(Format::Yaml.decode("variables: {name: wrong}").is_err());
        assert!(
            Format::Json
                .decode(&format!(
                    r#"[{{"name":"too_large","value":"{}"}}]"#,
                    "x".repeat(super::super::MAX_VARIABLE_VALUE_BYTES + 1)
                ))
                .is_err()
        );
        let profile = Profile::new();
        let path = profile.0.join("huge.xml");
        fs::File::create(&path)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert!(Action::Import(path).run().is_err());
        let path = profile.0.join("invalid.xml");
        assert!(
            Action::Export(
                path.clone(),
                vec![Variable {
                    name: "control".into(),
                    value: "\x01".into()
                }]
            )
            .run()
            .is_err()
        );
        assert!(!path.exists());
    }

    #[test]
    fn variable_exchange_reads_reference_handwritten_inputs() {
        let json = Format::Json
            .decode(r#"[{"name":" a ","value":12.00e-07},null,{"value":"orphan"}]"#)
            .unwrap();
        assert_eq!(json.len(), 1);
        assert_eq!((&*json[0].name, &*json[0].value), ("a", "12.00e-07"));
        let yaml = Format::Yaml.decode(
            "# exported by hand\nvariables:\n  - name: user\n    value: 'it''s me'\n  - name: port\n    value: 8080 # default\n  - value: orphan\n",
        ).unwrap();
        assert_eq!(yaml.len(), 2);
        assert_eq!(yaml[0].value, "it's me");
        assert_eq!(yaml[1].value, "8080");
        let xml = Format::Xml
            .decode("<snippetVariables><variable><name>host</name></variable></snippetVariables>")
            .unwrap();
        assert_eq!(xml[0].name, "host");
        assert!(xml[0].default_value().is_none());
    }
}
