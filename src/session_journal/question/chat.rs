//! Search evidence from linked chat messages stored inside the reviewed journal XML.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io;

use roxmltree::{Document, Node};
use zeroize::Zeroizing;

use super::{
    Candidate, MAX_EVIDENCE_BYTES, SensitiveBlocks, Target, check_cancel, clip, keep, score_terms,
};
use crate::session_journal::{
    Journal,
    agent_chat::{MAX_MESSAGE_TEXT, MAX_SNAPSHOT_TEXT, valid_reference},
    bounded_field, display_text, entries as xml_entries, invalid, scalar_text,
};

const MAX_MESSAGES: usize = 128;
const MAX_JOURNAL_MESSAGES: usize = 512;
const MAX_CANDIDATES: usize = 3;

pub(super) fn entries(
    document: &Document<'_>,
    journal: &Journal,
    terms: &[String],
    mask: &impl Fn(&str) -> io::Result<String>,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<(Vec<Candidate>, bool)> {
    let linked = journal
        .timeline
        .iter()
        .filter(|entry| entry.has_agent_chat() && valid_reference(&entry.id));
    let mut nodes: HashMap<_, _> = linked
        .clone()
        .map(|entry| (entry.id.as_str(), (0_usize, None)))
        .collect();
    let mut partial = false;
    let mut best = Vec::new();
    let mut recent = None;
    let mut remaining = MAX_JOURNAL_MESSAGES;
    if nodes.is_empty() {
        return Ok((best, partial));
    }
    for node in xml_entries(document) {
        check_cancel(cancelled)?;
        let Ok(id) = bounded_field(node, "id") else {
            partial = true;
            continue;
        };
        if let Some((count, matching)) = nodes.get_mut(id.as_ref()) {
            *count += 1;
            *matching = Some(node);
        }
    }
    for entry in linked {
        check_cancel(cancelled)?;
        let Some((1, Some(node))) = nodes.remove(entry.id.as_str()) else {
            partial = true;
            continue;
        };
        let Ok(messages) = messages(node) else {
            partial = true;
            continue;
        };
        if messages.len() > remaining {
            partial = true;
            break;
        }
        remaining -= messages.len();
        if messages.is_empty() {
            continue;
        }
        let title = mask(&entry.title)?;
        let title = format!("Linked chat · {}", clip(&title, 104));
        let mut sensitive = SensitiveBlocks::default();
        for message in messages {
            check_cancel(cancelled)?;
            let text = Zeroizing::new(display_text(&message));
            if sensitive.excludes(&text) || text.trim().is_empty() {
                continue;
            }
            let masked = mask(&text)?;
            let excerpt = clip(masked.trim(), MAX_EVIDENCE_BYTES);
            if excerpt.is_empty() {
                continue;
            }
            let score = score_terms(excerpt, terms);
            let candidate = Candidate {
                score,
                title: title.clone(),
                excerpt: excerpt.to_owned(),
                target: Target::ChatEntry(entry.id.clone()),
            };
            if score == 0 {
                recent = Some(candidate);
            } else if !best.iter().any(|previous: &Candidate| {
                matches!(&previous.target, Target::ChatEntry(id) if id == &entry.id)
                    && previous.excerpt == candidate.excerpt
            }) {
                keep(&mut best, candidate);
                best.truncate(MAX_CANDIDATES);
            }
        }
    }
    if best.is_empty() {
        best.extend(recent);
    }
    Ok((best, partial))
}

fn messages<'a>(entry: Node<'a, '_>) -> io::Result<Vec<Cow<'a, str>>> {
    if entry.tag_name().namespace().is_some() {
        return Err(invalid("Invalid linked journal chat entry"));
    }
    let Some(snapshot) = one_child(entry, "chatSnapshot")? else {
        // A link without a recorded snapshot does not point to evidence in this document.
        return Ok(Vec::new());
    };
    let Some(messages) = one_child(snapshot, "messages")? else {
        return Ok(Vec::new());
    };
    let mut result = Vec::new();
    let mut bytes = 0;
    for message in messages
        .children()
        .filter(|node| node.has_tag_name("message"))
    {
        if result.len() == MAX_MESSAGES || message.tag_name().namespace().is_some() {
            return Err(invalid("Linked journal chat exceeds its message limit"));
        }
        let content = one_child(message, "content")?
            .map(scalar_text)
            .transpose()?
            .unwrap_or_default();
        bytes += content.len();
        if content.len() > MAX_MESSAGE_TEXT || bytes > MAX_SNAPSHOT_TEXT {
            return Err(invalid("Linked journal chat exceeds its text limit"));
        }
        result.push(content);
    }
    Ok(result)
}

fn one_child<'a, 'input>(
    parent: Node<'a, 'input>,
    name: &str,
) -> io::Result<Option<Node<'a, 'input>>> {
    let mut fields = parent.children().filter(|node| node.has_tag_name(name));
    let field = fields.next();
    if fields.next().is_some() || field.is_some_and(|node| node.tag_name().namespace().is_some()) {
        return Err(invalid("Invalid linked journal chat field"));
    }
    Ok(field)
}
