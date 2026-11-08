//! Web links in imported and locally recorded journal prose.

use std::fmt;
use std::ops::Range;
use std::sync::{Arc, LazyLock};

use regex::Regex;

use super::is_display_character;

const MAX_LINK_CHARACTERS: usize = 8192;
const MAX_TARGET_BYTES: usize = 8192;

static NOTE_ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i-u:https?://)[^\s<>"']+"#).expect("journal note address pattern")
});
static SUMMARY_ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i-u:https?://|www\.)[^\s<>"'`]+"#).expect("journal summary address pattern")
});

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinkField {
    Text,
    Note,
}

/// A parsed, normalized HTTP(S) destination obtained from journal prose.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct LinkTarget(Arc<str>);

impl LinkTarget {
    pub(crate) fn uri(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for LinkTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JournalLinkTarget")
            .finish_non_exhaustive()
    }
}

pub(crate) struct LinkSpan<'a> {
    pub(crate) text: &'a str,
    pub(crate) target: Option<&'a LinkTarget>,
}

#[derive(Clone, Debug)]
pub(super) struct Link {
    range: Range<usize>,
    target: LinkTarget,
}

#[derive(Clone, Copy)]
pub(super) enum Source {
    Note,
    Summary,
}

pub(super) fn load(raw: &str, source: Option<Source>) -> (String, Vec<Link>) {
    let Some(source) = source else {
        return (super::display_text(raw), Vec::new());
    };
    let pattern = match source {
        Source::Note => &*NOTE_ADDRESS,
        Source::Summary => &*SUMMARY_ADDRESS,
    };
    let mut displayed = String::with_capacity(raw.len());
    let mut links = Vec::new();
    let mut copied = 0;
    let mut characters = 0;
    for candidate in pattern.find_iter(raw) {
        characters += append_display(&mut displayed, &raw[copied..candidate.start()]);
        copied = candidate.start();
        if characters >= MAX_LINK_CHARACTERS {
            break;
        }
        let address = trim_punctuation(candidate.as_str());
        if address.len() <= MAX_TARGET_BYTES
            && characters + address.chars().count() <= MAX_LINK_CHARACTERS
            && has_safe_boundaries(raw, candidate.range(), source)
            && let Some(target) = target(address)
        {
            let start = displayed.len();
            links.push(Link {
                range: start..start + address.len(),
                target,
            });
        }
        characters += append_display(&mut displayed, candidate.as_str());
        copied = candidate.end();
    }
    append_display(&mut displayed, &raw[copied..]);
    (displayed, links)
}

fn append_display(output: &mut String, raw: &str) -> usize {
    let start = output.len();
    output.extend(
        raw.chars()
            .filter(|&character| is_display_character(character)),
    );
    output[start..].chars().count()
}

fn has_safe_boundaries(raw: &str, range: Range<usize>, source: Source) -> bool {
    let before = raw[..range.start].chars().next_back();
    let after = raw[range.end..].chars().next();
    // A removed character must not glue a clickable fragment to adjacent text.
    if before
        .into_iter()
        .chain(after)
        .any(|character| !is_display_character(character))
    {
        return false;
    }
    !matches!(source, Source::Summary)
        || before.is_none_or(|character| {
            !character.is_ascii_alphanumeric() && !matches!(character, '_' | '@' | '.' | '/' | '-')
        })
}

fn target(address: &str) -> Option<LinkTarget> {
    if !address.chars().all(is_display_character) {
        return None;
    }
    let uri = if address
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("www."))
    {
        // Validate the whole bare address so a bad attached suffix cannot
        // turn its valid-looking prefix into a link to a different target.
        let host = address.split(['/', '?', '#', ':']).next()?;
        if !host.is_ascii() || host.trim_end_matches('.').split('.').count() < 3 {
            return None;
        }
        crate::terminal_links::parse_web_target(&format!("https://{address}"))?
    } else {
        crate::terminal_links::parse_web_target(address)?
    };
    let uri = uri.as_str();
    (uri.len() <= MAX_TARGET_BYTES && uri.bytes().all(|byte| byte.is_ascii_graphic()))
        .then(|| LinkTarget(Arc::from(uri)))
}

fn trim_punctuation(address: &str) -> &str {
    let mut parentheses = address.bytes().fold(0_i32, |count, byte| match byte {
        b'(' => count - 1,
        b')' => count + 1,
        _ => count,
    });
    let mut brackets = address.bytes().fold(0_i32, |count, byte| match byte {
        b'[' => count - 1,
        b']' => count + 1,
        _ => count,
    });
    let mut end = address.len();
    for &byte in address.as_bytes().iter().rev() {
        match byte {
            b'.' | b',' | b';' | b':' | b'!' | b'?' => {}
            b')' if parentheses > 0 => parentheses -= 1,
            b']' if brackets > 0 => brackets -= 1,
            _ => break,
        }
        end -= 1;
    }
    &address[..end]
}

pub(super) fn spans<'a>(text: &'a str, links: &'a [Link], limit: usize) -> Vec<LinkSpan<'a>> {
    let end = text
        .char_indices()
        .nth(limit)
        .map_or(text.len(), |(index, _)| index);
    let mut spans = Vec::new();
    let mut start = 0;
    for link in links.iter().take_while(|link| link.range.end <= end) {
        if start < link.range.start {
            spans.push(LinkSpan {
                text: &text[start..link.range.start],
                target: None,
            });
        }
        spans.push(LinkSpan {
            text: &text[link.range.clone()],
            target: Some(&link.target),
        });
        start = link.range.end;
    }
    if start < end {
        spans.push(LinkSpan {
            text: &text[start..end],
            target: None,
        });
    }
    if end < text.len() {
        spans.push(LinkSpan {
            text: "…",
            target: None,
        });
    }
    spans
}
