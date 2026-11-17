//! Imports the marker registry and ordered rules from Java global settings.

use std::collections::BTreeMap;
use std::io;

use roxmltree::Node;
use sha2::{Digest, Sha256};

use crate::terminal_theme::Rgb;

use super::{
    BUILT_INS, Definition, LegacyMarker, MAX_DEFINITIONS, MAX_RULES, Rule, Settings, builtin_id,
    compile, java_whitespace, normalize_marker, storage, trim, validate_name,
};

pub(crate) struct Import {
    pub(crate) contents: Option<Vec<u8>>,
    pub(crate) definitions: usize,
    pub(crate) rules: usize,
    pub(crate) omitted: usize,
    pub(crate) review: usize,
}

pub(crate) fn read(root: Node<'_, '_>) -> io::Result<Import> {
    let mut omitted = 0;
    let mut review = 0;
    let mut settings = Settings {
        enabled: match flag(root, "sessionJournalMarkerRulesEnabled", false) {
            Ok(enabled) => enabled,
            Err(()) => {
                omitted += 1;
                false
            }
        },
        ..Settings::default()
    };
    for node in rows(root, "sessionJournalMarkers", "marker", &mut omitted) {
        if settings.definitions.len() < MAX_DEFINITIONS
            && let Ok(definition) = definition(node, &mut omitted)
            && !settings.has_marker(&definition.id)
        {
            settings.definitions.push(definition);
        } else {
            omitted += 1;
        }
    }
    for node in rows(root, "sessionJournalMarkerRules", "rule", &mut omitted) {
        if settings.rules.len() < MAX_RULES
            && let Ok(mut rule) = rule(node)
            && settings.has_marker(&rule.marker_id)
            && rule.valid_pattern_text()
        {
            if rule.enabled
                && rule.regex
                && !rule.pattern.chars().all(java_whitespace)
                && !match rule.expression() {
                    Ok((expression, _)) => compile(&[expression]).is_ok(),
                    Err(_) => rule.advanced_matcher().is_ok(),
                }
            {
                rule.enabled = false;
                review += 1;
            }
            settings.rules.push(rule);
        } else {
            omitted += 1;
        }
    }
    review += fit_rules(&mut settings, 0)?;
    let definitions = settings.definitions.len();
    let rules = settings.rules.len();
    let contents = (definitions != 0 || rules != 0 || settings.enabled)
        .then(|| storage::encode(&settings))
        .transpose()?;
    Ok(Import {
        contents,
        definitions,
        rules,
        omitted,
        review,
    })
}

pub(crate) fn merge(local: Option<&[u8]>, incoming: &[u8]) -> io::Result<Option<Vec<u8>>> {
    let incoming = storage::decode(incoming)?;
    incoming.validate()?;
    let Some(local) = local else {
        return Ok(Some(storage::encode(&incoming)?));
    };
    let mut saved = storage::decode(local)?;
    saved.validate()?;
    let definition_count = saved.definitions.len();
    let rule_count = saved.rules.len();
    let mut ids = BTreeMap::new();
    for definition in incoming.definitions {
        if let Some(id) = add_definition(&mut saved, &definition)? {
            ids.insert(definition.id, id);
        }
    }
    let mut used = vec![false; rule_count];
    for mut rule in incoming.rules {
        if let Some(id) = ids.get(&rule.marker_id) {
            rule.marker_id.clone_from(id);
        } else if !builtin_id(&rule.marker_id) {
            continue;
        }
        if let Some((_, used)) = saved.rules.iter().zip(&mut used).find(|(existing, used)| {
            !**used
                && existing.marker_id == rule.marker_id
                && existing.pattern == rule.pattern
                && existing.regex == rule.regex
                && existing.ignore_case == rule.ignore_case
                && existing.java_regex == rule.java_regex
        }) {
            *used = true;
        } else if saved.rules.len() < MAX_RULES {
            saved.rules.push(rule);
        }
    }
    if saved.definitions.len() == definition_count && saved.rules.len() == rule_count {
        return Ok(None);
    }
    fit_rules(&mut saved, rule_count)?;
    Ok(Some(storage::encode(&saved)?))
}

fn add_definition(settings: &mut Settings, definition: &Definition) -> io::Result<Option<String>> {
    let matching = settings
        .definitions
        .iter()
        .find(|item| item.id == definition.id);
    if matching == Some(definition) {
        return Ok(Some(definition.id.clone()));
    }
    let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(definition)?));
    let alias = format!("java-{}", &digest[..26]);
    let mut alias_definition = definition.clone();
    alias_definition.id = alias;
    if let Some(existing) = settings
        .definitions
        .iter()
        .find(|item| item.id == alias_definition.id)
    {
        if existing == &alias_definition {
            return Ok(Some(alias_definition.id));
        }
        if matching.is_some() {
            return Ok(None);
        }
    }
    if settings.definitions.len() >= MAX_DEFINITIONS {
        return Ok(None);
    }
    let candidate = if matching.is_some() {
        alias_definition
    } else {
        definition.clone()
    };
    let id = candidate.id.clone();
    settings.definitions.push(candidate);
    Ok(Some(id))
}

fn fit_rules(settings: &mut Settings, previous: usize) -> io::Result<usize> {
    if settings.validate().is_ok() {
        return Ok(0);
    }
    // Keep the existing rules and the longest prefix of new rules that fits the matcher.
    let mut added = settings.rules.split_off(previous);
    settings.validate()?;
    let mut last_valid = 0;
    let mut first_invalid = added.len();
    while last_valid + 1 < first_invalid {
        let test = (last_valid + first_invalid) / 2;
        settings.rules.extend_from_slice(&added[..test]);
        if settings.validate().is_ok() {
            last_valid = test;
        } else {
            first_invalid = test;
        }
        settings.rules.truncate(previous);
    }
    let mut disabled = 0;
    for rule in &mut added[last_valid..] {
        disabled += usize::from(std::mem::replace(&mut rule.enabled, false));
    }
    settings.rules.extend(added);
    settings.validate()?;
    Ok(disabled)
}

fn definition(node: Node<'_, '_>, omitted: &mut usize) -> Result<Definition, ()> {
    let id = field(node, "id")?.and_then(normalize_marker).ok_or(())?;
    let name = field(node, "name")?.unwrap_or_default();
    validate_name(name).map_err(|_| ())?;
    let legacy = match field(node, "legacyMarker")? {
        None | Some("") => LegacyMarker::Info,
        Some(value) => BUILT_INS
            .iter()
            .map(|(_, _, marker)| *marker)
            .find(|marker| marker.xml() == value)
            .ok_or(())?,
    };
    let color = field(node, "color")?
        .filter(|value| !value.is_empty())
        .and_then(|value| {
            let value = value.strip_prefix('#').unwrap_or(value);
            if (1..=6).contains(&value.len())
                && let Ok(hex) = u32::from_str_radix(value, 16)
            {
                return Some(Rgb::from_hex(hex).bytes());
            }
            *omitted += 1;
            None
        });
    Ok(Definition {
        id,
        name: name.to_owned(),
        color,
        legacy,
    })
}

fn rule(node: Node<'_, '_>) -> Result<Rule, ()> {
    let regex = flag(node, "regex", false)?;
    Ok(Rule {
        marker_id: field(node, "markerId")?
            .and_then(normalize_marker)
            .ok_or(())?,
        pattern: field(node, "pattern")?.unwrap_or_default().to_owned(),
        regex,
        java_regex: regex,
        enabled: flag(node, "enabled", true)?,
        ignore_case: flag(node, "ignoreCase", true)?,
    })
}

fn rows<'a, 'input>(
    root: Node<'a, 'input>,
    section: &str,
    kind: &str,
    omitted: &mut usize,
) -> Vec<Node<'a, 'input>> {
    let mut sections = root.children().filter(|node| matches_tag(*node, section));
    let Some(section) = sections.next() else {
        return Vec::new();
    };
    if sections.next().is_some() || section.attributes().next().is_some() {
        *omitted += 1;
        return Vec::new();
    }
    let mut rows = Vec::new();
    for child in section.children().filter(Node::is_element) {
        if matches_tag(child, kind)
            && child.attributes().next().is_none()
            && rows.len() < MAX_DEFINITIONS.max(MAX_RULES) * 2
        {
            rows.push(child);
        } else {
            *omitted += 1;
        }
    }
    rows
}

fn field<'a>(parent: Node<'a, '_>, kind: &str) -> Result<Option<&'a str>, ()> {
    let mut fields = parent.children().filter(|node| matches_tag(*node, kind));
    let Some(field) = fields.next() else {
        return Ok(None);
    };
    if fields.next().is_some() || field.attributes().next().is_some() {
        return Err(());
    }
    let mut children = field.children();
    match (children.next(), children.next()) {
        (None, None) => Ok(Some("")),
        (Some(text), None) if text.is_text() => Ok(Some(trim(text.text().ok_or(())?))),
        _ => Err(()),
    }
}

fn flag(parent: Node<'_, '_>, kind: &str, default: bool) -> Result<bool, ()> {
    match field(parent, kind)? {
        None => Ok(default),
        Some("true" | "1") => Ok(true),
        Some("false" | "0") => Ok(false),
        _ => Err(()),
    }
}

fn matches_tag(node: Node<'_, '_>, kind: &str) -> bool {
    node.has_tag_name(kind) && node.tag_name().namespace().is_none()
}
