//! Masked Agent cards and Java-compatible chat snapshots.

use std::{fmt::Write as _, io};

use jiff::Timestamp;
use zeroize::Zeroizing;

use super::{
    MAX_DOCUMENT_BYTES, bounded_field, child, document_id, entry_edit, invalid,
    metadata::xml_character, number, parse_document, timeline_details, viewer::Source,
};

const CHAT_FIELDS: [&str; 3] = ["chatId", "chatLinkedAt", "chatSnapshot"];
pub(crate) const MAX_SNAPSHOT_TEXT: usize = 128 * 1024;
pub(crate) const MAX_MESSAGE_TEXT: usize = 24 * 1024;

pub(crate) fn valid_reference(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn text(value: &str, limit: usize) -> bool {
    value.len() <= limit && value.chars().all(xml_character)
}

#[derive(Clone)]
pub(crate) struct Card {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) answer: String,
    pub(crate) model: String,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) tokens: Option<u64>,
}

impl Card {
    pub(crate) fn validate(&self) -> io::Result<()> {
        if !valid_reference(&self.id)
            || self.title.trim().is_empty()
            || !text(&self.title, 560)
            || !text(&self.answer, MAX_MESSAGE_TEXT)
            || !text(&self.model, 1024)
            || self
                .duration_ms
                .is_some_and(|value| i64::try_from(value).is_err())
            || self
                .tokens
                .is_some_and(|value| i64::try_from(value).is_err())
        {
            return Err(invalid("The Agent journal card exceeds its limits"));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct Snapshot {
    pub(crate) chat_id: String,
    pub(crate) title: String,
    pub(crate) captured_at: Timestamp,
    pub(crate) frozen: bool,
    pub(crate) messages: Vec<Message>,
}

#[derive(Clone)]
pub(crate) struct Message {
    pub(crate) role: Role,
    pub(crate) content: String,
    pub(crate) profile: String,
    pub(crate) steps: Vec<Step>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Role {
    User,
    Assistant,
}

#[derive(Clone)]
pub(crate) struct Step {
    pub(crate) kind: &'static str,
    pub(crate) status: Option<&'static str>,
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) command: Option<String>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) output: String,
    pub(crate) elapsed_seconds: Option<u64>,
    pub(crate) truncated: Option<bool>,
}

impl Snapshot {
    pub(crate) fn validate(&self) -> io::Result<()> {
        if !valid_reference(&self.chat_id)
            || self.title.trim().is_empty()
            || !text(&self.title, 560)
            || self.messages.is_empty()
            || self.messages.len() > 128
        {
            return Err(invalid("The Agent chat snapshot exceeds its limits"));
        }
        let mut bytes = self.title.len();
        for message in &self.messages {
            if message.content.trim().is_empty()
                || !text(&message.content, MAX_MESSAGE_TEXT)
                || !text(&message.profile, 1024)
                || message.steps.len() > 24
                || (message.role == Role::User && !message.steps.is_empty())
            {
                return Err(invalid(
                    "The Agent chat snapshot contains an invalid message",
                ));
            }
            bytes += message.content.len() + message.profile.len();
            for step in &message.steps {
                let command = step.command.as_deref().unwrap_or("");
                if !matches!(step.kind, "ACTION" | "MESSAGE" | "QUESTION" | "ERROR")
                    || step.status.is_some_and(|status| {
                        !matches!(status, "RUNNING" | "COMPLETED" | "CANCELLED" | "FAILED")
                    })
                    || !text(&step.title, 1024)
                    || !text(&step.summary, 4096)
                    || !text(command, 4096)
                    || !text(&step.output, MAX_MESSAGE_TEXT)
                    || step
                        .elapsed_seconds
                        .is_some_and(|value| i64::try_from(value).is_err())
                {
                    return Err(invalid("The Agent chat snapshot contains an invalid step"));
                }
                bytes += step.title.len() + step.summary.len() + command.len() + step.output.len();
            }
            if bytes > MAX_SNAPSHOT_TEXT {
                return Err(invalid("The Agent chat snapshot exceeds its text limit"));
            }
        }
        Ok(())
    }

    pub(crate) fn write_xml(&self, xml: &mut String, linked_at: Timestamp) {
        element(xml, "chatId", &self.chat_id);
        element(xml, "chatLinkedAt", &linked_at.to_string());
        xml.push_str("<chatSnapshot>\n");
        element(xml, "title", &self.title);
        element(xml, "capturedAt", &self.captured_at.to_string());
        element(xml, "frozen", if self.frozen { "true" } else { "false" });
        xml.push_str("<messages>\n");
        for message in &self.messages {
            xml.push_str("<message>\n");
            element(
                xml,
                "role",
                match message.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                },
            );
            element(xml, "content", &message.content);
            if !message.profile.is_empty() {
                element(xml, "aiProfileName", &message.profile);
            }
            if !message.steps.is_empty() {
                xml.push_str("<steps>\n");
                for step in &message.steps {
                    step.write_xml(xml);
                }
                xml.push_str("</steps>\n");
            }
            xml.push_str("</message>\n");
        }
        xml.push_str("</messages>\n</chatSnapshot>\n");
    }
}

impl Step {
    fn write_xml(&self, xml: &mut String) {
        xml.push_str("<step>\n");
        element(xml, "type", self.kind);
        if let Some(status) = self.status {
            element(xml, "status", status);
        }
        element(xml, "title", &self.title);
        element(xml, "summary", &self.summary);
        if let Some(command) = &self.command {
            element(xml, "command", command);
        }
        if let Some(code) = self.exit_code {
            element(xml, "exitCode", &code.to_string());
        }
        if !self.output.is_empty() {
            element(xml, "output", &self.output);
        }
        if let Some(elapsed) = self.elapsed_seconds {
            element(xml, "elapsedSeconds", &elapsed.to_string());
        }
        if let Some(truncated) = self.truncated {
            element(xml, "truncated", if truncated { "true" } else { "false" });
        }
        xml.push_str("</step>\n");
    }
}

pub(crate) fn verify_card(source: &Source, id: &str) -> io::Result<()> {
    source.with_document(|_, document| {
        let entry = entry_edit::find_entry(document, id)?;
        require_agent(entry)
    })
}

pub(crate) struct LinkedChat {
    pub(crate) chat_id: Option<String>,
    pub(crate) transcript: Option<Transcript>,
}

#[derive(Clone)]
pub(crate) struct Transcript {
    pub(crate) title: String,
    pub(crate) text: String,
}

/// Reads only the selected card and checks its identity against the current journal.
pub(crate) fn read_linked_chat(source: &Source, id: &str) -> io::Result<LinkedChat> {
    let read = || {
        source.with_document(|_, document| {
            let entry = entry_edit::find_entry(document, id)?;
            require_agent(entry)?;
            let chat_id = bounded_field(entry, "chatId")?;
            let chat_id = valid_reference(&chat_id).then(|| chat_id.into_owned());
            let transcript = child(entry, "chatSnapshot").and_then(|snapshot| {
                let rows = timeline_details::chat_rows(snapshot, &jiff::tz::TimeZone::system());
                if rows.values.is_empty() {
                    return None;
                }
                let title = bounded_field(snapshot, "title").unwrap_or_default();
                let title: String = title
                    .chars()
                    .filter(|&ch| !ch.is_control() && super::is_display_character(ch))
                    .take(140)
                    .collect();
                Some(Transcript {
                    title: title.trim().to_owned(),
                    text: rows.values.join("\n\n"),
                })
            });
            Ok(LinkedChat {
                chat_id,
                transcript,
            })
        })
    };
    match read() {
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => read(),
        result => result,
    }
}

pub(crate) fn link_closed(
    source: &Source,
    id: &str,
    mut snapshot: Snapshot,
    check: impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    snapshot.validate()?;
    snapshot.frozen = true;
    let lock = entry_edit::lock_source(source)?;
    let (stamp, xml) = lock.read_document()?;
    let document = parse_document(&xml)?;
    if document_id(&document)? != source.id()
        || child(document.root_element(), "meta")
            .and_then(|meta| child(meta, "endedAt"))
            .is_none()
    {
        return Err(invalid("The selected closed journal changed"));
    }
    let entry = entry_edit::find_entry(&document, id)?;
    require_agent(entry)?;
    let previous = bounded_field(entry, "chatId")?;
    if !previous.is_empty() && previous != snapshot.chat_id {
        return Err(invalid("The Agent card is linked to another chat"));
    }
    if previous == snapshot.chat_id && child(entry, "chatSnapshot").is_some() {
        // A stopped journal keeps the copy it already froze.
        return lock.publish_document(&stamp, None, |directory| {
            source.require_location(source.directory(), directory)?;
            check()
        });
    }
    let linked_at = number::<Timestamp>(entry, "chatLinkedAt")?.unwrap_or(snapshot.captured_at);
    let end = xml[..entry.range().end]
        .rfind("</")
        .ok_or_else(|| invalid("Invalid Agent journal card"))?;
    let mut ranges: Vec<_> = entry
        .children()
        .filter(|node| node.is_element() && CHAT_FIELDS.contains(&node.tag_name().name()))
        .map(|node| node.range())
        .collect();
    ranges.sort_unstable_by_key(|range| range.start);
    let mut updated = Zeroizing::new(String::with_capacity(xml.len()));
    let mut offset = 0;
    for range in ranges {
        updated.push_str(&xml[offset..range.start]);
        offset = range.end;
    }
    updated.push_str(&xml[offset..end]);
    snapshot.write_xml(&mut updated, linked_at);
    updated.push_str(&xml[end..]);
    if updated.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(invalid("Session journal document exceeds its size limit"));
    }
    lock.publish_document(&stamp, Some(&updated), |directory| {
        source.require_location(source.directory(), directory)?;
        check()
    })
}

fn require_agent(entry: roxmltree::Node<'_, '_>) -> io::Result<()> {
    entry_edit::require_java_fields(entry, &["kind", "chatId", "chatLinkedAt", "chatSnapshot"])?;
    if bounded_field(entry, "kind")? != "AGENT" {
        return Err(invalid("The selected journal entry is not an Agent card"));
    }
    if CHAT_FIELDS.iter().any(|name| {
        entry
            .children()
            .filter(|node| node.has_tag_name(*name))
            .take(2)
            .count()
            > 1
    }) {
        return Err(invalid("The Agent journal chat fields are ambiguous"));
    }
    Ok(())
}

fn element(xml: &mut String, name: &str, value: &str) {
    let _ = write!(xml, "<{name}>");
    for character in value.chars() {
        match character {
            '&' => xml.push_str("&amp;"),
            '<' => xml.push_str("&lt;"),
            '>' => xml.push_str("&gt;"),
            '\r' => xml.push_str("&#13;"),
            _ => xml.push(character),
        }
    }
    let _ = writeln!(xml, "</{name}>");
}
