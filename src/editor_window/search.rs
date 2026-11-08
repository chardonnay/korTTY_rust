use std::ops::Range;
use std::time::{Duration, Instant};

use iced::widget::text_editor::{self, Cursor, LineEnding, Position};
use regex::{Regex, RegexBuilder};

use super::{MAX_FILE_BYTES, SearchDirection};

const MAX_QUERY_BYTES: usize = 4096;
const TOO_LARGE: &str = "The file would exceed the editor size limit.";
const NO_SELECTION: &str = "Select text, then enable Selection only again.";
const QUERY_TOO_LARGE: &str = "Search text must fit within 4096 UTF-8 bytes.";
const SEARCH_TIME_LIMIT: &str = "Search took too long. Try a narrower expression or selection.";
const SEARCH_BUDGET: Duration = Duration::from_millis(150);

#[derive(Default)]
pub(super) struct Search {
    pub(super) query: String,
    pub(super) replacement: String,
    pub(super) regex: bool,
    pub(super) case_sensitive: bool,
    query_too_large: bool,
    replacement_too_large: bool,
    scope: Scope,
}

#[derive(Default)]
enum Scope {
    #[default]
    Document,
    Selection(Option<Range<usize>>),
}

impl Search {
    pub(super) fn set_query(&mut self, query: String) -> Result<(), &'static str> {
        self.query_too_large = query.len() > MAX_QUERY_BYTES;
        if self.query_too_large {
            return Err(QUERY_TOO_LARGE);
        }
        self.query = query;
        Ok(())
    }

    pub(super) fn set_replacement(&mut self, replacement: String) -> Result<(), &'static str> {
        self.replacement_too_large = replacement.len() > MAX_FILE_BYTES;
        if self.replacement_too_large {
            return Err(TOO_LARGE);
        }
        self.replacement = replacement;
        Ok(())
    }

    pub(super) fn replacement_for_edit(&self) -> Result<&str, &'static str> {
        if self.replacement_too_large {
            return Err(TOO_LARGE);
        }
        Ok(&self.replacement)
    }

    pub(super) fn selection_only(&self) -> bool {
        matches!(self.scope, Scope::Selection(_))
    }

    pub(super) fn set_scope(
        &mut self,
        selection_only: bool,
        content: &text_editor::Content,
    ) -> Result<(), &'static str> {
        self.scope = Scope::Document;
        if !selection_only {
            return Ok(());
        }
        self.scope = Scope::Selection(None);
        let source = Text::read(content)?;
        let selected = source.selection.clone().filter(|range| !range.is_empty());
        self.scope = Scope::Selection(Some(source.raw_range(selected.ok_or(NO_SELECTION)?)));
        Ok(())
    }

    pub(super) fn invalidate_scope(&mut self) {
        if self.selection_only() {
            self.scope = Scope::Selection(None);
        }
    }

    pub(super) fn update_scope(&mut self, before: usize, after: usize) {
        if let Scope::Selection(Some(range)) = &mut self.scope {
            // All changes were inside this raw byte range.
            range.end = range.end + after - before;
        }
    }

    pub(super) fn plan(&self, source: &Text) -> Result<Plan, &'static str> {
        if self.query_too_large {
            return Err(QUERY_TOO_LARGE);
        }
        if self.query.is_empty() {
            return Err("Enter search text.");
        }
        let scope = match &self.scope {
            Scope::Document => 0..source.normalized.len(),
            Scope::Selection(Some(range)) => {
                let start = source.normalized_offset(range.start).ok_or(NO_SELECTION)?;
                let end = source.normalized_offset(range.end).ok_or(NO_SELECTION)?;
                start..end
            }
            Scope::Selection(None) => return Err(NO_SELECTION),
        };
        let escaped;
        let pattern = if self.regex {
            self.query.as_str()
        } else {
            escaped = regex::escape(&self.query);
            escaped.as_str()
        };
        let expression = RegexBuilder::new(pattern)
            .case_insensitive(!self.case_sensitive)
            .size_limit(256 * 1024)
            .dfa_size_limit(2 * 1024 * 1024)
            .build()
            .map_err(|_| "Invalid or unsupported search expression.")?;
        let deadline = self.regex.then(|| Instant::now() + SEARCH_BUDGET);
        Ok(Plan {
            expression,
            scope,
            deadline,
        })
    }
}

pub(super) struct Text {
    pub(super) raw: String,
    normalized: String,
    // A two-byte line ending contributes exactly one additional raw byte.
    extra_endings: Vec<usize>,
    caret: usize,
    selection: Option<Range<usize>>,
}

impl Text {
    pub(super) fn read(content: &text_editor::Content) -> Result<Self, &'static str> {
        let cursor = content.cursor();
        let mut raw = String::new();
        let mut normalized = String::new();
        let mut extra_endings = Vec::new();
        let mut caret = None;
        let mut anchor = None;
        let mut lines = content.lines().enumerate().peekable();
        while let Some((number, line)) = lines.next() {
            let ending = ending(line.ending, lines.peek().is_some());
            if raw.len() + line.text.len() + ending.len() > MAX_FILE_BYTES {
                return Err(TOO_LARGE);
            }
            if number == cursor.position.line {
                caret = Some(normalized.len() + cursor.position.column.min(line.text.len()));
            }
            if let Some(position) = cursor.selection
                && number == position.line
            {
                anchor = Some(normalized.len() + position.column.min(line.text.len()));
            }
            raw.push_str(&line.text);
            raw.push_str(ending);
            normalized.push_str(&line.text);
            if !ending.is_empty() {
                normalized.push('\n');
                if ending.len() == 2 {
                    extra_endings.push(normalized.len());
                }
            }
        }
        let caret = caret.unwrap_or(normalized.len());
        let selection = anchor.map(|anchor| anchor.min(caret)..anchor.max(caret));
        Ok(Self {
            raw,
            normalized,
            extra_endings,
            caret,
            selection,
        })
    }

    fn raw_offset(&self, normalized: usize) -> usize {
        normalized
            + self
                .extra_endings
                .partition_point(|&offset| offset <= normalized)
    }

    fn normalized_offset(&self, raw: usize) -> Option<usize> {
        let mut bounds = 0..self.normalized.len();
        while bounds.start < bounds.end {
            let middle = bounds.start + (bounds.end - bounds.start) / 2;
            if self.raw_offset(middle) < raw {
                bounds.start = middle + 1;
            } else {
                bounds.end = middle;
            }
        }
        let position = bounds.start;
        (self.raw_offset(position) == raw && self.normalized.is_char_boundary(position))
            .then_some(position)
    }

    fn raw_range(&self, hit: Range<usize>) -> Range<usize> {
        self.raw_offset(hit.start)..self.raw_offset(hit.end)
    }

    fn position(&self, offset: usize) -> Position {
        let prefix = &self.normalized[..offset];
        let line = prefix.bytes().filter(|&byte| byte == b'\n').count();
        let column = prefix
            .rsplit_once('\n')
            .map_or(prefix.len(), |(_, tail)| tail.len());
        Position { line, column }
    }

    pub(super) fn selection_cursor(&self, hit: Range<usize>) -> Cursor {
        Cursor {
            position: self.position(hit.end),
            selection: Some(self.position(hit.start)),
        }
    }

    pub(super) fn rewrite(
        &self,
        hits: impl Iterator<Item = Result<Range<usize>, &'static str>>,
        replacement: &str,
    ) -> Result<Rewrite, &'static str> {
        let mut output = String::new();
        let mut cursor = 0;
        let mut count = 0;
        let mut position = 0;
        for hit in hits {
            let range = self.raw_range(hit?);
            if &self.raw[range.clone()] == replacement {
                continue;
            }
            append(&mut output, &self.raw[cursor..range.start])?;
            append(&mut output, replacement)?;
            cursor = range.end;
            position = output.len();
            count += 1;
        }
        if count > 0 {
            append(&mut output, &self.raw[cursor..])?;
        }
        Ok(Rewrite {
            output,
            count,
            position,
        })
    }
}

pub(super) struct Rewrite {
    pub(super) output: String,
    pub(super) count: usize,
    pub(super) position: usize,
}

pub(super) struct Plan {
    expression: Regex,
    scope: Range<usize>,
    deadline: Option<Instant>,
}

impl Plan {
    pub(super) fn hits<'a>(
        &'a self,
        source: &'a Text,
    ) -> impl Iterator<Item = Result<Range<usize>, &'static str>> + 'a {
        let start = self.scope.start;
        let mut matches = self
            .expression
            .find_iter(&source.normalized[self.scope.clone()]);
        let expired = move || {
            self.deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        };
        let mut stopped = false;
        // Unlike a single regex search, advancing a match iterator can be quadratic
        // across the complete document. Check between matches and discard partial edits.
        std::iter::from_fn(move || {
            if stopped {
                return None;
            }
            let found = if expired() { None } else { matches.next() };
            if expired() {
                stopped = true;
                return Some(Err(SEARCH_TIME_LIMIT));
            }
            found.map(|found| Ok(start + found.start()..start + found.end()))
        })
    }

    pub(super) fn hit(
        &self,
        source: &Text,
        direction: SearchDirection,
        prefer_selection: bool,
    ) -> Result<Option<Range<usize>>, &'static str> {
        let selected = source.selection.as_ref();
        let from = selected.map_or(source.caret, |range| range.start);
        let to = selected.map_or(source.caret, |range| range.end);
        let mut first = None;
        let mut last = None;
        let mut previous = None;
        for hit in self.hits(source) {
            let hit = hit?;
            if prefer_selection && selected == Some(&hit) {
                return Ok(Some(hit));
            }
            match direction {
                SearchDirection::Next if hit.start >= to && selected != Some(&hit) => {
                    return Ok(Some(hit));
                }
                SearchDirection::Previous if hit.start < from => previous = Some(hit.clone()),
                _ => {}
            }
            first.get_or_insert_with(|| hit.clone());
            last = Some(hit);
        }
        Ok(match direction {
            SearchDirection::Next => first,
            SearchDirection::Previous => previous.or(last),
        })
    }
}

fn append(output: &mut String, part: &str) -> Result<(), &'static str> {
    if output.len().saturating_add(part.len()) > MAX_FILE_BYTES {
        return Err(TOO_LARGE);
    }
    output.push_str(part);
    Ok(())
}

fn ending(value: LineEnding, between: bool) -> &'static str {
    match (between, value) {
        (true, LineEnding::None) => "\n",
        (true, ending) => ending.as_str(),
        (false, _) => "",
    }
}

pub(super) fn position_after_edit(content: &text_editor::Content, raw: usize) -> Position {
    let mut offset = 0;
    let mut last = Position { line: 0, column: 0 };
    for (line, value) in content.lines().enumerate() {
        let end = offset + value.text.len();
        if raw <= end {
            let mut column = raw.saturating_sub(offset);
            while !value.text.is_char_boundary(column) {
                column -= 1;
            }
            return Position { line, column };
        }
        offset = end + ending(value.ending, true).len();
        last = Position {
            line,
            column: value.text.len(),
        };
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_regex_deadline_discards_unpublished_replacements() {
        let source = Text::read(&text_editor::Content::with_text("aa")).unwrap();
        let search = Search {
            query: "a".into(),
            regex: true,
            ..Search::default()
        };
        let mut plan = search.plan(&source).unwrap();
        plan.deadline = Some(Instant::now());
        assert_eq!(
            plan.hit(&source, SearchDirection::Next, false).unwrap_err(),
            SEARCH_TIME_LIMIT
        );
        // Test double for expiry after the iterator yielded one valid match.
        let hits = [Ok(0..1), Err(SEARCH_TIME_LIMIT)].into_iter();
        assert!(matches!(
            source.rewrite(hits, "changed"),
            Err(SEARCH_TIME_LIMIT)
        ));
        assert_eq!(source.raw, "aa");
    }
}
