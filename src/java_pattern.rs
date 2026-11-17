//! Java Pattern syntax translated for bounded native matchers.

use std::{
    borrow::Cow,
    fmt::Write as _,
    io,
    ops::Range,
    sync::{Arc, OnceLock},
};

use regex::{Regex, RegexBuilder};

mod class;
mod names;

const UNSUPPORTED: &str = "This Java pattern needs changes before it can run.";
const NEEDS_UTF16: &str = "This Java pattern needs UTF-16 positions.";
const FINAL_BREAK_CAPTURE: &str = "__kortty_final_line_break";
const MAX_ADVANCED_INPUT: usize = 16 * 1024;
const MAX_ADVANCED_MATCHES: usize = 128;
const ADVANCED_BACKTRACK_LIMIT: usize = 16_384;

#[derive(Clone, Debug)]
pub(crate) enum Pattern {
    Linear {
        regex: Regex,
        final_break: bool,
    },
    JavaLinear {
        regex: Regex,
        final_break: bool,
        word_boundaries: bool,
        line_endings: bool,
        utf16: Arc<Utf16Fallback>,
    },
    JavaRegular(fancy_regex::Regex),
    Advanced(fancy_regex::Regex),
}

pub(crate) enum Matches<'r, 't> {
    Plain(regex::Matches<'r, 't>),
    FinalBreak(regex::CaptureMatches<'r, 't>),
    JavaLinear(JavaMatches<'r, 't>),
    JavaExact(fancy_regex::Matches<'r, 't>),
    Advanced(std::iter::Take<fancy_regex::Matches<'r, 't>>),
    Empty,
}

impl Iterator for Matches<'_, '_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Plain(matches) => matches.next().map(|found| found.range()),
            Self::FinalBreak(matches) => matches.next().map(|captures| {
                let found = captures.get_match();
                let end = captures
                    .name(FINAL_BREAK_CAPTURE)
                    .map_or(found.end(), |delimiter| delimiter.start());
                found.start()..end
            }),
            Self::JavaLinear(matches) => matches.next(),
            Self::JavaExact(matches) => matches
                .next()
                .and_then(Result::ok)
                .map(|found| found.range()),
            Self::Advanced(matches) => matches
                .next()
                .and_then(Result::ok)
                .map(|found| found.range()),
            Self::Empty => None,
        }
    }
}

pub(crate) struct JavaMatches<'r, 't> {
    regex: &'r Regex,
    text: &'t str,
    final_break: bool,
    position: usize,
    utf16: &'r Utf16Fallback,
}

#[derive(Debug)]
pub(crate) struct Utf16Fallback {
    source: String,
    ignore_case: bool,
    limit: usize,
    regex: OnceLock<Option<fancy_regex::Regex>>,
}

impl Utf16Fallback {
    fn regex(&self) -> Option<&fancy_regex::Regex> {
        self.regex
            .get_or_init(|| build_advanced(&self.source, self.ignore_case, self.limit, true).ok())
            .as_ref()
    }
}

impl Iterator for JavaMatches<'_, '_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.position > self.text.len() {
            return None;
        }
        if !self.text.is_char_boundary(self.position) {
            let low_surrogate = self.position;
            self.position += 3;
            // Only this position needs the VM. The rest of the search stays
            // in the linear matcher even for inputs above the VM's size limit.
            if let Some(found) = self.utf16.regex().and_then(|regex| {
                regex
                    .kortty_match_low_surrogate(self.text, low_surrogate)
                    .ok()
                    .flatten()
            }) {
                let range = found.range();
                if !range.is_empty() {
                    self.position = range.end;
                }
                return Some(range);
            }
        }
        let range = if self.final_break {
            let captures = self.regex.captures_at(self.text, self.position)?;
            let found = captures.get_match();
            let end = captures
                .name(FINAL_BREAK_CAPTURE)
                .map_or(found.end(), |delimiter| delimiter.start());
            found.start()..end
        } else {
            self.regex.find_at(self.text, self.position)?.range()
        };
        self.position = if range.is_empty() {
            range.end
                + self.text[range.end..].chars().next().map_or(1, |ch| {
                    if ch.len_utf16() == 2 {
                        1
                    } else {
                        ch.len_utf8()
                    }
                })
        } else {
            range.end
        };
        Some(range)
    }
}

fn find_iter<'r, 't>(pattern: &'r Regex, text: &'t str, final_break: bool) -> Matches<'r, 't> {
    if final_break {
        Matches::FinalBreak(pattern.captures_iter(text))
    } else {
        Matches::Plain(pattern.find_iter(text))
    }
}

impl Pattern {
    pub(crate) fn java(
        source: &str,
        ignore_case: bool,
        limit: usize,
        nesting: u32,
    ) -> io::Result<Self> {
        match translate_linear(source, ignore_case) {
            Ok(translated) => {
                let regex = build_linear(&translated.expression, false, limit, nesting)?;
                Ok(Self::JavaLinear {
                    regex,
                    final_break: translated.final_break,
                    word_boundaries: translated.word_boundaries,
                    line_endings: translated.line_endings,
                    utf16: Arc::new(Utf16Fallback {
                        source: source.to_owned(),
                        ignore_case,
                        limit,
                        regex: OnceLock::new(),
                    }),
                })
            }
            Err(_) => Self::advanced(source, ignore_case, limit),
        }
    }

    pub(crate) fn linear(
        expression: &str,
        ignore_case: bool,
        final_break: bool,
        limit: usize,
        nesting: u32,
    ) -> io::Result<Self> {
        let regex = build_linear(expression, ignore_case, limit, nesting)?;
        Ok(Self::Linear { regex, final_break })
    }

    pub(crate) fn advanced(source: &str, ignore_case: bool, limit: usize) -> io::Result<Self> {
        build_advanced(source, ignore_case, limit, false).map(Self::Advanced)
    }

    pub(crate) fn java_whole_word(
        source: &str,
        ignore_case: bool,
        limit: usize,
    ) -> io::Result<Self> {
        const WORD: &str = r"[\p{L}\p{N}_]";
        let regular = translate_linear(source, ignore_case).is_ok();
        let wrapped = format!("(?<!{WORD})(?:{source})(?!{WORD})");
        let regex = build_advanced(&wrapped, ignore_case, limit, regular)?;
        Ok(if regular {
            Self::JavaRegular(regex)
        } else {
            Self::Advanced(regex)
        })
    }

    pub(crate) fn is_match(&self, text: &str) -> bool {
        self.find_iter(text).next().is_some()
    }

    pub(crate) fn find_iter<'r, 't>(&'r self, text: &'t str) -> Matches<'r, 't> {
        match self {
            Self::Linear { regex, final_break } => find_iter(regex, text, *final_break),
            Self::JavaLinear {
                regex,
                final_break,
                word_boundaries,
                line_endings,
                utf16,
            } => {
                if (*word_boundaries && java_boundary_context(text)
                    || *line_endings && text.contains("\r\n"))
                    && let Some(exact) = utf16.regex()
                {
                    return Matches::JavaExact(exact.find_iter(text));
                }
                Matches::JavaLinear(JavaMatches {
                    regex,
                    text,
                    final_break: *final_break,
                    position: 0,
                    utf16,
                })
            }
            Self::JavaRegular(regex) => Matches::JavaExact(regex.find_iter(text)),
            Self::Advanced(regex) if text.len() <= MAX_ADVANCED_INPUT => {
                Matches::Advanced(regex.find_iter(text).take(MAX_ADVANCED_MATCHES))
            }
            Self::Advanced(_) => Matches::Empty,
        }
    }
}

fn java_boundary_context(text: &str) -> bool {
    if text.is_ascii() {
        return false;
    }
    static SPECIAL: OnceLock<Regex> = OnceLock::new();
    SPECIAL
        .get_or_init(|| Regex::new(r"[\p{Mn}\x{10000}-\x{10ffff}]").expect("boundary context"))
        .is_match(text)
}

fn build_linear(
    expression: &str,
    ignore_case: bool,
    limit: usize,
    nesting: u32,
) -> io::Result<Regex> {
    RegexBuilder::new(expression)
        .case_insensitive(ignore_case)
        .size_limit(limit)
        .dfa_size_limit(limit)
        .nest_limit(nesting)
        .build()
        .map_err(io::Error::other)
}

fn build_advanced(
    source: &str,
    ignore_case: bool,
    limit: usize,
    regular_source: bool,
) -> io::Result<fancy_regex::Regex> {
    // This assertion leaves the delimiter available to the rest of the expression.
    let final_anchor = r"(?=\z|\r\n\z|(?<!\r)\n\z|[\r\x{85}\x{2028}\x{2029}]\z)";
    let translated = translate_with_end(source, ignore_case, final_anchor, true, true)
        .map_err(io::Error::other)?;
    let tree = fancy_regex::Expr::parse_tree_with_flags(
        &translated.expression,
        fancy_regex::internal::FLAG_KORTTY_JAVA,
    )
    .map_err(|_| io::Error::other(UNSUPPORTED))?;
    if advanced_cost(&tree.expr, &mut vec![0]).is_none() {
        return Err(io::Error::other(
            "This Java pattern exceeds the repetition limit.",
        ));
    }
    fancy_regex::RegexBuilder::new(&translated.expression)
        .kortty_java(true)
        .kortty_java_utf16(translated.codepoint_search)
        .kortty_java_linear_budget(regular_source)
        .delegate_size_limit(limit)
        .delegate_dfa_size_limit(limit)
        .backtrack_limit(ADVANCED_BACKTRACK_LIMIT)
        .build()
        .map_err(|_| io::Error::other(UNSUPPORTED))
}

// The VM limits backtracking, but analysis and mandatory repetitions can run
// before any backtracking occurs. Bound combined repetitions and group widths.
fn advanced_cost(expression: &fancy_regex::Expr, groups: &mut Vec<usize>) -> Option<usize> {
    use fancy_regex::Expr::*;
    let cost = match expression {
        Concat(children) | Alt(children) => children.iter().try_fold(0usize, |sum, child| {
            Some(sum.saturating_add(advanced_cost(child, groups)?))
        })?,
        Group(child) => {
            let group = groups.len();
            groups.push(0);
            let cost = advanced_cost(child, groups)?;
            groups[group] = cost;
            cost
        }
        LookAround(child, _) | AtomicGroup(child) | JavaGroup { child, .. } => {
            advanced_cost(child, groups)?
        }
        Repeat { child, lo, hi, .. } => {
            let count = if *hi == usize::MAX { *lo } else { *hi };
            count.max(1).saturating_mul(advanced_cost(child, groups)?)
        }
        Backref { group, .. } => groups.get(*group).copied().unwrap_or(1).max(1),
        Literal { val, .. } => val.len().max(1),
        Empty
        | Any { .. }
        | Assertion(_)
        | Delegate { .. }
        | ContinueFromPreviousMatchEnd
        | JavaGrapheme
        | JavaGraphemeBoundary
        | JavaSurrogate(_)
        | JavaClass { .. } => 1,
        JavaLineEnding => 2,
        _ => return None,
    };
    (cost <= MAX_ADVANCED_INPUT).then_some(cost)
}

pub(crate) fn translate(pattern: &str, ignore_case: bool) -> Result<(String, bool), &'static str> {
    translate_with_end(pattern, ignore_case, r"\z", false, false)
        .map(|pattern| (pattern.expression, pattern.final_break))
}

#[cfg(test)]
pub(crate) fn translate_ranges(
    pattern: &str,
    ignore_case: bool,
) -> Result<(String, bool), &'static str> {
    translate_linear(pattern, ignore_case).map(|pattern| (pattern.expression, pattern.final_break))
}

fn translate_linear(pattern: &str, ignore_case: bool) -> Result<Translation, &'static str> {
    // Java's final-break anchor is zero-width. Capture the consumed delimiter
    // separately so the caller can return the original Java match span.
    translate_with_end(
        pattern,
        ignore_case,
        r"(?:\z|(?<__kortty_final_line_break>(?:(?mR:$)(?:\r\n|[\r\n])|[\x{85}\x{2028}\x{2029}]))\z)",
        false,
        false,
    )
}

struct Translation {
    expression: String,
    final_break: bool,
    codepoint_search: bool,
    word_boundaries: bool,
    line_endings: bool,
}

fn translate_with_end(
    pattern: &str,
    ignore_case: bool,
    final_anchor: &str,
    advanced: bool,
    utf16: bool,
) -> Result<Translation, &'static str> {
    let mut flags = Flags {
        ignore_case,
        unicode_case: ignore_case,
        ..Flags::default()
    };
    let pattern = expand_quotes(pattern);
    let last_supplementary = pattern.rfind(|ch: char| ch.len_utf16() == 2);
    let mut codepoint_search = last_supplementary.is_some();
    let mut output = String::with_capacity(pattern.len());
    emit_case_flag(&mut output, false, flags.native_case());
    let mut remaining = pattern.as_ref();
    let mut scopes = Vec::new();
    let mut strict_end = false;
    let mut java_end = false;
    let mut word_boundaries = false;
    let mut line_endings = false;
    let mut capture_names: Vec<Option<String>> = Vec::new();
    let mut edits = Vec::new();
    let mut literals = Vec::new();
    loop {
        remaining = flags.skip_trivia(remaining);
        if let Some(literal) = take_literal(&mut remaining, flags)? {
            literals.push(literal);
            continue;
        }
        // Java's quantifier takes the last literal as a separate atom.
        if literals.len() > 1 && remaining.starts_with(['*', '+', '?', '{']) {
            let last = literals.pop().expect("literal before quantifier");
            codepoint_search |= write_literals(&mut output, &literals, flags, utf16)?;
            literals.clear();
            literals.push(last);
        }
        codepoint_search |= write_literals(&mut output, &literals, flags, utf16)?;
        literals.clear();
        let Some(ch) = take(&mut remaining) else {
            break;
        };
        if ch == '(' {
            remaining = flags.skip_trivia(remaining);
        }
        match ch {
            '\\' => {
                let escaped = take(&mut remaining).ok_or(UNSUPPORTED)?;
                if flags.unicode_classes && matches!(escaped, 'd' | 'D' | 's' | 'S' | 'w' | 'W') {
                    codepoint_search = true;
                    if utf16 {
                        class::shorthand_lows(escaped)
                            .write(&mut output, &format!(r"[\{escaped}]"));
                    } else {
                        output.push('\\');
                        output.push(escaped);
                    }
                    continue;
                }
                if let Some((class, word)) = shorthand(escaped) {
                    codepoint_search |= escaped.is_ascii_uppercase();
                    let isolate_case = word && flags.native_case();
                    if isolate_case {
                        output.push_str("(?-i:");
                    }
                    if utf16 {
                        class::shorthand_lows(escaped).write(&mut output, class);
                    } else {
                        output.push_str(class);
                    }
                    if isolate_case {
                        output.push(')');
                    }
                    continue;
                }
                match escaped {
                    'X' if advanced => output.push_str(r"\X"),
                    'b' if flags.skip_trivia(remaining).starts_with("{g") => {
                        if !advanced {
                            return Err(UNSUPPORTED);
                        }
                        remaining = flags
                            .skip_trivia(&flags.skip_trivia(remaining)[2..])
                            .strip_prefix('}')
                            .ok_or(UNSUPPORTED)?;
                        output.push_str(r"\b{g}");
                    }
                    'b' | 'B' => {
                        word_boundaries = true;
                        if advanced {
                            output.push('\\');
                            output.push(escaped);
                            output.push_str(if flags.unicode_classes { "{u}" } else { "{a}" });
                        } else if flags.unicode_classes {
                            output.push('\\');
                            output.push(escaped);
                        } else {
                            output.push_str(if escaped == 'b' {
                                r"(?-u:\b)"
                            } else {
                                r"(?-u:\B)"
                            });
                        }
                    }
                    'R' => {
                        line_endings = true;
                        output.push_str(if utf16 {
                            r"\R"
                        } else {
                            r"(?:\r\n|[\n\x0B\x0C\r\x{85}\x{2028}\x{2029}])"
                        });
                    }
                    '1'..='9' if advanced => {
                        let group = numeric_backreference(
                            escaped,
                            &mut remaining,
                            capture_names.len(),
                            flags,
                        );
                        edits.push((output.len(), group, flags));
                    }
                    'k' if advanced => {
                        let (name, tail) = capture_name(flags.skip_trivia(remaining), flags)?;
                        let group = capture_names
                            .iter()
                            .position(|candidate| candidate.as_deref() == Some(name.as_str()))
                            .ok_or(UNSUPPORTED)?
                            + 1;
                        edits.push((output.len(), group, flags));
                        remaining = tail;
                    }
                    'p' | 'P' => {
                        if flags.canonical && !utf16 {
                            return Err(NEEDS_UTF16);
                        }
                        if flags.native_case() {
                            output.push_str("(?-i:");
                        }
                        let mut scalar = String::new();
                        let (lows, codepoints) =
                            write_property(&mut scalar, &mut remaining, escaped, flags, utf16)?;
                        codepoint_search |= if flags.canonical {
                            escaped == 'P'
                        } else {
                            codepoints
                        };
                        if utf16 {
                            lows.write_with_canonical(&mut output, &scalar, flags.canonical);
                        } else {
                            output.push_str(&scalar);
                        }
                        if flags.native_case() {
                            output.push(')');
                        }
                    }
                    'Z' => end_anchor(
                        flags.skip_trivia(remaining),
                        &mut output,
                        &mut java_end,
                        flags.final_anchor(final_anchor, advanced)?,
                        advanced,
                    )?,
                    'z' => {
                        strict_end = true;
                        output.push_str(r"\z");
                    }
                    'A' => output.push_str(r"\A"),
                    'G' if advanced => output.push_str(r"\G"),
                    _ => return Err(UNSUPPORTED),
                }
            }
            '[' => {
                if flags.native_case() {
                    output.push_str("(?-i:");
                }
                let (class, codepoints) = class::translate(&mut remaining, flags, utf16)?;
                codepoint_search |= codepoints;
                output.push_str(&class);
                if flags.native_case() {
                    output.push(')');
                }
            }
            '(' if remaining.starts_with('?') => {
                if let Some((next, scoped, tail)) = scoped_flags(remaining, flags)? {
                    if scoped {
                        scopes.push(flags);
                        output.push_str(if utf16 { "(?j:" } else { "(?:" });
                    }
                    emit_case_flag(&mut output, flags.native_case(), next.native_case());
                    flags = next;
                    remaining = tail;
                    continue;
                }
                scopes.push(flags);
                if let Some(tail) = remaining.strip_prefix("?:") {
                    output.push_str(if utf16 { "(?j:" } else { "(?:" });
                    remaining = tail;
                } else if advanced
                    && let Some(prefix) = ["?<=", "?<!", "?=", "?!", "?>"]
                        .into_iter()
                        .find(|prefix| remaining.starts_with(*prefix))
                {
                    output.push('(');
                    remaining = &remaining[prefix.len()..];
                    if matches!(prefix, "?<=" | "?<!")
                        && last_supplementary
                            .is_none_or(|offset| offset < pattern.len() - remaining.len())
                    {
                        output.push_str("?<y");
                        output.push_str(&prefix[2..]);
                    } else if utf16 && prefix == "?>" {
                        output.push_str("?j>");
                    } else {
                        output.push_str(prefix);
                    }
                } else {
                    let (name, tail) =
                        capture_name(remaining.strip_prefix('?').ok_or(UNSUPPORTED)?, flags)?;
                    if advanced {
                        if capture_names
                            .iter()
                            .any(|other| other.as_ref() == Some(&name))
                        {
                            return Err(UNSUPPORTED);
                        }
                        capture_names.push(Some(name));
                        output.push('(');
                    } else {
                        output.push_str("(?<");
                        output.push_str(&name);
                        output.push('>');
                    }
                    remaining = tail;
                }
            }
            '(' => {
                if advanced {
                    capture_names.push(None);
                }
                scopes.push(flags);
                output.push('(');
            }
            ')' => {
                flags = scopes.pop().ok_or(UNSUPPORTED)?;
                output.push(')');
            }
            '.' => {
                if flags.dot_all {
                    output.push_str("(?s:.)");
                } else {
                    let scalar = if flags.unix_lines {
                        r"[^\n]"
                    } else {
                        r"[^\r\n\x{85}\x{2028}\x{2029}]"
                    };
                    if utf16 {
                        class::LowSurrogates::ALL.write(&mut output, scalar);
                    } else {
                        output.push_str(scalar);
                    }
                }
            }
            '^' if flags.multiline => {
                output.push_str(multiline_anchor(flags.unix_lines, true, advanced)?)
            }
            '$' if flags.multiline => {
                output.push_str(multiline_anchor(flags.unix_lines, false, advanced)?)
            }
            '$' => end_anchor(
                flags.skip_trivia(remaining),
                &mut output,
                &mut java_end,
                flags.final_anchor(final_anchor, advanced)?,
                advanced,
            )?,
            '{' => {
                let repetition = counted_repetition(&mut remaining, flags)?;
                output.push_str(&repetition);
                if !advanced && flags.skip_trivia(remaining).starts_with('+') {
                    return Err(UNSUPPORTED);
                }
            }
            '*' | '+' | '?' if !advanced && flags.skip_trivia(remaining).starts_with('+') => {
                return Err(UNSUPPORTED);
            }
            other => output.push(other),
        }
    }
    if !scopes.is_empty() || (!advanced && strict_end && java_end) {
        return Err(UNSUPPORTED);
    }
    if advanced {
        render_references(&mut output, capture_names.len(), edits);
    }
    Ok(Translation {
        expression: output,
        final_break: java_end,
        codepoint_search,
        word_boundaries,
        line_endings,
    })
}

fn is_java_supplementary(value: u32) -> bool {
    value >= 0x10000 || (0xD800..=0xDFFF).contains(&value)
}

fn take_literal(remaining: &mut &str, flags: Flags) -> Result<Option<u32>, &'static str> {
    let mut tail = *remaining;
    let Some(ch) = take(&mut tail) else {
        return Ok(None);
    };
    let literal = match ch {
        '[' | '(' | ')' | '.' | '^' | '$' | '|' | '?' | '*' | '+' | '{' => return Ok(None),
        '\\' => {
            let escape = take(&mut tail).ok_or(UNSUPPORTED)?;
            let Some(literal) = literal_escape(&mut tail, escape, flags)? else {
                return Ok(None);
            };
            literal
        }
        literal => u32::from(literal),
    };
    *remaining = tail;
    Ok(Some(literal))
}

fn write_literals(
    output: &mut String,
    literals: &[u32],
    flags: Flags,
    utf16: bool,
) -> Result<bool, &'static str> {
    let codepoint_search = matches!(literals, [literal] if is_java_supplementary(*literal)
        || flags.native_case() && char::from_u32(*literal).is_some_and(fancy_regex::internal::java_cased_literal));
    if flags.native_case()
        && let Some(scalars) = literals
            .iter()
            .copied()
            .map(char::from_u32)
            .collect::<Option<Vec<_>>>()
    {
        fancy_regex::internal::write_java_unicode_literals(output, &scalars);
        return Ok(codepoint_search);
    }
    for &literal in literals {
        if let Some(character) = char::from_u32(literal) {
            if flags.native_case() {
                fancy_regex::internal::write_java_unicode_literals(output, &[character]);
            } else if flags.ascii_case() && character.is_ascii_alphabetic() {
                push_ascii_case_literal(output, character);
            } else {
                write!(output, r"\x{{{literal:x}}}").expect("string write");
            }
        } else if utf16 {
            write!(output, r"\y{literal:04x}").expect("string write");
        } else {
            return Err(NEEDS_UTF16);
        }
    }
    Ok(codepoint_search)
}

fn literal_escape(
    remaining: &mut &str,
    escape: char,
    flags: Flags,
) -> Result<Option<u32>, &'static str> {
    Ok(Some(match escape {
        'x' | 'u' => hex_escape(remaining, escape, flags)?,
        'N' => {
            let (name, tail) = flags
                .skip_trivia(remaining)
                .strip_prefix('{')
                .and_then(|body| body.split_once('}'))
                .ok_or(UNSUPPORTED)?;
            let literal = names::resolve(name).ok_or(UNSUPPORTED)?;
            *remaining = tail;
            literal
        }
        '0' => u32::from(octal_escape(remaining, flags)?),
        'c' => {
            *remaining = flags.skip_trivia(remaining);
            u32::from(take(remaining).ok_or(UNSUPPORTED)?) ^ 0x40
        }
        'a' => 7,
        'e' => 27,
        'f' => 12,
        'n' => 10,
        'r' => 13,
        't' => 9,
        punctuation if !punctuation.is_ascii_alphanumeric() => u32::from(punctuation),
        _ => return Ok(None),
    }))
}

fn octal_escape(remaining: &mut &str, flags: Flags) -> Result<char, &'static str> {
    let first = take_octal_digit(remaining, flags).ok_or(UNSUPPORTED)?;
    let mut value = first;
    if let Some(second) = take_octal_digit(remaining, flags) {
        value = first * 8 + second;
        if first <= 3
            && let Some(third) = take_octal_digit(remaining, flags)
        {
            value = value * 8 + third;
        }
    }
    char::from_u32(value).ok_or(UNSUPPORTED)
}

fn take_octal_digit(remaining: &mut &str, flags: Flags) -> Option<u32> {
    let mut tail = flags.skip_trivia(remaining);
    let digit = take(&mut tail)?.to_digit(8)?;
    *remaining = tail;
    Some(digit)
}

fn expand_quotes(pattern: &str) -> Cow<'_, str> {
    let mut remaining = pattern;
    let first_quote = loop {
        let Some(offset) = remaining.find('\\') else {
            return Cow::Borrowed(pattern);
        };
        remaining = &remaining[offset + 1..];
        if take(&mut remaining) == Some('Q') {
            break pattern.len() - remaining.len() - 2;
        }
    };

    // Java removes quote delimiters before it reads any tokens, even in
    // comments or between parts of another construct.
    let mut expanded = String::with_capacity(pattern.len());
    expanded.push_str(&pattern[..first_quote]);
    let mut quoted = true;
    let mut first = true;
    while let Some(ch) = take(&mut remaining) {
        if quoted {
            if ch == '\\' && remaining.starts_with('E') {
                remaining = &remaining[1..];
                quoted = false;
            } else {
                if first && ch.is_ascii_digit() {
                    // Keep a leading quoted digit separate from an earlier escape.
                    expanded.push_str(r"\x3");
                } else if ch.is_ascii() && !ch.is_ascii_alphanumeric() {
                    expanded.push('\\');
                }
                expanded.push(ch);
            }
            first = false;
        } else if ch == '\\' && remaining.starts_with('Q') {
            remaining = &remaining[1..];
            quoted = true;
            first = true;
        } else {
            expanded.push(ch);
            if ch == '\\'
                && let Some(escaped) = take(&mut remaining)
            {
                expanded.push(escaped);
            }
        }
    }
    Cow::Owned(expanded)
}

fn numeric_backreference(
    first: char,
    remaining: &mut &str,
    opened_groups: usize,
    flags: Flags,
) -> usize {
    let mut group = first as usize - '0' as usize;
    while let Some(digit @ b'0'..=b'9') = flags.skip_trivia(remaining).as_bytes().first().copied() {
        let next = group
            .saturating_mul(10)
            .saturating_add(usize::from(digit - b'0'));
        if next > opened_groups {
            break;
        }
        group = next;
        *remaining = &flags.skip_trivia(remaining)[1..];
    }
    group
}

fn render_references(output: &mut String, groups: usize, edits: Vec<(usize, usize, Flags)>) {
    // All references use ordinal captures in the advanced matcher. The group
    // prevents adjacent digits from being parsed as part of the reference.
    for (offset, group, flags) in edits.into_iter().rev() {
        let replacement = if group > groups {
            // Java accepts a reference to a missing group, but it never matches.
            r"[\s&&\S]".to_owned()
        } else if flags.ascii_case() {
            format!(r"(?:\j{group})")
        } else if flags.native_case() {
            format!(r"(?:\J{group})")
        } else {
            format!(r"(?:\{group})")
        };
        output.insert_str(offset, &replacement);
    }
}

fn capture_name(pattern: &str, flags: Flags) -> Result<(String, &str), &'static str> {
    let mut remaining = flags.skip_trivia(pattern.strip_prefix('<').ok_or(UNSUPPORTED)?);
    let first = take(&mut remaining)
        .filter(char::is_ascii_alphabetic)
        .ok_or(UNSUPPORTED)?;
    let mut name = String::from(first);
    loop {
        remaining = flags.skip_trivia(remaining);
        match take(&mut remaining) {
            Some('>') => return Ok((name, remaining)),
            Some(ch) if ch.is_ascii_alphanumeric() => name.push(ch),
            _ => return Err(UNSUPPORTED),
        }
    }
}

fn hex_escape(remaining: &mut &str, escape: char, flags: Flags) -> Result<u32, &'static str> {
    let mut value = hex_code_point(remaining, escape, flags)?;
    if escape == 'u'
        && (0xD800..=0xDBFF).contains(&value)
        && let Some(mut low_escape) = flags
            .skip_trivia(remaining)
            .strip_prefix('\\')
            .and_then(|tail| flags.skip_trivia(tail).strip_prefix('u'))
    {
        let low = hex_code_point(&mut low_escape, 'u', flags)?;
        if (0xDC00..=0xDFFF).contains(&low) {
            value = 0x10000 + ((value - 0xD800) << 10) + (low - 0xDC00);
            *remaining = low_escape;
        }
    }
    (value <= 0x10FFFF).then_some(value).ok_or(UNSUPPORTED)
}

fn hex_code_point(remaining: &mut &str, escape: char, flags: Flags) -> Result<u32, &'static str> {
    *remaining = flags.skip_trivia(remaining);
    let width = if escape == 'u' { 4 } else { 2 };
    let braced = escape == 'x' && remaining.starts_with('{');
    if braced {
        *remaining = &remaining[1..];
    }
    let mut value = 0u32;
    let mut count = 0;
    loop {
        *remaining = flags.skip_trivia(remaining);
        let ch = take(remaining).ok_or(UNSUPPORTED)?;
        if braced && ch == '}' && count != 0 {
            break;
        }
        let digit = ch.to_digit(16).ok_or(UNSUPPORTED)?;
        value = value
            .checked_mul(16)
            .and_then(|v| v.checked_add(digit))
            .ok_or(UNSUPPORTED)?;
        count += 1;
        if !braced && count == width {
            break;
        }
    }
    Ok(value)
}

fn counted_repetition(remaining: &mut &str, flags: Flags) -> Result<String, &'static str> {
    // Java requires the first digit immediately after the opening brace.
    let first = take(remaining)
        .filter(char::is_ascii_digit)
        .ok_or(UNSUPPORTED)?;
    let mut repetition = format!("{{{first}");
    loop {
        *remaining = flags.skip_trivia(remaining);
        match take(remaining) {
            Some('}') => {
                repetition.push('}');
                return Ok(repetition);
            }
            Some(ch) if ch.is_ascii_digit() || ch == ',' => repetition.push(ch),
            _ => return Err(UNSUPPORTED),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Flags {
    ignore_case: bool,
    unicode_case: bool,
    dot_all: bool,
    unicode_classes: bool,
    multiline: bool,
    unix_lines: bool,
    comments: bool,
    canonical: bool,
}

impl Flags {
    fn native_case(self) -> bool {
        self.ignore_case && self.unicode_case
    }

    fn ascii_case(self) -> bool {
        self.ignore_case && !self.unicode_case
    }

    fn skip_trivia(self, mut text: &str) -> &str {
        if self.comments {
            loop {
                text = text.trim_start_matches(|ch| matches!(ch, ' ' | '\t'..='\r'));
                let Some(comment) = text.strip_prefix('#') else {
                    break;
                };
                text = comment
                    .find(|ch| {
                        ch == '\n'
                            || !self.unix_lines
                                && matches!(ch, '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
                    })
                    .map_or("", |end| &comment[end..]);
            }
        }
        text
    }

    fn final_anchor(self, default: &str, advanced: bool) -> Result<&str, &'static str> {
        if !self.unix_lines {
            Ok(default)
        } else if advanced {
            Ok(r"(?=\n\z|\z)")
        } else {
            Err(UNSUPPORTED)
        }
    }
}

fn scoped_flags(
    pattern: &str,
    mut flags: Flags,
) -> Result<Option<(Flags, bool, &str)>, &'static str> {
    let raw = pattern.strip_prefix('?').ok_or(UNSUPPORTED)?;
    let mut rest = flags.skip_trivia(raw);
    if rest.len() != raw.len()
        && let Some(tail) = rest.strip_prefix(':')
    {
        return Ok(Some((flags, true, tail)));
    }
    if !rest.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '-') {
        return Ok(None);
    }
    let mut set = true;
    loop {
        match take(&mut rest) {
            Some('i') => flags.ignore_case = set,
            Some('u') => flags.unicode_case = set,
            Some('U') => {
                flags.unicode_classes = set;
                flags.unicode_case = set;
            }
            Some('s') => flags.dot_all = set,
            Some('m') => flags.multiline = set,
            Some('d') => flags.unix_lines = set,
            Some('x') => flags.comments = set,
            Some('c') => flags.canonical = set,
            Some('-') if set => set = false,
            Some(':') => return Ok(Some((flags, true, rest))),
            Some(')') => return Ok(Some((flags, false, rest))),
            _ => return Err(UNSUPPORTED),
        }
        rest = flags.skip_trivia(rest);
    }
}

fn emit_case_flag(output: &mut String, previous: bool, current: bool) {
    if previous != current {
        output.push_str(if current { "(?i)" } else { "(?-i)" });
    }
}

fn push_ascii_case_literal(output: &mut String, literal: char) {
    if literal.is_ascii_alphabetic() {
        output.push('[');
        output.push(literal.to_ascii_lowercase());
        output.push(literal.to_ascii_uppercase());
        output.push(']');
    } else {
        output.push_str(&regex::escape(&literal.to_string()));
    }
}

fn close_ascii_case_class(output: &mut String, start: usize) -> Result<(), &'static str> {
    // Java folds the positive class first and applies negation afterwards.
    let contents = &output[start + 1..];
    let contents = contents.strip_prefix('^').unwrap_or(contents);
    let class = RegexBuilder::new(&format!(r"\A[{contents}]\z"))
        .size_limit(128 * 1024)
        .build()
        .map_err(|_| UNSUPPORTED)?;
    for lower in 'a'..='z' {
        let upper = lower.to_ascii_uppercase();
        match (
            class.is_match(&lower.to_string()),
            class.is_match(&upper.to_string()),
        ) {
            (true, false) => output.push(upper),
            (false, true) => output.push(lower),
            _ => {}
        }
    }
    Ok(())
}

fn multiline_anchor(unix: bool, start: bool, advanced: bool) -> Result<&'static str, &'static str> {
    if !advanced {
        return Err(UNSUPPORTED);
    }
    Ok(match (unix, start) {
        (true, true) => r"(?:\A|(?<=\n))(?!\z)",
        (true, false) => r"(?=\n|\z)",
        (false, true) => r"(?:\A|(?<=\n)|(?<=\r)(?!\n)|(?<=[\x{85}\x{2028}\x{2029}]))(?!\z)",
        (false, false) => r"(?=\z|\r|(?<!\r)\n|[\x{85}\x{2028}\x{2029}])",
    })
}

fn take(remaining: &mut &str) -> Option<char> {
    let mut chars = remaining.chars();
    let next = chars.next()?;
    *remaining = chars.as_str();
    Some(next)
}

fn end_anchor(
    remaining: &str,
    output: &mut String,
    java_end: &mut bool,
    anchor: &str,
    advanced: bool,
) -> Result<(), &'static str> {
    if !advanced && !remaining.bytes().all(|byte| byte == b')') {
        return Err(UNSUPPORTED);
    }
    output.push_str(anchor);
    *java_end = true;
    Ok(())
}

fn shorthand(escape: char) -> Option<(&'static str, bool)> {
    Some((
        match escape {
            'd' => "[0-9]",
            'D' => "[^0-9]",
            's' => r"[ \t\n\x0B\x0C\r]",
            'S' => r"[^ \t\n\x0B\x0C\r]",
            'w' => "[a-zA-Z_0-9]",
            'W' => "[^a-zA-Z_0-9]",
            'h' => r"[ \t\x{A0}\x{1680}\x{180E}\x{2000}-\x{200A}\x{202F}\x{205F}\x{3000}]",
            'H' => r"[^ \t\x{A0}\x{1680}\x{180E}\x{2000}-\x{200A}\x{202F}\x{205F}\x{3000}]",
            'v' => r"[\n\x0B\x0C\r\x{85}\x{2028}\x{2029}]",
            'V' => r"[^\n\x0B\x0C\r\x{85}\x{2028}\x{2029}]",
            _ => return None,
        },
        matches!(escape, 'w' | 'W'),
    ))
}

fn write_property(
    output: &mut String,
    remaining: &mut &str,
    escape: char,
    flags: Flags,
    utf16: bool,
) -> Result<(class::LowSurrogates, bool), &'static str> {
    let body = flags.skip_trivia(remaining);
    let (property, tail) = if let Some(braced) = body.strip_prefix('{') {
        flags
            .skip_trivia(braced)
            .split_once('}')
            .ok_or(UNSUPPORTED)?
    } else {
        body.split_at(body.chars().next().ok_or(UNSUPPORTED)?.len_utf8())
    };
    *remaining = tail;
    let (property, unicode_alias, unicode_posix) =
        if let Some((key, value)) = property.split_once('=') {
            if key.eq_ignore_ascii_case("sc") || key.eq_ignore_ascii_case("script") {
                return write_script_property(output, value, escape).map(|lows| (lows, true));
            }
            if key.eq_ignore_ascii_case("blk") || key.eq_ignore_ascii_case("block") {
                return write_block_property(output, value, escape, utf16).map(|lows| (lows, true));
            }
            if !key.eq_ignore_ascii_case("gc") && !key.eq_ignore_ascii_case("general_category") {
                return Err(UNSUPPORTED);
            }
            (value, false, false)
        } else if let Some(name) = property.strip_prefix("In") {
            return write_block_property(output, name, escape, utf16).map(|lows| (lows, true));
        } else if let Some(name) = property.strip_prefix("Is") {
            (name, true, true)
        } else {
            (property, false, flags.unicode_classes)
        };
    if property == "Cs" && !utf16 {
        return Err(NEEDS_UTF16);
    }
    let surrogate_member = matches!(property, "C" | "Cs" | "all" | "javaDefined")
        || (unicode_alias && property.eq_ignore_ascii_case("Assigned"));
    let unicode_members = if unicode_alias {
        unicode_property_members(&property.to_ascii_uppercase(), flags.ignore_case)
    } else if unicode_posix {
        unicode_posix_members(&property.to_ascii_uppercase(), flags.ignore_case)
    } else {
        None
    };
    let ascii_members = base_property_members(property, flags.ignore_case);
    let bmp = escape == 'p'
        && unicode_members.is_none()
        && ascii_members.is_some()
        && !matches!(property, "LC" | "LD" | "Cs" | "all");
    let members = unicode_members
        .or(ascii_members)
        .or_else(|| java_property_members(property, flags.ignore_case));
    if let Some(members) = members {
        class::write(output, members, escape == 'P');
    } else if flags.ignore_case && matches!(property, "Lu" | "Ll" | "Lt") {
        class::write(output, r"\p{Lu}\p{Ll}\p{Lt}", escape == 'P');
    } else if general_category(property) {
        class::write(output, &format!(r"\p{{{property}}}"), escape == 'P');
    } else if unicode_alias {
        return write_script_property(output, property, escape).map(|lows| (lows, true));
    } else {
        return Err(UNSUPPORTED);
    }
    Ok((property_lows(surrogate_member, escape), !bmp))
}

fn write_script_property(
    output: &mut String,
    name: &str,
    escape: char,
) -> Result<class::LowSurrogates, &'static str> {
    let script = fancy_regex::internal::java_unicode_script(name).ok_or(UNSUPPORTED)?;
    if script == "Unknown" {
        // The matcher has no explicit Unknown script set. On Unicode scalar
        // values its members are the unassigned and private-use categories.
        class::write(output, r"\p{Cn}\p{Co}", escape == 'P');
    } else {
        class::write(output, &format!(r"\p{{sc={script}}}"), escape == 'P');
    }
    Ok(property_lows(script == "Unknown", escape))
}

fn write_block_property(
    output: &mut String,
    name: &str,
    escape: char,
    utf16: bool,
) -> Result<class::LowSurrogates, &'static str> {
    let (first, last) = fancy_regex::internal::java_unicode_block(name).ok_or(UNSUPPORTED)?;
    let mut lows = class::LowSurrogates::EMPTY;
    if (0xD800..=0xDFFF).contains(&first) {
        // Java's obsolete SURROGATES_AREA has no code points.
        if (first, last) != (0xD800, 0xDFFF) {
            if !utf16 {
                return Err(NEEDS_UTF16);
            }
            lows = class::LowSurrogates::range(first, last);
        }
        class::write(output, r"[\s&&\S]", escape == 'P');
    } else {
        class::write(
            output,
            &format!(r"\x{{{first:x}}}-\x{{{last:x}}}"),
            escape == 'P',
        );
    }
    Ok(if escape == 'P' { lows.negate() } else { lows })
}

fn property_lows(member: bool, escape: char) -> class::LowSurrogates {
    if member != (escape == 'P') {
        class::LowSurrogates::ALL
    } else {
        class::LowSurrogates::EMPTY
    }
}

const CASED_MEMBERS: &str = r"\p{Lowercase}\p{Uppercase}\p{Lt}";

fn base_property_members(property: &str, ignore_case: bool) -> Option<&'static str> {
    Some(match property {
        "Lower" | "Upper" if ignore_case => "a-zA-Z",
        "Lower" => "a-z",
        "Upper" => "A-Z",
        "ASCII" => r"\x00-\x7F",
        "Alpha" => "a-zA-Z",
        "Digit" => "0-9",
        "Alnum" => "a-zA-Z0-9",
        "Punct" => r"\x21-\x2F\x3A-\x40\x5B-\x60\x7B-\x7E",
        "Graph" => r"\x21-\x7E",
        "Print" => r"\x20-\x7E",
        "Blank" => r"\t\x20",
        "Cntrl" => r"\x00-\x1F\x7F",
        "XDigit" => "0-9a-fA-F",
        "Space" => r"\x09-\x0D\x20",
        "LC" => r"\p{Lu}\p{Ll}\p{Lt}",
        "LD" => r"\p{L}\p{Nd}",
        "L1" => r"\x00-\xFF",
        // Unicode scalar values exclude the surrogate category.
        "Cs" => r"[\s&&\S]",
        "all" => r"\s\S",
        _ => return None,
    })
}

fn unicode_posix_members(property: &str, ignore_case: bool) -> Option<&'static str> {
    Some(match property {
        "LOWER" | "UPPER" if ignore_case => CASED_MEMBERS,
        "LOWER" => r"\p{Lowercase}",
        "UPPER" => r"\p{Uppercase}",
        "ALPHA" => r"\p{Alphabetic}",
        "DIGIT" => r"\p{Nd}",
        "ALNUM" => r"\p{Alphabetic}\p{Nd}",
        "PUNCT" => r"\p{P}",
        "GRAPH" => r"[\x{0}-\x{10ffff}--[\p{Z}\p{Cc}\p{Cn}]]",
        "PRINT" => r"[\x{0}-\x{10ffff}--[\p{Zl}\p{Zp}\p{Cc}\p{Cn}]]",
        "BLANK" => r"\p{Zs}\t",
        "CNTRL" => r"\p{Cc}",
        "XDIGIT" => r"\p{Hex_Digit}\p{Nd}",
        "SPACE" => r"\p{White_Space}",
        _ => return None,
    })
}

fn unicode_property_members(property: &str, ignore_case: bool) -> Option<&'static str> {
    Some(match property {
        "LOWERCASE" | "UPPERCASE" | "TITLECASE" if ignore_case => CASED_MEMBERS,
        "LOWERCASE" => r"\p{Lowercase}",
        "UPPERCASE" => r"\p{Uppercase}",
        "TITLECASE" => r"\p{Lt}",
        "ALPHABETIC" => r"\p{Alphabetic}",
        "ASSIGNED" => r"\P{Cn}",
        "CONTROL" => r"\p{Cc}",
        "EMOJI" => r"\p{Emoji}",
        "EMOJI_PRESENTATION" => r"\p{Emoji_Presentation}",
        "EMOJI_MODIFIER" => r"\p{Emoji_Modifier}",
        "EMOJI_MODIFIER_BASE" => r"\p{Emoji_Modifier_Base}",
        "EMOJI_COMPONENT" => r"\p{Emoji_Component}",
        "EXTENDED_PICTOGRAPHIC" => r"\p{Extended_Pictographic}",
        "HEXDIGIT" | "HEX_DIGIT" => r"\p{Hex_Digit}\p{Nd}",
        "IDEOGRAPHIC" => r"\p{Ideographic}",
        "JOINCONTROL" | "JOIN_CONTROL" => r"\p{Join_Control}",
        "LETTER" => r"\p{L}",
        "NONCHARACTERCODEPOINT" | "NONCHARACTER_CODE_POINT" => r"\p{Noncharacter_Code_Point}",
        "PUNCTUATION" => r"\p{P}",
        "WHITESPACE" | "WHITE_SPACE" => r"\p{White_Space}",
        "WORD" => r"\p{Alphabetic}\p{M}\p{Nd}\p{Pc}\p{Join_Control}",
        _ => return unicode_posix_members(property, ignore_case),
    })
}

fn java_property_members(property: &str, ignore_case: bool) -> Option<&'static str> {
    // These predicates use regex-syntax's Unicode 16.0 property tables and
    // Java's additional sets for identifier characters and whitespace.
    Some(match property {
        "javaLowerCase" | "javaUpperCase" | "javaTitleCase" if ignore_case => CASED_MEMBERS,
        "javaLowerCase" => r"\p{Lowercase}",
        "javaUpperCase" => r"\p{Uppercase}",
        "javaTitleCase" => r"\p{Lt}",
        "javaAlphabetic" => r"\p{Alphabetic}",
        "javaIdeographic" => r"\p{Ideographic}",
        "javaDigit" => r"\p{Nd}",
        "javaDefined" => r"\P{Cn}",
        "javaLetter" => r"\p{L}",
        "javaLetterOrDigit" => r"\p{L}\p{Nd}",
        "javaJavaIdentifierStart" => r"\p{L}\p{Nl}\p{Sc}\p{Pc}",
        "javaJavaIdentifierPart" => {
            r"\p{L}\p{Nl}\p{Sc}\p{Pc}\p{Nd}\p{Mc}\p{Mn}\p{Cf}\x00-\x08\x0E-\x1B\x7F-\x9F"
        }
        // Java also admits U+2E2F VERTICAL TILDE in both Unicode identifier predicates.
        "javaUnicodeIdentifierStart" => r"\p{ID_Start}\x{2E2F}",
        "javaUnicodeIdentifierPart" => r"\p{ID_Continue}\x{2E2F}\p{Cf}\x00-\x08\x0E-\x1B\x7F-\x9F",
        "javaIdentifierIgnorable" => r"\p{Cf}\x00-\x08\x0E-\x1B\x7F-\x9F",
        "javaSpaceChar" => r"\p{Z}",
        "javaWhitespace" => r"[\p{Z}--[\x{A0}\x{2007}\x{202F}]]\x09-\x0D\x1C-\x1F",
        "javaISOControl" => r"\x00-\x1F\x7F-\x9F",
        "javaMirrored" => r"\p{Bidi_Mirrored}",
        _ => return None,
    })
}

fn general_category(property: &str) -> bool {
    matches!(
        property,
        "L" | "Lu"
            | "Ll"
            | "Lt"
            | "Lm"
            | "Lo"
            | "M"
            | "Mn"
            | "Mc"
            | "Me"
            | "N"
            | "Nd"
            | "Nl"
            | "No"
            | "Z"
            | "Zs"
            | "Zl"
            | "Zp"
            | "C"
            | "Cc"
            | "Cf"
            | "Cs"
            | "Co"
            | "Cn"
            | "P"
            | "Pc"
            | "Pd"
            | "Ps"
            | "Pe"
            | "Pi"
            | "Pf"
            | "Po"
            | "S"
            | "Sm"
            | "Sc"
            | "Sk"
            | "So"
    )
}

pub(crate) fn without_final_line_break(text: &str) -> Option<&str> {
    text.strip_suffix("\r\n")
        .or_else(|| text.strip_suffix(['\r', '\n', '\u{85}', '\u{2028}', '\u{2029}']))
}

#[cfg(test)]
// Expected values are match ranges, not expanded character indices.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    #[test]
    fn java_pattern_matches_jdk_21_boundaries_properties_and_final_breaks() {
        let matches = |source, text| {
            let (expression, final_break) = translate_ranges(source, false).unwrap();
            let pattern = Regex::new(&expression).unwrap();
            find_iter(&pattern, text, final_break).collect::<Vec<_>>()
        };
        assert_eq!(matches(r"\bTKT-\d+", "éTKT-42"), [2..8]);
        assert!(matches(r"(?U)\bTKT-\d+", "éTKT-42").is_empty());
        assert_eq!(matches(r"(?U)\d+", "١"), [0..2]);
        assert_eq!(matches(r"\R\n", "\r\n"), [0..2]);
        assert_eq!(matches(r"\p{Lu}+", "ÉΔ é"), [0..4]);
        assert_eq!(matches(r"\Qticket+\E", "ticket+ tickettt"), [0..7]);
        assert_eq!(matches(r"x|foo$", "x foo\r\n"), [0..1, 2..5]);
        assert!(matches(r"\r$", "x\r\n").is_empty());
        assert_eq!(matches(r"word\Z", "word\u{2028}"), [0..4]);
        assert!(matches(r"word\z", "word\u{2028}").is_empty());
    }

    #[test]
    fn java_pattern_matches_assertions_and_captured_references_within_limits() {
        let ranges = |source: &str, text: &str| {
            Pattern::java(source, false, 128 * 1024, 64)
                .unwrap()
                .find_iter(text)
                .collect::<Vec<_>>()
        };
        assert_eq!(ranges(r"(?<=ref:)\b\w+(?=;)", "é ref:apple;"), [7..12]);
        assert_eq!(
            ranges(r"(?<!old-)\b([A-Z]+)-\1(?!\d)$", "new-AB-AB\r\n"),
            [4..9]
        );
        assert!(ranges(r"(?<!old-)\b([A-Z]+)-\1(?!\d)$", "old-AB-AB").is_empty());
        assert_eq!(ranges(r"(?<word>[a-z]+):\k<word>", "ok:ok no:yes"), [0..5]);
        assert_eq!(ranges(r"(\w+):\1", "good:good wrong:no"), [0..9]);
        assert!(ranges(r"(?=a)(a+)+\1z", &"a".repeat(8192)).is_empty());
        assert!(ranges(r"(?=a)a", &"a".repeat(MAX_ADVANCED_INPUT + 1)).is_empty());
        assert!(Pattern::java(r"(a{50000}){50000}(?=z)", false, 128 * 1024, 64).is_err());
        assert!(Pattern::java(r"(?=a){999999}a", false, 128 * 1024, 64).is_err());
        assert_eq!(
            ranges(r"(?=a)a", &"a".repeat(256)).len(),
            MAX_ADVANCED_MATCHES
        );
        assert!(
            Pattern::java(r"\Qa(?=b)\E", false, 128 * 1024, 64)
                .unwrap()
                .is_match("a(?=b)")
        );
    }

    #[test]
    fn java_pattern_backreferences_use_the_flags_at_the_reference() {
        let matches = |source: &str, input: &str| {
            Pattern::java(source, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{source}: {error}"))
                .is_match(input)
        };
        assert!(matches(r"^(aé)-(?i:\1)$", "aé-Aé"));
        assert!(!matches(r"^(aé)-(?i:\1)$", "aé-AÉ"));
        assert!(!matches(r"^(aé)-(?i:\1)$", "aé-a"));
        assert!(matches(r"^(?<word>k):(?i:\k<word>)$", "k:K"));
        assert!(!matches(r"^(?<word>k):(?i:\k<word>)$", "k:K"));
        assert!(matches(r"^(?<word>É):(?iu:\k<word>)$", "É:é"));
        assert!(!matches(r"^(?i:(a)):\1$", "A:a"));
        assert!(matches(r"^(?i:(a)):\1$", "A:A"));
        assert!(matches(r"^(a)(?i:\12)$", "aA2"));
        assert!(matches(r"^(?i:\9)?a$", "a"));
        assert!(!matches(r"^(?i:\1)(a)$", "Aa"));
        assert!(Pattern::java(r"(a)\j1", false, 128 * 1024, 64).is_err());
        assert!(fancy_regex::Regex::new(r"(a)\j1").is_err());
    }

    #[test]
    fn java_pattern_unicode_backreferences_advance_by_the_candidate_bytes() {
        let cases = [
            (r"^(k)(?iu:\1)!$", "kK!", Some(0..5)),
            (r"^(?<word>K):(?iu:\k<word>)!$", "K:k!", Some(0..6)),
            (r"^(İ)(?iu:\1)(?-i:\1)$", "İıİ", Some(0..6)),
            (r"^(𐐀)(?iu:\1)!$", "𐐀𐐨!", Some(0..9)),
            (r"^(\x{1C89})(?iu:\1)!$", "\u{1C89}\u{1C8A}!", Some(0..7)),
            (r"^(ß)(?iu:\1)$", "ßẞ", Some(0..5)),
            (r"^(ß)(?iu:\1)$", "ßss", None),
            (r"^(?iu)(a?)\1!$", "!", Some(0..1)),
            (r"^(kK)(?iu:\1)$", "kKK", None),
            (r"^(k)(?i:\1)!$", "kK!", None),
        ];
        for (expression, input, expected) in cases {
            let pattern = Pattern::java(expression, false, 128 * 1024, 64).unwrap();
            assert_eq!(pattern.find_iter(input).next(), expected, "{expression}");
        }
        assert!(Pattern::java(r"(a)\J1", false, 128 * 1024, 64).is_err());
        assert!(fancy_regex::Regex::new(r"(a)\J1").is_err());
    }

    #[test]
    fn java_pattern_preserves_atomicity_and_numeric_reference_parsing() {
        let ranges = |source: &str, text: &str| {
            Pattern::java(source, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{source}: {error}"))
                .find_iter(text)
                .collect::<Vec<_>>()
        };
        assert_eq!(ranges(r"(?>ab|a)b", "ab abb"), [3..6]);
        assert!(ranges(r"^a++a$", "aa").is_empty());
        assert_eq!(ranges(r"^a{1,2}+a$", "aaa"), [0..3]);
        assert!(ranges(r"^a?+a$", "a").is_empty());
        assert_eq!(ranges(r"^a*+b$", "aaab"), [0..4]);
        assert!(Pattern::java(r"(?>(?:a{50000}){50000})", false, 128 * 1024, 64).is_err());
        assert_eq!(ranges(r"^(a)\12$", "aa2"), [0..3]);
        assert_eq!(
            ranges(r"^(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)\101$", "abcdefghijj1"),
            [0..12]
        );
        assert_eq!(
            ranges(
                r"^(a)\12(b)(c)(d)(e)(f)(g)(h)(i)(j)(k)(l)$",
                "aa2bcdefghijkl"
            ),
            [0..14]
        );
        assert_eq!(ranges(r"^(?<first>a)(b)\12\2\k<first>$", "aba2ba"), [0..6]);
        assert!(ranges(r"^\1(a)$", "aa").is_empty());
        assert_eq!(ranges(r"^(?:\1?(a))+$", "aaa"), [0..3]);
        assert_eq!(ranges(r"^(?:\1?(?<name>a))+$", "aaa"), [0..3]);
        assert!(Pattern::java(r"\k<unknown>(?<unknown>a)", false, 128 * 1024, 64).is_err());
        assert_eq!(ranges(r"^\9?a$", "a"), [0..1]);
    }

    #[test]
    fn java_pattern_restores_scoped_flags_and_java_line_boundaries() {
        let ranges = |source: &str, text: &str| {
            Pattern::java(source, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{source}: {error}"))
                .find_iter(text)
                .collect::<Vec<_>>()
        };
        assert_eq!(ranges("(?i:a)[a]", "Aa AA"), [0..2]);
        assert_eq!(ranges("(?i:[k-z])", "kKKzZ"), [0..1, 1..2, 5..6, 6..7]);
        assert_eq!(ranges("(?i:[^a-z])", "AéK9"), [1..3, 3..6, 6..7]);
        assert_eq!(ranges("(?iu:k)(?-i:k)", "Kk KK"), [0..4]);
        assert_eq!(ranges(r"(?i:\Qab+\E\u0043)\x44", "aB+cD AB+Cd"), [0..5]);
        assert_eq!(
            ranges(r"(?i:\p{Lu})", "AéaΔδ"),
            [0..1, 1..3, 3..4, 4..6, 6..8]
        );
        assert_eq!(ranges(r"(?U:\d+)-\d+", "١-2 ٢-٣"), [0..4]);
        assert_eq!(ranges("a(?i)bC(?-i)d", "aBcD aBcd"), [5..9]);
        assert_eq!(ranges("(?m)^b$", "a\r\nb\u{2028}c"), [3..4]);
        assert_eq!(ranges("(?m)^", "a\r\nb\n"), [0..0, 3..3]);
        assert!(ranges("(?m)^", "").is_empty());
        assert_eq!(ranges("(?m)$", "a\r\nb\n"), [1..1, 4..4, 5..5]);
        assert_eq!(ranges(r"(?d:x\Z)", "x\n"), [0..1]);
        assert!(ranges(r"(?d:x\Z)", "x\r\n").is_empty());
        assert_eq!(ranges("(?d:a.b)(?s:c.d)", "a\rb c\nd a\rbc\nd"), [8..14]);
        assert_eq!(ranges("(?iu:k(?-i:x))K", "KxK KXK"), [0..5]);
    }

    #[test]
    fn java_pattern_unicode_literals_preserve_run_boundaries() {
        let cases = [
            (r"(?iu)ß", "ẞ", None),
            (r"(?iu)ßa", "ẞA", Some(0..4)),
            (r"(?iu)ßa?", "ẞA", None),
            (r"(?iu)ß\Qa\E", "ẞA", Some(0..4)),
            (r"(?iu)\x{df}a", "ẞA", Some(0..4)),
            (r"(?iu)ß(?=a)a", "ẞA", None),
            (r"(?iu)ß{1}", "ẞ", None),
            ("(?iux)ß#comment\n a", "ẞA", Some(0..4)),
            (r"(?iu)ß(?i)a", "ẞA", None),
            (r"(?iu)ıİ", "Ii", Some(0..2)),
            (r"(?iu)k", "K", Some(0..3)),
            (r"(?iu)k+", "K", Some(0..3)),
            (r"(?iu)\Qßa\E+", "ẞA", None),
            (r"(?iu)ßab{1,2}", "ẞABB", Some(0..6)),
            (r"(?iu)ß(?-i:a)b", "ßaB", Some(0..4)),
            (r"(?iu)ß\Qa+\E", "ẞA+", Some(0..5)),
            (r"(?iu)ß\+a", "ẞ+A", Some(0..5)),
            (r"(?iu)\x{1C89}", "\u{1C8A}", Some(0..3)),
            (r"(?iu)ßa{1,2}+b", "ßAAB", Some(0..5)),
        ];
        for (expression, input, expected) in cases {
            let pattern = Pattern::java(expression, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{expression}: {error}"));
            assert_eq!(pattern.find_iter(input).next(), expected, "{expression}");
        }
    }

    #[test]
    fn java_pattern_unicode_classes_preserve_java_range_and_negation_rules() {
        let cases = [
            (r"(?iu)[i]+", "iIİı", Some(0..6)),
            (r"(?iu)[^i]+", "iIİı-x", Some(6..8)),
            (r"(?iu)[K]", "k", Some(0..1)),
            (r"(?iu)[K-K]", "k", None),
            (r"(?iu)[K-K]", "K", Some(0..3)),
            (r"(?iu)[ß]", "ẞ", None),
            (r"(?iu)[ß-ß]", "ẞ", Some(0..3)),
            (r"(?iu)[ς-ς]", "σ", None),
            (r"(?iu)[å-]+", "Å-", Some(0..4)),
            (r"(?iu)[-å]+", "-Å", Some(0..4)),
            (r"(?iu)[\w]+", "K-A", Some(4..5)),
            (r"(?iu)[\W]+", "K-", Some(0..4)),
            (r"(?iu)[\p{Lu}]", "\u{345}", None),
            (r"(?iu)\p{Lu}", "\u{345}", None),
            (r"(?iu)[\x{1C89}]", "\u{1C8A}", Some(0..3)),
            (r"(?iu)[K](?=!)", "k!", Some(0..1)),
            (r"(?iu)[\Q-å\E]+", "Å-", Some(0..4)),
            (r"(?iu)[k-i]", "i", None),
        ];
        for (expression, input, expected) in cases {
            let pattern = Pattern::java(expression, false, 128 * 1024, 64);
            if expression == r"(?iu)[k-i]" {
                assert!(pattern.is_err());
            } else {
                assert_eq!(
                    pattern.unwrap().find_iter(input).next(),
                    expected,
                    "{expression}"
                );
            }
        }
    }

    #[test]
    fn java_pattern_negation_excludes_both_sides_of_the_surrogate_gap() {
        let input = "\u{d7ff}\u{e000}A";
        let cases = [
            (r"[^\p{Cn}\p{Co}]+", Some(6..7)),
            (r"[^\p{Co}\p{Cn}]+", Some(6..7)),
            (r"[^\x{d7ff}\x{e000}]+", Some(6..7)),
            (r"[^\x{d7ff}-\x{e000}]+", Some(6..7)),
            (r"(?iu)[^\p{Cn}[\p{Co}]]+", Some(6..7)),
            (r"(?i)[^\p{Cn}\p{Co}]+", Some(6..7)),
            (r"[^\p{IsUnknown}&&[\p{Cn}\p{Co}]]+", Some(6..7)),
            (r"[^[\p{Cn}\p{Co}]]+", Some(6..7)),
            (r"\P{sc=Unknown}+", Some(6..7)),
            (r"\P{C}+", Some(6..7)),
            (r"\P{gc=C}+", Some(6..7)),
            (r"(?=.)[^\p{Cn}\p{Co}]", Some(6..7)),
            (r"(?=.)\P{C}", Some(6..7)),
            (r"(?=.)\P{IsUnknown}", Some(6..7)),
            (r"\P{InPrivate_Use_Area}+", Some(0..3)),
            (r"[^\p{InSurrogates_Area}]+", Some(0..7)),
            (r"\P{IsUnknown}", Some(6..7)),
            (r"\p{Cs}", None),
            (r"\P{gc=Cs}+", Some(0..7)),
        ];
        for (expression, expected) in cases {
            let pattern = Pattern::java(expression, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{expression}: {error}"));
            assert_eq!(pattern.find_iter(input).next(), expected, "{expression}");
        }
    }

    #[test]
    fn java_pattern_distinguishes_unicode_scripts_and_blocks() {
        let cases = [
            (r"\p{IsGreek}+", "ϢἀΩ", Some(2..7)),
            (r"\p{InGreek}+", "ϢἀΩ", Some(0..2)),
            (r"\p{InGreek and Coptic}+", "ϢἀΩ", Some(0..2)),
            (r"\p{sc=Grek}+", "ϢἀΩ", Some(2..7)),
            (r"\p{SCRIPT=old_italic}", "x𐌀", Some(1..5)),
            (r"\p{IsAdlm}", "A𞤀", Some(1..5)),
            (r"\p{InLatin-1 Supplement}+", "Añé", Some(1..5)),
            (r"\p{blk=Latin_1_Supplement}+", "Añé", Some(1..5)),
            (r"\p{block=CyrillicSupplementary}", "AԀ", Some(1..3)),
            (
                r"\p{InCombining Marks for Symbols}",
                "A\u{20DD}",
                Some(1..4),
            ),
            (r"\p{InCJKUnifiedIdeographsExtensionB}", "A𠀀", Some(1..5)),
            (r"\p{IsUnknown}+", "A\u{378}\u{E000}", Some(1..6)),
            (r"\P{sc=Zzzz}+", "\u{E000}Az", Some(3..5)),
            (r"\p{InHigh_Surrogates}", "A𠀀", None),
            (r"\p{InSurrogates_Area}", "A𠀀", None),
            (r"\P{InSurrogates_Area}+", "Aé\n", Some(0..4)),
            (r"(?iu)\p{sc=Greek}", "\u{345}Ω", Some(2..4)),
            (r"[\p{InGreek}&&\p{IsGreek}]+", "ϢἀΩ", Some(5..7)),
            (r"(?<=!)\P{InBasicLatin}", "!Ω", Some(1..3)),
            (r"(?<=!)\p{IsGrek}", "!Ω", Some(1..3)),
        ];
        for (expression, input, expected) in cases {
            let pattern = Pattern::java(expression, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{expression}: {error}"));
            assert_eq!(pattern.find_iter(input).next(), expected, "{expression}");
        }
        for unsupported in [
            r"\p{IsOldItalic}",
            r"\p{sc=Qaac}",
            r"\p{sc=Hrkt}",
            r"\p{InGreek_And_Coptic}",
            r"\p{blk=Unassigned}",
        ] {
            assert!(Pattern::java(unsupported, false, 128 * 1024, 64).is_err());
        }
    }

    #[test]
    fn java_pattern_distinguishes_ascii_posix_unicode_posix_and_binary_properties() {
        let cases = [
            (r"\p{Lower}+", "éabc3", Some(2..5)),
            (r"(?iu)\p{Lower}+", "KAz", Some(3..5)),
            (r"(?U)\p{Lower}+", "50ªé", Some(2..6)),
            (r"(?iU)\p{uPPer}+", "ªAǅ3", Some(0..5)),
            (r"(?U)\p{ASCII}+", "éA", Some(2..3)),
            (r"(?U)\p{ALNUM}+", "€ª٣", Some(3..7)),
            (r"(?U)\p{gc=Lower}+", "éaz", Some(2..4)),
            (r"\p{Isalnum}+", "€ª٣", Some(3..7)),
            (r"\p{Punct}+", "$!", Some(0..2)),
            (r"(?U)\p{Punct}+", "$!", Some(1..2)),
            (r"(?U)\p{Graph}+", "\u{A0}\u{200C}A ", Some(2..6)),
            (r"(?U)\p{Print}+", "\t\u{A0}A", Some(1..4)),
            (r"\p{Blank}", "\u{A0}\t", Some(2..3)),
            (r"(?U)\p{XDigit}+", "x٣Ｆ", Some(1..6)),
            (r"\p{IsHex_Digit}", "x٣", Some(1..3)),
            (r"\p{IsWord}+", "\u{200C}\u{300}A-", Some(0..6)),
            (r"\p{IsEmoji}+", "a1🙂", Some(1..6)),
            (r"\p{IsExtended_Pictographic}", "1🙂", Some(1..5)),
            (r"\p{IsNoncharacter_Code_Point}", "a\u{FFFF}", Some(1..4)),
            (r"(?<=!)\P{IsWhite_Space}", "!a", Some(1..2)),
            (r"(?U)[\p{Graph}&&[^\p{Alpha}]]+", "é٣!", Some(2..5)),
            (r"\p{all}", "\n", Some(0..1)),
            (r"\p{LC}+", "ªAzǅ", Some(2..6)),
            (r"\p{LD}+", "€A٣", Some(3..6)),
            (r"\p{L1}+", "€éA", Some(3..6)),
        ];
        for (expression, input, expected) in cases {
            let pattern = Pattern::java(expression, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{expression}: {error}"));
            assert_eq!(pattern.find_iter(input).next(), expected, "{expression}");
        }
        assert!(Pattern::java(r"\p{ALNUM}", false, 128 * 1024, 64).is_err());
    }

    #[test]
    fn java_pattern_supports_character_properties_in_both_matchers() {
        let cases = [
            (r"\p{javaLowerCase}+", "Aªé", Some(1..5)),
            (r"\p{javaUpperCase}+", "éⅠA", Some(2..6)),
            (r"\p{javaTitleCase}", "Ǆǅ", Some(2..4)),
            (r"(?i)\p{javaLowerCase}+", "ªAǅ", Some(0..5)),
            (r"(?iu)\P{javaUpperCase}+", "Aa-1", Some(2..4)),
            (r"\p{javaAlphabetic}", "\u{345}", Some(0..2)),
            (r"\p{javaIdeographic}", "A東京", Some(1..4)),
            (r"\p{javaDigit}", "x٣", Some(1..3)),
            (r"\p{javaDefined}+", "\u{378}A", Some(2..3)),
            (r"\p{javaLetter}", "\u{345}日", Some(2..5)),
            (r"\p{javaLetterOrDigit}+", "§A٣", Some(2..5)),
            (r"\p{javaJavaIdentifierStart}+", "3$Ⅳ_", Some(1..6)),
            (
                r"\p{javaJavaIdentifierPart}+",
                "$٣\u{200C}\u{7F}",
                Some(0..7),
            ),
            (
                r"\p{javaUnicodeIdentifierStart}+",
                "$℘A\u{2E2F}",
                Some(1..8),
            ),
            (
                r"\p{javaUnicodeIdentifierPart}+",
                "$٣\u{200C}\u{2E2F}",
                Some(1..9),
            ),
            (r"\p{javaIdentifierIgnorable}", "\n\u{200C}", Some(1..4)),
            (r"\p{javaSpaceChar}+", "x\u{A0} ", Some(1..4)),
            (
                r"\p{javaWhitespace}+",
                "\u{A0}\u{2007}\u{202F}\u{1F} ",
                Some(8..10),
            ),
            (r"\p{javaISOControl}+", "a\u{9F}\u{1F}", Some(1..4)),
            (r"\p{javaMirrored}+", "A(<", Some(1..3)),
            (r"[\p{javaLetter}&&[^\p{javaUpperCase}]]+", "Aé", Some(1..3)),
            (r"(?<=!)\p{javaWhitespace}", "!\u{1C}", Some(1..2)),
            (r"(?<=!)\P{javaWhitespace}", "!\u{A0}", Some(1..3)),
        ];
        for (expression, input, expected) in cases {
            let pattern = Pattern::java(expression, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{expression}: {error}"));
            assert_eq!(pattern.find_iter(input).next(), expected, "{expression}");
        }
        assert!(Pattern::java(r"\p{javaUnknownProperty}", false, 128 * 1024, 64).is_err());
    }

    #[test]
    fn java_pattern_nested_classes_support_unions_and_intersections() {
        let cases = [
            (r"[a-d[m-p]]+", "zadmq", Some(1..4)),
            (r"[a-z&&[^aeiou]&&[^x-z]]+", "abcz xy", Some(1..3)),
            (r"[a[b]&&[bc]c]", "acb", Some(2..3)),
            (r"[[a]&&[b]]", "ab", None),
            (r"[^a&&b]+", "a\nb", Some(0..3)),
            (r"(?i)[a&&[^A]]", "aA", None),
            (r"(?iu)[[K]&&[a-z]]+", "kKK!", Some(0..5)),
            (r"(?iu)[[K-K]&&[a-z]]", "kKK", Some(2..5)),
            (r"[\p{L}&&[^\p{Lu}]]+", "Aéδ1", Some(1..5)),
            (r"(?U)[\w&&[^\d]]+", "٣δ", Some(2..4)),
            (r"[]a]+", "x]a", Some(1..3)),
            (r"[^]a]+", "]ax", Some(2..3)),
            (r"[a-[b]]+", "a-bc", Some(0..3)),
            (r"[--a]", "^", Some(0..1)),
            (r"[!-&&a]+", "!&a", Some(0..3)),
            (r"[x\&\&y]+", "&y", Some(0..2)),
            (r"[~~]+", "~~", Some(0..2)),
            (r"(?x)[ ^a]", "^", Some(0..1)),
            (r"(?x)[ a-c & & [ b ] ]", "abc", Some(1..2)),
            (r"[[a-c]&&[^a]](?=!)", "bc!", Some(1..2)),
            (r"[\Q[&]\E]+", "[&]", Some(0..3)),
        ];
        for (expression, input, expected) in cases {
            let pattern = Pattern::java(expression, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{expression}: {error}"));
            assert_eq!(pattern.find_iter(input).next(), expected, "{expression}");
        }
        for invalid in ["[a--b]", "[&&]", "[[a]", "[b-a]"] {
            assert!(
                Pattern::java(invalid, false, 128 * 1024, 64).is_err(),
                "{invalid}"
            );
        }
        let too_deep = format!("{}a{}", "[".repeat(65), "]".repeat(65));
        assert!(Pattern::java(&too_deep, false, 128 * 1024, 64).is_err());
    }

    #[test]
    fn java_pattern_preserves_intersection_operand_boundaries() {
        for (expression, expected) in [
            (r"[&&a]", "a"),
            (r"[a&&]", "a"),
            (r"[[ab]&&&&]", "ab"),
            (r"[a-z&&b&&]", "b"),
            (r"[a&&b&&]", ""),
            (r"[a&&&&b]", ""),
            (r"[a&&[ab]&c]", "&ac"),
            (r"[[ab]&&[ab]&[ac]]", "&abc"),
            (r"[a&&b&c]", ""),
            (r"[a&&&bc]", "&abc"),
            (r"(?x)[a & & [ab] & c]", "ac"),
            (r"[\v-\r]", "\x0b\x0c\r"),
            (r"[\x{0}-\v]", "\x0b"),
            (r"[\Q\E\Q\E^a]", "&-^]bcxyz\x0b\x0c\r"),
            (r"[a-\Q\E]", "-a"),
            (r"[a-\Q\E[b]]", "-ab"),
            (r"[\Q^\Ea]", "^a"),
            (r"(?x)[\Q\E ^a]", "^a"),
            (r"[a&&[ab]&\Q\Ec]", "&ac"),
            (r"(?x)[a&&[ab]&\Q\E c]", "ac"),
            (r"[\v\Q\E-\r]", "\x0b\x0c\r"),
        ] {
            let expression = format!(r"\A(?:{expression})\z");
            let pattern = Pattern::java(&expression, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{expression}: {error}"));
            let actual: String = "&-^]abcxyz\x0b\x0c\r"
                .chars()
                .filter(|ch| pattern.is_match(ch.encode_utf8(&mut [0; 4])))
                .collect();
            assert_eq!(actual, expected, "{expression}");
        }
        for invalid in [
            r"[&&]",
            r"[&&&&a]",
            r"[a&&[]]",
            r"[a-\&\&b]",
            r"(?x)[a-\Q\E ]]",
        ] {
            assert!(
                Pattern::java(invalid, false, 128 * 1024, 64).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn java_pattern_x_mode_preserves_quoted_and_escaped_text() {
        let ranges = |source: &str, text: &str| {
            Pattern::java(source, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{source:?}: {error}"))
                .find_iter(text)
                .collect::<Vec<_>>()
        };
        assert_eq!(ranges("(?x)[a # note\n b]", "ab #"), [0..1, 1..2]);
        assert_eq!(ranges(r"(?x)[a\ b\#]", "a b#"), [0..1, 1..2, 2..3, 3..4]);
        assert_eq!(ranges("(?x)\\Q a# \n b\\E c", " a# \n bc"), [0..8]);
        assert_eq!(ranges(r"\p\QL\E+", "1Ab"), [1..3]);
        assert_eq!(ranges(r"(?\Q\Ei)A", "a"), [0..1]);
        assert_eq!(ranges(r"(?<n>a)\k<\Qn\E>", "aa"), [0..2]);
        assert_eq!(ranges(r"\01\Q2\E", "\x012"), [0..2]);
        assert_eq!(ranges(r"\\Q\Q?\\E", r"\Q?\"), [0..4]);
        assert_eq!(ranges("(?x)a# note\u{85}b", "a\u{85}b ab"), [0..4]);
        assert_eq!(ranges("(?x)a# note\u{2028}b", "a\u{2028}b ab"), [0..5]);
        assert_eq!(ranges("(?xd)a# note\rb", "ab a\rb"), [0..1, 3..4]);
        assert_eq!(ranges("(?x)(? -x: b # ) c", " b # c"), [0..6]);
        assert_eq!(ranges("(?x : a ( ? # note\n :b))", "ab"), [0..2]);
        assert_eq!(ranges("(?x)(?i : a)", "A"), [0..1]);
        assert!(ranges("(?x)a * + a", "aaa").is_empty());
        assert_eq!(ranges("(?x)a{2 # note\n , 3}", "aaa"), [0..3]);
        assert_eq!(ranges("(?x)a{1 2}", "aaaaaaaaaaaa"), [0..12]);
        assert_eq!(ranges(r"(?x)\x { 41 }\u 0 0 4 2\p { Lu}", "ABC"), [0..3]);
        assert_eq!(ranges(r"(?x)(?< a b >a)\k< a b >", "aa"), [0..2]);
        assert_eq!(
            ranges(
                r"(?x)(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)(k)(l)\1 2",
                "abcdefghijkll"
            ),
            [0..13]
        );
        for pattern in [
            "(?x)a{ 2 }",
            "(?x)(? <g>a)",
            "(?x)(? -x :a)",
            r"(?x)\p{Lu }",
        ] {
            assert!(
                Pattern::java(pattern, false, 128 * 1024, 64).is_err(),
                "{pattern}"
            );
        }
    }

    #[test]
    fn java_pattern_exact_escapes_and_previous_match_anchor() {
        let ranges = |source: &str, input: &str| {
            Pattern::java(source, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{source}: {error}"))
                .find_iter(input)
                .collect::<Vec<_>>()
        };
        for (source, input, range) in [
            (r"\00", "a\0", 1..2),
            (r"\07", "a\x07", 1..2),
            (r"\010", "a\x08", 1..2),
            (r"\0377", "xÿ", 1..3),
            (r"\0477", "x'7", 1..3),
            (r"\01418", "xa8", 1..3),
            (r"(?i)\0141", "xA", 1..2),
            (r"(?iu)\0377", "xŸ", 1..3),
            ("(?x)\\0 1#comment\n 4 1", "xa", 1..2),
            (r"[\0141-\0143]", "xdcb", 2..3),
            (r"\cA+", "x\u{1}\u{1}", 1..3),
            (r"\c?", "x\u{7f}", 1..2),
            (r"\cÄ", "x\u{84}", 1..3),
            (r"\c🙂", "x😂", 1..5),
            (r"(?x)\c a", "x!", 1..2),
            (r"\uD801\uDC00", "x𐐀", 1..5),
            (r"(?x)\uD801 \uDC00", "x𐐀", 1..5),
            (r"(?iu)\uD801\uDC00", "x𐐨", 1..5),
            (r"[\uD801\uDC00-\uD801\uDC03]", "x𐐁", 1..5),
            (r"\pL+", "3éA", 1..4),
            (r"\PL+", "é3!", 2..4),
            (r"[\pL]", "3é", 1..3),
        ] {
            assert_eq!(ranges(source, input).first(), Some(&range), "{source}");
        }
        for invalid in [r"\0", r"\08", r"\c", r"\p", r"\pZz{"] {
            assert!(
                Pattern::java(invalid, false, 128 * 1024, 64).is_err(),
                "{invalid}"
            );
        }
        for (source, input, expected) in [
            (r"\G\d+:", "12:3:x45:", vec![0..3, 3..5]),
            (r"\G[a-z]+|\G", "ab!", vec![0..2, 2..2]),
            (r"\G(?=a)|b", "ab", vec![0..0, 1..2]),
            (r"a|\G(?=z)", "az", vec![0..1, 1..1]),
            (r"\G|a", "aa", vec![0..0, 1..2, 2..2]),
            (r"a\G", "a", vec![]),
            (
                r"\G\d|x",
                "x12-x34",
                vec![0..1, 1..2, 2..3, 4..5, 5..6, 6..7],
            ),
            (r"^|\G.", "ab", vec![0..0]),
            (r"\G", "", vec![0..0]),
            (r"(?<=\Gx)y|x", "xy", vec![0..1]),
            (r"(?<=\G.)a", "ba", vec![1..2]),
        ] {
            assert_eq!(ranges(source, input), expected, "{source}");
        }
        let re = fancy_regex::Regex::new(r"\G|a").unwrap();
        let default = re
            .find_iter("aa")
            .map(|m| m.unwrap().range())
            .collect::<Vec<_>>();
        assert_eq!(default, [0..0, 1..2]);
        let java = fancy_regex::RegexBuilder::new(r"\G|a")
            .kortty_java(true)
            .build()
            .unwrap();
        let captures = java
            .captures_iter("aa")
            .map(|captures| captures.unwrap().get(0).unwrap().range())
            .collect::<Vec<_>>();
        assert_eq!(captures, [0..0, 1..2, 2..2]);
    }

    #[test]
    fn java_pattern_named_character_escapes() {
        for (source, input, expected) in [
            (r"\N{ latin capital letter a }", "xA", 1..2),
            (r"(?i)\N{LATIN CAPITAL LETTER A}", "xa", 1..2),
            (r"(?iu)\N{KELVIN SIGN}k", "xKK", 1..5),
            (r"\N{NULL}\N{LINE FEED (LF)}", "x\0\n", 1..3),
            (r"\N{BEL}\N{BELL}", "x\x07🔔", 1..6),
            (r"\N{latin 1 supplement 84}", "x\u{84}", 1..3),
            (r"\N{HANGUL SYLLABLES AC00}", "x가", 1..4),
            (r"\N{CJK UNIFIED IDEOGRAPHS 4e00}", "x一", 1..4),
            (
                r"\N{SUPPLEMENTARY PRIVATE USE AREA B 100000}",
                "x\u{100000}",
                1..5,
            ),
            (
                r"[\N{LATIN CAPITAL LETTER A}-\N{LATIN CAPITAL LETTER C}]",
                "xB",
                1..2,
            ),
            (r"\N{FULL STOP}(?=!)", "x.!", 1..2),
            (r"\N{LATIN CAPITAL LETTER ſ}", "xS", 1..2),
            ("(?x)\\N # comment\n { LATIN CAPITAL LETTER A }", "xA", 1..2),
        ] {
            let pattern = Pattern::java(source, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            assert_eq!(pattern.find_iter(input).next(), Some(expected), "{source}");
        }
        for invalid in [
            r"\N",
            r"\N{",
            r"\N{}",
            r"\N{BASIC LATIN 41}",
            r"\N{HANGUL SYLLABLES 0AC00}",
            r"\N{HANGUL SYLLABLES +AC00}",
            r"\N{HANGUL SYLLABLES D7A4}",
            r"\N{HANGUL SYLLABLE GA}",
            r"\N{ LATIN CAPITAL LETTER A}",
            "(?x)\\N{LATIN #comment\nCAPITAL LETTER A}",
        ] {
            assert!(
                Pattern::java(invalid, false, 128 * 1024, 64).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn java_pattern_surrogate_literals_use_utf16_positions_and_utf8_display_ranges() {
        let text = "😀x";
        for (source, expected) in [
            (r"\uD83D", vec![]),
            (r"\uDE00", vec![]),
            (r"\uD83D\uDE00", vec![0..4]),
            (r"\x{D83D}\x{DE00}", vec![]),
            (r"(?<=\uDE00)x", vec![4..5]),
            (r"(?<=\uD83D)x", vec![]),
            (r"(?<!\uDE00)x", vec![]),
            (r"(?<=\N{LOW SURROGATES DE00})x", vec![4..5]),
            (r"(?<=😀)x|\uDE00", vec![4..5]),
            (r"(?<=\x{1f600})x|\uDE00", vec![]),
            (r"\N{HIGH SURROGATES D800}", vec![]),
            (r"\uD83D*", vec![0..0, 0..0, 4..4, 5..5]),
            (r"\uDE00*", vec![0..0, 0..4, 4..4, 5..5]),
            (r"\uDE00|x", vec![4..5]),
            (r"(?!\uDE00)", vec![0..0, 4..4, 5..5]),
        ] {
            let pattern = Pattern::java(source, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            let actual = pattern.find_iter(text).collect::<Vec<_>>();
            assert_eq!(actual, expected, "{source}");
            assert!(actual.iter().all(|range| text.get(range.clone()).is_some()));
        }
        let captures = fancy_regex::RegexBuilder::new(r"(\yde00*)")
            .kortty_java(true)
            .build()
            .unwrap();
        let found = captures.captures_iter(text).nth(1).unwrap().unwrap();
        assert_eq!(found.get(1).unwrap().as_str(), "😀");
    }

    #[test]
    fn java_pattern_backreferences_keep_partial_utf16_captures() {
        for (source, text, expected) in [
            (
                r"(?:(?=(\uDE00))\1|(?=))",
                "😀x",
                vec![0..0, 0..4, 4..4, 5..5],
            ),
            (
                r"(?:(?=(?<s>\uDE00))(?i:\k<s>)|(?=))",
                "😀x",
                vec![0..0, 0..4, 4..4, 5..5],
            ),
            (
                r"(?:(?=(?<s>\uDE00))(?iu:\k<s>)|(?=))",
                "😀x",
                vec![0..0, 0..4, 4..4, 5..5],
            ),
            (r"(\x{1f600})\1|\uDE00", "😀😀!", vec![0..8]),
            (r"(?<d>\x{10400})(?iu:\k<d>)|\uDE00", "𐐀𐐨", vec![0..8]),
            (r"(?<d>\x{10400})(?i:\k<d>)|\uDE00", "𐐀𐐨", vec![]),
            (r"(?<s>A)(?i:\k<s>)|\uDE00", "Aa", vec![0..2]),
            (r"(?<s>A)\k<s>|\uDE00", "Aa", vec![]),
            (r"(?:(\uDE00)|x)\1", "xx", vec![]),
            (r"(\uDE00?)\1", "😀", vec![0..0, 0..0, 4..4]),
        ] {
            let pattern = Pattern::java(source, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            assert_eq!(
                pattern.find_iter(text).collect::<Vec<_>>(),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn java_pattern_classes_preserve_surrogate_membership() {
        let text = "😀x";
        for (source, expected) in [
            (r"[\uDE00]", vec![]),
            (r"[\uDE00]*", vec![0..0, 0..4, 4..4, 5..5]),
            (r"(?<=[^\uD83D])x", vec![4..5]),
            (r"(?<=[^\uDE00])x", vec![]),
            (r"(?<=[\uD7FF-\uE000])x", vec![4..5]),
            (r"(?<=[[\uDC00-\uDFFF]&&[^\uDE00]])x", vec![]),
            (r"(?<=\p{Cs})x", vec![4..5]),
            (r"(?<=\p{InLowSurrogates})x", vec![4..5]),
            (r"(?<=\p{InHighSurrogates})x", vec![]),
            (r"(?<=\p{C})x|\uD83D", vec![4..5]),
            (r"(?<=\p{IsAssigned})x|\uD83D", vec![4..5]),
            (r"(?<=\p{sc=Unknown})x|\uD83D", vec![4..5]),
            (r"(?<=\p{InSurrogates_Area})x|\uD83D", vec![]),
            (r"(?<=.)x|\uD83D", vec![4..5]),
            (r"(?<=\W)x|\uD83D", vec![4..5]),
            (r"(?<=\w)x|\uD83D", vec![]),
        ] {
            let pattern = Pattern::java(source, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            assert_eq!(
                pattern.find_iter(text).collect::<Vec<_>>(),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn java_linear_patterns_keep_empty_matches_and_check_low_surrogates() {
        for (source, text, expected) in [
            (r"(?:x|)", "xx", vec![0..1, 1..2, 2..2]),
            (r"(?:\p{C}|)", "😀x", vec![0..0, 0..4, 4..4, 5..5]),
            (r"(?:x|)", "😀x", vec![0..0, 0..0, 4..5, 5..5]),
        ] {
            let pattern = Pattern::java(source, false, 128 * 1024, 64).unwrap();
            assert!(matches!(pattern, Pattern::JavaLinear { .. }));
            assert_eq!(
                pattern.find_iter(text).collect::<Vec<_>>(),
                expected,
                "{source}"
            );
        }
        let pattern = Pattern::java(r"(?:\p{C}|)", false, 128 * 1024, 64).unwrap();
        let text = "😀".repeat(5000);
        assert_eq!(pattern.find_iter(&text).count(), 10_001);
    }

    #[test]
    fn java_word_boundaries_and_scanning_distinguish_utf16_positions() {
        for (source, text, expected) in [
            (r"\B(?=)", "x😀x", vec![1..1]),
            (r"(?U)\B(?=)", "𐐀x", vec![0..0, 4..4]),
            (r"\b(?=)", "a\u{0301}", vec![0..0, 3..3]),
            (r"\b(?=)", "_\u{0301}", vec![0..0, 1..1]),
            (r"(?=.)(?<!.)", "é😀𐐀x", vec![0..0, 2..2, 6..6]),
            (r"(?=[^\n])(?<!.)", "é😀𐐀x", vec![0..0]),
        ] {
            let pattern = Pattern::java(source, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            assert_eq!(
                pattern.find_iter(text).collect::<Vec<_>>(),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn java_inline_canonical_equivalence_matches_grapheme_prefixes() {
        for (source, text, expected) in [
            (r"(?c)[é]", "e\u{0301}", vec![0..3]),
            (r"(?c)é", "e\u{0301}", vec![]),
            (r"(?c:[é])(?-c:[é])", "e\u{0301}é", vec![0..5]),
            (r"(?c)\p{L}", "\u{1100}\u{1161}\u{11a8}", vec![0..9]),
            (r"(?c)[ÅǺ]\u0301", "A\u{030a}\u{0301}", vec![0..5]),
            (r"(?c)[ÅǺ]{1}\u0301", "A\u{030a}\u{0301}", vec![]),
            (r"(?c)(?:[ÅǺ])?\u0301", "A\u{030a}\u{0301}", vec![0..5]),
            (r"(?c)(?:\p{L}){2}+", "\u{1100}\u{1161}\u{11a8}", vec![]),
            (r"(?<=(?c:[é]))x", "e\u{0301}x", vec![]),
        ] {
            let pattern = Pattern::java(source, false, 128 * 1024, 64).unwrap();
            assert_eq!(
                pattern.find_iter(text).collect::<Vec<_>>(),
                expected,
                "{source}"
            );
        }
        let pattern = Pattern::java(r"(?c)[Å]", false, 128 * 1024, 64).unwrap();
        let text = format!("A\u{030a}{}", "\u{0301}".repeat(512));
        assert_eq!(pattern.find_iter(&text).next(), Some(0..3));
    }

    #[test]
    fn java_line_ending_retries_cr_only_for_a_standalone_atom() {
        for (source, text, expected) in [
            (r"\R\n", "\r\n", vec![0..2]),
            (r"(?=\R\n)", "\r\n", vec![0..0]),
            (r"(?<=\R)\n", "\r\n", vec![1..2]),
            (r"(?<!\R)\n", "\r\n", vec![]),
            (r"\R{2}", "\r\n", vec![]),
            (r"\R{2}", "\r\n\n", vec![0..3]),
            (r"\R+\n", "\r\n", vec![]),
            (r"\R*?\n", "\r\n", vec![1..2]),
            (r"(?>\R)\n", "\r\n", vec![]),
            (r"(\R)\n", "\r\n", vec![0..2]),
            (r"(\R)?\n", "\r\n", vec![0..2]),
            (r"(\R){1}\n", "\r\n", vec![]),
            (r"(?:\R\n){1}", "\r\n", vec![0..2]),
            (r"(?:\R|\n){2}+", "\r\n", vec![]),
        ] {
            let pattern = Pattern::java(source, false, 128 * 1024, 64).unwrap();
            assert_eq!(
                pattern.find_iter(text).collect::<Vec<_>>(),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn java_regular_word_boundaries_keep_exact_positions_on_long_input() {
        for (source, text, expected) in [
            (r"\B", "x😀x", vec![1..1]),
            (r"(?U)\B", "𐐀", vec![0..0]),
            (r"\b", "a\u{0301}", vec![0..0, 3..3]),
            (r"\b", "ä\u{0301}", vec![2..2, 4..4]),
        ] {
            let pattern = Pattern::java(source, false, 128 * 1024, 64).unwrap();
            assert!(matches!(pattern, Pattern::JavaLinear { .. }));
            assert_eq!(pattern.find_iter(text).collect::<Vec<_>>(), expected);
        }
        let pattern = Pattern::java(r"\btarget\b", false, 128 * 1024, 64).unwrap();
        assert!(matches!(
            pattern.find_iter("a target"),
            Matches::JavaLinear(_)
        ));
        let prefix = format!("😀{} ", "x".repeat(32_768));
        let input = format!("{prefix}{}", "target ".repeat(200));
        let mut matches = pattern.find_iter(&input);
        assert_eq!(matches.next(), Some(prefix.len()..prefix.len() + 6));
        assert_eq!(matches.count(), 199);
    }

    #[test]
    fn java_escaped_supplementary_literal_runs_keep_code_unit_scanning() {
        for (source, expected) in [
            (r"(?:\x{1f600}a|\B(?=))", vec![1..1]),
            (r"(?:\uD83D\uDE00z|\B(?=))", vec![1..1]),
            (r"(?i)(?:\x{1f600}a|\B(?=))", vec![1..1]),
            (r"(?iu)(?:\x{1f600}a|\B(?=))", vec![1..1]),
            (r"(?:\x{1f600}|\B(?=))", vec![1..5]),
        ] {
            let pattern = Pattern::java(source, false, 128 * 1024, 64).unwrap();
            assert_eq!(
                pattern.find_iter("x😀x").collect::<Vec<_>>(),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn java_advanced_patterns_use_utf16_without_explicit_surrogates() {
        for (source, text, expected) in [
            (r"(?<=\p{C})x", "😀x", vec![4..5]),
            (r"(?<!\p{C})x", "😀x", vec![]),
            (r"(?<=\P{L})x", "𐐀x", vec![4..5]),
            (r"(?<=\p{sc=Unknown}+)x", "😀😀x", vec![8..9]),
            (r"(?:\p{C}|(?=))", "😀x", vec![0..0, 0..4, 4..4, 5..5]),
            (r"(?=)", "😀x", vec![0..0, 0..0, 4..4, 5..5]),
        ] {
            let pattern = Pattern::java(source, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            assert_eq!(
                pattern.find_iter(text).collect::<Vec<_>>(),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn java_pattern_checks_variable_lookbehind_at_its_original_endpoint() {
        for (source, text, expected) in [
            (r"(?<=\uDE00a{1,3})x", "😀aax", Some(6..7)),
            (r"(?<=\uDE00a{1,3})x", "😀aaaax", None),
            (r"(?<!\uDE00a{1,3})x", "😀aaaax", Some(8..9)),
            (r"(?<=\uDE00a*)x", "😀x", Some(4..5)),
            (r"(?<=\uDE00.*)x", "😀aaxz", Some(6..7)),
            (r"(?<=\uDE00(?>a*))x", "😀aax", Some(6..7)),
            (r"(?<=\uDE00(?:ab*){0,1})x", "😀abx", None),
            (r"(?<=\uDE00(?:\R){1})x", "😀\r\nx", Some(6..7)),
            (r"(?<=\uDE00\R)\n", "😀\r\n", Some(5..6)),
            (r"(?<=\uDE00(?:a|bc))x", "😀bcx", Some(6..7)),
            (r"(?<=\p{Cs}\w+)x", "😀aax", Some(6..7)),
            (r"(?<=\uDE00\X)x", "😀ax", None),
            (r"(?<=(?<g>[a\uDE00]{1,3}))x\k<g>", "😀aaxaa", Some(6..8)),
            (r"(?<=(?<g>[a\uDE00]{1,3}))x\k<g>\z", "😀aaxaa", None),
        ] {
            let pattern = Pattern::java(source, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            assert_eq!(pattern.find_iter(text).next(), expected, "{source}");
        }
        for source in [
            r"(?<=\uDE00(?:a|bc){1,2})x",
            r"(?<=\uDE00(?:a|bc){1})x",
            r"(?<=\uDE00(?:\X){1})x",
            r"(?<=\uDE00(?:a?){1,2})x",
            r"(?<=\uDE00a*?)x",
            r"(?<=\uDE00(?:ab)*)x",
            r"(?<=\uDE00(a)\1)x",
        ] {
            assert!(
                Pattern::java(source, false, 128 * 1024, 64).is_err(),
                "{source}"
            );
        }
    }

    #[test]
    fn java_pattern_graphemes_are_atomic_and_boundaries_follow_previous_matches() {
        let ranges = |source: &str, text: &str| {
            Pattern::java(source, false, 128 * 1024, 64)
                .unwrap_or_else(|error| panic!("{source}: {error}"))
                .find_iter(text)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ranges(r"\X", "e\u{301}👩‍🔧🇩🇪\r\n"),
            [0..3, 3..14, 14..22, 22..24]
        );
        assert_eq!(ranges(r"^\X$", "\u{1100}\u{1161}\u{11a8}"), [0..9]);
        assert_eq!(ranges(r"^\X$", "क्ष"), [0..9]);
        assert_eq!(ranges(r"\X", "\u{378}\u{300}"), [0..4]);
        assert_eq!(ranges(r"\X", "\u{379}\u{300}"), [0..2, 2..4]);
        assert_eq!(ranges(r"\X", "\u{600}👩‍👩"), [0..9, 9..13]);
        assert!(ranges(r"\X\u0301", "e\u{301}").is_empty());
        assert_eq!(ranges(r"\b{g}", "Ae\u{301}Z"), [0..0, 1..1, 4..4, 5..5]);
        assert_eq!(ranges(r"\b{g}.", "Ae\u{301}Z"), [0..1, 4..5]);
        assert_eq!(ranges(r"(?x)\b {g } \X", "e\u{301}Z"), [0..3]);
        assert_eq!(ranges(r"\b{g}", ""), [0..0]);
    }
}
