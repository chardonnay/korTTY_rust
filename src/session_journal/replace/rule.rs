use std::fmt;
use std::io;
use std::ops::Range;

use regex::{CaptureLocations, Regex, RegexBuilder};
use zeroize::Zeroizing;

use crate::session_journal::entry_edit::MAX_FIELD_BYTES;

pub(crate) const MAX_RULE_BYTES: usize = 1024;

#[derive(Clone)]
pub(crate) struct Rule {
    matcher: Matcher,
    replacement: Zeroizing<String>,
}

#[derive(Clone)]
enum Matcher {
    Literal(Zeroizing<String>),
    Expression {
        regex: Regex,
        template: Option<Vec<Part>>,
    },
}

#[derive(Clone)]
enum Part {
    Text(Range<usize>),
    Group(usize),
}

impl fmt::Debug for Rule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JournalReplacementRule")
            .finish_non_exhaustive()
    }
}

impl Rule {
    pub(crate) fn compile(
        pattern: &str,
        replacement: &str,
        regex: bool,
        ignore_case: bool,
    ) -> io::Result<Self> {
        if pattern.is_empty()
            || pattern.len() > MAX_RULE_BYTES
            || replacement.len() > MAX_RULE_BYTES
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Enter a search term and keep both replacement fields within 1024 bytes",
            ));
        }
        let matcher = if !regex && !ignore_case {
            Matcher::Literal(Zeroizing::new(pattern.to_owned()))
        } else {
            let escaped;
            let expression = if regex {
                pattern
            } else {
                escaped = Zeroizing::new(regex::escape(pattern));
                escaped.as_str()
            };
            let expression = RegexBuilder::new(expression)
                .case_insensitive(ignore_case)
                .size_limit(256 * 1024)
                .dfa_size_limit(2 * 1024 * 1024)
                .build()
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Unsupported or invalid journal replacement expression",
                    )
                })?;
            let template = if regex {
                compile_template(replacement, &expression)
            } else {
                None
            };
            Matcher::Expression {
                regex: expression,
                template,
            }
        };
        Ok(Self {
            matcher,
            replacement: Zeroizing::new(replacement.to_owned()),
        })
    }

    /// Returns a new value only when the stored scalar changes.
    pub(crate) fn rewrite(&self, text: &str) -> io::Result<Option<Zeroizing<String>>> {
        if text.len() > MAX_FIELD_BYTES {
            return Err(too_large());
        }
        if text.is_empty() {
            return Ok(None);
        }
        let mut output = Zeroizing::new(String::new());
        let mut end = 0;
        match &self.matcher {
            Matcher::Literal(pattern) => {
                for (offset, matched) in text.match_indices(pattern.as_str()) {
                    append(&mut output, &text[end..offset])?;
                    append(&mut output, &self.replacement)?;
                    end = offset + matched.len();
                }
                if end == 0 {
                    return Ok(None);
                }
            }
            Matcher::Expression { regex, template } => {
                let mut captures = regex.capture_locations();
                let Some(mut matched) = regex.captures_read_at(&mut captures, text, 0) else {
                    return Ok(None);
                };
                loop {
                    append(&mut output, &text[end..matched.start()])?;
                    self.append_replacement(&mut output, text, &captures, template.as_deref())?;
                    end = matched.end();
                    // Java allows an empty match after a nonempty match ending at the same offset.
                    let next = if matched.is_empty() {
                        let Some(character) = text[end..].chars().next() else {
                            break;
                        };
                        end + character.len_utf8()
                    } else {
                        end
                    };
                    let Some(found) = regex.captures_read_at(&mut captures, text, next) else {
                        break;
                    };
                    matched = found;
                }
            }
        }
        append(&mut output, &text[end..])?;
        Ok((output.as_str() != text).then_some(output))
    }

    fn append_replacement(
        &self,
        output: &mut String,
        text: &str,
        captures: &CaptureLocations,
        template: Option<&[Part]>,
    ) -> io::Result<()> {
        let Some(template) = template else {
            return append(output, &self.replacement);
        };
        for part in template {
            match part {
                Part::Text(range) => append(output, &self.replacement[range.clone()])?,
                Part::Group(index) => {
                    if let Some((start, end)) = captures.get(*index) {
                        append(output, &text[start..end])?;
                    }
                }
            }
        }
        Ok(())
    }
}

// An invalid Java replacement template falls back to the full literal replacement.
fn compile_template(text: &str, regex: &Regex) -> Option<Vec<Part>> {
    let mut parts = Vec::new();
    let mut start = 0;
    while let Some(relative) = text[start..].find(['$', '\\']) {
        let special = start + relative;
        if start < special {
            parts.push(Part::Text(start..special));
        }
        let after = special + 1;
        let tail = &text[after..];
        if text.as_bytes()[special] == b'\\' {
            start = after + tail.chars().next()?.len_utf8();
            parts.push(Part::Text(after..start));
        } else {
            let (index, length) = group(tail, regex)?;
            parts.push(Part::Group(index));
            start = after + length;
        }
    }
    if start < text.len() {
        parts.push(Part::Text(start..text.len()));
    }
    Some(parts)
}

fn group(tail: &str, regex: &Regex) -> Option<(usize, usize)> {
    if let Some(named) = tail.strip_prefix('{') {
        let (name, _) = named.split_once('}')?;
        if !name.as_bytes().first()?.is_ascii_alphabetic()
            || !name.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return None;
        }
        let index = regex
            .capture_names()
            .position(|entry| entry == Some(name))?;
        return Some((index, name.len() + 2));
    }
    let mut digits = tail.bytes().take_while(u8::is_ascii_digit);
    let mut index = usize::from(digits.next()? - b'0');
    let groups = regex.captures_len();
    if index >= groups {
        return None;
    }
    let mut length = 1;
    for digit in digits {
        let next = index * 10 + usize::from(digit - b'0');
        if next >= groups {
            break;
        }
        index = next;
        length += 1;
    }
    Some((index, length))
}

fn append(output: &mut String, text: &str) -> io::Result<()> {
    if output.len().saturating_add(text.len()) > MAX_FIELD_BYTES {
        return Err(too_large());
    }
    output.push_str(text);
    Ok(())
}

fn too_large() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "A journal replacement text field exceeds the size limit",
    )
}

#[cfg(test)]
mod tests;
