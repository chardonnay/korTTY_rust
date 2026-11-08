use std::{collections::BTreeMap, ops::Range, sync::LazyLock};

use pgp::crypto::hash::HashAlgorithm;
use regex::Regex;

const CLOSE: &str = "<!-- /kortty:entry -->";

#[derive(Clone)]
pub(super) struct Entry {
    pub(super) range: Range<usize>,
    pub(super) id: String,
    pub(super) origin: String,
    pub(super) created: String,
    pub(super) source: String,
    pub(super) body: String,
}

pub(super) fn entries(document: &str) -> Vec<Entry> {
    static HEADER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^<!-- kortty:entry((?: [^\s<>]+)+) -->$").expect("valid memory fence")
    });
    let mut entries = Vec::new();
    let mut pending = None;
    let mut offset = 0;
    for line in document.split_inclusive('\n') {
        let end = offset + line.len();
        let header = line.trim_end_matches(['\r', '\n']);
        let header = if offset == 0 {
            header.trim_start_matches('\u{feff}')
        } else {
            header
        };
        if let Some(found) = HEADER.captures(header) {
            let start = offset + line.len() - line.trim_start_matches('\u{feff}').len();
            pending = Some((start, end, found.get(1).map_or("", |m| m.as_str())));
        } else if header == CLOSE
            && let Some((start, body_start, attributes)) = pending.take()
        {
            let body = document[body_start..offset]
                .lines()
                .collect::<Vec<_>>()
                .join("\n");
            if let Some(entry) = entry(start..end, attributes, body) {
                entries.push(entry);
            }
        }
        offset = end;
    }
    entries
}

fn entry(range: Range<usize>, raw: &str, body: String) -> Option<Entry> {
    let mut attributes = BTreeMap::new();
    for item in raw.trim_start().split(' ') {
        let (key, value) = item.split_once('=')?;
        if attributes.insert(key, value).is_some() {
            return None;
        }
    }
    let id = *attributes.get("id")?;
    let origin = *attributes.get("origin")?;
    let created = *attributes.get("created")?;
    let source = *attributes.get("source")?;
    if attributes.len() != 5
        || !origin.eq_ignore_ascii_case("kortty")
        || !tag(id, 32, false)
        || !tag(source, 64, true)
        || created.len() != 10
        || !created.bytes().enumerate().all(|(i, byte)| {
            if i == 4 || i == 7 {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
        || hash(&body).as_deref() != attributes.get("sha256").copied()
    {
        return None;
    }
    Some(Entry {
        range,
        id: id.to_owned(),
        origin: origin.to_owned(),
        created: created.to_owned(),
        source: source.to_owned(),
        body,
    })
}

fn tag(text: &str, limit: usize, dot: bool) -> bool {
    !text.is_empty()
        && text.len() <= limit
        && text.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || (dot && byte == b'.')
        })
}

pub(super) fn hash(text: &str) -> Option<String> {
    use std::fmt::Write;
    let bytes = HashAlgorithm::Sha256.digest(text.as_bytes()).ok()?;
    let mut hex = String::with_capacity(64);
    for byte in bytes {
        write!(hex, "{byte:02x}").ok()?;
    }
    Some(hex)
}

impl Entry {
    pub(super) fn rewrite(&self, text: &str) -> Option<String> {
        let text = text.trim().trim_start_matches("- ");
        let body = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if body.is_empty() || body.chars().count() > 300 || body.contains("kortty:entry") {
            return None;
        }
        let body = format!("- {body}");
        Some(format!(
            "<!-- kortty:entry id={} origin={} sha256={} created={} source={} -->\n{body}\n{CLOSE}\n",
            self.id,
            self.origin,
            hash(&body)?,
            self.created,
            self.source,
        ))
    }
}

pub(super) fn estimate(text: &str) -> usize {
    if text.trim().is_empty() {
        return 0;
    }
    text.chars()
        .count()
        .div_ceil(4)
        .max((text.split_whitespace().count() * 135).div_ceil(100))
}

pub(super) fn secret_line(document: &str, previous: &str) -> Option<usize> {
    static KNOWN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?ix)(?:-----BEGIN\s+(?:[A-Z]+\s+)*PRIVATE\s+KEY-----|\b(?:sk-(?:proj-)?|gh[pousr]_|github_pat_|xox[baprs]-|AIza)[A-Za-z0-9_-]{16,}|\b(?:AKIA|ASIA)[A-Z0-9]{16}\b|\bBearer\s+[A-Za-z0-9+./_=-]{8,}|://[^:\s/@]+:[^\s/@]+@)").expect("valid token patterns")
    });
    static CUE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)\b(?:[a-z0-9_-]*[_-])?(?:passwords?|passwd|passphrase|passwort|kennwort|private[_-]?key|api[_ -]?key|access[_-]?key|secret|token|pin)["']?(?:\s*[:=]\s*|\s+(?:ist|is|lautet)\s+)(?:["'`])?([^\s"',;`]+)"#).expect("valid credential cue")
    });
    static ENTROPY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/=_-]{20,}").expect("valid entropy token"));
    let intact: std::collections::HashSet<_> = entries(document)
        .into_iter()
        .filter_map(|entry| document[entry.range].lines().next())
        .collect();
    let old_headers: std::collections::HashSet<_> = previous
        .lines()
        .filter(|line| line.starts_with("<!-- kortty:entry "))
        .collect();
    document.lines().enumerate().find_map(|(index, line)| {
        let trusted_header = intact.contains(line) || old_headers.contains(line);
        let line: String = line.chars().filter(|c| !matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')).collect();
        let literal = CUE.captures_iter(&line).any(|found| {
            let value = &found[1];
            value.chars().count() >= 4
                && !value.starts_with(['$', '<', '{'])
                && !matches!(value.to_ascii_lowercase().as_str(), "shift" | "example" | "placeholder" | "redacted" | "none" | "null" | "password" | "token" | "secret" | "stored" | "required" | "managed" | "gespeichert" | "erforderlich")
                && !value.bytes().all(|byte| byte == b'*' || byte == b'x')
        });
        let random = ENTROPY.find_iter(&line).any(|found| {
            if trusted_header && found.as_str().starts_with("sha256=") {
                return false;
            }
            let token = found.as_str();
            let hex = token.bytes().all(|byte| byte.is_ascii_hexdigit());
            let mut counts = [0_u32; 128];
            for byte in token.bytes() {
                counts[usize::from(byte)] += 1;
            }
            let size = token.len() as f64;
            let entropy: f64 = counts.into_iter().filter(|&n| n != 0).map(|n| {
                let share = f64::from(n) / size;
                -share * share.log2()
            }).sum();
            entropy >= if hex { 3.0 } else { 4.5 }
        });
        (KNOWN.is_match(&line) || literal || random).then_some(index + 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_valid_hash_makes_an_entry_managed() {
        let text = Entry {
            range: 0..0,
            id: "demo".into(),
            origin: "kortty".into(),
            created: "2026-11-07".into(),
            source: "chat".into(),
            body: String::new(),
        }
        .rewrite("Use `rg` for text searches.")
        .unwrap();
        let mixed = format!("My note\r\n\r\n{}", text.replace('\n', "\r\n"));
        let found = entries(&mixed);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].body, "- Use `rg` for text searches.");
        assert!(entries(&mixed.replace("searches", "queries")).is_empty());
        assert_eq!(secret_line(&text, ""), None);
        assert_eq!(secret_line("Safe\napi_key: abc1234567", ""), Some(2));
    }
}
