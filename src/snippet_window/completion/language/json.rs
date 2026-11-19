use std::collections::HashSet;
use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::snippets::json_schema::{Schema, Segment};

use super::Suggestions;

#[derive(Clone, Copy)]
enum Place {
    Key,
    Value,
    Separator,
}

enum Container {
    Object {
        keys: HashSet<String>,
        current: Option<String>,
    },
    Array(usize),
}

impl Container {
    fn segment(&self) -> Option<Segment<'_>> {
        match self {
            Self::Object { current, .. } => current.as_deref().map(Segment::Property),
            Self::Array(index) => Some(Segment::Item(*index)),
        }
    }
}

pub(super) fn quoted_token(line: &str, column: usize) -> Option<Range<usize>> {
    let mut quote = None;
    let mut escaped = false;
    for (index, byte) in line.bytes().enumerate() {
        if index == column && quote.is_none() {
            return None;
        }
        if escaped {
            escaped = false;
        } else if quote.is_some() && byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            if let Some(start) = quote.take() {
                if index >= column {
                    return Some(start..index);
                }
            } else {
                quote = Some(index + 1);
            }
        }
    }
    Some(quote?..line.len())
}

pub(super) fn suggest(
    source: &str,
    lead: &str,
    tail: &str,
    schema: Option<&Schema>,
) -> Suggestions {
    let mut stack = Vec::new();
    let mut place = Place::Value;
    let mut quote = None;
    let mut escaped = false;
    for (index, byte) in lead.bytes().enumerate() {
        if let Some(start) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                if matches!(place, Place::Key)
                    && let Some(Container::Object { keys, current }) = stack.last_mut()
                    && let Ok(key) = serde_json::from_str::<String>(&lead[start..=index])
                {
                    keys.insert(key.clone());
                    *current = Some(key);
                }
                quote = None;
                place = Place::Separator;
            }
            continue;
        }
        match byte {
            b'"' => quote = Some(index),
            b'{' => {
                stack.push(Container::Object {
                    keys: HashSet::new(),
                    current: None,
                });
                place = Place::Key;
            }
            b'[' => {
                stack.push(Container::Array(0));
                place = Place::Value;
            }
            b'}' | b']' => {
                stack.pop();
                place = Place::Separator;
            }
            b':' => place = Place::Value,
            b',' => match stack.last_mut() {
                Some(Container::Object { current, .. }) => {
                    *current = None;
                    place = Place::Key;
                }
                array => {
                    if let Some(Container::Array(index)) = array {
                        *index += 1;
                    }
                    place = Place::Value;
                }
            },
            byte if !byte.is_ascii_whitespace() => place = Place::Separator,
            _ => {}
        }
    }
    if let Some((schema, path)) = schema.and_then(|schema| {
        let len = if matches!(place, Place::Key) {
            stack.len().checked_sub(1)?
        } else {
            stack.len()
        };
        if len > 128 {
            return None;
        }
        let path: Vec<_> = stack[..len]
            .iter()
            .map(Container::segment)
            .collect::<Option<_>>()?;
        Some((schema, path))
    }) {
        return schema.with_at(&path, |schemas| {
            choices(source, lead, tail, (place, quote), &stack, schemas)
        });
    }
    choices(source, lead, tail, (place, quote), &stack, &[])
}

fn choices(
    source: &str,
    lead: &str,
    tail: &str,
    (place, quote): (Place, Option<usize>),
    stack: &[Container],
    schemas: &[&Value],
) -> Suggestions {
    let mut result = Suggestions::default();
    match (place, quote) {
        (Place::Key, Some(start)) if start + 1 != lead.len() => {}
        (Place::Key, _) => {
            static KEY: LazyLock<Regex> =
                LazyLock::new(|| Regex::new(r#""([\p{L}_][\p{L}\p{N}_-]{0,95})"\s*:"#).unwrap());
            let properties = schemas
                .iter()
                .filter_map(|schema| schema.get("properties").and_then(Value::as_object))
                .flat_map(|properties| properties.keys().map(String::as_str));
            let observed = KEY
                .captures_iter(source)
                .filter_map(|found| found.get(1).map(|name| name.as_str()));
            let mut seen = HashSet::new();
            for name in properties.chain(observed) {
                let present = matches!(stack.last(), Some(Container::Object { keys, .. }) if keys.contains(name));
                if !present && seen.insert(name) {
                    let escaped = serde_json::to_string(name).expect("JSON string serialization");
                    let inner = &escaped[1..escaped.len() - 1];
                    let key = string_literal(inner, quote.is_some(), tail);
                    let colon = if tail.trim_start().starts_with(['"', ':']) {
                        ""
                    } else {
                        ": "
                    };
                    result.add(inner, &format!("{key}{colon}"));
                    if seen.len() == 200 {
                        break;
                    }
                }
            }
            if quote.is_none() && !tail.trim_start().starts_with(['"', ':']) {
                result.template("property", "\"{{key}}\": {{null}}");
            }
        }
        (Place::Value, _) => {
            let mut has_values = false;
            for schema in schemas {
                if result.items.len() >= 200 {
                    break;
                }
                let choices = schema
                    .get("enum")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .chain(schema.get("const"))
                    .chain(schema.get("default"))
                    .chain(
                        schema
                            .get("examples")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten(),
                    );
                for value in choices.take(200 - result.items.len()) {
                    let text = value.to_string();
                    if text.len() > 4096 || (quote.is_some() && !value.is_string()) {
                        continue;
                    }
                    has_values = true;
                    if value.is_string() {
                        let inner = &text[1..text.len() - 1];
                        result.add(inner, &string_literal(inner, quote.is_some(), tail));
                    } else {
                        result.add(&text, &text);
                    }
                }
                if let Some(kind) = schema.get("type").and_then(Value::as_str)
                    && quote.is_none()
                {
                    let words: &[&str] = match kind {
                        "boolean" => &["false", "true"],
                        "null" => &["null"],
                        "object" => &["{}"],
                        "array" => &["[]"],
                        _ => &[],
                    };
                    has_values |= !words.is_empty();
                    result.words(words);
                }
            }
            if !has_values && quote.is_none() {
                result.words(&["false", "null", "true"]);
                result.add("array", "[]");
                result.add("object", "{}");
            }
        }
        _ => {}
    }
    result
}

fn string_literal(inner: &str, opening_in_source: bool, tail: &str) -> String {
    let open = if opening_in_source { "" } else { "\"" };
    let close = if tail.starts_with('"') { "" } else { "\"" };
    format!("{open}{inner}{close}")
}
