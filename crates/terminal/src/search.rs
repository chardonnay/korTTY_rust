//! Literal search over the active terminal screen and its scrollback.

use std::collections::VecDeque;

use crate::{Cell, Terminal};

pub const MAX_SEARCH_QUERY_BYTES: usize = 1024;
pub const MAX_SEARCH_RESULTS: usize = 1024;

/// A zero-based cell position in the searched view. Row zero is the oldest
/// retained history row, or the first live row if there is no history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchPosition {
    pub row: usize,
    pub column: u16,
}

/// A half-open highlight range on one physical row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchSpan {
    pub row: usize,
    pub start_column: u16,
    pub end_column: u16,
}

/// One match. The end is exclusive and its column can equal the grid width.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchMatch {
    pub start: SearchPosition,
    pub end: SearchPosition,
    /// Exact cell ranges, including full wide glyphs and omitting wrap padding.
    pub spans: Box<[SearchSpan]>,
}

/// Matches ordered from oldest to newest in the view at the time of the search.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchResults {
    pub matches: Vec<SearchMatch>,
    /// Live row zero begins at this index in the search coordinate system.
    pub scrollback_lines: usize,
    /// An additional match was found beyond the result limit.
    pub truncated: bool,
}

impl Terminal {
    /// Searches literal text in the active screen and its history, joining
    /// soft-wrapped rows and restarting at hard line breaks. Matches do not
    /// overlap. Empty queries and queries beyond the byte limit return no
    /// matches. Case-insensitive search compares Unicode lowercase scalars.
    ///
    /// Packed history is decoded one row at a time. Search never changes the
    /// viewport or retains decoded rows.
    pub fn search(&self, query: &str, ignore_case: bool) -> SearchResults {
        let screen = self.model.screen();
        let mut result = SearchResults {
            matches: Vec::new(),
            scrollback_lines: screen.history.len(),
            truncated: false,
        };
        if query.is_empty() || query.len() > MAX_SEARCH_QUERY_BYTES {
            return result;
        }

        let mut matcher = Matcher::new(query, ignore_case);
        for (row, line) in screen.history.iter().enumerate() {
            let cells = line.cells();
            if !matcher.scan_row(row, &cells, line.wrap_length, &mut result) {
                return result;
            }
        }
        for (row, line) in screen.lines.iter().enumerate() {
            let row = row + result.scrollback_lines;
            if !matcher.scan_row(row, &line.cells, line.wrap_length, &mut result) {
                break;
            }
        }
        result
    }
}

struct Matcher {
    pattern: Vec<char>,
    prefix_lengths: Vec<usize>,
    /// Physical spans of the matched prefix, one per input scalar. Several
    /// scalars from a combining or lowercased glyph can share the same span.
    prefix_spans: VecDeque<SearchSpan>,
    ignore_case: bool,
}

impl Matcher {
    fn new(query: &str, ignore_case: bool) -> Self {
        let pattern: Vec<_> = if ignore_case {
            query.chars().flat_map(char::to_lowercase).collect()
        } else {
            query.chars().collect()
        };
        let mut prefix_lengths = vec![0; pattern.len()];
        for index in 1..pattern.len() {
            prefix_lengths[index] = extend_prefix(
                &pattern,
                &prefix_lengths,
                prefix_lengths[index - 1],
                pattern[index],
            );
        }
        Self {
            prefix_spans: VecDeque::with_capacity(pattern.len()),
            prefix_lengths,
            pattern,
            ignore_case,
        }
    }

    fn scan_row(
        &mut self,
        row: usize,
        cells: &[Cell],
        wrap_length: Option<usize>,
        results: &mut SearchResults,
    ) -> bool {
        let length = match wrap_length {
            Some(used) => used.min(cells.len()),
            None => cells
                .iter()
                .rposition(|cell| !cell.text.is_empty())
                .map_or(0, |index| {
                    (index + usize::from(cells[index].width)).min(cells.len())
                }),
        };
        for (column, cell) in cells[..length].iter().enumerate() {
            if cell.width == 0 {
                continue;
            }
            let span = SearchSpan {
                row,
                start_column: column as u16,
                end_column: (column + usize::from(cell.width)).min(length) as u16,
            };
            let text = if cell.text.is_empty() {
                " "
            } else {
                &cell.text
            };
            for ch in text.chars() {
                if self.ignore_case {
                    for lowercase in ch.to_lowercase() {
                        if !self.scan_scalar(lowercase, span, results) {
                            return false;
                        }
                    }
                } else if !self.scan_scalar(ch, span, results) {
                    return false;
                }
            }
        }
        if wrap_length.is_none() {
            self.prefix_spans.clear();
        }
        true
    }

    fn scan_scalar(&mut self, ch: char, span: SearchSpan, results: &mut SearchResults) -> bool {
        let previous_length = self.prefix_spans.len();
        let matched = extend_prefix(&self.pattern, &self.prefix_lengths, previous_length, ch);
        if matched == 0 {
            self.prefix_spans.clear();
            return true;
        }
        self.prefix_spans.drain(..previous_length + 1 - matched);
        self.prefix_spans.push_back(span);

        if matched == self.pattern.len() {
            if results.matches.len() == MAX_SEARCH_RESULTS {
                results.truncated = true;
                return false;
            }
            results.matches.push(self.take_match());
        }
        true
    }

    fn take_match(&mut self) -> SearchMatch {
        let first = self.prefix_spans[0];
        let last = self.prefix_spans[self.prefix_spans.len() - 1];
        let mut spans: Vec<SearchSpan> = Vec::new();
        for span in self.prefix_spans.drain(..) {
            if let Some(previous) = spans.last_mut()
                && previous.row == span.row
                && span.start_column <= previous.end_column
            {
                previous.end_column = previous.end_column.max(span.end_column);
            } else {
                spans.push(span);
            }
        }
        SearchMatch {
            start: SearchPosition {
                row: first.row,
                column: first.start_column,
            },
            end: SearchPosition {
                row: last.row,
                column: last.end_column,
            },
            spans: spans.into_boxed_slice(),
        }
    }
}

/// Advances the longest matching prefix without rescanning input text.
fn extend_prefix(
    pattern: &[char],
    prefix_lengths: &[usize],
    mut matched: usize,
    ch: char,
) -> usize {
    while matched != 0 && pattern[matched] != ch {
        matched = prefix_lengths[matched - 1];
    }
    matched + usize::from(pattern[matched] == ch)
}
