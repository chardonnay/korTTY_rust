//! UTF-16 positions over valid UTF-8 storage.
//!
//! A low-surrogate position is represented by the first continuation byte
//! of a four-byte scalar. Regex iteration can keep that position while
//! matches returned to Rust callers expose valid string boundaries.

use core::ops::Range;

use regex_automata::meta::Regex;

use crate::compile::compile_inner;
use crate::{
    java_case, next_utf8, prev_codepoint_ix, BackrefCase, Error, RegexOptions, Result, RuntimeError,
};

#[derive(Debug)]
pub(super) struct WordClasses {
    mark: Regex,
    base: Regex,
}

impl WordClasses {
    pub(super) fn new(options: &RegexOptions) -> Result<Self> {
        Ok(Self {
            mark: compile_inner(r"\A\p{Mn}\z", options)?,
            base: compile_inner(r"\A[\p{L}\p{Nd}]\z", options)?,
        })
    }

    pub(super) fn is_boundary(
        &self,
        text: &str,
        pos: usize,
        unicode: bool,
        budget: &mut usize,
    ) -> Result<bool> {
        let Some(left) = text.get(..pos) else {
            // Both sides are surrogate code units inside a supplementary scalar.
            return Ok(false);
        };
        let before = left.chars().next_back();
        let after = text[pos..].chars().next();
        let before_is_word = match before {
            Some(ch) => self.is_word(text, ch, previous_unit(text, pos), unicode, budget)?,
            None => false,
        };
        let after_is_word = match after {
            Some(ch) => self.is_word(text, ch, pos, unicode, budget)?,
            None => false,
        };
        Ok(before_is_word != after_is_word)
    }

    fn is_word(
        &self,
        text: &str,
        ch: char,
        mut pos: usize,
        unicode: bool,
        budget: &mut usize,
    ) -> Result<bool> {
        if ch.is_ascii() {
            return Ok(ch.is_ascii_alphanumeric() || ch == '_');
        }
        if unicode {
            return Ok(regex_syntax::is_word_character(ch));
        }
        if !self.mark.is_match(ch.encode_utf8(&mut [0; 4]).as_bytes()) {
            return Ok(false);
        }
        loop {
            *budget = budget
                .checked_sub(1)
                .ok_or_else(|| Error::RuntimeError(RuntimeError::BacktrackLimitExceeded))?;
            let Some(ch) = text.get(pos..).and_then(|suffix| suffix.chars().next()) else {
                return Ok(false);
            };
            let mut bytes = [0; 4];
            let scalar = ch.encode_utf8(&mut bytes).as_bytes();
            if !self.mark.is_match(scalar) {
                return Ok(self.base.is_match(scalar));
            }
            if pos == 0 {
                return Ok(false);
            }
            pos = previous_unit(text, pos);
        }
    }
}

pub(super) fn next_unit(text: &str, position: usize) -> usize {
    if !text.is_char_boundary(position) && position < text.len() {
        return next_utf8(text, position - 1);
    }
    match text.as_bytes().get(position) {
        Some(0xf0..=0xf4) => position + 1,
        _ => next_utf8(text, position),
    }
}

pub(super) fn previous_unit(text: &str, position: usize) -> usize {
    if !text.is_char_boundary(position) {
        return position - 1;
    }
    let start = prev_codepoint_ix(text, position);
    start + usize::from(position - start == 4)
}

pub(super) fn low_surrogate(text: &str, position: usize) -> Option<u16> {
    if position >= text.len() || text.is_char_boundary(position) {
        return None;
    }
    let first = position.checked_sub(1)?;
    let scalar = text.get(first..)?.chars().next()? as u32;
    (scalar >= 0x10000).then_some(0xdc00 | (scalar as u16 & 0x03ff))
}

fn code_point(text: &str, position: usize) -> Option<u32> {
    if let Some(suffix) = text.get(position..) {
        suffix.chars().next().map(u32::from)
    } else {
        low_surrogate(text, position).map(u32::from)
    }
}

fn unit_at(text: &str, position: usize) -> Option<u16> {
    let scalar = code_point(text, position)?;
    if scalar >= 0x10000 {
        Some(0xd800 | ((scalar - 0x10000) >> 10) as u16)
    } else {
        Some(scalar as u16)
    }
}

fn code_point_end(text: &str, position: usize) -> usize {
    if text.is_char_boundary(position) {
        next_utf8(text, position)
    } else {
        next_unit(text, position)
    }
}

pub(super) fn match_backref(
    text: &str,
    capture: Range<usize>,
    position: usize,
    case: BackrefCase,
) -> Option<usize> {
    let mut expected = capture.start;
    let mut found = position;
    if case == BackrefCase::Exact {
        while expected < capture.end {
            if unit_at(text, expected)? != unit_at(text, found)? {
                return None;
            }
            expected = next_unit(text, expected);
            found = next_unit(text, found);
        }
        return Some(found);
    }

    let mut units = 0usize;
    let mut end = position;
    while expected < capture.end {
        if end == text.len() {
            return None;
        }
        expected = next_unit(text, expected);
        end = next_unit(text, end);
        units += 1;
    }

    // Java compares code points but advances by the capture's UTF-16 width.
    expected = capture.start;
    while units != 0 {
        let a = code_point(text, expected)?;
        let b = code_point(text, found)?;
        if a != b {
            let equivalent = match case {
                BackrefCase::JavaAscii => {
                    a < 0x80 && b < 0x80 && (a as u8).eq_ignore_ascii_case(&(b as u8))
                }
                _ => {
                    java_case::map(a, java_case::FOLD_RANGES)
                        == java_case::map(b, java_case::FOLD_RANGES)
                }
            };
            if !equivalent {
                return None;
            }
        }
        expected = code_point_end(text, expected);
        found = code_point_end(text, found);
        units = units.saturating_sub(if b >= 0x10000 { 2 } else { 1 });
    }
    Some(end)
}

pub(super) fn visible_range(text: &str, start: usize, end: usize) -> Range<usize> {
    let mut first = start;
    while !text.is_char_boundary(first) {
        first -= 1;
    }
    let last = if start == end {
        first
    } else if text.is_char_boundary(end) {
        end
    } else {
        let mut boundary = end + 1;
        while !text.is_char_boundary(boundary) {
            boundary += 1;
        }
        boundary
    };
    first..last
}
