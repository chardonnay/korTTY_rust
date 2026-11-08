//! Resolves inert OSC 8 targets and printed network addresses for an explicit user action.

use std::fmt;
use std::sync::LazyLock;

use kortty_terminal::ScreenSnapshot;
use percent_encoding::{CONTROLS, percent_decode_str, utf8_percent_encode};
use regex::Regex;
use url::{Host, Url};

const MAX_TARGET: usize = 8192;
const MAX_VIEWPORT_BYTES: usize = 4096;
const MAX_ROWS: u16 = 16;

static ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i:(?:https?|ftps?)://|mailto:)[\p{L}\p{M}\p{N}\-._~:/?#\[\]@!$&'()*+,;=%{}]+")
        .unwrap()
});
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+\-]{1,64}@[A-Za-z0-9\-]{1,63}(?:\.[A-Za-z0-9\-]{1,63})+").unwrap()
});
static FORBIDDEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{Cf}\p{Cc}\s]").unwrap());

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Hit {
    target: Url,
    plain_text: Option<String>,
    pub(crate) mismatch: Option<String>,
}

impl fmt::Debug for Hit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalLink")
            .finish_non_exhaustive()
    }
}

impl Hit {
    pub(crate) fn uri(&self) -> String {
        utf8_percent_encode(self.target.as_str(), CONTROLS).to_string()
    }

    pub(crate) fn address(&self) -> String {
        self.plain_text.clone().unwrap_or_else(|| self.uri())
    }

    pub(crate) fn display_target(&self) -> String {
        let Some(host) = self.target.host_str() else {
            let uri = self.uri();
            let shortened = shorten(&uri, 199);
            return if shortened.len() == uri.len() {
                uri
            } else {
                format!("{shortened}…")
            };
        };
        let mut destination = format!("{}://{host}", self.target.scheme());
        if let Some(port) = self.target.port() {
            destination.push(':');
            destination.push_str(&port.to_string());
        }
        let rest = &self.target[url::Position::BeforePath..];
        let rest = utf8_percent_encode(rest, CONTROLS).to_string();
        let shortened = shorten(&rest, 200_usize.saturating_sub(destination.len()));
        destination.push_str(shortened);
        if shortened.len() < rest.len() {
            destination.push('…');
        }
        destination
    }
}

fn shorten(value: &str, limit: usize) -> &str {
    let mut end = limit.min(value.len());
    if end < value.len()
        && let Some(percent) = value[..end].rfind('%')
        && end - percent < 3
    {
        end = percent;
    }
    &value[..end]
}

/// Validates an HTTP(S) destination whose address is shown directly as a link.
/// Exclude user information because it can make the visible host misleading.
pub(crate) fn parse_web_target(target: &str) -> Option<Url> {
    let (scheme, rest) = target.split_once("://")?;
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
        return None;
    }
    if rest.split(['/', '?', '#']).next()?.contains('@') {
        return None;
    }
    parse(target)
}

fn parse(target: &str) -> Option<Url> {
    if target.is_empty()
        || target.len() > MAX_TARGET
        || FORBIDDEN.is_match(target)
        || target.contains(['\\', '"', '<', '>', '`', '^', '|', '{', '}'])
        || target.as_bytes().iter().enumerate().any(|(index, byte)| {
            *byte == b'%'
                && !target
                    .as_bytes()
                    .get(index + 1..index + 3)
                    .is_some_and(|bytes| bytes.iter().all(u8::is_ascii_hexdigit))
        })
    {
        return None;
    }

    let mut uri = Url::parse(target).ok()?;
    if uri.scheme() == "mailto" {
        return (uri.cannot_be_a_base() && safe_mailto(&uri)).then_some(uri);
    }
    if !matches!(uri.scheme(), "http" | "https" | "ftp" | "ftps") {
        return None;
    }

    let authority = target.split_once("://")?.1.split(['/', '?', '#']).next()?;
    let (user_info, raw_host_port) = authority.rsplit_once('@').unwrap_or(("", authority));
    if user_info.contains('@') {
        return None;
    }
    let raw_host = if raw_host_port.starts_with('[') {
        raw_host_port.get(..=raw_host_port.find(']')?)?
    } else {
        raw_host_port.split(':').next()?
    };
    if raw_host.is_empty() || raw_host.contains('%') || !uri.has_host() {
        return None;
    }
    let host = Host::parse(raw_host).ok()?;
    if let Host::Domain(domain) = &host
        && domain.trim_end_matches('.').split('.').any(|label| {
            label.is_empty()
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return None;
    }
    uri.set_host(Some(&host.to_string())).ok()?;
    Some(uri)
}

fn safe_mailto(uri: &Url) -> bool {
    let safe_header_value = |value: &str| {
        percent_decode_str(value)
            .decode_utf8()
            .is_ok_and(|value| !value.chars().any(char::is_control))
    };
    safe_header_value(uri.path())
        && uri.query().is_none_or(|query| {
            query
                .split('&')
                .filter(|field| !field.is_empty())
                .all(|field| {
                    let (name, value) = field.split_once('=').unwrap_or((field, ""));
                    let name = percent_decode_str(name)
                        .decode_utf8_lossy()
                        .to_ascii_lowercase();
                    match name.as_str() {
                        "to" | "cc" | "bcc" | "subject" | "in-reply-to" => safe_header_value(value),
                        "body" => !percent_decode_str(value).any(|byte| byte == 0),
                        _ => false,
                    }
                })
        })
}

pub(crate) fn address_candidates(line: &str) -> impl Iterator<Item = (&str, usize)> {
    ADDRESS.find_iter(line).filter_map(|candidate| {
        let prefix = line[..candidate.start()].bytes().next_back();
        if prefix.is_some_and(|byte| byte.is_ascii_alphanumeric() || b"+.-".contains(&byte)) {
            return None;
        }
        let mut matched = candidate
            .as_str()
            .trim_end_matches(['.', ',', ';', ':', '!', '?', '\'', '"']);
        for (left, right) in [('(', ')'), ('[', ']'), ('{', '}')] {
            while matched.ends_with(right)
                && matched.matches(right).count() > matched.matches(left).count()
            {
                matched = matched.strip_suffix(right)?;
            }
        }
        Some((matched, candidate.start()))
    })
}

pub(crate) fn email_candidates(line: &str) -> impl Iterator<Item = (&str, usize)> {
    EMAIL.find_iter(line).filter_map(|candidate| {
        (!line[..candidate.start()]
            .bytes()
            .next_back()
            .is_some_and(email_part)
            && !line[candidate.end()..]
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
        .then_some((candidate.as_str(), candidate.start()))
    })
}

pub(crate) fn printed_link(text: &str, email: bool) -> Option<Hit> {
    let target = if email {
        parse(&format!("mailto:{text}"))?
    } else {
        parse(text)?
    };
    Some(Hit {
        target,
        plain_text: Some(text.to_owned()),
        mismatch: None,
    })
}

fn comparable(host: &str) -> &str {
    let host = host.trim_end_matches('.');
    host.strip_prefix("www.").unwrap_or(host)
}

fn explicit(target: &str, visible: &str) -> Option<Hit> {
    let target = parse(target)?;
    let mismatch = address_candidates(visible).find_map(|(printed, _)| {
        let printed = parse(printed)?;
        let shown = printed.host_str()?;
        (!target
            .host_str()
            .is_some_and(|host| comparable(host) == comparable(shown)))
        .then(|| shown.to_owned())
    });
    Some(Hit {
        target,
        plain_text: None,
        mismatch,
    })
}

pub(crate) fn at(screen: &ScreenSnapshot, row: u16, column: u16, plain_text: bool) -> Option<Hit> {
    let hovered = screen.row(row)?.get(usize::from(column))?;
    if hovered.style.hidden || hovered.text.is_empty() && hovered.width != 0 {
        return None;
    }
    let target = hovered.hyperlink.as_deref();
    if target.is_none() && !plain_text {
        return None;
    }
    let (start, end) = wrapped_rows(screen, row);
    let mut line = String::new();
    let mut hovered_offset = None;
    let mut explicit_start = 0;
    let mut explicit_end = None;
    for index in start..=end {
        let mut previous_offset = line.len();
        let cells = screen.row(index)?;
        let columns = screen
            .wrapped_columns(index)
            .unwrap_or(screen.columns)
            .min(screen.columns);
        for (current, cell) in cells[..usize::from(columns)].iter().enumerate() {
            if line.len() >= MAX_VIEWPORT_BYTES {
                return None;
            }
            let outside_link = target.is_some() && cell.hyperlink.as_deref() != target;
            if outside_link && hovered_offset.is_some() {
                explicit_end.get_or_insert(line.len());
            }
            if cell.width > 0 {
                previous_offset = line.len();
                if cell.text.is_empty()
                    || cell.style.hidden
                    || (target.is_none() && cell.hyperlink.is_some())
                {
                    line.push(' ');
                } else {
                    line.push_str(&cell.text);
                }
            }
            if outside_link && hovered_offset.is_none() {
                explicit_start = line.len();
            }
            if index == row && current == usize::from(column) {
                hovered_offset = Some(previous_offset);
            }
        }
    }
    let offset = hovered_offset?;
    if let Some(target) = target {
        let visible = line.get(explicit_start..explicit_end.unwrap_or(line.len()))?;
        return explicit(target, visible);
    }

    for (candidate, start) in address_candidates(&line) {
        let candidate_end = start + candidate.len();
        if (start..candidate_end).contains(&offset)
            && (candidate_end != line.len() || screen.wrapped_columns(end).is_none())
        {
            return printed_link(candidate, false);
        }
    }
    for (address, start) in email_candidates(&line) {
        if (start..start + address.len()).contains(&offset) {
            return printed_link(address, true);
        }
    }
    None
}

fn email_part(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"._%+-".contains(&byte)
}

fn wrapped_rows(screen: &ScreenSnapshot, row: u16) -> (u16, u16) {
    let row_limit = (MAX_VIEWPORT_BYTES / usize::from(screen.columns).max(1)).max(1) as u16;
    let lookaround = MAX_ROWS.min(row_limit.saturating_sub(1) / 2);
    let mut start = row;
    while row - start < lookaround && start > 0 && screen.wrapped_columns(start - 1).is_some() {
        start -= 1;
    }
    let mut end = row;
    while end - row < lookaround && end + 1 < screen.rows && screen.wrapped_columns(end).is_some() {
        end += 1;
    }
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opener_requires_a_network_scheme_and_safe_mail_headers() {
        for value in [
            "file:///tmp/script",
            "javascript:alert(1)",
            "https:example.com",
            "https:///example.com",
            "https://a\\@b",
            "https://%65xample.com",
            "https://example.com/%A",
            "https://example.com/\u{202e}",
            "mailto:user@example.com?%61ttach=/secret",
            "mailto:user@example.com?Attachment=x",
            "mailto:user@example.com?subject=hello%0D%0AAttach:%20x",
        ] {
            assert!(parse(value).is_none(), "{value}");
        }
        for value in [
            "https://bücher.example/",
            "ftps://bücher.example/",
            "ftp://localhost:21/pub",
            "https://[::1]:444/",
            "mailto:user@example.com?subject=hello&body=a%0D%0Ab",
        ] {
            assert!(parse(value).is_some(), "{value}");
        }
        assert_eq!(
            parse("ftps://bücher.example/").unwrap().host_str(),
            Some("xn--bcher-kva.example")
        );
    }

    #[test]
    fn osc_target_uses_punycode_and_reports_a_different_visible_host() {
        let hit = explicit("https://bücher.example/文", "(https://another.example/)").unwrap();
        assert_eq!(hit.mismatch.as_deref(), Some("another.example"));
        assert_eq!(hit.uri(), "https://xn--bcher-kva.example/%E6%96%87");
        assert_eq!(hit.address(), hit.uri());
        assert!(
            explicit("https://bücher.example/", "https://www.BÜCHER.example./")
                .unwrap()
                .mismatch
                .is_none()
        );
        assert!(
            explicit("mailto:user@example.com", "https://example.com")
                .unwrap()
                .mismatch
                .is_some()
        );
    }
    #[test]
    fn link_detection_joins_soft_wraps_and_respects_the_plain_text_setting() {
        let mut terminal = kortty_terminal::Terminal::new(12, 6);
        terminal.advance(b"https://example.net/one");
        let screen = terminal.snapshot();
        assert_eq!(
            at(&screen, 1, 2, true).unwrap().address(),
            "https://example.net/one"
        );
        assert!(at(&screen, 1, 2, false).is_none());

        let mut terminal = kortty_terminal::Terminal::new(12, 6);
        terminal.advance(b"https://exam\r\nple.net/one");
        let screen = terminal.snapshot();
        assert!(at(&screen, 1, 2, true).is_none());
    }

    #[test]
    fn osc_link_near_wide_glyphs_resolves_only_its_own_visible_text() {
        let mut terminal = kortty_terminal::Terminal::new(16, 6);
        terminal.advance(
            "界 \x1b]8;;https://target.example\x07https://shown.example/\x1b]8;;\x07".as_bytes(),
        );
        let screen = terminal.snapshot();
        let hit = at(&screen, 1, 2, false).unwrap();
        assert_eq!(hit.mismatch.as_deref(), Some("shown.example"));
        assert_eq!(hit.uri(), "https://target.example/");
        assert!(at(&screen, 0, 1, false).is_none());
    }
}
