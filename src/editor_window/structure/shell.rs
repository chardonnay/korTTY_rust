//! Bash declarations and here-document boundaries for the structure scanner.

use std::collections::HashMap;

use super::{identifier, leading_keyword};

pub(super) struct Binding<'a> {
    pub(super) name: &'a str,
    pub(super) kind: &'static str,
    pub(super) container: Option<&'static str>,
    pub(super) scope: Scope,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Scope {
    Default,
    Local,
    Global,
}

pub(super) fn bindings<'a>(text: &'a str, mut emit: impl FnMut(Binding<'a>)) {
    let text = text.trim_start();
    if let Some((keyword, rest)) =
        leading_keyword(text, &["declare", "local", "typeset", "readonly", "export"])
    {
        declared(keyword, rest, emit);
        return;
    }
    if let Some((_, rest)) = leading_keyword(text, &["mapfile", "readarray"]) {
        if let Some(name) = mapfile_binding(rest) {
            emit(default_binding(name, "array"));
        }
        return;
    }
    let command = leading_keyword(text, &["while"]).map_or(text, |(_, tail)| tail);
    let after_ifs = if command.starts_with("IFS=") {
        command_words(command).next().map_or(command, |assignment| {
            command[assignment.len()..].trim_start()
        })
    } else {
        command
    };
    if let Some((_, rest)) = leading_keyword(after_ifs, &["read"]) {
        read_bindings(rest, emit);
        return;
    }
    if let Some((_, rest)) = leading_keyword(command, &["getopts"]) {
        if let Some(name) = command_words(rest).nth(1).and_then(bare_name) {
            emit(default_binding(name, "variable"));
        }
        return;
    }
    if let Some(name) = loop_binding(text) {
        emit(default_binding(name, "variable"));
    } else if let Some((name, kind)) = assignment(text) {
        emit(default_binding(name, kind));
    }
}

fn default_binding<'a>(name: &'a str, kind: &'static str) -> Binding<'a> {
    Binding {
        name,
        kind,
        container: None,
        scope: Scope::Default,
    }
}

fn declared<'a>(keyword: &str, text: &'a str, mut emit: impl FnMut(Binding<'a>)) {
    let mut flags = [false; 128];
    let mut parsing_flags = true;
    for word in command_words(text) {
        if parsing_flags && word == "--" {
            parsing_flags = false;
            continue;
        }
        if parsing_flags && let Some(options) = word.strip_prefix('-') {
            for option in options.bytes().filter(u8::is_ascii) {
                flags[usize::from(option)] = true;
            }
            continue;
        }
        let flag = |option: u8| flags[usize::from(option)];
        if flag(b'f') || flag(b'F') {
            continue;
        }
        let Some((name, shape)) =
            assignment(word).or_else(|| bare_name(word).map(|name| (name, "variable")))
        else {
            continue;
        };
        let shape = if flag(b'A') {
            "hash"
        } else if flag(b'a') {
            "array"
        } else {
            shape
        };
        let kind = if keyword == "readonly" || flag(b'r') {
            "constant"
        } else if keyword == "export" || flag(b'x') {
            "exported"
        } else {
            shape
        };
        let scope = if keyword == "local" || matches!(keyword, "declare" | "typeset") && !flag(b'g')
        {
            Scope::Local
        } else {
            Scope::Global
        };
        emit(Binding {
            name,
            kind,
            container: (shape != kind && shape != "variable").then_some(shape),
            scope,
        });
    }
}

fn read_bindings<'a>(text: &'a str, mut emit: impl FnMut(Binding<'a>)) {
    let mut words = command_words(text);
    while let Some(word) = words.next() {
        if word == "--" {
            for name in words.filter_map(bare_name) {
                emit(default_binding(name, "variable"));
            }
            break;
        }
        match word.strip_prefix('-').and_then(|flag| flag.bytes().last()) {
            Some(b'a') => {
                if let Some(name) = words.next().and_then(bare_name) {
                    emit(default_binding(name, "array"));
                }
            }
            Some(option) if b"pdnNtui".contains(&option) => {
                words.next();
            }
            Some(_) => {}
            None => {
                if let Some(name) = bare_name(word) {
                    emit(default_binding(name, "variable"));
                }
            }
        }
    }
}

fn mapfile_binding(text: &str) -> Option<&str> {
    let mut words = command_words(text);
    while let Some(word) = words.next() {
        if word == "--" {
            return words.next().and_then(bare_name);
        }
        let Some(flags) = word.strip_prefix('-') else {
            return bare_name(word);
        };
        if flags
            .bytes()
            .last()
            .is_some_and(|flag| b"dnOsuCc".contains(&flag))
        {
            words.next();
        }
    }
    None
}

fn loop_binding(text: &str) -> Option<&str> {
    if let Some(arithmetic) = text
        .strip_prefix("for")
        .and_then(|rest| rest.trim_start().strip_prefix("(("))
    {
        let (name, tail) = name_prefix(arithmetic.trim_start())?;
        return tail.trim_start().starts_with('=').then_some(name);
    }
    let (_, rest) = leading_keyword(text, &["for", "select"])?;
    let mut words = command_words(rest);
    let name = bare_name(words.next()?)?;
    (words.next()? == "in").then_some(name)
}

pub(super) fn function(text: &str) -> Option<&str> {
    let explicit = leading_keyword(text, &["function"]).map(|(_, name)| name);
    let text = explicit.unwrap_or(text);
    let (prefix, _) = name_prefix(text)?;
    let suffix = text[prefix.len()..]
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(byte))
        .count();
    let (name, tail) = text.split_at(prefix.len() + suffix);
    let tail = tail.trim_start();
    let body = match tail.strip_prefix('(') {
        Some(parameters) => parameters.trim_start().strip_prefix(')')?.trim_start(),
        None if explicit.is_some() => tail,
        None => return None,
    };
    (body.is_empty() || body.starts_with('{')).then_some(name)
}

fn assignment(text: &str) -> Option<(&str, &'static str)> {
    let (name, tail) = name_prefix(text)?;
    let (tail, indexed) = if let Some(index) = tail.strip_prefix('[') {
        (index.split_once(']')?.1, true)
    } else {
        (tail, false)
    };
    let value = tail.strip_prefix("+=").or_else(|| tail.strip_prefix('='))?;
    let kind = if indexed || value.starts_with('(') {
        "array"
    } else {
        "variable"
    };
    Some((name, kind))
}

fn name_prefix(text: &str) -> Option<(&str, &str)> {
    identifier(text).filter(|(name, _)| name.is_ascii())
}

fn bare_name(text: &str) -> Option<&str> {
    name_prefix(text).and_then(|(name, tail)| tail.is_empty().then_some(name))
}

/// Split an inspected command into words without expanding quotes or substitutions.
/// A compound assignment stays one word, including its value list.
fn command_words(mut text: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        text = text.trim_start_matches([' ', '\t']);
        if text.is_empty() || text.starts_with([';', '|', '&', '#', '<', '>']) {
            return None;
        }
        let mut quote = None;
        let mut depth = 0usize;
        let mut chars = text.char_indices();
        let mut end = text.len();
        while let Some((offset, ch)) = chars.next() {
            match (quote, ch) {
                (open, '\\') if open != Some('\'') => {
                    chars.next();
                }
                (Some(open), close) if open == close => quote = None,
                (None, '\'' | '"' | '`') => quote = Some(ch),
                (None, '(') => depth += 1,
                (None, ')') => depth = depth.saturating_sub(1),
                (None, ' ' | '\t' | ';' | '|' | '&' | '<' | '>') if depth == 0 => {
                    end = offset;
                    break;
                }
                _ => {}
            }
        }
        let (word, rest) = text.split_at(end);
        text = rest;
        Some(word)
    })
}

pub(super) struct Terminators<'a> {
    exact: HashMap<&'a str, usize>,
    tab_stripped: HashMap<&'a str, usize>,
}

impl<'a> Terminators<'a> {
    pub(super) fn new(lines: impl Iterator<Item = &'a str>) -> Self {
        let mut exact = HashMap::new();
        let mut tab_stripped = HashMap::new();
        for (position, line) in lines.enumerate() {
            exact.insert(line, position);
            if line.starts_with('\t') {
                tab_stripped.insert(line.trim_start_matches('\t'), position);
            }
        }
        Self {
            exact,
            tab_stripped,
        }
    }

    pub(super) fn last_after(&self, document: &HereDocument, intro: usize) -> Option<usize> {
        let delimiter = document.delimiter.as_str();
        let exact = self.exact.get(delimiter);
        let tabs = if document.strip_tabs {
            self.tab_stripped.get(delimiter)
        } else {
            None
        };
        exact
            .into_iter()
            .chain(tabs)
            .copied()
            .max()
            .filter(|&last| last > intro)
    }
}

pub(super) struct HereDocument {
    delimiter: String,
    strip_tabs: bool,
    expands: bool,
}

impl HereDocument {
    pub(super) fn parse(text: &str) -> Option<(Self, usize)> {
        let strip_tabs = text.starts_with('-');
        let word = text[usize::from(strip_tabs)..].trim_start_matches([' ', '\t']);
        if word.is_empty() || word.starts_with('#') {
            return None;
        }
        let mut delimiter = String::new();
        let mut quoted = false;
        let mut quote = None;
        let mut chars = word.char_indices().peekable();
        let mut end = word.len();
        while let Some((offset, character)) = chars.next() {
            match (quote, character) {
                (None, ' ' | '\t' | ';' | '|' | '&' | '<' | '>') => {
                    end = offset;
                    break;
                }
                (None, '(' | ')') => return None,
                (None, '\'' | '"') => {
                    quote = Some(character);
                    quoted = true;
                }
                (Some(open), close) if open == close => quote = None,
                (None, '\\') => {
                    delimiter.push(chars.next()?.1);
                    quoted = true;
                }
                (Some('"'), '\\')
                    if chars
                        .peek()
                        .is_some_and(|&(_, escaped)| matches!(escaped, '$' | '`' | '"' | '\\')) =>
                {
                    delimiter.push(chars.next()?.1);
                }
                _ => delimiter.push(character),
            }
        }
        if quote.is_some()
            || !quoted && !identifier(&delimiter).is_some_and(|(_, tail)| tail.is_empty())
        {
            return None;
        }
        Some((
            Self {
                delimiter,
                strip_tabs,
                expands: !quoted,
            },
            text.len() - word.len() + end,
        ))
    }

    pub(super) fn ends_at(&self, line: &str) -> bool {
        let candidate = if self.strip_tabs {
            line.trim_start_matches('\t')
        } else {
            line
        };
        candidate == self.delimiter
    }

    pub(super) fn mask_body(&self, line: &str, references: bool) -> String {
        if !references || !self.expands {
            return " ".repeat(line.len());
        }
        let mut bytes = line.as_bytes().to_vec();
        let mut offset = 0;
        while offset < bytes.len() {
            if bytes[offset] == b'\\' {
                match bytes.get(offset + 1) {
                    Some(b'$') => {
                        bytes[offset + 1] = b' ';
                        offset += 2;
                        continue;
                    }
                    Some(b'\\' | b'`') => {
                        offset += 2;
                        continue;
                    }
                    _ => {}
                }
            }
            offset += 1;
        }
        String::from_utf8(bytes).expect("masking an ASCII dollar preserves UTF-8")
    }
}
