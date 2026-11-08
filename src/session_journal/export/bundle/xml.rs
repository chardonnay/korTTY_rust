//! Patches only known journal export fields and leaves unrelated XML untouched.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::io;
use std::ops::Range;

use roxmltree::{Document, Node};

use super::super::super::MAX_DOCUMENT_BYTES;
use super::super::{Entry, Options, normalize_marker};
use super::{BundleStats, bounded_field, child, invalid, parse_document};

const CHAT_FIELDS: [&str; 3] = ["chatId", "chatLinkedAt", "chatSnapshot"];

pub(super) fn rewrite<'a>(
    source: &'a str,
    document: &Document<'_>,
    nodes: &[Node<'_, '_>],
    selected: &[&Entry],
    options: &Options,
    stats: &BundleStats,
) -> io::Result<Cow<'a, str>> {
    let filtered = options.filter.is_active();
    if !filtered && options.include_chats {
        return Ok(Cow::Borrowed(source));
    }
    let mut changes = Vec::new();
    let mut used_markers = HashSet::new();
    let kept = filtered.then(|| {
        selected
            .iter()
            .map(|entry| entry.xml_index)
            .collect::<HashSet<_>>()
    });
    for (index, node) in nodes.iter().copied().enumerate() {
        if kept.as_ref().is_some_and(|kept| !kept.contains(&index)) {
            changes.push(Edit::remove(node));
            continue;
        }
        if filtered && let Some(id) = normalize_marker(&bounded_field(node, "markerId")?) {
            used_markers.insert(id);
        }
        if !options.include_chats {
            changes.extend(
                node.children()
                    .filter(|field| CHAT_FIELDS.iter().any(|name| field.has_tag_name(*name)))
                    .map(Edit::remove),
            );
        }
    }
    if filtered {
        let root = document.root_element();
        if let Some(definitions) = child(root, "markerDefinitions") {
            for marker in definitions
                .children()
                .filter(|node| node.has_tag_name("marker"))
            {
                let id = normalize_marker(&bounded_field(marker, "id")?);
                if id.as_ref().is_none_or(|id| !used_markers.contains(id)) {
                    changes.push(Edit::remove(marker));
                }
            }
        }
        let meta =
            child(root, "meta").ok_or_else(|| invalid("Missing session journal metadata"))?;
        recount(source, meta, stats, &mut changes)?;
    }
    if changes.is_empty() {
        return Ok(Cow::Borrowed(source));
    }
    let rewritten = apply(source, changes)?;
    if rewritten.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(invalid(
            "The filtered journal XML exceeds the document size limit",
        ));
    }
    parse_document(&rewritten)?;
    Ok(Cow::Owned(rewritten))
}

struct Edit {
    range: Range<usize>,
    text: String,
}

impl Edit {
    fn remove(node: Node<'_, '_>) -> Self {
        Self {
            range: node.range(),
            text: String::new(),
        }
    }

    fn content(source: &str, node: Node<'_, '_>, text: String, append: bool) -> io::Result<Self> {
        let range = node.range();
        let element = &source[range.clone()];
        if element.ends_with("/>") {
            let name = element[1..]
                .split([' ', '\t', '\n', '\r', '/', '>'])
                .next()
                .ok_or_else(|| invalid("Invalid journal XML element name"))?;
            return Ok(Self {
                range: range.end - 2..range.end,
                text: format!(">{text}</{name}>"),
            });
        }
        let end = range.start
            + element
                .rfind("</")
                .ok_or_else(|| invalid("Invalid journal XML closing tag"))?;
        let start = if append {
            end
        } else {
            range.start
                + opening_end(element).ok_or_else(|| invalid("Invalid journal XML opening tag"))?
                + 1
        };
        Ok(Self {
            range: start..end,
            text,
        })
    }
}

fn recount(
    source: &str,
    meta: Node<'_, '_>,
    stats: &BundleStats,
    edits: &mut Vec<Edit>,
) -> io::Result<()> {
    let mut missing = String::new();
    for (name, count) in [
        ("commandCount", stats.command_count),
        ("errorCount", stats.error_count),
        ("screenshotCount", stats.screenshot_count),
    ] {
        // Check duplicate or non-scalar fields before editing the original element.
        bounded_field(meta, name)?;
        if let Some(field) = child(meta, name) {
            edits.push(Edit::content(source, field, count.to_string(), false)?);
        } else {
            write!(missing, "<{name}>{count}</{name}>").expect("writing to a String");
        }
    }
    if !missing.is_empty() {
        edits.push(Edit::content(source, meta, missing, true)?);
    }
    Ok(())
}

fn opening_end(element: &str) -> Option<usize> {
    let mut quote = None;
    for (index, byte) in element.bytes().enumerate() {
        match (quote, byte) {
            (Some(delimiter), value) if delimiter == value => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => return Some(index),
            _ => {}
        }
    }
    None
}

fn apply(source: &str, mut edits: Vec<Edit>) -> io::Result<String> {
    edits.sort_unstable_by_key(|edit| edit.range.start);
    let mut output = String::with_capacity(source.len());
    let mut start = 0;
    for edit in edits {
        let before = source
            .get(start..edit.range.start)
            .ok_or_else(|| invalid("Overlapping journal XML export edits"))?;
        output.push_str(before);
        output.push_str(&edit.text);
        start = edit.range.end;
    }
    output.push_str(
        source
            .get(start..)
            .ok_or_else(|| invalid("Invalid journal XML export range"))?,
    );
    Ok(output)
}
