//! Local matching and occurrence navigation over the Viewer’s rendered timeline fields.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::io;
use std::ops::Range;
use std::str::CharIndices;

const MAX_HITS: usize = 65_536;

#[derive(Clone)]
pub(super) struct Terms(Vec<String>);

impl Terms {
    pub(super) fn parse(mut query: &str) -> Self {
        let mut terms = Vec::new();
        loop {
            query = query.trim_start_matches(char::is_whitespace);
            if query.is_empty() {
                break;
            }
            let (term, rest) = if let Some(quoted) = query.strip_prefix('"')
                && let Some(end) = quoted.find('"').filter(|end| *end > 0)
            {
                (&quoted[..end], &quoted[end + 1..])
            } else {
                let end = query.find(char::is_whitespace).unwrap_or(query.len());
                query.split_at(end)
            };
            let term = term.trim().to_lowercase();
            if !term.is_empty() && !terms.contains(&term) {
                terms.push(term);
            }
            query = rest;
        }
        // Terms sharing a starting offset are ordered longest first.
        terms.sort_by_key(|term| Reverse(term.len()));
        Self(terms)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Hit {
    pub(super) entry: usize,
    pub(super) field: usize,
    pub(super) range: Range<usize>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Results {
    /// Matches in timeline display order, even when their source indices are out of order.
    pub(super) matches: Vec<usize>,
    pub(super) hits: Vec<Hit>,
    pub(super) total_entries: usize,
    pub(super) hits_limited: bool,
    by_entry: HashMap<usize, Range<usize>>,
}

impl Results {
    pub(super) fn hits_for_entry(&self, entry: usize) -> &[Hit] {
        self.by_entry
            .get(&entry)
            .and_then(|range| self.hits.get(range.clone()))
            .unwrap_or_default()
    }
}

/// Each source index must appear once. Field indices identify the exact text projected by the
/// caller. The cap limits retained occurrences while the entry count stays complete.
pub(super) fn scan<'a>(
    terms: &Terms,
    rows: impl IntoIterator<Item = (usize, Vec<Cow<'a, str>>)>,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Results> {
    let mut results = Results::default();
    let mut folded = String::new();
    let mut field_ranges = Vec::new();
    let mut next_hits = Vec::new();
    check_cancelled(cancelled)?;
    for (entry, fields) in rows {
        check_cancelled(cancelled)?;
        results.total_entries += 1;
        if terms.is_empty() {
            results.matches.push(entry);
            continue;
        }
        fold_fields(&fields, &mut folded, &mut field_ranges, cancelled)?;
        if !matches_entry(terms, &folded, cancelled)? {
            continue;
        }
        results.matches.push(entry);
        let first_hit = results.hits.len();
        for (field, (source, range)) in fields.iter().zip(&field_ranges).enumerate() {
            if results.hits_limited {
                break;
            }
            let text = &folded[range.clone()];
            let hit_field = Field {
                entry,
                field,
                source,
                text,
            };
            hit_field.scan(terms, &mut next_hits, &mut results, cancelled)?;
        }
        if first_hit != results.hits.len() {
            results
                .by_entry
                .insert(entry, first_hit..results.hits.len());
        }
    }
    check_cancelled(cancelled)?;
    Ok(results)
}

fn fold_fields(
    fields: &[Cow<'_, str>],
    text: &mut String,
    ranges: &mut Vec<Range<usize>>,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<()> {
    text.clear();
    ranges.clear();
    for (index, field) in fields.iter().enumerate() {
        check_cancelled(cancelled)?;
        // Java searches the card's text nodes joined by one space. Highlights still have to
        // lie in a single node, since their offsets are relative to that original field.
        if index != 0 {
            text.push(' ');
        }
        let start = text.len();
        if field.is_ascii() {
            text.push_str(field);
            text[start..].make_ascii_lowercase();
        } else {
            text.push_str(&field.to_lowercase());
        }
        ranges.push(start..text.len());
    }
    Ok(())
}

fn matches_entry(terms: &Terms, text: &str, cancelled: &dyn Fn() -> bool) -> io::Result<bool> {
    for term in &terms.0 {
        check_cancelled(cancelled)?;
        if !text.contains(term) {
            return Ok(false);
        }
    }
    Ok(true)
}

struct Field<'a> {
    entry: usize,
    field: usize,
    source: &'a str,
    text: &'a str,
}

impl Field<'_> {
    fn scan(
        &self,
        terms: &Terms,
        next_hits: &mut Vec<Option<usize>>,
        results: &mut Results,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        check_cancelled(cancelled)?;
        let first_hit = results.hits.len();
        let mut offsets = (!self.source.is_ascii()).then(|| OriginalRanges::new(self.source));
        next_hits.clear();
        for term in &terms.0 {
            check_cancelled(cancelled)?;
            next_hits.push(self.text.find(term));
        }
        while let Some((term, start)) = next_hit(next_hits) {
            check_cancelled(cancelled)?;
            let end = start + terms.0[term].len();
            let range = match &mut offsets {
                Some(offsets) => offsets.original_range(start..end)?,
                None => start..end,
            };
            // A single original character may expand to several lowercase characters. A hit
            // on either part must select the whole character, so coalesce any overlap.
            if let Some(previous) = results
                .hits
                .get_mut(first_hit..)
                .and_then(|hits| hits.last_mut())
                && range.start < previous.range.end
            {
                previous.range.end = previous.range.end.max(range.end);
            } else if results.hits.len() < MAX_HITS {
                results.hits.push(Hit {
                    entry: self.entry,
                    field: self.field,
                    range,
                });
            } else {
                results.hits_limited = true;
                break;
            }
            // Only refresh consumed or overlapping candidates. A term that cannot occur
            // again in this field stays absent, even if another term occurs many times.
            for (hit, term) in next_hits.iter_mut().zip(&terms.0) {
                if hit.is_some_and(|offset| offset < end) {
                    *hit = self.text[end..].find(term).map(|offset| end + offset);
                }
            }
        }
        Ok(())
    }
}

fn next_hit(hits: &[Option<usize>]) -> Option<(usize, usize)> {
    hits.iter()
        .enumerate()
        .filter_map(|(term, offset)| offset.map(|offset| (term, offset)))
        .min_by_key(|&(term, offset)| (offset, term))
}

struct OriginalRanges<'a> {
    characters: CharIndices<'a>,
    original: Range<usize>,
    folded_end: usize,
}

impl<'a> OriginalRanges<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            characters: source.char_indices(),
            original: 0..0,
            folded_end: 0,
        }
    }

    fn original_range(&mut self, range: Range<usize>) -> io::Result<Range<usize>> {
        while self.folded_end <= range.start {
            self.advance()?;
        }
        let start = self.original.start;
        while self.folded_end < range.end {
            self.advance()?;
        }
        Ok(start..self.original.end)
    }

    fn advance(&mut self) -> io::Result<()> {
        let (start, character) = self.characters.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid journal search text range",
            )
        })?;
        self.original = start..start + character.len_utf8();
        // Whole-string lowercase chooses the contextual Greek sigma form; both lowercase
        // variants have the same UTF-8 width. Scalar lengths also account for expansions.
        self.folded_end += character.to_lowercase().map(char::len_utf8).sum::<usize>();
        Ok(())
    }
}

pub(super) fn step(position: usize, len: usize, forward: bool) -> Option<usize> {
    if position >= len {
        return None;
    }
    Some(match (forward, position) {
        (true, position) => (position + 1) % len,
        (false, 0) => len - 1,
        (false, position) => position - 1,
    })
}

fn check_cancelled(cancelled: &dyn Fn() -> bool) -> io::Result<()> {
    if cancelled() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Journal search cancelled",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
