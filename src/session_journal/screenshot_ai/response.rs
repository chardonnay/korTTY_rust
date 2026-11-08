use std::time::Instant;

use serde_json::Value;
use zeroize::Zeroizing;

use super::{Analysis, Context, MAX_DESCRIPTION_BYTES, MAX_TAGS, REDACTION_TIMEOUT, clip_bytes};
use crate::local_ai::reply::{sanitize_reply, strip_fence};
use crate::session_journal::display_text;
use crate::terminal_logging::Redactor;

const MAX_CANDIDATE_TAGS: usize = 8;
const MAX_TAG_UTF16: usize = 40;
const EMPTY: &str = "The local AI reply contained no usable screenshot analysis";
const REDACTION_FAILED: &str = "Could not safely redact the screenshot analysis";

pub(super) fn parse(
    reply: &str,
    context: &Context,
    model: &str,
    redactor: Option<&Redactor>,
) -> Result<Analysis, String> {
    let reply = sanitize_reply(reply);
    let deadline = Instant::now() + REDACTION_TIMEOUT;
    let redact = |value: &str| {
        redactor.map_or_else(
            || Ok(Zeroizing::new(display_text(value))),
            |redactor| {
                redactor
                    .journal_text(value, deadline)
                    .ok_or(REDACTION_FAILED)
            },
        )
    };
    let mut tags = Vec::new();
    let json = serde_json::from_str::<Value>(strip_fence(&reply));
    let description = if let Ok(Value::Object(json)) = json {
        let mut normalized = Vec::new();
        if let Some(candidates) = json.get("tags").and_then(Value::as_array) {
            for candidate in candidates.iter().filter_map(Value::as_str) {
                if let Some(tag) = normalize_tag(&redact(candidate)?)
                    && !normalized.contains(&tag)
                {
                    normalized.push(tag);
                    if normalized.len() >= MAX_CANDIDATE_TAGS {
                        break;
                    }
                }
            }
        }
        tags.extend(
            normalized
                .into_iter()
                .filter(|tag| is_useful(tag, context))
                .take(MAX_TAGS),
        );
        redact(
            json.get("description")
                .and_then(Value::as_str)
                .unwrap_or(""),
        )?
    } else {
        // The pinned Java viewer accepts plain prose when the model skips the requested shape.
        redact(&reply)?
    };
    let mut description = description.trim().to_owned();
    clip_bytes(&mut description, MAX_DESCRIPTION_BYTES);
    if description.is_empty() && tags.is_empty() {
        return Err(EMPTY.into());
    }
    Ok(Analysis {
        description,
        tags,
        model: model.to_owned(),
    })
}

fn normalize_tag(value: &str) -> Option<String> {
    let mut result = String::new();
    let mut pending_space = false;
    let mut length = 0;
    for character in value
        .chars()
        .filter(|&character| !matches!(character, '`' | '"' | '#'))
    {
        if character.is_whitespace() {
            pending_space = !result.is_empty();
            continue;
        }
        length += character.len_utf16() + usize::from(pending_space);
        if length > MAX_TAG_UTF16 {
            break;
        }
        if pending_space {
            result.push(' ');
            pending_space = false;
        }
        result.push(character);
    }
    let mut lower = result.to_lowercase();
    // Unicode lowercasing can expand a scalar. Keep the stored tag within the original cap.
    let mut length = 0;
    if let Some((cut, _)) = lower.char_indices().find(|(_, character)| {
        length += character.len_utf16();
        length > MAX_TAG_UTF16
    }) {
        lower.truncate(cut);
    }
    (!lower.is_empty()).then_some(lower)
}

fn is_useful(tag: &str, context: &Context) -> bool {
    const NOISE: &[&str] = &[
        "terminal",
        "ssh",
        "shell",
        "bash",
        "zsh",
        "console",
        "konsole",
        "screenshot",
        "bildschirmfoto",
        "cli",
        "tty",
        "prompt",
        "kommandozeile",
        "command line",
        "befehlszeile",
        "linux",
        "unix",
        "fedora",
        "ubuntu",
        "debian",
        "centos",
        "rhel",
        "red hat",
        "redhat",
        "rocky",
        "almalinux",
        "suse",
        "opensuse",
        "macos",
        "windows",
        "deutsch",
        "german",
        "englisch",
        "english",
        "französisch",
        "french",
        "spanisch",
        "spanish",
        "user",
        "benutzer",
        "host",
    ];
    const COLORS: &[&str] = &[
        "blue",
        "blau",
        "blauer",
        "blaues",
        "blaue",
        "red",
        "rot",
        "roter",
        "rote",
        "green",
        "grün",
        "gruen",
        "yellow",
        "gelb",
        "black",
        "schwarz",
        "white",
        "weiß",
        "weiss",
        "grey",
        "gray",
        "grau",
        "orange",
        "cyan",
        "türkis",
        "magenta",
        "purple",
        "lila",
        "violett",
        "pink",
        "rosa",
        "brown",
        "braun",
        "dark",
        "dunkel",
        "light",
        "hell",
        "color",
        "colour",
        "farbe",
        "farben",
        "colors",
        "colours",
        "farbschema",
        "colorscheme",
        "colourscheme",
    ];
    !NOISE.contains(&tag)
        && ![&context.username, &context.host].into_iter().any(|value| {
            let value = value.trim().to_lowercase();
            value == tag
                || value
                    .split_once('.')
                    .is_some_and(|(prefix, _)| prefix == tag)
        })
        && !tag
            .split([' ', '_', '-'])
            .any(|part| COLORS.contains(&part))
}
