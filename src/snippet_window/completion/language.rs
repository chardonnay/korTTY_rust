//! Local language suggestions for the native completion list.

use super::{Candidate, literal, template};
use crate::snippets::json_schema::Schema;

mod json;
mod markup;
mod script;

#[derive(Default)]
pub(super) struct Suggestions {
    pub(super) generic: bool,
    pub(super) items: Vec<(String, Candidate)>,
}

impl Suggestions {
    fn words(&mut self, words: &[&str]) {
        for &word in words {
            self.add(word, word);
        }
    }

    fn add(&mut self, name: &str, text: &str) {
        self.items
            .push((name.trim_start_matches('$').into(), literal(text)));
    }

    fn template(&mut self, name: &str, text: &str) {
        self.items.push((name.into(), template(text)));
    }
}

pub(super) fn suggestions(
    language: &str,
    source: &str,
    lead: &str,
    tail: &str,
    schema: Option<&Schema>,
) -> Option<Suggestions> {
    Some(match language {
        "javascript" | "typescript" => script::suggest(lead, language == "typescript"),
        "json" => json::suggest(source, lead, tail, schema),
        "css" => markup::css(source, lead),
        "html" => markup::html(lead, tail),
        _ => return None,
    })
}

pub(super) fn quoted_token(
    language: &str,
    line: &str,
    column: usize,
) -> Option<std::ops::Range<usize>> {
    (language == "json")
        .then(|| json::quoted_token(line, column))
        .flatten()
}

fn identifier(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '$')
}

/// Finds the active quote or comment at the end of a script or style prefix.
fn in_code(lead: &str, script: bool) -> bool {
    let mut bytes = lead.bytes().peekable();
    while let Some(byte) = bytes.next() {
        match byte {
            b'/' if bytes.next_if_eq(&b'*').is_some() => loop {
                match bytes.next() {
                    Some(b'*') if bytes.next_if_eq(&b'/').is_some() => break,
                    Some(_) => {}
                    None => return false,
                }
            },
            b'/' if script && bytes.next_if_eq(&b'/').is_some() => {
                if !bytes.any(|byte| byte == b'\n') {
                    return false;
                }
            }
            quote if matches!(quote, b'\'' | b'"') || (script && quote == b'`') => loop {
                match bytes.next() {
                    Some(b'\\') => {
                        bytes.next();
                    }
                    Some(byte) if byte == quote => break,
                    Some(_) => {}
                    None => return false,
                }
            },
            _ => {}
        }
    }
    true
}
