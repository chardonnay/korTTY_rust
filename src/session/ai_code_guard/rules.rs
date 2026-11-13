//! Screen rules from Java 1ebbe724ee84's coding-agents resources.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, LazyLock};

use kortty_terminal::ScreenSnapshot;
use regex::{Regex, RegexBuilder, RegexSet, RegexSetBuilder};
use serde::{Deserialize, Deserializer};

use super::overrides;

const MAX_ROWS: usize = 256;
const MAX_CELLS: usize = 512;
const MAX_RULES: usize = 128;
const MAX_LINE_PATTERNS: usize = 64;
const MAX_REGEX_BYTES: usize = 4 * 1024;
const REGEX_SIZE_LIMIT: usize = 256 * 1024;
const EXPLAIN_EVIDENCE_LIMIT: usize = 256;
const ROSTER_EVIDENCE_LIMIT: usize = 80;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(super) enum Kind {
    Aider,
    Amp,
    Antigravity,
    ClaudeCode,
    Cline,
    Codex,
    Continue,
    Copilot,
    Crush,
    CursorAgent,
    GeminiCli,
    Goose,
    Grok,
    GrokBuild,
    Jules,
    Kilo,
    Kiro,
    MinimaxCode,
    Opencode,
    QwenCode,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum State {
    Blocked,
    Done,
    Working,
    #[default]
    Idle,
    Unknown,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct RuleDetails {
    pub(crate) id: Arc<str>,
    pub(crate) evidence: Arc<str>,
}

impl RuleDetails {
    pub(crate) fn roster_evidence(&self) -> Cow<'_, str> {
        limit_evidence(&self.evidence, ROSTER_EVIDENCE_LIMIT)
    }
}

impl fmt::Debug for RuleDetails {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RuleDetails([redacted])")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuleFile {
    kind: Kind,
    version: u32,
    #[serde(rename = "comment")]
    _comment: Option<String>,
    #[serde(default, deserialize_with = "default_if_null")]
    fallback_state: State,
    rules: Vec<RuleSource>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuleSource {
    id: String,
    state: State,
    priority: i32,
    region: Option<Region>,
    #[serde(default, deserialize_with = "default_if_null")]
    any_line_regex: Vec<String>,
    regex: Option<String>,
    #[serde(default, deserialize_with = "default_if_null")]
    contains: Vec<String>,
    #[serde(default, deserialize_with = "default_if_null")]
    not_contains: Vec<String>,
    title: Option<String>,
    alternate_screen: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Region {
    #[serde(default)]
    bottom_non_empty_lines: usize,
}

pub(super) struct RuleSet {
    fallback: State,
    rules: Vec<Rule>,
}

struct Rule {
    id: String,
    state: State,
    bottom_non_empty_lines: usize,
    any_line: RegexSet,
    joined: Option<Regex>,
    contains: Vec<String>,
    not_contains: Vec<String>,
    title: Option<Regex>,
    alternate: Option<bool>,
}

static BUNDLED: LazyLock<HashMap<Kind, RuleSet>> = LazyLock::new(|| {
    let files: Vec<RuleFile> =
        serde_json::from_str(include_str!("java-bundled.json")).expect("valid bundled agent rules");
    files
        .into_iter()
        .map(|file| {
            let kind = file.kind;
            (kind, file.compile().expect("valid bundled agent rules"))
        })
        .collect()
});

fn default_if_null<'de, T: Deserialize<'de> + Default, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

impl RuleSet {
    pub(super) fn from_override(kind: Kind, bytes: &[u8]) -> Result<Self, String> {
        let file: RuleFile = serde_json::from_slice(bytes).map_err(|error| {
            format!(
                "Invalid rule schema at line {}, column {}.",
                error.line(),
                error.column()
            )
        })?;
        if file.kind != kind {
            return Err("The kind does not match the file name.".into());
        }
        file.compile()
    }
}

impl RuleFile {
    fn compile(mut self) -> Result<RuleSet, String> {
        if self.version != 1 {
            return Err("The rule file must use version 1.".into());
        }
        if self.rules.len() > MAX_RULES {
            return Err(format!("A rule file supports up to {MAX_RULES} rules."));
        }
        let mut ids = HashSet::new();
        for rule in &self.rules {
            if rule.id.is_empty()
                || rule.id.len() > 128
                || !rule
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            {
                return Err(
                    "A rule ID must use letters, digits, dots, underscores or dashes.".into(),
                );
            }
            if !ids.insert(rule.id.as_str()) {
                return Err(format!("Duplicate rule ID: {}.", rule.id));
            }
        }
        self.rules
            .sort_by_key(|rule| std::cmp::Reverse(rule.priority));
        let rules = self
            .rules
            .into_iter()
            .map(RuleSource::compile)
            .collect::<Result<_, _>>()?;
        Ok(RuleSet {
            fallback: self.fallback_state,
            rules,
        })
    }
}

impl RuleSource {
    fn compile(self) -> Result<Rule, String> {
        let id = self.id;
        if self.state == State::Unknown {
            return Err(format!("Rule {id} cannot use UNKNOWN as its state."));
        }
        if self.any_line_regex.is_empty()
            && self.regex.is_none()
            && self.contains.is_empty()
            && self.title.is_none()
            && self.alternate_screen.is_none()
        {
            return Err(format!("Rule {id} has no matching condition."));
        }
        if self
            .any_line_regex
            .iter()
            .chain(&self.contains)
            .chain(&self.not_contains)
            .any(String::is_empty)
        {
            return Err(format!("Rule {id} contains an empty condition."));
        }
        if self.any_line_regex.len() > MAX_LINE_PATTERNS
            || self
                .any_line_regex
                .iter()
                .chain(self.regex.iter())
                .chain(self.title.iter())
                .any(|pattern| pattern.len() > MAX_REGEX_BYTES)
        {
            return Err(format!("Rule {id} exceeds the regular expression limit."));
        }
        let regex_error =
            |_| format!("Rule {id} has an invalid or unsupported regular expression.");
        let any_line = RegexSetBuilder::new(&self.any_line_regex)
            .size_limit(REGEX_SIZE_LIMIT)
            .dfa_size_limit(REGEX_SIZE_LIMIT)
            .build()
            .map_err(regex_error)?;
        let compile = |pattern: String, multiline| {
            RegexBuilder::new(&pattern)
                .multi_line(multiline)
                .dot_matches_new_line(multiline)
                .size_limit(REGEX_SIZE_LIMIT)
                .dfa_size_limit(REGEX_SIZE_LIMIT)
                .build()
                .map_err(regex_error)
        };
        let joined = self
            .regex
            .map(|pattern| compile(pattern, true))
            .transpose()?;
        let title = self
            .title
            .map(|pattern| compile(pattern, false))
            .transpose()?;
        Ok(Rule {
            id,
            state: self.state,
            bottom_non_empty_lines: self
                .region
                .map_or(0, |region| region.bottom_non_empty_lines),
            any_line,
            joined,
            contains: self.contains,
            not_contains: self.not_contains,
            title,
            alternate: self.alternate_screen,
        })
    }
}

struct Evidence<'a> {
    lines: Vec<&'a str>,
    title: Option<&'a str>,
    alternate: bool,
}

struct RuleMatch<'a> {
    id: &'a str,
    evidence: Option<Arc<str>>,
}

impl Evidence<'_> {
    fn region(&self, count: usize) -> Vec<&str> {
        if count == 0 {
            return self.lines.clone();
        }
        let mut lines = self
            .lines
            .iter()
            .rev()
            .copied()
            .filter(|line| !line.is_empty())
            .take(count)
            .collect::<Vec<_>>();
        lines.reverse();
        lines
    }

    fn classify<'a>(&self, rules: &'a RuleSet, capture: bool) -> (State, Option<RuleMatch<'a>>) {
        for rule in &rules.rules {
            let mut evidence = None;
            if rule.matches(self, capture.then_some(&mut evidence)) {
                return (
                    rule.state,
                    Some(RuleMatch {
                        id: &rule.id,
                        evidence,
                    }),
                );
            }
        }
        (rules.fallback, None)
    }
}

impl Rule {
    fn matches(&self, evidence: &Evidence<'_>, capture: Option<&mut Option<Arc<str>>>) -> bool {
        if self
            .alternate
            .is_some_and(|alternate| alternate != evidence.alternate)
            || self
                .title
                .as_ref()
                .is_some_and(|pattern| evidence.title.is_none_or(|title| !pattern.is_match(title)))
        {
            return false;
        }
        let lines = evidence.region(self.bottom_non_empty_lines);
        let mut line_evidence = None;
        if !self.any_line.is_empty() {
            line_evidence = lines
                .iter()
                .copied()
                .find(|line| self.any_line.is_match(line));
            if line_evidence.is_none() {
                return false;
            }
        }
        let joined =
            (self.joined.is_some() || !self.contains.is_empty() || !self.not_contains.is_empty())
                .then(|| lines.join("\n"));
        let joined = joined.as_deref().unwrap_or_default();
        if !self.contains.iter().all(|needle| joined.contains(needle))
            || self
                .not_contains
                .iter()
                .any(|needle| joined.contains(needle))
        {
            return false;
        }
        let mut regex_evidence = None;
        if let Some(pattern) = &self.joined {
            if capture.is_some() && line_evidence.is_none() {
                let Some(matched) = pattern.find(joined) else {
                    return false;
                };
                regex_evidence = Some(matched.as_str());
            } else if !pattern.is_match(joined) {
                return false;
            }
        }
        if let Some(out) = capture {
            let source = line_evidence
                .or(regex_evidence)
                .or_else(|| self.title.as_ref().and(evidence.title))
                .unwrap_or(&self.id);
            *out = Some(clean_evidence(source));
        }
        true
    }
}

pub(super) fn matched_state(screen: &ScreenSnapshot, kind: Kind) -> Option<State> {
    let (state, matched) = classify_screen(screen, kind);
    matched.then_some(state)
}

pub(super) fn classify_screen(screen: &ScreenSnapshot, kind: Kind) -> (State, bool) {
    let rules = overrides::for_kind(kind);
    let (state, matched) = classify_screen_with(screen, kind, rules.as_deref(), false);
    (state, matched.is_some())
}

pub(super) fn classify_screen_details(
    screen: &ScreenSnapshot,
    kind: Kind,
) -> (State, Option<RuleDetails>) {
    let rules = overrides::for_kind(kind);
    let (state, matched) = classify_screen_with(screen, kind, rules.as_deref(), true);
    let details = matched.and_then(|matched| {
        matched.evidence.map(|evidence| RuleDetails {
            id: Arc::from(matched.id),
            evidence,
        })
    });
    (state, details)
}

#[cfg(test)]
pub(super) fn matched_state_with(
    screen: &ScreenSnapshot,
    kind: Kind,
    custom: Option<&RuleSet>,
) -> Option<State> {
    let (state, matched) = classify_screen_with(screen, kind, custom, false);
    matched.is_some().then_some(state)
}

fn classify_screen_with<'a>(
    screen: &ScreenSnapshot,
    kind: Kind,
    custom: Option<&'a RuleSet>,
    capture: bool,
) -> (State, Option<RuleMatch<'a>>) {
    let lines = (screen.rows.saturating_sub(MAX_ROWS as u16)..screen.rows)
        .filter_map(|row| {
            let mut text = String::new();
            for cell in screen.row(row)?.iter().take(MAX_CELLS) {
                if cell.width == 0 {
                    continue;
                }
                if cell.style.hidden || cell.text.is_empty() {
                    text.push(' ');
                } else {
                    text.extend(cell.text.chars().take(8));
                }
            }
            Some(text)
        })
        .collect::<Vec<_>>();
    Evidence {
        lines: lines.iter().map(|line| line.trim_end()).collect(),
        title: osc_title(&screen.title),
        alternate: screen.alternate_screen,
    }
    .classify(custom.unwrap_or_else(|| &BUNDLED[&kind]), capture)
}

fn osc_title(title: &str) -> Option<&str> {
    (!title.trim().is_empty() && title != "Terminal").then_some(title)
}

fn clean_evidence(source: &str) -> Arc<str> {
    for candidate in source.split([
        '\n', '\r', '\u{000b}', '\u{000c}', '\u{0085}', '\u{2028}', '\u{2029}',
    ]) {
        let sanitized: String = candidate
            .chars()
            .filter(|character| {
                !character.is_control()
                    && !matches!(
                        character,
                        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{206f}'
                    )
            })
            .collect();
        let line = sanitized.trim_matches(java_whitespace);
        if !line.is_empty() {
            return Arc::from(limit_evidence(line, EXPLAIN_EVIDENCE_LIMIT).as_ref());
        }
    }
    Arc::from("")
}

fn java_whitespace(character: char) -> bool {
    matches!(character, '\u{001c}'..='\u{001f}')
        || (character.is_whitespace()
            && !matches!(character, '\u{0085}' | '\u{00a0}' | '\u{2007}' | '\u{202f}'))
}

fn limit_evidence(value: &str, limit: usize) -> Cow<'_, str> {
    if value.encode_utf16().count() <= limit {
        return Cow::Borrowed(value);
    }
    let mut remaining = limit - 1;
    let end = value
        .char_indices()
        .find_map(|(index, character)| {
            let units = character.len_utf16();
            if units > remaining {
                return Some(index);
            }
            remaining -= units;
            None
        })
        .expect("overlong evidence");
    Cow::Owned(format!(
        "{}…",
        value[..end].trim_end_matches(java_whitespace)
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[derive(Deserialize)]
    struct Fixture<'a> {
        kind: Kind,
        name: &'a str,
        text: String,
    }

    #[test]
    fn coding_agent_rule_evidence_uses_the_pinned_precedence() {
        let rules = RuleSet::from_override(
            Kind::GeminiCli,
            br#"{
                "kind": "GEMINI_CLI", "version": 1,
                "rules": [
                    {"id": "line", "state": "BLOCKED", "priority": 40, "title": "Agent",
                     "anyLineRegex": ["decision"], "regex": "action needed", "contains": ["pick"]},
                    {"id": "regex", "state": "WORKING", "priority": 30,
                     "title": "Agent", "regex": "doing work"},
                    {"id": "title", "state": "DONE", "priority": 20, "title": "Agent"},
                    {"id": "alternate", "state": "IDLE", "priority": 10, "alternateScreen": true}
                ]
            }"#,
        )
        .unwrap();
        for (lines, title, alternate, state, id, excerpt) in [
            (
                vec![
                    "action needed to pick",
                    "  first decision  ",
                    "second decision",
                ],
                Some("Agent title"),
                false,
                State::Blocked,
                Some("line"),
                Some("first decision"),
            ),
            (
                vec!["first decision", "prefix doing work suffix"],
                Some("Agent title"),
                false,
                State::Working,
                Some("regex"),
                Some("doing work"),
            ),
            (
                vec![],
                Some("Agent title"),
                false,
                State::Done,
                Some("title"),
                Some("Agent title"),
            ),
            (
                vec![],
                None,
                true,
                State::Idle,
                Some("alternate"),
                Some("alternate"),
            ),
            (vec![], None, false, State::Idle, None, None),
        ] {
            let evidence = Evidence {
                lines,
                title,
                alternate,
            };
            let (actual, matched) = evidence.classify(&rules, true);
            assert_eq!(actual, state);
            assert_eq!(matched.as_ref().map(|rule| rule.id), id);
            assert_eq!(
                matched.as_ref().and_then(|rule| rule.evidence.as_deref()),
                excerpt
            );
            let (guard_state, guard_match) = evidence.classify(&rules, false);
            assert_eq!(guard_state, state);
            assert_eq!(guard_match.map(|rule| rule.id), id);
        }
    }

    #[test]
    fn coding_agent_evidence_is_single_line_and_bounded() {
        let evidence = clean_evidence(
            " \u{202e}\n \u{001b}   Confirm \u{2069}this  \u{2028} private next line",
        );
        assert_eq!(evidence.as_ref(), "Confirm this");
        assert!(clean_evidence("\u{202e}\u{001b} ").is_empty());
        let long = "x".repeat(78) + "🙂";
        assert_eq!(limit_evidence(&long, 80), long);
        assert_eq!(limit_evidence(&(long + "y"), 80), "x".repeat(78) + "…");
        let long = "x".repeat(255) + "🙂";
        let evidence = clean_evidence(&long);
        assert_eq!(evidence.encode_utf16().count(), 256);
        assert!(evidence.ends_with('…'));
        let details = RuleDetails {
            id: Arc::from("secret-rule"),
            evidence,
        };
        assert_eq!(details.roster_evidence().encode_utf16().count(), 80);
        assert_eq!(format!("{details:?}"), "RuleDetails([redacted])");
    }

    #[test]
    fn coding_agent_guard_replays_all_pinned_java_screen_rules() {
        let fixtures: Vec<Fixture<'_>> =
            serde_json::from_str(include_str!("java-fixtures.json")).unwrap();
        let mut covered = HashSet::new();
        assert_eq!(BUNDLED.len(), 20);
        for fixture in fixtures {
            let mut lines = fixture.text.lines().peekable();
            let (mut state, mut rule, mut title, mut alternate) = (None, None, None, false);
            while let Some(directive) = lines.peek().and_then(|line| line.strip_prefix("#! ")) {
                if let Some(expect) = directive.strip_prefix("expect ") {
                    for field in expect.split_whitespace() {
                        if let Some(value) = field.strip_prefix("state=") {
                            state = Some(serde_json::from_value(serde_json::json!(value)).unwrap());
                        } else if let Some(value) = field.strip_prefix("rule=") {
                            rule = (value != "none").then_some(value);
                        }
                    }
                } else if let Some(value) = directive.strip_prefix("title=") {
                    title = osc_title(value);
                } else if directive == "alt=true" {
                    alternate = true;
                }
                lines.next();
            }
            let (actual, matched) = Evidence {
                lines: lines.map(str::trim_end).collect(),
                title,
                alternate,
            }
            .classify(&BUNDLED[&fixture.kind], true);
            let matched = matched.map(|rule| rule.id);
            assert_eq!(
                (actual, matched),
                (state.unwrap(), rule),
                "{}",
                fixture.name
            );
            if let Some(rule) = matched {
                covered.insert((fixture.kind, rule));
            }
        }
        assert_eq!(
            covered.len(),
            BUNDLED.values().map(|set| set.rules.len()).sum::<usize>()
        );
    }
}
