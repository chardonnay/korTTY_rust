use std::fmt;
use std::io;
use std::ops::Range;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::java_pattern;

pub(crate) const DEFAULT_ALPHABET: &str = "asdfqwerzxcvjklmiuopghtybn";
pub(crate) const MAX_PATTERNS: usize = 16;
const MAX_PATTERN_UNITS: usize = 512;
pub(crate) const MAX_PATTERN_BYTES: usize = MAX_PATTERN_UNITS * 3;
const MAX_PATTERN_PROGRAM: usize = 128 * 1024;
const PATTERN_BUDGET: Duration = Duration::from_millis(10);
const CAPTURE_BUDGET: Duration = Duration::from_millis(100);

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Preferences {
    pub(crate) alphabet: String,
    pub(crate) patterns: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) java_patterns: bool,
}

impl fmt::Debug for Preferences {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuickSelectPreferences")
            .finish_non_exhaustive()
    }
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            alphabet: DEFAULT_ALPHABET.into(),
            patterns: Vec::new(),
            java_patterns: false,
        }
    }
}

impl Preferences {
    pub(crate) fn is_default(&self) -> bool {
        !self.java_patterns
            && self.patterns.is_empty()
            && (self.alphabet == DEFAULT_ALPHABET || blank(&self.alphabet))
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        Self::validate_alphabet(&self.alphabet)?;
        let patterns = self.patterns.iter().filter(|pattern| !blank(pattern));
        if patterns.clone().take(MAX_PATTERNS + 1).count() > MAX_PATTERNS {
            return Err(invalid("Enter no more than 16 Quick Select patterns."));
        }
        for pattern in patterns {
            Self::validate_pattern(pattern, self.java_patterns)?;
        }
        Ok(())
    }

    pub(crate) fn validate_alphabet(alphabet: &str) -> io::Result<()> {
        if blank(alphabet) {
            return Ok(());
        }
        let mut seen = 0_u32;
        for letter in alphabet.bytes() {
            if letter.is_ascii_uppercase() {
                return Err(invalid("Use lowercase letters for Quick Select labels."));
            }
            if !letter.is_ascii_lowercase() {
                return Err(invalid(
                    "Quick Select labels can only contain letters a to z.",
                ));
            }
            let bit = 1 << (letter - b'a');
            if seen & bit != 0 {
                return Err(invalid("Use each Quick Select label letter only once."));
            }
            seen |= bit;
        }
        if alphabet.len() < 2 {
            return Err(invalid("Enter at least two Quick Select label letters."));
        }
        Ok(())
    }

    pub(crate) fn validate_pattern(pattern: &str, java: bool) -> io::Result<()> {
        compile_pattern(pattern, java).map(|_| ())
    }

    pub(crate) fn compile(&self) -> Compiled {
        let alphabet = if !blank(&self.alphabet) && Self::validate_alphabet(&self.alphabet).is_ok()
        {
            self.alphabet.as_str()
        } else {
            DEFAULT_ALPHABET
        };
        let patterns = self
            .patterns
            .iter()
            .filter_map(|pattern| compile_pattern(pattern, self.java_patterns).ok().flatten())
            .take(MAX_PATTERNS)
            .collect();
        Compiled {
            alphabet: alphabet.into(),
            patterns,
        }
    }
}

fn compile_pattern(pattern: &str, java: bool) -> io::Result<Option<java_pattern::Pattern>> {
    if blank(pattern) {
        return Ok(None);
    }
    if pattern.len() > MAX_PATTERN_BYTES || pattern.encode_utf16().count() > MAX_PATTERN_UNITS {
        return Err(invalid(
            "A Quick Select pattern can use up to 512 UTF-16 units.",
        ));
    }
    if !pattern.chars().all(|character| matches!(character, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')) {
        return Err(invalid("A Quick Select pattern contains an unsupported character."));
    }
    let regex = if java {
        java_pattern::Pattern::java(pattern, false, MAX_PATTERN_PROGRAM, 64)
    } else {
        java_pattern::Pattern::linear(pattern, false, false, MAX_PATTERN_PROGRAM, 64)
    }
    .map_err(|_| invalid("Invalid, unsupported, or oversized Quick Select regular expression."))?;
    if regex.is_match("") {
        return Err(invalid(
            "A Quick Select pattern cannot match an empty string.",
        ));
    }
    Ok(Some(regex))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn blank(value: &str) -> bool {
    value.chars().all(whitespace)
}

fn whitespace(character: char) -> bool {
    matches!(character, '\u{1c}'..='\u{1f}')
        || character.is_whitespace()
            && !matches!(character, '\u{85}' | '\u{a0}' | '\u{2007}' | '\u{202f}')
}

pub(crate) struct Compiled {
    pub(super) alphabet: String,
    patterns: Vec<java_pattern::Pattern>,
}

impl Compiled {
    pub(super) fn matching(&self) -> Option<Matching<'_>> {
        (!self.patterns.is_empty()).then(|| Matching {
            patterns: &self.patterns,
            overrun: 0,
            deadline: Instant::now() + CAPTURE_BUDGET,
        })
    }
}

pub(super) struct Matching<'a> {
    patterns: &'a [java_pattern::Pattern],
    overrun: u16,
    deadline: Instant,
}

impl Matching<'_> {
    pub(super) fn spans(&mut self, line: &str) -> Vec<(Range<usize>, usize)> {
        let mut spans = Vec::new();
        for (index, pattern) in self.patterns.iter().enumerate() {
            let now = Instant::now();
            if now >= self.deadline {
                break;
            }
            if self.overrun & (1 << index) != 0 {
                continue;
            }
            let deadline = (now + PATTERN_BUDGET).min(self.deadline);
            let first = spans.len();
            for found in pattern.find_iter(line) {
                if Instant::now() >= deadline {
                    break;
                }
                let value = &line[found.clone()];
                let start_trimmed = value.trim_start_matches(whitespace);
                let trimmed = start_trimmed.trim_end_matches(whitespace);
                if !trimmed.is_empty() {
                    let start = found.start + value.len() - start_trimmed.len();
                    spans.push((start..start + trimmed.len(), index));
                }
            }
            if Instant::now() >= deadline {
                spans.truncate(first);
                self.overrun |= 1 << index;
            }
        }
        spans
    }
}
