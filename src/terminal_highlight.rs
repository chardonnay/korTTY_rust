//! Bounded, presentation-only terminal highlighting.

mod interchange;
mod network;
pub(crate) use interchange::{decode_interchange, encode_java};

use std::collections::{HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::io::{self, Read};
use std::ops::Range;
use std::path::Path;

use kortty_terminal::{Cell, Color, OutputSnapshot, ScreenSnapshot, Style};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::private_files::Directory;

pub(crate) const FILE: &str = "terminal-highlights.json";
pub(crate) const MAX_BYTES: u64 = 1024 * 1024;
pub(crate) const MAX_RULES: usize = 64;
pub(crate) const MAX_SETS: usize = 32;
const MAX_TEXT: usize = 128 * 1024;
const MAX_HITS: usize = 512;
const MAX_OUTPUT_TEXT: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Configuration {
    pub(crate) version: u8,
    pub(crate) enabled: bool,
    pub(crate) alternate_screen: bool,
    pub(crate) triggers_enabled: bool,
    pub(crate) default_set: Option<String>,
    pub(crate) sets: Vec<RuleSet>,
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: true,
            alternate_screen: false,
            triggers_enabled: false,
            default_set: None,
            sets: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuleSet {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) rules: Vec<Rule>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Rule {
    pub(crate) id: Option<String>,
    pub(crate) name: String,
    pub(crate) pattern: String,
    pub(crate) enabled: bool,
    pub(crate) regex: bool,
    pub(crate) ignore_case: bool,
    pub(crate) whole_word: bool,
    pub(crate) whole_line: bool,
    pub(crate) foreground: Option<String>,
    pub(crate) background: Option<String>,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) underline: bool,
    pub(crate) action: Action,
    pub(crate) notify_with_text: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            id: None,
            name: String::new(),
            pattern: String::new(),
            enabled: true,
            regex: false,
            ignore_case: true,
            whole_word: false,
            whole_line: false,
            foreground: Some("ansi:3".into()),
            background: None,
            bold: true,
            italic: false,
            underline: false,
            action: Action::None,
            notify_with_text: false,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Action {
    #[default]
    None,
    Notify,
    RunSnippet(String),
}

#[derive(Clone, Debug)]
pub(crate) struct CompiledSet {
    pub(crate) id: String,
    rules: Vec<CompiledRule>,
}

#[derive(Clone, Debug)]
struct CompiledRule {
    rule: Rule,
    matcher: Matcher,
    foreground: Option<Color>,
    background: Option<Color>,
}

#[derive(Clone, Debug)]
enum Matcher {
    Regex(Regex),
    Network(network::Kind),
}

enum Matches<'r, 't> {
    Regex(regex::Matches<'r, 't>),
    Network(network::Matches<'t>),
}

impl Iterator for Matches<'_, '_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Regex(matches) => matches.next().map(|found| found.range()),
            Self::Network(matches) => matches.next(),
        }
    }
}

impl Matcher {
    fn find_iter<'r, 't>(&'r self, text: &'t str) -> Matches<'r, 't> {
        match self {
            Self::Regex(matcher) => Matches::Regex(matcher.find_iter(text)),
            Self::Network(kind) => Matches::Network(kind.find_iter(text)),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Hit {
    pub(crate) row: usize,
    pub(crate) rule: usize,
    pub(crate) rule_id: String,
    pub(crate) name: String,
    pub(crate) action: Action,
    pub(crate) signature: u64,
    pub(crate) cursor_line: bool,
    pub(crate) matched_text: Option<String>,
}

pub(crate) struct Painted {
    pub(crate) screen: ScreenSnapshot,
    pub(crate) hits: Vec<Hit>,
}

pub(crate) struct Preview {
    pub(crate) screen: ScreenSnapshot,
    pub(crate) rules: Vec<RuleCheck>,
    pub(crate) problem: Option<String>,
}

#[derive(Default)]
pub(crate) struct RuleCheck {
    pub(crate) hits: usize,
    pub(crate) problem: Option<String>,
}

pub(crate) struct OutputScan {
    pub(crate) hits: Vec<Hit>,
    pub(crate) truncated: bool,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(crate) fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn color(value: Option<&str>) -> io::Result<Option<Color>> {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    if let Some(index) = value.strip_prefix("ansi:") {
        return index
            .parse::<u8>()
            .ok()
            .filter(|index| *index < 16)
            .map(Color::Indexed)
            .map(Some)
            .ok_or_else(|| invalid("Choose ansi:0 through ansi:15 or #RRGGBB."));
    }
    crate::terminal_theme::Rgb::parse(value)
        .map(|rgb| {
            let [r, g, b] = rgb.bytes();
            Some(Color::Rgb(r, g, b))
        })
        .ok_or_else(|| invalid("Choose ansi:0 through ansi:15 or #RRGGBB."))
}

impl CompiledRule {
    fn paints(&self) -> bool {
        self.foreground.is_some()
            || self.background.is_some()
            || self.rule.bold
            || self.rule.italic
            || self.rule.underline
    }

    fn new(rule: &Rule) -> io::Result<Self> {
        if rule.pattern.is_empty()
            || rule.id.as_deref().is_some_and(|id| !valid_id(id))
            || rule.pattern.len() > 2048
            || rule.pattern.chars().count() > 512
            || rule
                .pattern
                .chars()
                .any(|character| character.is_control() && character != '\t')
            || rule.name.len() > 160
            || rule.name.chars().any(char::is_control)
        {
            return Err(invalid(
                "A rule needs a pattern of at most 512 characters and a short name.",
            ));
        }
        let matcher = if rule.regex
            && let Some(kind) = network::Kind::pinned(&rule.pattern)
        {
            Matcher::Network(kind)
        } else {
            let expression = if rule.regex {
                rule.pattern.clone()
            } else {
                regex::escape(&rule.pattern)
            };
            let matcher = RegexBuilder::new(&expression)
                .case_insensitive(rule.ignore_case)
                .size_limit(256 * 1024)
                .dfa_size_limit(256 * 1024)
                .build()
                .map_err(|error| invalid(format!("Invalid bounded regular expression: {error}")))?;
            if matcher.is_match("") {
                return Err(invalid("A rule must not match empty text."));
            }
            Matcher::Regex(matcher)
        };
        if let Action::RunSnippet(id) = &rule.action
            && crate::snippets::SnippetId::try_from(id.clone()).is_err()
        {
            return Err(invalid("Choose an existing snippet ID."));
        }
        Ok(Self {
            rule: rule.clone(),
            matcher,
            foreground: color(rule.foreground.as_deref())?,
            background: color(rule.background.as_deref())?,
        })
    }

    fn style(&self, base: &mut Style) {
        if let Some(color) = self.foreground {
            base.foreground = color;
        }
        if let Some(color) = self.background {
            base.background = color;
        }
        base.bold |= self.rule.bold;
        base.italic |= self.rule.italic;
        base.underline |= self.rule.underline;
    }
}

impl RuleSet {
    fn validate_header(&self) -> io::Result<()> {
        if !valid_id(&self.id)
            || self.name.trim().is_empty()
            || self.name.len() > 160
            || self.name.chars().any(char::is_control)
            || self.rules.len() > MAX_RULES
        {
            return Err(invalid("A set needs an ID, a name and at most 64 rules."));
        }
        Ok(())
    }

    pub(crate) fn compile(&self) -> io::Result<CompiledSet> {
        self.validate_header()?;
        let rules = self
            .rules
            .iter()
            .map(CompiledRule::new)
            .collect::<io::Result<Vec<_>>>()?;
        Ok(CompiledSet {
            id: self.id.clone(),
            rules,
        })
    }

    pub(crate) fn preview(&self, original: &ScreenSnapshot) -> Preview {
        let mut compiled = Vec::new();
        let mut indices = Vec::new();
        let mut rules = Vec::with_capacity(self.rules.len());
        let mut problem = self.validate_header().err().map(|error| error.to_string());
        for (index, rule) in self.rules.iter().enumerate() {
            let checked = CompiledRule::new(rule).and_then(|rule| {
                if !rule.paints() && rule.rule.action == Action::None {
                    return Err(invalid("Choose a color, style or action."));
                }
                Ok(rule)
            });
            let error = match checked {
                Ok(rule) => {
                    compiled.push(rule);
                    indices.push(index);
                    None
                }
                Err(error) => {
                    problem.get_or_insert_with(|| format!("Rule {} · {error}", index + 1));
                    Some(error.to_string())
                }
            };
            rules.push(RuleCheck {
                hits: 0,
                problem: error,
            });
        }
        let mut counts = vec![0; compiled.len()];
        let set = CompiledSet {
            id: self.id.clone(),
            rules: compiled,
        };
        let screen = set.paint_counted(original, &mut counts).screen;
        for (index, hits) in indices.into_iter().zip(counts) {
            rules[index].hits = hits;
        }
        Preview {
            screen,
            rules,
            problem,
        }
    }
}

impl Configuration {
    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.version != 1 || self.sets.len() > MAX_SETS {
            return Err(invalid(
                "Unsupported highlighting version or more than 32 user sets.",
            ));
        }
        let mut ids = HashSet::new();
        for set in &self.sets {
            if set.id == "none" || set.id.starts_with("builtin.") || !ids.insert(&set.id) {
                return Err(invalid(
                    "Set IDs must be unique and must not use the reserved builtin. prefix.",
                ));
            }
            set.compile()?;
        }
        if self.default_set.as_deref().is_some_and(|id| !valid_id(id)) {
            return Err(invalid("Invalid default set ID."));
        }
        Ok(())
    }

    pub(crate) fn all_sets(&self) -> Vec<RuleSet> {
        builtins().into_iter().chain(self.sets.clone()).collect()
    }

    #[cfg(test)]
    fn resolve(&self, pane: Option<&str>, connection: Option<&str>) -> Option<CompiledSet> {
        if !self.enabled {
            return None;
        }
        let sets = self.all_sets();
        for id in [pane, connection, self.default_set.as_deref()]
            .into_iter()
            .flatten()
        {
            if id == "none" {
                return None;
            }
            if let Some(set) = sets.iter().find(|set| set.id == id) {
                return set.compile().ok();
            }
        }
        None
    }

    pub(crate) fn read(root: &Path) -> io::Result<Self> {
        let directory = Directory::open(root, false)?;
        let file = match directory.open_private_file(FILE) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        Self::decode(read_bounded(file)?)
    }

    pub(crate) fn decode(bytes: Vec<u8>) -> io::Result<Self> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("Highlighting file exceeds 1 MiB."));
        }
        let configuration: Self =
            serde_json::from_slice(&bytes).map_err(|error| invalid(error.to_string()))?;
        configuration.validate()?;
        Ok(configuration)
    }

    pub(crate) fn save(&self, root: &Path) -> io::Result<()> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| invalid(error.to_string()))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("Highlighting file exceeds 1 MiB."));
        }
        Directory::open(root, true)?.replace_file(FILE, &bytes)
    }
}

pub(crate) fn read_bounded(file: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("Highlighting file exceeds 1 MiB."));
    }
    Ok(bytes)
}

fn word(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn hit_signature(start: usize, text: &str) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    start.hash(&mut hash);
    text.hash(&mut hash);
    hash.finish()
}

fn append_output_row(text: &mut String, cells: &[Cell], wrapped: Option<usize>) -> bool {
    for cell in &cells[..wrapped.unwrap_or(cells.len()).min(cells.len())] {
        if cell.width == 0 {
            continue;
        }
        let glyph = if cell.style.hidden || cell.text.is_empty() {
            " "
        } else {
            &cell.text
        };
        if text.len().saturating_add(glyph.len()) > MAX_TEXT {
            return false;
        }
        text.push_str(glyph);
    }
    true
}

impl CompiledSet {
    /// Matches retained live output on the worker, using stable absolute history coordinates.
    pub(crate) fn scan_output(
        &self,
        output: &OutputSnapshot,
        visible: &ScreenSnapshot,
    ) -> OutputScan {
        let live = output.live.as_ref().unwrap_or(visible);
        let mut lines = VecDeque::new();
        let mut text = String::new();
        let mut start = output.first_row;
        let mut skip = output.starts_with_continuation;
        let mut bytes = 0;
        let mut truncated = false;
        {
            let mut finish_row = |row: usize, cells: &[Cell], wrapped: Option<usize>| {
                if !skip && !append_output_row(&mut text, cells, wrapped) {
                    skip = true;
                    truncated = true;
                    text.clear();
                }
                if wrapped.is_none() {
                    if !skip {
                        bytes += text.len();
                        lines.push_back((start, row, std::mem::take(&mut text)));
                        while bytes > MAX_OUTPUT_TEXT {
                            truncated = true;
                            if let Some((_, _, line)) = lines.pop_front() {
                                bytes -= line.len();
                            }
                        }
                    }
                    text.clear();
                    start = row.wrapping_add(1);
                    skip = false;
                }
            };
            for (index, row) in output.history.iter().enumerate() {
                finish_row(
                    output.first_row.wrapping_add(index),
                    &row.cells(),
                    row.wrapped_columns(),
                );
            }
            for row in 0..live.rows {
                if let Some(cells) = live.row(row) {
                    finish_row(
                        output.live_row.wrapping_add(usize::from(row)),
                        cells,
                        live.wrapped_columns(row).map(usize::from),
                    );
                }
            }
        }
        if !skip && !text.is_empty() {
            if bytes.saturating_add(text.len()) > MAX_OUTPUT_TEXT {
                truncated = true;
            }
            lines.push_back((start, output.live_row + usize::from(live.rows), text));
        }
        let cursor = output.live_row.wrapping_add(usize::from(live.cursor.row));
        let mut hits = Vec::new();
        for (first, last, text) in lines.into_iter().rev() {
            for (index, rule) in self
                .rules
                .iter()
                .enumerate()
                .filter(|(_, rule)| rule.rule.enabled)
            {
                let found = rule.matcher.find_iter(&text).take(MAX_HITS).find(|found| {
                    !rule.rule.whole_word
                        || !(text[..found.start].chars().next_back().is_some_and(word)
                            || text[found.end..].chars().next().is_some_and(word))
                });
                if let Some(found) = found {
                    hits.push(Hit {
                        row: first,
                        rule: index,
                        rule_id: rule
                            .rule
                            .id
                            .as_ref()
                            .map(|id| format!("{}:{id}", self.id))
                            .unwrap_or_else(|| format!("{}:{index}", self.id)),
                        name: if rule.rule.name.is_empty() {
                            rule.rule.pattern.clone()
                        } else {
                            rule.rule.name.clone()
                        },
                        action: rule.rule.action.clone(),
                        signature: hit_signature(found.start, &text[found.clone()]),
                        cursor_line: (first..=last).contains(&cursor),
                        matched_text: rule
                            .rule
                            .notify_with_text
                            .then(|| text[found].chars().take(100).collect()),
                    });
                    if hits.len() == MAX_HITS {
                        hits.sort_by_key(|hit| (hit.row, hit.rule));
                        return OutputScan {
                            hits,
                            truncated: true,
                        };
                    }
                }
            }
        }
        hits.sort_by_key(|hit| (hit.row, hit.rule));
        OutputScan { hits, truncated }
    }

    pub(crate) fn paint(&self, original: &ScreenSnapshot) -> Painted {
        self.paint_counted(original, &mut [])
    }

    fn paint_counted(&self, original: &ScreenSnapshot, counts: &mut [usize]) -> Painted {
        let mut screen = original.clone();
        let mut claimed = vec![false; screen.cells.len()];
        let mut hits = Vec::new();
        let mut matches_left = MAX_HITS;
        let mut consumed = 0;
        let mut row = 0;
        while row < original.rows
            && consumed < MAX_TEXT
            && hits.len() < MAX_HITS
            && matches_left != 0
        {
            let first_row = row;
            let mut text = String::new();
            let mut indices = Vec::new();
            loop {
                if let Some(cells) = original.row(row) {
                    let end = original
                        .wrapped_columns(row)
                        .map_or(cells.len(), usize::from)
                        .min(cells.len());
                    for (column, cell) in cells[..end].iter().enumerate() {
                        if cell.width == 0 {
                            continue;
                        }
                        let start = text.len();
                        if cell.style.hidden || cell.text.is_empty() {
                            text.push(' ');
                        } else {
                            text.push_str(&cell.text);
                        }
                        indices.push((
                            start..text.len(),
                            usize::from(row) * usize::from(original.columns) + column,
                            cell.width,
                        ));
                    }
                }
                let wrap = original.wrapped_columns(row).is_some();
                row += 1;
                if !wrap || row == original.rows || text.len() >= MAX_TEXT {
                    break;
                }
            }
            consumed += text.len();
            if consumed > MAX_TEXT {
                break;
            }
            for (rule_index, rule) in self
                .rules
                .iter()
                .enumerate()
                .filter(|(_, rule)| rule.rule.enabled)
            {
                let mut signature = None;
                for found in rule.matcher.find_iter(&text).take(matches_left) {
                    if rule.rule.whole_word
                        && (text[..found.start].chars().next_back().is_some_and(word)
                            || text[found.end..].chars().next().is_some_and(word))
                    {
                        continue;
                    }
                    signature
                        .get_or_insert_with(|| hit_signature(found.start, &text[found.clone()]));
                    matches_left -= 1;
                    if !rule.paints() {
                        if rule.rule.action != Action::None
                            && let Some(hits) = counts.get_mut(rule_index)
                        {
                            *hits += 1;
                        }
                        break;
                    }
                    let first = if rule.rule.whole_line {
                        0
                    } else {
                        indices.partition_point(|(range, _, _)| range.end <= found.start)
                    };
                    let last = if rule.rule.whole_line {
                        indices.len()
                    } else {
                        indices.partition_point(|(range, _, _)| range.start < found.end)
                    };
                    let mut painted = false;
                    for &(_, index, width) in &indices[first..last] {
                        let end = (index + usize::from(width)).min(screen.cells.len());
                        for (cell, claimed) in screen.cells[index..end]
                            .iter_mut()
                            .zip(&mut claimed[index..end])
                        {
                            if !*claimed && !cell.style.hidden {
                                rule.style(&mut cell.style);
                                *claimed = true;
                                painted = true;
                            }
                        }
                    }
                    if painted && let Some(hits) = counts.get_mut(rule_index) {
                        *hits += 1;
                    }
                    if rule.rule.whole_line || matches_left == 0 {
                        break;
                    }
                }
                if let Some(signature) = signature {
                    hits.push(Hit {
                        row: original
                            .scrollback_lines
                            .saturating_add(usize::from(first_row)),
                        rule: rule_index,
                        rule_id: rule
                            .rule
                            .id
                            .as_ref()
                            .map(|id| format!("{}:{id}", self.id))
                            .unwrap_or_else(|| format!("{}:{rule_index}", self.id)),
                        name: if rule.rule.name.is_empty() {
                            rule.rule.pattern.clone()
                        } else {
                            rule.rule.name.clone()
                        },
                        action: rule.rule.action.clone(),
                        signature,
                        cursor_line: (first_row..row).contains(&original.cursor.row),
                        matched_text: None,
                    });
                }
                if hits.len() == MAX_HITS || matches_left == 0 {
                    break;
                }
            }
        }
        Painted { screen, hits }
    }
}

pub(crate) fn builtins() -> Vec<RuleSet> {
    let rule =
        |id: &str, name: &str, pattern: &str, color: &str, ignore_case, whole_word, bold: bool| {
            Rule {
                id: Some(id.into()),
                name: name.into(),
                pattern: pattern.into(),
                regex: true,
                ignore_case,
                whole_word,
                foreground: Some(color.into()),
                bold,
                underline: !bold,
                ..Rule::default()
            }
        };
    vec![
        RuleSet {
            id: "builtin.errors".into(),
            name: "Errors and warnings".into(),
            rules: vec![
                rule(
                    "builtin.errors.error",
                    "Errors",
                    r"errors?|fail(?:s|ed|ure|ures)?|fatal|critical|denied|refused|panic|exceptions?|traceback|segmentation fault",
                    "ansi:1",
                    true,
                    true,
                    true,
                ),
                rule(
                    "builtin.errors.exception-class",
                    "Exception classes",
                    r"\b[A-Z][A-Za-z0-9_]*(?:Exception|Error)\b",
                    "ansi:1",
                    false,
                    false,
                    true,
                ),
                rule(
                    "builtin.errors.warning",
                    "Warnings",
                    r"warn(?:s|ings?)?|deprecated|timed out",
                    "ansi:3",
                    true,
                    true,
                    false,
                ),
            ],
        },
        RuleSet {
            id: "builtin.network".into(),
            name: "Network addresses".into(),
            rules: vec![
                rule(
                    "builtin.network.ipv4",
                    "IPv4",
                    network::IPV4,
                    "ansi:6",
                    false,
                    false,
                    false,
                ),
                rule(
                    "builtin.network.ipv6",
                    "IPv6",
                    network::IPV6,
                    "ansi:6",
                    false,
                    false,
                    false,
                ),
                rule(
                    "builtin.network.mac",
                    "MAC",
                    network::MAC,
                    "ansi:5",
                    false,
                    false,
                    false,
                ),
                rule(
                    "builtin.network.mac-dotted",
                    "Cisco dotted MAC",
                    network::DOTTED,
                    "ansi:5",
                    false,
                    false,
                    false,
                ),
            ],
        },
        RuleSet {
            id: "builtin.network-devices".into(),
            name: "Network devices".into(),
            rules: vec![
                rule(
                    "builtin.network-devices.down",
                    "Down",
                    r"err-disabled|notconnect|not connected|administratively down|down|disabled|inactive|suspended|unreachable",
                    "ansi:1",
                    true,
                    true,
                    true,
                ),
                rule(
                    "builtin.network-devices.up",
                    "Up",
                    r"up|connected|established|forwarding",
                    "ansi:2",
                    true,
                    true,
                    true,
                ),
                rule(
                    "builtin.network-devices.interface",
                    "Interface",
                    r"(?:gi(?:gabitethernet)?|te(?:ngig(?:e|abitethernet))?|tw(?:entyfivegig(?:e|abitethernet))?|fo(?:rtygig(?:e|abitethernet))?|hu(?:ndredgig(?:e|abitethernet))?|fa(?:stethernet)?|et(?:h(?:ernet)?)?|po(?:rt-channel)?|vlan|lo(?:opback)?|tu(?:nnel)?|mgmt|ma(?:nagement)?)[0-9]+(?:/[0-9]+){0,3}(?:\.[0-9]+)?|(?:xe|ge|et)-[0-9]+(?:/[0-9]+){0,3}(?:\.[0-9]+)?|ae[0-9]+(?:\.[0-9]+)?",
                    "ansi:6",
                    true,
                    true,
                    false,
                ),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use kortty_terminal::Terminal;

    #[test]
    fn highlighting_preserves_text_and_styles_first_rule_wins() {
        let mut terminal = Terminal::new(40, 3);
        terminal.advance(b"error warning not connected");
        let original = terminal.snapshot();
        let painted = builtins()[0].compile().unwrap().paint(&original);
        assert_eq!(painted.screen.cells[0].text, original.cells[0].text);
        assert_eq!(original.cells[0].style.foreground, Color::Default);
        assert_eq!(painted.screen.cells[0].style.foreground, Color::Indexed(1));
        assert_eq!(painted.screen.cells[6].style.foreground, Color::Indexed(3));
        let device = builtins()[2].compile().unwrap().paint(&original);
        assert_eq!(device.screen.cells[19].style.foreground, Color::Indexed(1));
    }

    #[test]
    fn builtin_network_rules_match_the_pinned_java_cases() {
        let builtins = builtins();
        let set = builtins[1].compile().unwrap();
        let check = |id: &str, input: &str, expected: &[&str]| {
            let rule = set
                .rules
                .iter()
                .find(|rule| rule.rule.id.as_deref() == Some(id))
                .unwrap();
            let matches: Vec<_> = rule
                .matcher
                .find_iter(input)
                .map(|span| &input[span])
                .collect();
            assert_eq!(matches, expected, "{id} in {input}");
        };
        let ipv4 = "builtin.network.ipv4";
        check(
            ipv4,
            "inet 10.0.0.1/24 brd 10.0.0.255",
            &["10.0.0.1/24", "10.0.0.255"],
        );
        check(ipv4, "ping 8.8.8.8.", &["8.8.8.8"]);
        check(ipv4, "ssh admin@192.168.1.20:22", &["192.168.1.20"]);
        check(ipv4, "route 0.0.0.0/0", &["0.0.0.0/0"]);
        for invalid in [
            "999.1.1.1",
            "1.2.3.4.5",
            "256.0.0.1",
            "10.0.0",
            "v1.2.3",
            "1.2.3.4567",
        ] {
            check(ipv4, invalid, &[]);
        }
        let ipv6 = "builtin.network.ipv6";
        check(
            ipv6,
            "inet6 fe80::1%eth0/64 scope link",
            &["fe80::1%eth0/64"],
        );
        check(ipv6, "2001:db8::ff00:42:8329", &["2001:db8::ff00:42:8329"]);
        check(
            ipv6,
            "2001:0db8:0000:0000:0000:ff00:0042:8329",
            &["2001:0db8:0000:0000:0000:ff00:0042:8329"],
        );
        check(ipv6, "listening on [::1]:8080 and [::]:22", &["::1", "::"]);
        check(
            ipv6,
            "fe80::1%abc-abcdefghijklmnopqrstuvwxyz1234/64",
            &["fe80::1%abc"],
        );
        for invalid in [
            "12:30:45",
            "std::vector",
            "Foo::bar()",
            "aa:bb:cc:dd:ee:ff",
            "a:b",
        ] {
            check(ipv6, invalid, &[]);
        }
        let mac = "builtin.network.mac";
        check(
            mac,
            "link/ether aa:bb:cc:dd:ee:ff brd ff:ff:ff:ff:ff:ff",
            &["aa:bb:cc:dd:ee:ff", "ff:ff:ff:ff:ff:ff"],
        );
        check(
            mac,
            "Physical Address: AA-BB-CC-DD-EE-FF",
            &["AA-BB-CC-DD-EE-FF"],
        );
        for invalid in ["aa:bb-cc:dd:ee:ff", "aa:bb:cc:dd:ee:ff:00"] {
            check(mac, invalid, &[]);
        }
        let dotted = "builtin.network.mac-dotted";
        check(dotted, "0050.7966.6800 dynamic Gi0/1", &["0050.7966.6800"]);
        check(dotted, "aabb.ccdd.eeff", &["aabb.ccdd.eeff"]);
        check(dotted, "aabb.ccdd.eeff.0011", &[]);
    }

    #[test]
    fn builtin_highlight_styles_and_network_copy_keep_reference_behavior() {
        let sets = builtins();
        for (set, styles) in sets.iter().zip([
            &[true, true, false][..],
            &[false, false, false, false],
            &[true, true, false],
        ]) {
            assert_eq!(set.rules.len(), styles.len());
            for (rule, &bold) in set.rules.iter().zip(styles) {
                assert_eq!((rule.bold, rule.underline), (bold, !bold));
                assert!(rule.foreground.as_deref().unwrap().starts_with("ansi:"));
                assert!(rule.background.is_none());
            }
        }
        let mut copy = sets[1].clone();
        copy.id = "network-copy".into();
        let config = Configuration {
            sets: vec![copy],
            ..Configuration::default()
        };
        let restored = decode_interchange(encode_java(&config).unwrap()).unwrap();
        assert_eq!(restored, config);
        let compiled = restored.sets[0].compile().unwrap();
        let mut terminal = Terminal::new(80, 2);
        terminal.advance(b"aa:bb:cc:dd:ee:ff 12:30:45 fe80::1%eth0/64");
        let painted = compiled.paint(&terminal.snapshot());
        assert_eq!(painted.screen.cells[0].style.foreground, Color::Indexed(5));
        assert_eq!(painted.screen.cells[18].style.foreground, Color::Default);
        assert_eq!(painted.screen.cells[40].style.foreground, Color::Indexed(6));
        assert!(painted.screen.cells[40].style.underline);
        assert!(!painted.screen.cells[40].style.bold);
    }

    #[test]
    fn action_only_highlight_keeps_hits_without_claiming_style() {
        let mut terminal = Terminal::new(30, 3);
        terminal.advance(b"disk full");
        let set = RuleSet {
            id: "disk".into(),
            name: "Disk warnings".into(),
            rules: vec![
                Rule {
                    pattern: "disk full".into(),
                    whole_line: true,
                    foreground: None,
                    bold: false,
                    action: Action::Notify,
                    ..Rule::default()
                },
                Rule {
                    pattern: "disk full".into(),
                    foreground: Some("ansi:1".into()),
                    bold: true,
                    ..Rule::default()
                },
            ],
        };
        let painted = set.compile().unwrap().paint(&terminal.snapshot());
        assert!(painted.hits.iter().any(|hit| hit.action == Action::Notify));
        assert_eq!(painted.screen.cells[0].style.foreground, Color::Indexed(1));
        assert!(painted.screen.cells[0].style.bold);
    }

    #[test]
    fn malformed_unbounded_and_empty_patterns_are_rejected() {
        for pattern in ["(", "a*", "(?=x)"] {
            let set = RuleSet {
                id: "test".into(),
                name: "Test".into(),
                rules: vec![Rule {
                    pattern: pattern.into(),
                    regex: true,
                    ..Rule::default()
                }],
            };
            assert!(set.compile().is_err());
        }
        assert!(builtins().iter().all(|set| set.compile().is_ok()));
    }

    #[test]
    fn resolution_skips_unknown_ids_and_honors_explicit_none() {
        let config = Configuration {
            default_set: Some("builtin.errors".into()),
            ..Default::default()
        };
        assert_eq!(
            config.resolve(Some("unknown"), None).unwrap().id,
            "builtin.errors"
        );
        assert!(
            config
                .resolve(Some("none"), Some("builtin.errors"))
                .is_none()
        );
    }

    #[test]
    fn wrapped_chunked_unicode_text_keeps_cell_boundaries() {
        let set = RuleSet {
            id: "test".into(),
            name: "Test".into(),
            rules: vec![Rule {
                pattern: "界error".into(),
                foreground: Some("#112233".into()),
                ..Rule::default()
            }],
        }
        .compile()
        .unwrap();
        let mut terminal = Terminal::new(5, 4);
        terminal.advance("abc界er".as_bytes());
        assert!(set.paint(&terminal.snapshot()).hits.is_empty());
        terminal.advance(b"ror");
        let original = terminal.snapshot();
        let painted = set.paint(&original);
        assert_eq!(painted.hits.len(), 1);
        assert_eq!(painted.screen.cells[2].style.foreground, Color::Default);
        for cell in &painted.screen.cells[3..10] {
            assert_eq!(cell.style.foreground, Color::Rgb(17, 34, 51));
        }
        assert_eq!(original.cells[3].style.foreground, Color::Default);
    }

    #[test]
    fn hidden_text_does_not_match_and_first_hit_signature_ignores_progress_suffix() {
        let set = builtins().remove(0).compile().unwrap();
        let mut terminal = Terminal::new(40, 2);
        terminal.advance(b"\x1b[8merror\x1b[0m");
        assert!(set.paint(&terminal.snapshot()).hits.is_empty());
        terminal.advance(b"\r\x1b[2Kerror 1");
        let first = set.paint(&terminal.snapshot());
        terminal.advance(b"\r\x1b[2Kerror 2");
        let progress = set.paint(&terminal.snapshot());
        assert_eq!(first.hits[0].signature, progress.hits[0].signature);
        assert!(progress.hits[0].cursor_line);
        terminal.advance(b"\r\x1b[2K error");
        assert_ne!(
            first.hits[0].signature,
            set.paint(&terminal.snapshot()).hits[0].signature
        );
    }

    #[test]
    fn configuration_round_trip_preserves_rules_without_enabling_triggers() {
        let configuration = Configuration {
            default_set: Some("custom".into()),
            sets: vec![RuleSet {
                id: "custom".into(),
                name: "Custom".into(),
                rules: vec![Rule {
                    pattern: "error".into(),
                    action: Action::Notify,
                    ..Rule::default()
                }],
            }],
            ..Configuration::default()
        };
        let bytes = serde_json::to_vec(&configuration).unwrap();
        let restored = Configuration::decode(bytes).unwrap();
        assert_eq!(restored, configuration);
        assert!(!restored.triggers_enabled);
        assert!(read_bounded(vec![b'x'; MAX_BYTES as usize + 1].as_slice()).is_err());
    }

    #[test]
    fn retained_output_matches_offscreen_lines_and_tracks_absolute_trimmed_rows() {
        let set = builtins().remove(0).compile().unwrap();
        let mut terminal = Terminal::new(20, 3);
        terminal.advance(b"error\r\n");
        for _ in 0..30 {
            terminal.advance(b"ordinary output\r\n");
        }
        let live = terminal.snapshot();
        assert!(set.paint(&live).hits.is_empty());
        let hits = set
            .scan_output(&terminal.output_snapshot(false), &live)
            .hits;
        assert!(hits.iter().any(|hit| hit.row == 0 && !hit.cursor_line));
        for _ in 0..10_200 {
            terminal.advance(b"ordinary output\r\n");
        }
        terminal.advance(b"error\r\n");
        let output = terminal.output_snapshot(false);
        let hits = set.scan_output(&output, &terminal.snapshot()).hits;
        assert!(hits.iter().any(|hit| hit.row > 10_000));
        assert!(output.first_row > 0);
    }

    #[test]
    fn wrapped_match_crosses_history_live_boundary_while_viewport_is_scrolled() {
        let set = RuleSet {
            id: "test".into(),
            name: "Test".into(),
            rules: vec![Rule {
                pattern: "abcdefghijkl".into(),
                ..Rule::default()
            }],
        }
        .compile()
        .unwrap();
        let mut terminal = Terminal::new(5, 2);
        terminal.advance(b"abcdefghijkl");
        let output = terminal.output_snapshot(true);
        let hits = set
            .scan_output(&output, &terminal.viewport_snapshot(1))
            .hits;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].row, 0);
        assert!(hits[0].cursor_line);
    }

    #[test]
    fn excessive_rule_hits_report_truncated_output() {
        let rule = Rule {
            pattern: "error".into(),
            ..Rule::default()
        };
        let set = RuleSet {
            id: "test".into(),
            name: "Test".into(),
            rules: vec![rule; MAX_RULES],
        }
        .compile()
        .unwrap();
        let mut terminal = Terminal::new(20, 4);
        for _ in 0..20 {
            terminal.advance(b"error\r\n");
        }
        let scan = set.scan_output(&terminal.output_snapshot(false), &terminal.snapshot());
        assert!(scan.truncated);
        assert_eq!(scan.hits.len(), MAX_HITS);
    }
}
