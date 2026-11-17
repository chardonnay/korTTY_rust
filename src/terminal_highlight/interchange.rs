//! Java's JAXB highlighting fields from global-settings.xml.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::io;

use roxmltree::{Document, Node, ParsingOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Action, CompiledRule, Configuration, MAX_BYTES, MAX_SETS, Rule, RuleSet, invalid};

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
    let sets = container
        .children()
        .filter(Node::is_element)
        .map(read_set)
        .collect::<io::Result<Vec<_>>>()?;
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

fn read_set(set: Node<'_, '_>) -> io::Result<RuleSet> {
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
            let regex = boolean(rule, "regex", false)?;
            let pattern = scalar(rule, "pattern")?.unwrap_or_default();
            let java_regex = !regex || super::network::Kind::pinned(&pattern).is_none();
            rules.push(Rule {
                id: scalar(rule, "id")?,
                name: scalar(rule, "name")?.unwrap_or_default(),
                pattern,
                enabled: boolean(rule, "enabled", true)?,
                regex,
                java_regex,
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
    Ok(RuleSet {
        id: scalar(set, "id")?.ok_or_else(|| invalid("Java highlight set needs an ID."))?,
        name: scalar(set, "name")?.unwrap_or_else(|| "Imported set".into()),
        rules,
    })
}

#[derive(Default)]
pub(crate) struct BackupImport {
    pub(crate) contents: Option<Vec<u8>>,
    pub(crate) sets: usize,
    pub(crate) omitted_sets: usize,
    pub(crate) omitted_rules: usize,
    pub(crate) omitted_actions: usize,
    pub(crate) preferences: usize,
    pub(crate) omitted_preferences: usize,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PresentationPreferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    alternate_screen: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_set: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BackupData {
    presentation: PresentationPreferences,
    sets: Vec<RuleSet>,
}

impl PresentationPreferences {
    fn read(root: Node<'_, '_>, report: &mut BackupImport) -> Self {
        let mut flag = |name| match scalar(root, name) {
            Ok(Some(value)) if matches!(value.as_str(), "true" | "1") => Some(true),
            Ok(Some(value)) if matches!(value.as_str(), "false" | "0") => Some(false),
            Ok(None) => None,
            _ => {
                report.omitted_preferences += 1;
                None
            }
        };
        let enabled = flag("terminalHighlightingEnabled");
        let alternate_screen = flag("terminalHighlightAlternateScreen");
        let default_set = match scalar(root, "defaultHighlightRuleSetId") {
            Ok(None) => None,
            Ok(Some(id)) if id.trim().is_empty() => Some("none".into()),
            Ok(Some(id)) if super::valid_id(id.trim()) => Some(id.trim().to_owned()),
            _ => {
                report.omitted_preferences += 1;
                None
            }
        };
        report.preferences = usize::from(enabled.is_some())
            + usize::from(alternate_screen.is_some())
            + usize::from(default_set.is_some());
        Self {
            enabled,
            alternate_screen,
            default_set,
        }
    }
}

pub(crate) fn read_backup(root: Node<'_, '_>) -> io::Result<BackupImport> {
    let mut report = BackupImport::default();
    let mut backup = BackupData {
        presentation: PresentationPreferences::read(root, &mut report),
        sets: Vec::new(),
    };
    if report.preferences != 0 {
        report.contents = bounded_json(&backup)?;
    }
    let Some(container) = child(root, "highlightRuleSets")? else {
        return Ok(report);
    };
    let mut ids = HashSet::new();
    for node in container.children().filter(Node::is_element) {
        let Ok(mut set) = read_set(node) else {
            report.omitted_sets += 1;
            continue;
        };
        if backup.sets.len() == MAX_SETS
            || set.validate_header().is_err()
            || set.id == "none"
            || set.id.starts_with("builtin.")
            || !ids.insert(set.id.clone())
        {
            report.omitted_sets += 1;
            continue;
        }
        let original_rules = set.rules.len();
        set.rules.retain_mut(|rule| {
            if rule.action != Action::None || rule.notify_with_text {
                report.omitted_actions += 1;
                rule.action = Action::None;
                rule.notify_with_text = false;
            }
            CompiledRule::new(rule).is_ok()
        });
        report.omitted_rules += original_rules - set.rules.len();
        if original_rules != 0 && set.rules.is_empty() {
            report.omitted_sets += 1;
            continue;
        }
        backup.sets.push(set);
        if let Some(bytes) = bounded_json(&backup)? {
            report.contents = Some(bytes);
        } else {
            backup.sets.pop();
            report.omitted_sets += 1;
        }
    }
    report.sets = backup.sets.len();
    Ok(report)
}

pub(crate) fn validate_backup(bytes: &[u8]) -> io::Result<BackupData> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("Highlighting file exceeds 1 MiB."));
    }
    let configuration = match serde_json::from_slice::<BackupData>(bytes) {
        Ok(backup) => {
            let configuration = Configuration {
                default_set: backup.presentation.default_set.clone(),
                sets: backup.sets,
                ..Configuration::default()
            };
            return without_actions(configuration, backup.presentation);
        }
        // Older pending Java backups stored an ordinary configuration with defaults.
        Err(_) => Configuration::decode(bytes.to_vec())?,
    };
    without_actions(configuration, PresentationPreferences::default())
}

fn without_actions(
    mut configuration: Configuration,
    presentation: PresentationPreferences,
) -> io::Result<BackupData> {
    if configuration.triggers_enabled
        || configuration
            .sets
            .iter()
            .flat_map(|set| &set.rules)
            .any(|rule| rule.action != Action::None || rule.notify_with_text)
    {
        return Err(invalid(
            "Java highlight backup contains active trigger settings.",
        ));
    }
    for rule in configuration.sets.iter_mut().flat_map(|set| &mut set.rules) {
        if !rule.regex {
            rule.java_regex = true;
        }
    }
    configuration.validate()?;
    Ok(BackupData {
        presentation,
        sets: configuration.sets,
    })
}

#[derive(Default)]
pub(crate) struct BackupMerge {
    pub(crate) contents: Option<Vec<u8>>,
    pub(crate) set_ids: BTreeMap<String, String>,
}

pub(crate) fn merge_backup(local: Option<Vec<u8>>, incoming: &[u8]) -> io::Result<BackupMerge> {
    let mut fields: serde_json::Map<String, serde_json::Value> = local
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()?
        .unwrap_or_default();
    let mut local = local
        .map(Configuration::decode)
        .transpose()?
        .unwrap_or_default();
    let backup = validate_backup(incoming)?;
    let mut result = BackupMerge::default();
    let mut added = false;
    let mut sets_changed = false;
    for mut set in backup.sets {
        let source_id = set.id.clone();
        let remapped_id = format!("java-{:x}", Sha256::digest(serde_json::to_vec(&set)?));
        let mut legacy = set.clone();
        for rule in &mut legacy.rules {
            if !rule.regex {
                rule.java_regex = false;
            }
        }
        if legacy != set {
            legacy.id = format!("java-{:x}", Sha256::digest(serde_json::to_vec(&legacy)?));
            if let Some(index) = local.sets.iter().position(|known| known.id == legacy.id) {
                let mut upgraded = set.clone();
                upgraded.id = legacy.id.clone();
                if local.sets[index] == upgraded {
                    result.set_ids.insert(source_id, upgraded.id);
                    continue;
                }
                if local.sets[index] == legacy {
                    // The former content-derived ID confirms the import's
                    // provenance. Preserve links to that ID on upgrade.
                    let previous = std::mem::replace(&mut local.sets[index], upgraded);
                    if bounded_json(&local)?.is_some() {
                        result.set_ids.insert(source_id, legacy.id);
                        sets_changed = true;
                        continue;
                    }
                    local.sets[index] = previous;
                }
            }
        }
        let existing = local
            .sets
            .iter()
            .find(|candidate| candidate.id == source_id);
        let remapped = local
            .sets
            .iter()
            .find(|candidate| candidate.id == remapped_id);
        if remapped.is_some() || existing.is_some_and(|candidate| *candidate != set) {
            set.id = remapped_id;
        }
        match local.sets.iter().find(|candidate| candidate.id == set.id) {
            Some(candidate) if *candidate != set => continue,
            Some(_) => {}
            None if local.sets.len() == MAX_SETS => continue,
            None => {
                local.sets.push(set.clone());
                if bounded_json(&local)?.is_none() {
                    local.sets.pop();
                    continue;
                }
                sets_changed = true;
            }
        }
        result.set_ids.insert(source_id, set.id);
    }
    let preferences = backup.presentation;
    for (key, value) in [
        ("enabled", preferences.enabled.map(serde_json::Value::Bool)),
        (
            "alternate_screen",
            preferences.alternate_screen.map(serde_json::Value::Bool),
        ),
        (
            "default_set",
            preferences.default_set.as_deref().and_then(|id| {
                resolve_backup_set(&result.set_ids, id).map(serde_json::Value::String)
            }),
        ),
    ] {
        if let Some(value) = value
            && !fields.contains_key(key)
        {
            fields.insert(key.into(), value);
            added = true;
        }
    }
    if sets_changed {
        fields.insert("sets".into(), serde_json::to_value(&local.sets)?);
    }
    if added || sets_changed {
        result.contents = Some(
            bounded_json(&fields)?.ok_or_else(|| invalid("Highlighting file exceeds 1 MiB."))?,
        );
    }
    Ok(result)
}

pub(crate) fn resolve_backup_set(ids: &BTreeMap<String, String>, id: &str) -> Option<String> {
    if matches!(
        id,
        "none" | "builtin.errors" | "builtin.network" | "builtin.network-devices"
    ) {
        Some(id.to_owned())
    } else {
        ids.get(id).cloned()
    }
}

fn bounded_json(configuration: &impl Serialize) -> io::Result<Option<Vec<u8>>> {
    // Use the same representation and size limit as the native settings editor.
    let bytes = serde_json::to_vec_pretty(configuration)?;
    Ok((bytes.len() as u64 <= MAX_BYTES).then_some(bytes))
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
// Expected values are match ranges, not expanded character indices.
#[allow(clippy::single_range_in_vec_init)]
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
            r"<globalSettings><highlightRuleSets><ruleSet><id>ops</id><name>Ops</name><rules><rule><pattern>\p{javaUnknownProperty}</pattern><regex>true</regex></rule></rules></ruleSet></highlightRuleSets></globalSettings>",
        ] {
            assert!(decode_interchange(source.as_bytes().to_vec()).is_err());
        }
    }

    #[test]
    fn java_backup_upgrades_literal_rules_with_confirmed_legacy_ids() {
        let legacy = RuleSet {
            id: "ops".into(),
            name: "Ops".into(),
            rules: vec![Rule {
                id: Some("literal".into()),
                pattern: "foo".into(),
                whole_word: true,
                ..Rule::default()
            }],
        };
        let incoming = serde_json::to_vec(&BackupData {
            sets: vec![legacy.clone()],
            presentation: PresentationPreferences::default(),
        })
        .unwrap();
        let mut remapped = legacy.clone();
        remapped.id = format!(
            "java-{:x}",
            Sha256::digest(serde_json::to_vec(&legacy).unwrap())
        );
        let local = Configuration {
            default_set: Some(remapped.id.clone()),
            sets: vec![legacy, remapped.clone()],
            ..Configuration::default()
        };
        let merged = merge_backup(Some(serde_json::to_vec(&local).unwrap()), &incoming).unwrap();
        assert_eq!(merged.set_ids["ops"], remapped.id);
        let bytes = merged.contents.unwrap();
        let saved = Configuration::decode(bytes.clone()).unwrap();
        assert_eq!(saved.default_set, local.default_set);
        assert_eq!(saved.sets[0], local.sets[0]);
        assert_eq!(saved.sets[1].id, remapped.id);
        assert!(saved.sets[1].rules[0].java_regex);
        let replay = merge_backup(Some(bytes), &incoming).unwrap();
        assert_eq!(replay.set_ids, merged.set_ids);
        assert!(replay.contents.is_none());
    }

    #[test]
    fn java_whole_word_highlights_retry_alternatives_and_keep_literals() {
        let xml = br#"<globalSettings><highlightRuleSets><ruleSet><id>ops</id><name>Ops</name>
            <rules>
            <rule><pattern>foo-bar|foo</pattern><regex>true</regex><wholeWord>true</wholeWord>
            <foreground>ansi:2</foreground></rule>
            <rule><pattern>X\E+</pattern><regex>false</regex><wholeWord>true</wholeWord>
            <foreground>ansi:3</foreground></rule>
            </rules></ruleSet></highlightRuleSets></globalSettings>"#;
        let configuration = decode_interchange(xml.to_vec()).unwrap();
        let saved = Configuration::decode(serde_json::to_vec(&configuration).unwrap()).unwrap();
        let compiled = saved.sets[0].compile().unwrap();
        let ranges = |text| {
            compiled
                .presentation_ranges(text)
                .map(|(range, _)| range)
                .collect::<Vec<_>>()
        };
        assert_eq!(ranges("foo-barx"), [0..3]);
        assert!(ranges("zfoo-barx").is_empty());
        assert_eq!(ranges(r"(X\E+)"), [1..5]);
        assert!(ranges(r"(X\E+z)").is_empty());
    }

    #[test]
    fn java_highlight_regex_keeps_source_round_trips_and_paints_java_ranges() {
        let xml = br#"<globalSettings><highlightRuleSets><ruleSet><id>ops</id><name>Ops</name>
            <rules><rule><id>ticket-1</id><name>Ticket</name><pattern>(?&lt;!old-)\b(?&lt;ticket&gt;(?>\QTKT+\E)\d++)$</pattern>
            <regex>true</regex><ignoreCase>false</ignoreCase><foreground>ansi:2</foreground>
            </rule></rules></ruleSet></highlightRuleSets></globalSettings>"#;
        let configuration = decode_interchange(xml.to_vec()).unwrap();
        let rule = &configuration.sets[0].rules[0];
        assert!(rule.java_regex);
        assert_eq!(rule.pattern, r"(?<!old-)\b(?<ticket>(?>\QTKT+\E)\d++)$");
        let compiled = configuration.sets[0].compile().unwrap();
        let ranges = |text| {
            compiled
                .presentation_ranges(text)
                .map(|(range, _)| range)
                .collect::<Vec<_>>()
        };
        assert_eq!(ranges("éTKT+42\r\n"), [2..8]);
        assert!(ranges("éTKT+١").is_empty());
        assert!(ranges("old-TKT+42").is_empty());
        assert_eq!(
            Configuration::decode(serde_json::to_vec(&configuration).unwrap()).unwrap(),
            configuration
        );
        assert_eq!(
            decode_interchange(encode_java(&configuration).unwrap()).unwrap(),
            configuration
        );
    }
}
