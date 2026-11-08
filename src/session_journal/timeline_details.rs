//! Visible card fields shared by the native journal timeline and its text search.

use std::io;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use roxmltree::Node;

use super::metadata::java_whitespace;
use super::{boolean, bounded_field, child, display_text, number, scalar_text};

const MAX_MARKER_UNITS: usize = 256;
const MAX_EXTRA_UNITS: usize = 32 * 1024;
const MAX_EXTRA_ROWS: usize = 64;
const MAX_CHAT_UNITS: usize = 200_000;
const MAX_CHAT_ROWS: usize = 512;

pub(super) fn marker_name(name: &str, limited: &mut bool) -> String {
    let (text, clipped) = prefix(name, MAX_MARKER_UNITS);
    if clipped {
        *limited = true;
        format!("{}…", prefix(text, MAX_MARKER_UNITS - 1).0)
    } else {
        text.to_owned()
    }
}

pub(super) fn read(
    entry: Node<'_, '_>,
    kind: &str,
    has_screenshot: bool,
    zone: &TimeZone,
    limited: &mut bool,
) -> (Vec<String>, Vec<String>) {
    let mut extra = Rows::new(MAX_EXTRA_ROWS, MAX_EXTRA_UNITS);
    let mut chat = Rows::new(MAX_CHAT_ROWS - 1, MAX_CHAT_UNITS);
    if append_extra(entry, kind, has_screenshot, &mut extra).is_err() {
        extra.limited = true;
    }
    if kind == "AGENT"
        && let Some(snapshot) = child(entry, "chatSnapshot")
        && append_chat(snapshot, zone, &mut chat).is_err()
    {
        chat.limited = true;
    }
    if chat.limited && !chat.values.is_empty() {
        chat.values.push("Chat transcript shortened".to_owned());
    }
    *limited |= extra.limited || chat.limited;
    (extra.values, chat.values)
}

fn append_extra(
    entry: Node<'_, '_>,
    kind: &str,
    has_screenshot: bool,
    rows: &mut Rows,
) -> io::Result<()> {
    match bounded_field(entry, "state")?.as_ref() {
        "RAW" => rows.push("raw"),
        "FAILED" => rows.push("failed"),
        _ => {}
    }
    match kind {
        "SESSION_SUMMARY" => rows.push("session summary"),
        "AGENT" => rows.push("AI agent"),
        _ => {}
    }
    match bounded_field(entry, "commandState")?.as_ref() {
        "RUNNING" => rows.push("still running"),
        "ENDED_WITH_SESSION" => rows.push("session ended"),
        _ => {}
    }
    if kind == "AGENT"
        && (child(entry, "chatSnapshot").is_some()
            || !strip(&bounded_field(entry, "chatId")?).is_empty())
    {
        rows.push("AI chat");
    }
    if bounded_field(entry, "runAsUser")? == "root" {
        rows.push("ROOT");
    }
    if kind == "AGENT" {
        rows.push(&agent_meta(entry)?);
    }
    if kind == "SCREENSHOT" && has_screenshot {
        let description = bounded_field(entry, "aiDescription")?;
        let mut tags = child(entry, "aiTags")
            .into_iter()
            .flat_map(|tags| tags.children())
            .filter(|node| node.has_tag_name("tag"))
            .peekable();
        if !strip(&description).is_empty() || tags.peek().is_some() {
            rows.push("AI analysis");
            rows.push(&description);
        }
        for tag in tags {
            rows.push(&scalar_text(tag)?);
            if rows.limited {
                break;
            }
        }
    }
    Ok(())
}

fn agent_meta(entry: Node<'_, '_>) -> io::Result<String> {
    let model = bounded_field(entry, "agentModel")?;
    let mut parts = Vec::new();
    if !strip(&model).is_empty() {
        parts.push(strip(&model).to_owned());
    }
    if let Some(millis) = number::<i64>(entry, "agentDurationMillis")?.filter(|value| *value > 0) {
        let seconds = (millis.saturating_add(500) / 1000).max(1);
        parts.push(match seconds {
            3600.. => format!("{} h {} min", seconds / 3600, (seconds / 60) % 60),
            60.. => format!("{} min {} s", seconds / 60, seconds % 60),
            _ => format!("{seconds} s"),
        });
    }
    if let Some(tokens) = number::<i64>(entry, "agentTokens")?.filter(|value| *value > 0) {
        parts.push(if tokens < 1000 {
            format!("{tokens} tokens")
        } else {
            let tenths = (tokens as u64 + 50) / 100;
            format!("{}.{}k tokens", tenths / 10, tenths % 10)
        });
    }
    Ok(parts.join(" · "))
}

fn append_chat(snapshot: Node<'_, '_>, zone: &TimeZone, rows: &mut Rows) -> io::Result<()> {
    let Some(messages) = child(snapshot, "messages") else {
        return Ok(());
    };
    let messages = messages
        .children()
        .filter(|node| node.has_tag_name("message"));
    let count = messages.clone().count();
    if count == 0 {
        return Ok(());
    }
    let as_of = formatted_time(snapshot, "capturedAt", zone, "%F %H:%M")?;
    let as_of = as_of.as_deref().unwrap_or("?");
    let frozen = if boolean(snapshot, "frozen")? {
        " (final)"
    } else {
        ""
    };
    rows.push(&format!(
        "Show AI chat ({count} messages) · as of {as_of}{frozen}"
    ));
    for message in messages {
        if rows.limited {
            break;
        }
        append_message(message, zone, rows)?;
    }
    Ok(())
}

fn append_message(message: Node<'_, '_>, zone: &TimeZone, rows: &mut Rows) -> io::Result<()> {
    let profile = bounded_field(message, "aiProfileName")?;
    let label = if bounded_field(message, "role")? == "user" {
        "You"
    } else if strip(&profile).is_empty() {
        "AI:"
    } else {
        strip(&profile)
    };
    if let Some(time) = formatted_time(message, "createdAt", zone, "%H:%M")? {
        rows.push(&format!("{label} · {time}"));
    } else {
        rows.push(label);
    }
    rows.push(&bounded_field(message, "content")?);
    if rows.limited {
        return Ok(());
    }
    if let Some(steps) = child(message, "steps") {
        let steps = steps.children().filter(|node| node.has_tag_name("step"));
        let count = steps.clone().count();
        if count > 0 {
            rows.push(&format!("Steps ({count})"));
        }
        for step in steps {
            if rows.limited {
                break;
            }
            append_step(step, rows)?;
        }
    }
    Ok(())
}

fn append_step(step: Node<'_, '_>, rows: &mut Rows) -> io::Result<()> {
    let status = bounded_field(step, "status")?;
    let title = bounded_field(step, "title")?;
    let summary = bounded_field(step, "summary")?;
    let exit = number::<i32>(step, "exitCode")?.map(|code| format!("Exit {code}"));
    let title = [
        strip(&status),
        strip(&title),
        strip(&summary),
        exit.as_deref().unwrap_or(""),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    rows.push(&title);
    let command = bounded_field(step, "command")?;
    if !strip(&command).is_empty() {
        rows.push(&format!("$ {command}"));
    }
    rows.push(&bounded_field(step, "output")?);
    if boolean(step, "truncated")? {
        rows.push("Output truncated");
    }
    Ok(())
}

fn formatted_time(
    node: Node<'_, '_>,
    name: &str,
    zone: &TimeZone,
    format: &str,
) -> io::Result<Option<String>> {
    Ok(number::<Timestamp>(node, name)?
        .map(|time| time.to_zoned(zone.clone()).strftime(format).to_string()))
}

struct Rows {
    values: Vec<String>,
    max_rows: usize,
    remaining: usize,
    limited: bool,
}

impl Rows {
    fn new(max_rows: usize, remaining: usize) -> Self {
        Self {
            values: Vec::new(),
            max_rows,
            remaining,
            limited: false,
        }
    }

    fn push(&mut self, value: &str) {
        if self.limited || strip(value).is_empty() {
            return;
        }
        let clean = display_text(value);
        if strip(&clean).is_empty() {
            return;
        }
        if self.values.len() == self.max_rows || self.remaining == 0 {
            self.limited = true;
            return;
        }
        let (text, clipped) = prefix(&clean, self.remaining);
        if !text.is_empty() {
            self.remaining -= text.encode_utf16().count();
            self.values.push(text.to_owned());
        }
        self.limited |= clipped;
    }
}

fn prefix(value: &str, limit: usize) -> (&str, bool) {
    let mut remaining = limit;
    for (offset, character) in value.char_indices() {
        let Some(next) = remaining.checked_sub(character.len_utf16()) else {
            return (&value[..offset], true);
        };
        remaining = next;
    }
    (value, false)
}

fn strip(value: &str) -> &str {
    value.trim_matches(java_whitespace)
}

#[cfg(test)]
mod tests;
