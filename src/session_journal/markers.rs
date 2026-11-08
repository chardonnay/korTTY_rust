//! Profile markers and an immutable, ordered matcher for live and saved journals.

use std::collections::HashSet;
use std::fmt;
use std::io;

use regex::{RegexSet, RegexSetBuilder};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::entry_edit::LegacyMarker;
use super::export::normalize_marker;
use super::is_display_character;
use super::metadata::java_whitespace;

mod storage;

pub(crate) use storage::{FILE, MAX_FILE_BYTES, validate_backup};

pub(crate) const MAX_NAME_BYTES: usize = 256;
pub(crate) const MAX_PATTERN_BYTES: usize = 1024;
pub(crate) const MAX_DEFINITIONS: usize = 124;
pub(crate) const MAX_RULES: usize = 128;
const MAX_ID_BYTES: usize = 32;
const MAX_MATCH_UNITS: usize = 20_000;
const MAX_REGEX_BYTES: usize = 2 * 1024 * 1024;

const BUILT_INS: [(&str, &str, LegacyMarker); 4] = [
    ("none", "None", LegacyMarker::None),
    ("info", "Info", LegacyMarker::Info),
    ("important", "Important", LegacyMarker::Important),
    ("error", "Error", LegacyMarker::Error),
];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Definition {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) color: Option<[u8; 3]>,
    #[serde(with = "legacy")]
    pub(crate) legacy: LegacyMarker,
}

impl Definition {
    pub(crate) fn is_builtin(&self) -> bool {
        builtin_id(&self.id)
    }

    fn name(&self) -> &str {
        let name = trim(&self.name);
        if name.is_empty() { &self.id } else { name }
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Rule {
    pub(crate) enabled: bool,
    pub(crate) marker_id: String,
    pub(crate) pattern: String,
    pub(crate) regex: bool,
    pub(crate) ignore_case: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            enabled: true,
            marker_id: String::new(),
            pattern: String::new(),
            regex: false,
            ignore_case: true,
        }
    }
}

impl fmt::Debug for Rule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JournalMarkerRule")
            .field("enabled", &self.enabled)
            .field("marker_id", &self.marker_id)
            .field("regex", &self.regex)
            .field("ignore_case", &self.ignore_case)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    pub(crate) definitions: Vec<Definition>,
    pub(crate) rules: Vec<Rule>,
    /// Opt-in for new entries. An explicit Apply Rules action uses the same rules independently.
    pub(crate) enabled: bool,
}

impl Settings {
    pub(crate) fn validate(&self) -> io::Result<()> {
        Compiled::new(self).map(|_| ())
    }

    pub(crate) fn registry(&self) -> Vec<Definition> {
        let mut definitions = builtin_definitions();
        definitions.extend(self.definitions.iter().map(|definition| Definition {
            name: definition.name().to_owned(),
            ..definition.clone()
        }));
        definitions
    }

    pub(crate) fn add_definition(
        &mut self,
        name: &str,
        color: [u8; 3],
        legacy: LegacyMarker,
    ) -> io::Result<usize> {
        self.insert_definition(name, name, Some(color), legacy)
    }

    pub(crate) fn duplicate_definition(&mut self, id: &str) -> io::Result<usize> {
        let source = self
            .definitions
            .iter()
            .find(|definition| definition.id == id)
            .ok_or_else(|| invalid("Select a custom journal marker to duplicate."))?;
        let name = source.name();
        let suffix = " (2)";
        let display = format!(
            "{}{suffix}",
            &name[..name.floor_char_boundary(MAX_NAME_BYTES - suffix.len())]
        );
        let base = format!("{name}-copy");
        self.insert_definition(&base, &display, source.color, source.legacy)
    }

    fn insert_definition(
        &mut self,
        id_source: &str,
        name: &str,
        color: Option<[u8; 3]>,
        legacy: LegacyMarker,
    ) -> io::Result<usize> {
        if self.definitions.len() >= MAX_DEFINITIONS {
            return Err(invalid(
                "The journal marker definition limit has been reached.",
            ));
        }
        validate_name(name)?;
        let base = normalize_marker(id_source).unwrap_or_else(|| "marker".into());
        let taken = |id: &str| {
            builtin_id(id)
                || self
                    .definitions
                    .iter()
                    .any(|definition| definition.id == id)
        };
        let mut id = base.clone();
        for counter in 2..=MAX_DEFINITIONS + BUILT_INS.len() + 1 {
            if !taken(&id) {
                break;
            }
            let suffix = format!("-{counter}");
            let prefix = &base[..base.len().min(MAX_ID_BYTES - suffix.len())];
            id = format!("{}{suffix}", prefix.trim_end_matches('-'));
        }
        if taken(&id) {
            return Err(invalid("Could not allocate a unique journal marker ID."));
        }
        let index = self.definitions.len();
        self.definitions.push(Definition {
            id,
            name: trim(name).into(),
            color,
            legacy,
        });
        Ok(index)
    }

    fn validate_fields(&self) -> io::Result<()> {
        if self.definitions.len() > MAX_DEFINITIONS || self.rules.len() > MAX_RULES {
            return Err(invalid(
                "The journal marker settings exceed their row limits.",
            ));
        }
        let mut ids: HashSet<_> = BUILT_INS.iter().map(|(id, _, _)| *id).collect();
        for definition in &self.definitions {
            validate_name(&definition.name)?;
            if !valid_id(&definition.id) || !ids.insert(definition.id.as_str()) {
                return Err(invalid(
                    "A journal marker ID is invalid, reserved or duplicated.",
                ));
            }
        }
        for rule in &self.rules {
            if rule.pattern.len() > MAX_PATTERN_BYTES
                || rule.pattern.chars().any(|character| {
                    character.is_control() && !matches!(character, '\n' | '\r' | '\t')
                })
            {
                return Err(invalid(
                    "A journal marker pattern exceeds its text limit or contains invalid characters.",
                ));
            }
            if !(ids.contains(rule.marker_id.as_str())
                || (rule.marker_id.is_empty() && trim(&rule.pattern).chars().all(java_whitespace)))
            {
                return Err(invalid(
                    "A journal marker rule refers to an unavailable marker.",
                ));
            }
        }
        Ok(())
    }
}

/// Shared, immutable profile snapshot. RegexSet uses Rust's bounded non-backtracking engine.
pub(crate) struct Compiled {
    enabled: bool,
    definitions: Vec<Definition>,
    matcher: Option<RegexSet>,
    targets: Vec<usize>,
}

impl Default for Compiled {
    fn default() -> Self {
        Self {
            enabled: false,
            definitions: builtin_definitions(),
            matcher: None,
            targets: Vec::new(),
        }
    }
}

impl fmt::Debug for Compiled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompiledJournalMarkers")
            .field("enabled", &self.enabled)
            .field("definitions", &self.definitions.len())
            .field("rules", &self.targets.len())
            .finish()
    }
}

impl Compiled {
    pub(crate) fn new(settings: &Settings) -> io::Result<Self> {
        settings.validate_fields()?;
        let definitions = settings.registry();
        let mut targets = Vec::new();
        let mut expressions = Vec::new();
        let mut rows = Vec::new();
        for (index, rule) in settings.rules.iter().enumerate() {
            let pattern = trim(&rule.pattern);
            if !rule.enabled || pattern.chars().all(java_whitespace) {
                continue;
            }
            let target = definitions
                .iter()
                .position(|definition| definition.id == rule.marker_id)
                .ok_or_else(|| invalid("A journal marker rule refers to an unavailable marker."))?;
            let expression = if rule.regex {
                pattern.into()
            } else {
                regex::escape(pattern)
            };
            expressions.push(if rule.ignore_case {
                format!("(?i){expression}")
            } else {
                expression
            });
            targets.push(target);
            rows.push(index + 1);
        }
        let matcher = if expressions.is_empty() {
            None
        } else {
            Some(compile(&expressions).map_err(|error| {
                if matches!(error, regex::Error::CompiledTooBig(_)) {
                    return invalid(
                        "The combined journal marker patterns exceed the matcher limit.",
                    );
                }
                let invalid_row = expressions.iter().zip(&rows).find_map(|(pattern, row)| {
                    compile(std::slice::from_ref(pattern))
                        .is_err()
                        .then_some(row)
                });
                invalid_row.map_or_else(
                    || invalid("The combined journal marker patterns exceed the matcher limit."),
                    |row| {
                        invalid(format!(
                            "Journal marker rule {row} has an invalid or unsupported pattern."
                        ))
                    },
                )
            })?)
        };
        Ok(Self {
            enabled: settings.enabled,
            definitions,
            matcher,
            targets,
        })
    }

    pub(crate) fn registry(&self) -> &[Definition] {
        &self.definitions
    }

    pub(crate) fn find(&self, id: &str) -> Option<&Definition> {
        self.definitions
            .iter()
            .find(|definition| definition.id == id)
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// Matches the first enabled rule in list order, even when live application is disabled.
    pub(crate) fn first_match(&self, haystack: &str) -> Option<&Definition> {
        let matcher = self.matcher.as_ref().filter(|_| !haystack.is_empty())?;
        let prefix = if haystack.len() <= MAX_MATCH_UNITS {
            haystack
        } else {
            utf16_prefix(haystack, MAX_MATCH_UNITS).0
        };
        let rule = matcher.matches(prefix).into_iter().next()?;
        Some(&self.definitions[self.targets[rule]])
    }
}

fn compile(expressions: &[String]) -> Result<RegexSet, regex::Error> {
    RegexSetBuilder::new(expressions)
        .size_limit(MAX_REGEX_BYTES)
        .dfa_size_limit(MAX_REGEX_BYTES)
        .nest_limit(32)
        .build()
}

/// Joins title, text, note and both excerpts in that order. A scalar is never split at the cap.
pub(crate) fn match_text<'a>(parts: impl IntoIterator<Item = &'a str>) -> Zeroizing<String> {
    let mut text = Zeroizing::new(String::with_capacity(512));
    let mut remaining = MAX_MATCH_UNITS;
    for part in parts.into_iter().filter(|part| !part.is_empty()) {
        if !text.is_empty() {
            text.push('\n');
            remaining -= 1;
        }
        let (prefix, used) = utf16_prefix(part, remaining);
        text.push_str(prefix);
        remaining -= used;
        if remaining == 0 || prefix.len() != part.len() {
            break;
        }
    }
    text
}

fn utf16_prefix(text: &str, limit: usize) -> (&str, usize) {
    let mut used = 0;
    for (index, character) in text.char_indices() {
        let units = character.len_utf16();
        if used + units > limit {
            return (&text[..index], used);
        }
        used += units;
    }
    (text, used)
}

fn validate_name(value: &str) -> io::Result<()> {
    if value.len() > MAX_NAME_BYTES
        || value
            .chars()
            .any(|character| character.is_control() || !is_display_character(character))
    {
        return Err(invalid(
            "A journal marker name exceeds its text limit or contains invalid characters.",
        ));
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    id.len() <= MAX_ID_BYTES && normalize_marker(id).as_deref() == Some(id)
}

fn builtin_id(id: &str) -> bool {
    BUILT_INS.iter().any(|(reserved, _, _)| *reserved == id)
}

fn builtin_definitions() -> Vec<Definition> {
    BUILT_INS
        .into_iter()
        .map(|(id, name, legacy)| Definition {
            id: id.into(),
            name: name.into(),
            color: None,
            legacy,
        })
        .collect()
}

fn trim(value: &str) -> &str {
    value.trim_matches(|character| character <= ' ')
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

mod legacy {
    use serde::{Deserialize, Deserializer, Serializer};

    use super::{BUILT_INS, LegacyMarker};

    pub(super) fn serialize<S: Serializer>(
        value: &LegacyMarker,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(value.xml())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<LegacyMarker, D::Error> {
        let value = String::deserialize(deserializer)?;
        BUILT_INS
            .iter()
            .find_map(|(_, _, marker)| (marker.xml() == value).then_some(*marker))
            .ok_or_else(|| serde::de::Error::custom("invalid legacy journal marker"))
    }
}

#[cfg(test)]
mod tests;
