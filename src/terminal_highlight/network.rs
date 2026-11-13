//! Bounded scanners for the pinned Java network rules.

use std::ops::Range;

// Store the reference expressions verbatim so copies of these rules exchange
// with Java unchanged. Only these exact expressions use the native scanners.
pub(super) const IPV4: &str = r"(?<![\w.])(?=\d)(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)(?:/(?:3[0-2]|[12]?\d))?(?!\w|\.\d)";
pub(super) const IPV6: &str = r"(?<![\w:.])(?=[0-9A-Fa-f]{0,4}:)(?:(?:[0-9A-Fa-f]{1,4}:){7}[0-9A-Fa-f]{1,4}|(?:[0-9A-Fa-f]{1,4}(?::[0-9A-Fa-f]{1,4}){0,6})?::(?:[0-9A-Fa-f]{1,4}(?::[0-9A-Fa-f]{1,4}){0,6})?)(?:%[\w.-]{1,32})?(?:/(?:12[0-8]|1[01]\d|[1-9]?\d))?(?![\w:]|\.\d)";
pub(super) const MAC: &str =
    r"(?<![\w:-])[0-9A-Fa-f]{2}([:-])[0-9A-Fa-f]{2}(?:\1[0-9A-Fa-f]{2}){4}(?![\w:-])";
pub(super) const DOTTED: &str =
    r"(?<![\w.])[0-9A-Fa-f]{4}\.[0-9A-Fa-f]{4}\.[0-9A-Fa-f]{4}(?!\w|\.\w)";

#[derive(Clone, Copy, Debug)]
pub(super) enum Kind {
    Ipv4,
    Ipv6,
    Mac,
    Dotted,
}

impl Kind {
    pub(super) fn pinned(pattern: &str) -> Option<Self> {
        match pattern {
            IPV4 => Some(Self::Ipv4),
            IPV6 => Some(Self::Ipv6),
            MAC => Some(Self::Mac),
            DOTTED => Some(Self::Dotted),
            _ => None,
        }
    }

    pub(super) fn find_iter(self, text: &str) -> Matches<'_> {
        Matches {
            kind: self,
            text: text.as_bytes(),
            next: 0,
        }
    }

    fn find_end(self, bytes: &[u8], start: usize) -> Option<usize> {
        let blocked_prefix: &[u8] = match self {
            Self::Ipv4 | Self::Dotted => b".",
            Self::Ipv6 => b":.",
            Self::Mac => b":-",
        };
        if start
            .checked_sub(1)
            .is_some_and(|index| word(bytes[index]) || blocked_prefix.contains(&bytes[index]))
        {
            return None;
        }
        match self {
            Self::Ipv4 => ipv4(bytes, start),
            Self::Ipv6 => ipv6(bytes, start),
            Self::Mac => mac(bytes, start),
            Self::Dotted => dotted(bytes, start),
        }
    }
}

pub(super) struct Matches<'a> {
    kind: Kind,
    text: &'a [u8],
    next: usize,
}

impl Iterator for Matches<'_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        for start in self.next..self.text.len() {
            if let Some(end) = self.kind.find_end(self.text, start) {
                self.next = end;
                return Some(start..end);
            }
        }
        self.next = self.text.len();
        None
    }
}

fn word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn digits(bytes: &[u8], start: usize, limit: u16) -> Option<usize> {
    let mut value = 0;
    let mut count = 0;
    for byte in bytes
        .get(start..)?
        .iter()
        .take(3)
        .copied()
        .take_while(u8::is_ascii_digit)
    {
        value = value * 10 + u16::from(byte - b'0');
        count += 1;
    }
    (count != 0 && value <= limit && (count == 1 || bytes[start] != b'0')).then_some(start + count)
}

fn with_prefix(
    bytes: &[u8],
    end: usize,
    limit: u16,
    boundary: impl Fn(&[u8]) -> bool,
) -> Option<usize> {
    if bytes.get(end) == Some(&b'/')
        && let Some(prefix) = digits(bytes, end + 1, limit)
        && boundary(&bytes[prefix..])
    {
        return Some(prefix);
    }
    boundary(&bytes[end..]).then_some(end)
}

fn ip_boundary(tail: &[u8], ipv6: bool) -> bool {
    !tail
        .first()
        .copied()
        .is_some_and(|byte| word(byte) || (ipv6 && byte == b':'))
        && !matches!(tail, [b'.', next, ..] if next.is_ascii_digit())
}

fn ipv4(bytes: &[u8], start: usize) -> Option<usize> {
    let mut end = digits(bytes, start, 255)?;
    for _ in 0..3 {
        if bytes.get(end) != Some(&b'.') {
            return None;
        }
        end = digits(bytes, end + 1, 255)?;
    }
    with_prefix(bytes, end, 32, |tail| ip_boundary(tail, false))
}

fn ipv6(bytes: &[u8], start: usize) -> Option<usize> {
    // Seven 4-digit groups on either side of :: plus twelve single colons.
    const MAX_CORE: usize = 70;
    let core_len = bytes[start..]
        .iter()
        .take(MAX_CORE + 1)
        .take_while(|byte| byte.is_ascii_hexdigit() || **byte == b':')
        .count();
    if core_len == 0 || core_len > MAX_CORE {
        return None;
    }
    let end = start + core_len;
    let core = &bytes[start..end];
    let groups = |side: &[u8], count: Range<usize>| {
        if side.is_empty() {
            return count.contains(&0);
        }
        let mut groups = side.split(|&byte| byte == b':');
        count.contains(&groups.clone().count())
            && groups.all(|group| (1..=4).contains(&group.len()))
    };
    let valid = if let Some(double) = core.windows(2).position(|part| part == b"::") {
        groups(&core[..double], 0..8) && groups(&core[double + 2..], 0..8)
    } else {
        groups(core, 8..9)
    };
    if !valid {
        return None;
    }
    if bytes.get(end) == Some(&b'%') {
        let zone = bytes[end + 1..]
            .iter()
            .copied()
            .take(32)
            .take_while(|&byte| word(byte) || matches!(byte, b'.' | b'-'))
            .count();
        for length in (1..=zone).rev() {
            if let Some(scoped) =
                with_prefix(bytes, end + 1 + length, 128, |tail| ip_boundary(tail, true))
            {
                return Some(scoped);
            }
        }
    }
    with_prefix(bytes, end, 128, |tail| ip_boundary(tail, true))
}

fn mac(bytes: &[u8], start: usize) -> Option<usize> {
    let address = bytes.get(start..start + 17)?;
    let separator = address[2];
    if !matches!(separator, b':' | b'-')
        || !address.iter().enumerate().all(|(index, &byte)| {
            if index % 3 == 2 {
                byte == separator
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return None;
    }
    let end = start + address.len();
    (!bytes
        .get(end)
        .copied()
        .is_some_and(|byte| word(byte) || matches!(byte, b':' | b'-')))
    .then_some(end)
}

fn dotted(bytes: &[u8], start: usize) -> Option<usize> {
    let address = bytes.get(start..start + 14)?;
    if !address.iter().enumerate().all(|(index, &byte)| {
        if index % 5 == 4 {
            byte == b'.'
        } else {
            byte.is_ascii_hexdigit()
        }
    }) {
        return None;
    }
    let end = start + address.len();
    let tail = &bytes[end..];
    (!tail.first().copied().is_some_and(word) && !matches!(tail, [b'.', next, ..] if word(*next)))
        .then_some(end)
}
