//! Java's JAXB highlighting fields from global-settings.xml.

use std::fmt::Write as _;
use std::io;

use roxmltree::{Document, Node, ParsingOptions};

use super::{Action, Configuration, MAX_BYTES, Rule, RuleSet, invalid};

pub(crate) fn decode_interchange(bytes: Vec<u8>) -> io::Result<Configuration> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("Highlighting file exceeds 1 MiB."));
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    if !bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| byte == b'<')
    {
        return Configuration::decode(bytes.to_vec());
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| invalid("Highlighting XML must use UTF-8."))?;
    let document = Document::parse_with_options(
        text,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 32768,
            entity_resolver: None,
        },
    )
    .map_err(|error| invalid(format!("Invalid highlighting XML: {error}")))?;
    let root = document.root_element();
    if !root.has_tag_name("globalSettings") {
        return Err(invalid(
            "Expected Java globalSettings XML or native highlighting JSON.",
        ));
    }
    let container = child(root, "highlightRuleSets")?
        .ok_or_else(|| invalid("The Java file has no highlightRuleSets section."))?;
    let mut sets = Vec::new();
    for set in container.children().filter(Node::is_element) {
        if !set.has_tag_name("ruleSet") {
            return Err(invalid("Unexpected highlight rule set element."));
        }
        let mut rules = Vec::new();
        if let Some(container) = child(set, "rules")? {
            for rule in container.children().filter(Node::is_element) {
                if !rule.has_tag_name("rule") {
                    return Err(invalid("Unexpected highlighting rule element."));
                }
                let action = match scalar(rule, "action")?.as_deref().unwrap_or("NONE") {
                    "NONE" => Action::None,
                    "NOTIFY" => Action::Notify,
                    "RUN_SNIPPET" => {
                        let id = scalar(rule, "snippetId")?
                            .unwrap_or_default()
                            .replace('-', "");
                        Action::RunSnippet(id)
                    }
                    _ => return Err(invalid("Unsupported Java highlight action.")),
                };
                rules.push(Rule {
                    id: scalar(rule, "id")?,
                    name: scalar(rule, "name")?.unwrap_or_default(),
                    pattern: scalar(rule, "pattern")?.unwrap_or_default(),
                    enabled: boolean(rule, "enabled", true)?,
                    regex: boolean(rule, "regex", false)?,
                    ignore_case: boolean(rule, "ignoreCase", true)?,
                    whole_word: boolean(rule, "wholeWord", false)?,
                    whole_line: match scalar(rule, "scope")?.as_deref().unwrap_or("MATCH") {
                        "MATCH" => false,
                        "LINE" => true,
                        _ => return Err(invalid("Unsupported Java highlight scope.")),
                    },
                    foreground: scalar(rule, "foreground")?,
                    background: scalar(rule, "background")?,
                    bold: boolean(rule, "bold", false)?,
                    italic: boolean(rule, "italic", false)?,
                    underline: boolean(rule, "underline", false)?,
                    action,
                    notify_with_text: boolean(rule, "notifyWithText", false)?,
                });
            }
        }
        sets.push(RuleSet {
            id: scalar(set, "id")?.ok_or_else(|| invalid("Java highlight set needs an ID."))?,
            name: scalar(set, "name")?.unwrap_or_else(|| "Imported set".into()),
            rules,
        });
    }
    let configuration = Configuration {
        enabled: boolean(root, "terminalHighlightingEnabled", true)?,
        alternate_screen: boolean(root, "terminalHighlightAlternateScreen", false)?,
        triggers_enabled: boolean(root, "terminalTriggersEnabled", false)?,
        default_set: scalar(root, "defaultHighlightRuleSetId")?.filter(|value| !value.is_empty()),
        sets,
        ..Configuration::default()
    };
    configuration.validate()?;
    Ok(configuration)
}

fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> io::Result<Option<Node<'a, 'input>>> {
    let mut children = node.children().filter(|child| child.has_tag_name(name));
    let first = children.next();
    if children.next().is_some() {
        return Err(invalid(format!("Duplicate highlighting XML field {name}.")));
    }
    Ok(first)
}

fn scalar(node: Node<'_, '_>, name: &str) -> io::Result<Option<String>> {
    let Some(value) = child(node, name)? else {
        return Ok(None);
    };
    if value.children().any(|child| child.is_element()) {
        return Err(invalid("Nested highlighting scalar."));
    }
    Ok(Some(value.text().unwrap_or_default().to_owned()))
}

fn boolean(node: Node<'_, '_>, name: &str, default: bool) -> io::Result<bool> {
    match scalar(node, name)?.as_deref() {
        None => Ok(default),
        Some("true" | "1") => Ok(true),
        Some("false" | "0") => Ok(false),
        _ => Err(invalid(format!("Invalid highlighting boolean {name}."))),
    }
}

fn field(xml: &mut String, name: &str, value: impl std::fmt::Display) {
    let _ = write!(xml, "<{name}>");
    for character in value.to_string().chars() {
        match character {
            '&' => xml.push_str("&amp;"),
            '<' => xml.push_str("&lt;"),
            '>' => xml.push_str("&gt;"),
            '"' => xml.push_str("&quot;"),
            '\'' => xml.push_str("&apos;"),
            _ => xml.push(character),
        }
    }
    let _ = writeln!(xml, "</{name}>");
}

pub(crate) fn encode_java(configuration: &Configuration) -> io::Result<Vec<u8>> {
    configuration.validate()?;
    if configuration
        .sets
        .iter()
        .flat_map(|set| {
            std::iter::once(set.name.as_str()).chain(
                set.rules
                    .iter()
                    .flat_map(|rule| [rule.name.as_str(), rule.pattern.as_str()]),
            )
        })
        .any(|text| {
            text.chars().any(|character| {
                !matches!(character as u32,
        0x9 | 0xa | 0xd | 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff)
            })
        })
    {
        return Err(invalid(
            "A highlighting value cannot be represented in XML 1.0.",
        ));
    }
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<globalSettings>\n");
    field(
        &mut xml,
        "terminalHighlightingEnabled",
        configuration.enabled,
    );
    field(
        &mut xml,
        "terminalHighlightAlternateScreen",
        configuration.alternate_screen,
    );
    field(
        &mut xml,
        "terminalTriggersEnabled",
        configuration.triggers_enabled,
    );
    if let Some(id) = &configuration.default_set {
        field(&mut xml, "defaultHighlightRuleSetId", id);
    }
    xml.push_str("<highlightRuleSets>\n");
    for set in &configuration.sets {
        xml.push_str("<ruleSet>\n");
        field(&mut xml, "id", &set.id);
        field(&mut xml, "name", &set.name);
        xml.push_str("<rules>\n");
        for (index, rule) in set.rules.iter().enumerate() {
            xml.push_str("<rule>\n");
            field(
                &mut xml,
                "id",
                rule.id.clone().unwrap_or_else(|| {
                    let hash = set.id.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
                        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
                    });
                    format!("rule.{hash:016x}.{index}")
                }),
            );
            field(&mut xml, "name", &rule.name);
            field(&mut xml, "pattern", &rule.pattern);
            field(&mut xml, "enabled", rule.enabled);
            field(&mut xml, "regex", rule.regex);
            field(&mut xml, "ignoreCase", rule.ignore_case);
            field(&mut xml, "wholeWord", rule.whole_word);
            field(
                &mut xml,
                "scope",
                if rule.whole_line { "LINE" } else { "MATCH" },
            );
            if let Some(color) = &rule.foreground {
                field(&mut xml, "foreground", color);
            }
            if let Some(color) = &rule.background {
                field(&mut xml, "background", color);
            }
            field(&mut xml, "bold", rule.bold);
            field(&mut xml, "italic", rule.italic);
            field(&mut xml, "underline", rule.underline);
            field(&mut xml, "notifyWithText", rule.notify_with_text);
            field(
                &mut xml,
                "action",
                match rule.action {
                    Action::None => "NONE",
                    Action::Notify => "NOTIFY",
                    Action::RunSnippet(_) => "RUN_SNIPPET",
                },
            );
            if let Action::RunSnippet(id) = &rule.action {
                let uuid = format!(
                    "{}-{}-{}-{}-{}",
                    &id[..8],
                    &id[8..12],
                    &id[12..16],
                    &id[16..20],
                    &id[20..]
                );
                field(&mut xml, "snippetId", uuid);
            }
            xml.push_str("</rule>\n");
        }
        xml.push_str("</rules>\n</ruleSet>\n");
    }
    xml.push_str("</highlightRuleSets>\n</globalSettings>\n");
    if xml.len() as u64 > MAX_BYTES {
        return Err(invalid("Highlighting XML exceeds 1 MiB."));
    }
    Ok(xml.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_jaxb_fields_round_trip_with_ids_scope_and_snippet_reference() {
        let xml = br#"<globalSettings><terminalHighlightingEnabled>true</terminalHighlightingEnabled>
            <terminalTriggersEnabled>false</terminalTriggersEnabled><defaultHighlightRuleSetId>ops</defaultHighlightRuleSetId>
            <highlightRuleSets><ruleSet><id>ops</id><name>Ops &amp; logs</name><rules><rule>
            <id>rule-1</id><name>Deploy</name><pattern>ready</pattern><scope>LINE</scope><bold>true</bold>
            <foreground>ansi:2</foreground><action>RUN_SNIPPET</action>
            <snippetId>00112233-4455-6677-8899-aabbccddeeff</snippetId><notifyWithText>true</notifyWithText>
            </rule></rules></ruleSet></highlightRuleSets></globalSettings>"#;
        let configuration = decode_interchange(xml.to_vec()).unwrap();
        let rule = &configuration.sets[0].rules[0];
        assert_eq!(rule.id.as_deref(), Some("rule-1"));
        assert!(rule.whole_line && rule.bold && rule.notify_with_text);
        assert_eq!(
            rule.action,
            Action::RunSnippet("00112233445566778899aabbccddeeff".into())
        );
        assert_eq!(
            decode_interchange(encode_java(&configuration).unwrap()).unwrap(),
            configuration
        );
    }

    #[test]
    fn java_import_rejects_dtd_duplicate_fields_and_unsupported_regex() {
        for source in [
            "<!DOCTYPE globalSettings [<!ENTITY x SYSTEM 'file:///private'>]><globalSettings><highlightRuleSets/></globalSettings>",
            "<globalSettings><highlightRuleSets/><highlightRuleSets/></globalSettings>",
            "<globalSettings><highlightRuleSets><ruleSet><id>ops</id><name>Ops</name><rules><rule><pattern>(?&lt;=x)y</pattern><regex>true</regex></rule></rules></ruleSet></highlightRuleSets></globalSettings>",
        ] {
            assert!(decode_interchange(source.as_bytes().to_vec()).is_err());
        }
    }
}
