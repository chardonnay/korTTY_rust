//! Saved native diagrams and the exact snippet ranges they represent.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use kortty_diagram::{
    Diagram, DiagramKind, MAX_NODES, MAX_SCRIPT_BYTES, MAX_SCRIPT_LINES, MAX_SOURCE_BYTES,
    SourceRange, parse_mermaid,
};
use serde::{Deserialize, Serialize};

use super::{Library, MAX_CONTENT_BYTES, MAX_NAME_BYTES, SnippetId, invalid, valid_field};
use crate::diagram_window::Chart;
use crate::snippet_analysis::model::{content_hash, now_millis};

pub(crate) const MAX_DIAGRAMS: usize = 16;
pub(crate) const MAX_INSTRUCTIONS: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Id(SnippetId);

impl Id {
    fn new() -> std::io::Result<Self> {
        SnippetId::new().map(Self)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Family {
    #[default]
    LogicalStructure,
    Sequence,
    State,
    Class,
    Er,
}

impl Family {
    pub(crate) const ALL: [Self; 5] = [
        Self::LogicalStructure,
        Self::Sequence,
        Self::State,
        Self::Class,
        Self::Er,
    ];

    pub(crate) fn kind(self) -> DiagramKind {
        match self {
            Self::LogicalStructure => DiagramKind::Flowchart,
            Self::Sequence => DiagramKind::Sequence,
            Self::State => DiagramKind::State,
            Self::Class => DiagramKind::Class,
            Self::Er => DiagramKind::EntityRelationship,
        }
    }
}

impl fmt::Display for Family {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::LogicalStructure => "Logical structure",
            Self::Sequence => "Sequence",
            Self::State => "State",
            Self::Class => "Class",
            Self::Er => "ER diagram",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum Scope {
    #[default]
    Whole,
    Lines {
        start: u32,
        end: u32,
    },
}

impl Scope {
    /// Snaps a nonempty selection to complete lines. A trailing cursor at the
    /// start of the next line does not include that line.
    pub(crate) fn selected(first: (usize, usize), last: (usize, usize)) -> Option<Self> {
        if first == last {
            return None;
        }
        let (first, last) = (first.min(last), first.max(last));
        let start = u32::try_from(first.0).ok()?.checked_add(1)?;
        let end = u32::try_from(last.0)
            .ok()?
            .checked_add(u32::from(last.1 != 0 || first.0 == last.0))?;
        let scope = Self::Lines { start, end };
        scope.valid().then_some(scope)
    }

    fn valid(self) -> bool {
        match self {
            Self::Whole => true,
            Self::Lines { start, end } => {
                start > 0
                    && end >= start
                    && end as usize <= MAX_CONTENT_BYTES + 1
                    && ((end - start) as usize) < MAX_SCRIPT_LINES
            }
        }
    }

    pub(crate) fn offset(self) -> u32 {
        match self {
            Self::Whole => 0,
            Self::Lines { start, .. } => start.saturating_sub(1),
        }
    }

    /// The selected text keeps its interior line endings and never includes an
    /// adjacent source line. Missing lines invalidate the selection.
    pub(crate) fn text(self, source: &str) -> Option<&str> {
        if !self.valid() {
            return None;
        }
        match self {
            Self::Whole => Some(source),
            Self::Lines { start, end } => {
                let mut lines = line_offsets(source).skip(start as usize - 1);
                let first = lines.next()?;
                let last = if start == end {
                    first.end
                } else {
                    lines.nth((end - start - 1) as usize)?.end
                };
                Some(&source[first.start..last])
            }
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Whole => formatter.write_str("Whole snippet"),
            Self::Lines { start, end } if start == end => write!(formatter, "Line {start}"),
            Self::Lines { start, end } => write!(formatter, "Lines {start}–{end}"),
        }
    }
}

fn line_offsets(source: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut next = Some(0);
    std::iter::from_fn(move || {
        let start = next.take()?;
        let Some(relative_end) = source[start..].find(['\r', '\n']) else {
            return Some(start..source.len());
        };
        let end = start + relative_end;
        let bytes = source.as_bytes();
        let pair = bytes
            .get(end + 1)
            .is_some_and(|&after| matches!((bytes[end], after), (b'\r', b'\n') | (b'\n', b'\r')));
        next = Some(end + 1 + usize::from(pair));
        Some(start..end)
    })
}

#[derive(Clone)]
pub(crate) struct Source {
    pub(crate) scope: Scope,
    pub(crate) text: Arc<str>,
    scoped_sha256: String,
    full_sha256: String,
    lines: u32,
}

impl Source {
    pub(crate) fn capture(full: &str, scope: Scope) -> Result<Self, String> {
        if full.len() > MAX_CONTENT_BYTES {
            return Err("The snippet exceeds the source size limit.".into());
        }
        let text = scope
            .text(full)
            .ok_or("The selected snippet lines no longer exist.")?;
        if text.trim().is_empty() || text.len() > MAX_SCRIPT_BYTES {
            return Err("Choose nonempty diagram source up to 2 MiB.".into());
        }
        let lines = line_offsets(text).take(MAX_SCRIPT_LINES + 1).count();
        if lines > MAX_SCRIPT_LINES {
            return Err("The diagram source exceeds 100,000 lines.".into());
        }
        let scoped_sha256 = hash(text)?;
        let full_sha256 = if scope == Scope::Whole {
            scoped_sha256.clone()
        } else {
            hash(full)?
        };
        Ok(Self {
            scope,
            text: Arc::from(text),
            scoped_sha256,
            full_sha256,
            lines: lines as u32,
        })
    }

    pub(crate) fn matches(&self, full: &str) -> bool {
        self.scope.text(full) == Some(self.text.as_ref())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Saved {
    pub(crate) id: Id,
    pub(crate) title: String,
    pub(crate) family: Family,
    pub(crate) scope: Scope,
    pub(crate) mermaid: String,
    // Java's persisted digest covers the whole source. A native scoped digest
    // takes precedence so independent edits do not age an unrelated diagram.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scoped_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    full_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) instructions: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    references: Vec<Reference>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    fallback: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fallback_reason: Option<String>,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reference {
    pub(super) node: String,
    pub(super) start: u32,
    pub(super) end: u32,
}

pub(super) struct JavaRecord {
    pub(super) id: SnippetId,
    pub(super) title: String,
    pub(super) family: Family,
    pub(super) scope: Scope,
    pub(super) mermaid: String,
    pub(super) full_sha256: Option<String>,
    pub(super) instructions: String,
    pub(super) references: Vec<Reference>,
    pub(super) created_at: u64,
    pub(super) updated_at: u64,
}

impl Saved {
    pub(super) fn from_java(record: JavaRecord, source: &str) -> Result<(Self, usize), String> {
        let mut saved = Self {
            id: Id(record.id),
            title: record.title,
            family: record.family,
            scope: record.scope,
            mermaid: record.mermaid,
            scoped_sha256: None,
            full_sha256: record.full_sha256,
            instructions: record.instructions,
            references: Vec::new(),
            fallback: false,
            fallback_reason: None,
            created_at: record.created_at,
            updated_at: record.updated_at,
        };
        let diagram = saved.parse()?;
        let mut last = saved.reference_limit();
        // Historical line positions remain meaningful even if a stale source was shortened.
        let through = saved.java_reference_end(&record.references);
        if saved.full_matches(source) && java_lines_align_through(source, through) {
            if saved.scope.text(source).is_none() {
                return Err("The Java diagram's selection is outside its source.".into());
            }
            last = last.min(line_offsets(source).count() as u32);
        }
        let mut seen = HashSet::new();
        let total = record.references.len();
        for reference in record.references {
            if saved.valid_reference(&diagram, &reference, last)
                && seen.insert(reference.node.clone())
            {
                saved.references.push(reference);
            }
        }
        let skipped = total - saved.references.len();
        Ok((saved, skipped))
    }

    pub(crate) fn capture(
        chart: &Chart,
        source: &Source,
        title: &str,
        instructions: &str,
        previous: Option<&Self>,
    ) -> Result<Self, String> {
        let family = Family::ALL
            .into_iter()
            .find(|family| family.kind() == chart.diagram().kind())
            .ok_or("This diagram family cannot be generated for a snippet.")?;
        if previous.is_some_and(|old| {
            matches!(old.scope, Scope::Whole) != matches!(source.scope, Scope::Whole)
                || old.family != family
        }) {
            return Err("Regeneration must retain the diagram family and source mode.".into());
        }
        let references = chart
            .diagram()
            .nodes()
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                let range = chart.source_range(node)?;
                if range.end() > source.lines {
                    return None;
                }
                // The flowchart and sequence exporters canonicalize their IDs.
                let node = match family {
                    Family::LogicalStructure => format!("n{index}"),
                    Family::Sequence => format!("p{index}"),
                    _ => node.id().to_owned(),
                };
                Some(Reference {
                    node,
                    start: range.start().checked_add(source.scope.offset())?,
                    end: range.end().checked_add(source.scope.offset())?,
                })
            })
            .collect();
        let time = now_millis();
        let title = title.trim();
        let saved = Self {
            id: match previous {
                Some(previous) => previous.id,
                None => Id::new().map_err(|error| error.to_string())?,
            },
            title: if title.is_empty() {
                family.to_string()
            } else {
                title.into()
            },
            family,
            scope: source.scope,
            mermaid: chart.diagram().to_mermaid(),
            scoped_sha256: Some(source.scoped_sha256.clone()),
            full_sha256: Some(source.full_sha256.clone()),
            instructions: instructions.into(),
            references,
            fallback: chart.is_fallback(),
            fallback_reason: chart.fallback_reason().map(str::to_owned),
            created_at: previous.map_or(time, |previous| previous.created_at),
            updated_at: previous.map_or(time, |previous| time.max(previous.updated_at)),
        };
        saved.parse()?;
        Ok(saved)
    }

    pub(crate) fn is_current(&self, full: &str) -> bool {
        let Some(scoped) = self.scope.text(full) else {
            return false;
        };
        if let Some(expected) = &self.scoped_sha256 {
            return hash(scoped).is_ok_and(|digest| digest == *expected);
        }
        self.full_matches(full)
            && java_lines_align_through(full, self.java_reference_end(&self.references))
    }

    pub(crate) fn absolute_reference(&self, range: SourceRange) -> Option<SourceRange> {
        let offset = self.scope.offset();
        let start = range.start().checked_add(offset)?;
        let end = range.end().checked_add(offset)?;
        if let Scope::Lines {
            start: first,
            end: last,
        } = self.scope
            && (start < first || end > last)
        {
            return None;
        }
        let absolute = SourceRange::new(start, end)?;
        self.references
            .iter()
            .any(|reference| reference.start == start && reference.end == end)
            .then_some(absolute)
    }

    pub(crate) fn captured_for(&self, full: &str) -> bool {
        self.full_matches(full) && self.is_current(full)
    }

    fn full_matches(&self, full: &str) -> bool {
        self.full_sha256
            .as_ref()
            .is_some_and(|expected| hash(full).is_ok_and(|digest| digest == *expected))
    }

    fn reference_limit(&self) -> u32 {
        match self.scope {
            Scope::Whole => MAX_SCRIPT_LINES as u32,
            Scope::Lines { end, .. } => end,
        }
    }

    fn java_reference_end(&self, references: &[Reference]) -> u32 {
        match self.scope {
            Scope::Whole => references
                .iter()
                .map(|reference| reference.end)
                .max()
                .unwrap_or(0),
            Scope::Lines { end, .. } => end,
        }
    }

    fn valid_reference(&self, diagram: &Diagram, reference: &Reference, last: u32) -> bool {
        reference.start > self.scope.offset()
            && reference.end >= reference.start
            && reference.end <= last
            && diagram
                .nodes()
                .iter()
                .any(|node| node.id() == reference.node)
    }

    fn parse(&self) -> Result<Diagram, String> {
        if self.title.is_empty()
            || !valid_field(&self.title, MAX_NAME_BYTES)
            || !self.scope.valid()
            || self.mermaid.len() > MAX_SOURCE_BYTES
            || self.instructions.len() > MAX_INSTRUCTIONS
            || !self.instructions.chars().all(acceptable_text)
            || self.fallback_reason.as_ref().is_some_and(|reason| {
                reason.len() > MAX_INSTRUCTIONS || !reason.chars().all(acceptable_text)
            })
            || self.references.len() > MAX_NODES
            || self.updated_at < self.created_at
            || [&self.scoped_sha256, &self.full_sha256]
                .into_iter()
                .flatten()
                .any(|digest| !valid_hash(digest))
        {
            return Err("The saved snippet diagram exceeds its metadata limits.".into());
        }
        let diagram = parse_mermaid(&self.mermaid).map_err(|error| error.to_string())?;
        if diagram.kind() != self.family.kind() || diagram.nodes().is_empty() {
            return Err("The saved diagram is empty or has a different family.".into());
        }
        let mut seen = HashSet::new();
        for reference in &self.references {
            if !self.valid_reference(&diagram, reference, self.reference_limit())
                || !seen.insert(reference.node.as_str())
            {
                return Err("The saved diagram has an invalid source reference.".into());
            }
        }
        Ok(diagram)
    }

    /// Stale diagrams retain their scene. Source links are attached only when
    /// the current snippet still matches the captured scope.
    pub(crate) fn restore<'a>(&self, full: &'a str) -> Result<(Chart, Option<&'a str>), String> {
        let diagram = self.parse()?;
        let source = if self.is_current(full) {
            self.scope.text(full)
        } else {
            None
        };
        let mut links = HashMap::new();
        if let Some(source) = source {
            let offset = self.scope.offset();
            let lines = line_offsets(source).count();
            for reference in &self.references {
                if let Some(range) =
                    SourceRange::new(reference.start - offset, reference.end - offset)
                        .filter(|range| range.end() as usize <= lines)
                {
                    links.insert(reference.node.clone(), range);
                }
            }
        }
        Ok((
            Chart::stored(diagram, links, self.fallback, self.fallback_reason.clone()),
            source,
        ))
    }
}

// Java's editor uses LF for navigation, while its diagram processing also uses
// regex \R. Retain the original coordinates when these conventions disagree.
fn java_lines_align_through(source: &str, last: u32) -> bool {
    let bytes = source.as_bytes();
    line_offsets(source).take(last as usize).all(|range| {
        let end = range.end;
        !source[range].contains(['\u{000b}', '\u{000c}', '\u{0085}', '\u{2028}', '\u{2029}'])
            && !matches!(bytes.get(end..), Some([b'\n', b'\r', ..]))
            && (bytes.get(end) != Some(&b'\r') || bytes.get(end + 1) == Some(&b'\n'))
    })
}

pub(crate) fn validate_collection(diagrams: &[Saved]) -> Result<(), String> {
    if diagrams.len() > MAX_DIAGRAMS {
        return Err("A snippet can have at most 16 saved diagrams.".into());
    }
    let mut ids = HashSet::new();
    for diagram in diagrams {
        if !ids.insert(diagram.id) {
            return Err("A snippet contains duplicate diagram IDs.".into());
        }
        diagram.parse()?;
    }
    Ok(())
}

/// Move links when a previously current selection survives a source edit at a
/// single line-aligned location. Repeated candidates keep the old diagram stale.
pub(super) fn rebase(diagrams: &mut [Saved], before: &str, after: &str) {
    if diagrams.is_empty() || before == after || after.len() > MAX_CONTENT_BYTES {
        return;
    }
    for diagram in diagrams {
        if diagram.scoped_sha256.is_none() {
            // Imported Java ranges refer to the exact full revision.
            continue;
        }
        let Scope::Lines { start, end } = diagram.scope else {
            continue;
        };
        let Some(selected) = diagram.scope.text(before) else {
            continue;
        };
        if selected.len() > MAX_SCRIPT_BYTES
            || diagram.scope.text(after) == Some(selected)
            || !diagram.is_current(before)
        {
            continue;
        }
        if unique_line_match(before, selected) != Some(start) {
            continue;
        }
        let Some(new_start) = unique_line_match(after, selected) else {
            continue;
        };
        let new_end = new_start + end - start;
        let new_scope = Scope::Lines {
            start: new_start,
            end: new_end,
        };
        if new_scope.text(after) != Some(selected) {
            continue;
        }
        for reference in &mut diagram.references {
            reference.start = new_start + reference.start - start;
            reference.end = new_start + reference.end - start;
        }
        diagram.scope = new_scope;
    }
}

fn unique_line_match(source: &str, selected: &str) -> Option<u32> {
    if selected.is_empty() {
        return None;
    }
    // Track overlapping matches in linear time, including repeated lines.
    let pattern = selected.as_bytes();
    let mut prefix = vec![0; pattern.len()];
    let mut matched = 0;
    for (index, &byte) in pattern.iter().enumerate().skip(1) {
        matched = prefix_match(pattern, &prefix, matched, byte);
        prefix[index] = matched as u32;
    }
    let mut lines = line_offsets(source).enumerate().peekable();
    let mut found = None;
    matched = 0;
    for (index, byte) in source.bytes().enumerate() {
        matched = prefix_match(pattern, &prefix, matched, byte);
        if matched != pattern.len() {
            continue;
        }
        let start = index + 1 - matched;
        matched = prefix[matched - 1] as usize;
        while lines.peek().is_some_and(|(_, line)| line.start < start) {
            lines.next();
        }
        if let Some((line_index, line)) = lines.peek()
            && line.start == start
            && source
                .as_bytes()
                .get(index + 1)
                .is_none_or(|byte| matches!(byte, b'\r' | b'\n'))
            && found.replace(*line_index as u32 + 1).is_some()
        {
            return None;
        }
    }
    found
}

fn prefix_match(pattern: &[u8], prefix: &[u32], mut matched: usize, byte: u8) -> usize {
    while matched > 0 && byte != pattern[matched] {
        matched = prefix[matched - 1] as usize;
    }
    matched + usize::from(byte == pattern[matched])
}

impl Library {
    pub(crate) fn save_diagram(
        &mut self,
        snippet: SnippetId,
        source: &Source,
        saved: Saved,
        previous: Option<&Saved>,
    ) -> std::io::Result<()> {
        let mut candidate = self.document.as_ref().clone();
        let snippet = candidate
            .snippets
            .iter_mut()
            .find(|entry| entry.id() == snippet)
            .ok_or_else(|| invalid("The snippet no longer exists"))?;
        if !source.matches(snippet.content()) {
            return Err(invalid("The diagram source changed. Open generation again"));
        }
        let diagrams = &mut snippet.metadata.diagrams;
        match previous {
            Some(previous) => {
                let entry = diagrams
                    .iter_mut()
                    .find(|entry| **entry == *previous)
                    .ok_or_else(|| invalid("The saved diagram changed before regeneration"))?;
                *entry = saved;
            }
            None => diagrams.push(saved),
        }
        self.commit(candidate)
    }

    pub(crate) fn delete_diagram(
        &mut self,
        snippet: SnippetId,
        expected: &Saved,
    ) -> std::io::Result<()> {
        let mut candidate = self.document.as_ref().clone();
        let snippet = candidate
            .snippets
            .iter_mut()
            .find(|entry| entry.id() == snippet)
            .ok_or_else(|| invalid("The snippet no longer exists"))?;
        let diagrams = &mut snippet.metadata.diagrams;
        let index = diagrams
            .iter()
            .position(|entry| entry == expected)
            .ok_or_else(|| invalid("The saved diagram changed before deletion"))?;
        diagrams.remove(index);
        self.commit(candidate)
    }
}

fn hash(text: &str) -> Result<String, String> {
    content_hash(text).map_err(|_| "Could not hash the snippet diagram source.".into())
}

fn valid_hash(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn acceptable_text(character: char) -> bool {
    matches!(character, '\n' | '\r' | '\t')
        || (!character.is_control() && !matches!(character, '\u{fffe}' | '\u{ffff}'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::{Input, tests::Profile};

    fn fixture_chart() -> Chart {
        let diagram = parse_mermaid("flowchart TD\ncheck[Check] --> done[Done]").unwrap();
        Chart::stored(
            diagram,
            HashMap::from([("check".into(), SourceRange::new(1, 2).unwrap())]),
            false,
            None,
        )
    }

    #[test]
    fn java_private_snippet_library_keeps_ambiguous_ranges_without_source_links() {
        let capture = |source: &str, scope, line| {
            Saved::from_java(
                JavaRecord {
                    id: SnippetId::new().unwrap(),
                    title: "Imported Java diagram".into(),
                    family: Family::LogicalStructure,
                    scope,
                    mermaid: "flowchart TD\na[Check] --> b[Done]".into(),
                    full_sha256: Some(hash(source).unwrap()),
                    instructions: String::new(),
                    references: vec![Reference {
                        node: "a".into(),
                        start: line,
                        end: line,
                    }],
                    created_at: 0,
                    updated_at: 0,
                },
                source,
            )
            .unwrap()
        };
        for source in ["top\n\rnext\nend", "top\rnext\nend", "top\u{2028}next\nend"] {
            let scope = Scope::Lines { start: 2, end: 2 };
            let (saved, skipped) = capture(source, scope, 2);
            assert_eq!(skipped, 0);
            assert_eq!(saved.scope, scope);
            assert_eq!(saved.references[0].start, 2);
            assert_eq!(
                saved.full_sha256.as_deref(),
                Some(hash(source).unwrap().as_str())
            );
            let (chart, linked) = saved.restore(source).unwrap();
            assert!(linked.is_none());
            assert!(
                chart
                    .diagram()
                    .nodes()
                    .iter()
                    .all(|node| chart.source_range(node).is_none())
            );
        }
        let source = "top\r\nnext\nend\rrest";
        let (saved, skipped) = capture(source, Scope::Whole, 2);
        assert_eq!(skipped, 0);
        assert!(saved.is_current(source));
        let (chart, linked) = saved.restore(source).unwrap();
        assert_eq!(linked, Some(source));
        assert_eq!(
            chart.source_range(&chart.diagram().nodes()[0]),
            SourceRange::new(2, 2)
        );
    }

    #[test]
    fn selection_excludes_next_line_and_preserves_unicode_and_mixed_line_endings() {
        let full = "prefix\nprüfen\r\n終了\rsuffix";
        let scope = Scope::selected((3, 0), (1, 2)).unwrap();
        assert_eq!(scope, Scope::Lines { start: 2, end: 3 });
        let source = Source::capture(full, scope).unwrap();
        assert_eq!(&*source.text, "prüfen\r\n終了");
        assert!(source.matches("unrelated\nprüfen\r\n終了\rdifferent"));
        assert!(!source.matches("unrelated\nchanged\r\n終了\rdifferent"));
        assert!(Scope::selected((0, 0), (0, 0)).is_none());
        assert!(Source::capture(full, Scope::Lines { start: 3, end: 8 }).is_err());
    }

    #[test]
    fn scoped_and_whole_diagrams_age_independently_and_restore_without_ai() {
        let full = "prefix\ncheck\ndone\nsuffix";
        let scope = Scope::Lines { start: 2, end: 3 };
        let source = Source::capture(full, scope).unwrap();
        let chart = fixture_chart();
        let saved = Saved::capture(&chart, &source, "Check", "", None).unwrap();
        let whole = Saved::capture(
            &chart,
            &Source::capture(full, Scope::Whole).unwrap(),
            "Whole",
            "",
            None,
        )
        .unwrap();
        let diagrams = [saved, whole];
        let restored: Vec<Saved> =
            serde_json::from_slice(&serde_json::to_vec(&diagrams).unwrap()).unwrap();
        validate_collection(&restored).unwrap();
        assert!(validate_collection(&[restored[0].clone(), restored[0].clone()]).is_err());
        let outside = "prefix changed\ncheck\ndone\nsuffix";
        assert!(restored[0].is_current(outside));
        assert!(!restored[1].is_current(outside));
        let (chart, source) = restored[0].restore(outside).unwrap();
        assert_eq!(source, Some("check\ndone"));
        assert_eq!(
            chart.source_range(&chart.diagram().nodes()[0]),
            SourceRange::new(1, 2)
        );
        let (chart, source) = restored[0]
            .restore("prefix\nchanged\ndone\nsuffix")
            .unwrap();
        assert_eq!(source, None);
        assert_eq!(chart.diagram().nodes().len(), 2);
        assert!(chart.source_range(&chart.diagram().nodes()[0]).is_none());
    }

    #[test]
    fn regeneration_retains_identity_and_refuses_a_different_scope() {
        let full = "prefix\ncheck\ndone\nsuffix";
        let scope = Scope::Lines { start: 2, end: 3 };
        let chart = fixture_chart();
        let original = Saved::capture(
            &chart,
            &Source::capture(full, scope).unwrap(),
            "Check",
            "",
            None,
        )
        .unwrap();
        let changed = "prefix\nchanged\ndone\nsuffix";
        let updated = Saved::capture(
            &chart,
            &Source::capture(changed, scope).unwrap(),
            "Check",
            "",
            Some(&original),
        )
        .unwrap();
        assert_eq!(original.id, updated.id);
        assert_eq!(original.created_at, updated.created_at);
        assert!(updated.is_current(changed));
        assert!(
            Saved::capture(
                &chart,
                &Source::capture(full, Scope::Whole).unwrap(),
                "Check",
                "",
                Some(&original)
            )
            .is_err()
        );
    }

    #[test]
    fn library_rebases_unique_selection_and_checks_old_revisions_before_mutation() {
        fn input(content: &str) -> Input {
            Input {
                name: "Check".into(),
                language: "bash".into(),
                category: String::new(),
                content: content.into(),
            }
        }

        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let content = "prefix\ncheck\ndone\nsuffix";
        let snippet = library.save(None, input(content)).unwrap();
        let scope = Scope::Lines { start: 2, end: 3 };
        let source = Source::capture(content, scope).unwrap();
        let chart = fixture_chart();
        let diagram = Saved::capture(&chart, &source, "Check", "", None).unwrap();
        library
            .save_diagram(snippet, &source, diagram.clone(), None)
            .unwrap();
        let whole = Source::capture(content, Scope::Whole).unwrap();
        let whole_diagram = Saved::capture(&chart, &whole, "Whole", "", None).unwrap();
        library
            .save_diagram(snippet, &whole, whole_diagram, None)
            .unwrap();

        library
            .save_with_metadata(
                Some(snippet),
                input(content),
                Some(super::super::Metadata {
                    description: "Updated details".into(),
                    ..Default::default()
                }),
            )
            .unwrap();
        let metadata = library.get(snippet).unwrap().metadata();
        assert_eq!(metadata.description, "Updated details");
        assert_eq!(metadata.diagrams.len(), 2);

        let shifted = "new\nprefix\ncheck\ndone\nsuffix";
        library
            .save_with_metadata(
                Some(snippet),
                input(shifted),
                Some(super::super::Metadata::default()),
            )
            .unwrap();
        let mut library = Library::open(profile.path()).unwrap();
        let records = &library.get(snippet).unwrap().metadata().diagrams;
        assert_eq!(records.len(), 2);
        let moved = records[0].clone();
        assert_eq!(moved.id, diagram.id);
        assert_eq!(moved.scope, Scope::Lines { start: 3, end: 4 });
        assert_eq!(moved.references[0].start, 3);
        assert_eq!(moved.restore(shifted).unwrap().1, Some("check\ndone"));
        assert!(!records[1].is_current(shifted));
        assert!(library.delete_diagram(snippet, &diagram).is_err());
        assert!(
            library
                .save_diagram(snippet, &source, diagram.clone(), Some(&diagram))
                .is_err()
        );

        let ambiguous = "a\nb\nc\ncheck\ndone\ncheck\ndone";
        library
            .replace_contents([(snippet, shifted, ambiguous)])
            .unwrap();
        let saved = &library.get(snippet).unwrap().metadata().diagrams[0];
        assert_eq!(saved.scope, moved.scope);
        assert_eq!(saved.restore(ambiguous).unwrap().1, None);
        assert_eq!(unique_line_match("echo\necho\necho", "echo\necho"), None);

        let refreshed = Source::capture(ambiguous, moved.scope).unwrap();
        let replaced = Saved::capture(&chart, &refreshed, "Regenerated", "", Some(&moved)).unwrap();
        library
            .save_diagram(snippet, &refreshed, replaced.clone(), Some(&moved))
            .unwrap();
        assert!(library.delete_diagram(snippet, &moved).is_err());
        library.delete_diagram(snippet, &replaced).unwrap();
        assert_eq!(
            Library::open(profile.path())
                .unwrap()
                .get(snippet)
                .unwrap()
                .metadata()
                .diagrams
                .len(),
            1
        );
    }
}
