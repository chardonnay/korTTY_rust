use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::net::Ipv6Addr;
use std::ops::Range;
use std::sync::LazyLock;

use iced::font::Weight;
use iced::mouse;
use iced::widget::{Canvas, canvas, stack};
use iced::{Color, Element, Font, Length, Pixels, Point, Rectangle, Renderer, Size, Theme};
use kortty_terminal::{Cell, ScreenSnapshot};
use regex::{Captures, Regex};

use crate::terminal_links::{self, Hit as Link};
use crate::terminal_theme::Appearance;
use crate::workspace::Message;

use super::Metrics;

mod preferences;
pub(crate) use preferences::{
    Compiled, DEFAULT_ALPHABET, MAX_PATTERN_BYTES, MAX_PATTERNS, Preferences,
};

const MAX_LINE_BYTES: usize = 16 * 1024;
const MAX_VIEWPORT_BYTES: usize = 128 * 1024;
const MAX_OCCURRENCES: usize = 2048;
const MAX_LINK_BYTES: usize = 8192;

const PATH_BODY: &str = r"\p{L}\p{M}\p{N}._~+@%/\\\-";
static PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?P<root>[A-Za-z]:[\\/]|~/|\.\.?/|/)?(?P<body>[{PATH_BODY}]+)(?P<suffix>:[0-9]{{1,9}}(?::[0-9]{{1,9}})?|\([0-9]{{1,9}},[0-9]{{1,9}}\))?"
    )).unwrap()
});
static PATH_CHARACTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^[{PATH_BODY}]$")).unwrap());
static BOUNDARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\p{L}\p{M}\p{N}\-._~:/?#\[\]@!$&'()*+,;=%{}\\]").unwrap());
static TOKENS: LazyLock<[(Kind, Regex); 5]> = LazyLock::new(|| {
    [
        (
            Kind::Uuid,
            r"[0-9A-Fa-f]{8}(?:-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12}",
        ),
        (Kind::Ipv6, r"[0-9A-Fa-f:.]{2,}(?:%[0-9A-Za-z_.\-]+)?"),
        (Kind::Ipv4, r"(?:[0-9]{1,3}\.){3}[0-9]{1,3}(?::[0-9]{1,5})?"),
        (Kind::Hash, r"[0-9a-f]{7,40}"),
        (Kind::Number, r"[0-9]{4,}"),
    ]
    .map(|(kind, pattern)| (kind, Regex::new(pattern).unwrap()))
});

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum Kind {
    Url,
    Path,
    Email,
    Uuid,
    Ipv6,
    Ipv4,
    Hash,
    Number,
    Custom,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Position {
    row: u16,
    column: u16,
}

#[derive(Clone, Copy)]
struct Span {
    start: Position,
    end: Position,
}

struct Match {
    kind: Kind,
    text: String,
    span: Span,
}

struct CellText {
    bytes: Range<usize>,
    span: Span,
}

pub(crate) struct Target {
    kind: Kind,
    pub(crate) text: String,
    label: String,
    occurrences: Vec<Span>,
}

pub(crate) struct FileTarget {
    pub(crate) path: String,
    pub(crate) line: Option<usize>,
    pub(crate) column: Option<usize>,
}

impl Target {
    pub(crate) fn link(&self) -> Option<Link> {
        match self.kind {
            Kind::Url => terminal_links::printed_link(&self.text, false),
            Kind::Email => terminal_links::printed_link(&self.text, true),
            _ => None,
        }
    }

    pub(crate) fn file(&self) -> Option<FileTarget> {
        if self.kind != Kind::Path {
            return None;
        }
        let captures = PATH.captures(&self.text)?;
        let full = captures.get(0)?;
        if full.as_str() != self.text {
            return None;
        }
        let (path, line, column) = if let Some(suffix) = captures.name("suffix") {
            let text = suffix.as_str();
            let (line, column) = if let Some(pair) = text.strip_prefix('(') {
                let (line, column) = pair.strip_suffix(')')?.split_once(',')?;
                (line, Some(column))
            } else {
                let rest = text.strip_prefix(':')?;
                rest.split_once(':')
                    .map_or((rest, None), |(line, column)| (line, Some(column)))
            };
            (
                &self.text[..suffix.start()],
                position(line),
                column.and_then(position),
            )
        } else {
            (self.text.as_str(), None, None)
        };
        Some(FileTarget {
            path: path.into(),
            line,
            column,
        })
    }
}

fn position(value: &str) -> Option<usize> {
    value.parse().ok().filter(|&value| value > 0)
}

pub(crate) struct Labels {
    targets: Vec<Target>,
    prefix: String,
}

impl Labels {
    pub(crate) fn capture(screen: &ScreenSnapshot, settings: &Compiled) -> Option<Self> {
        let mut hits = capture(screen, settings);
        if hits.is_empty() {
            return None;
        }
        hits.sort_unstable_by_key(|hit| (Reverse(hit.span.start.row), hit.span.start.column));
        let alphabet = settings.alphabet.as_bytes();
        let mut indices = HashMap::<String, usize>::new();
        let mut targets = Vec::<Target>::new();
        for hit in hits {
            if let Some(&index) = indices.get(&hit.text) {
                targets[index].occurrences.push(hit.span);
            } else if targets.len() < alphabet.len() * alphabet.len() {
                indices.insert(hit.text.clone(), targets.len());
                targets.push(Target {
                    kind: hit.kind,
                    text: hit.text,
                    label: String::new(),
                    occurrences: vec![hit.span],
                });
            }
        }
        let single_letter = targets.len() <= alphabet.len();
        for (index, target) in targets.iter_mut().enumerate() {
            if !single_letter {
                target
                    .label
                    .push(char::from(alphabet[index / alphabet.len()]));
            }
            target
                .label
                .push(char::from(alphabet[index % alphabet.len()]));
        }
        Some(Self {
            targets,
            prefix: String::new(),
        })
    }

    pub(crate) fn backspace(&mut self) {
        self.prefix.pop();
    }

    pub(crate) fn letter(&mut self, letter: char) -> Option<&Target> {
        self.prefix.push(letter);
        if let Some(index) = self
            .targets
            .iter()
            .position(|target| target.label == self.prefix)
        {
            return self.targets.get(index);
        }
        if !self
            .targets
            .iter()
            .any(|target| target.label.starts_with(&self.prefix))
        {
            self.prefix.pop();
        }
        None
    }

    pub(crate) fn retain_visible(&mut self, screen: &ScreenSnapshot) -> bool {
        self.targets.retain_mut(|target| {
            target
                .occurrences
                .retain(|span| span.still_shows(screen, &target.text));
            !target.occurrences.is_empty()
        });
        if !self
            .targets
            .iter()
            .any(|target| target.label.starts_with(&self.prefix))
        {
            self.prefix.clear();
        }
        !self.targets.is_empty()
    }
}

impl Span {
    fn rows(self, screen: &ScreenSnapshot) -> impl Iterator<Item = (u16, Range<u16>)> + '_ {
        (self.start.row..=self.end.row).map(move |row| {
            let start = if row == self.start.row {
                self.start.column
            } else {
                0
            };
            let end = if row == self.end.row {
                self.end.column.saturating_add(1)
            } else {
                screen.wrapped_columns(row).unwrap_or(screen.columns)
            };
            (row, start..end)
        })
    }

    fn still_shows(self, screen: &ScreenSnapshot, mut expected: &str) -> bool {
        for (row, columns) in self.rows(screen) {
            if row < self.end.row && screen.wrapped_columns(row).is_none() {
                return false;
            }
            let Some(cells) = screen
                .row(row)
                .and_then(|cells| cells.get(usize::from(columns.start)..usize::from(columns.end)))
            else {
                return false;
            };
            for cell in cells.iter().filter(|cell| cell.width > 0) {
                let Some(rest) = expected.strip_prefix(cell_text(cell)) else {
                    return false;
                };
                expected = rest;
            }
        }
        expected.is_empty()
    }
}

fn cell_text(cell: &Cell) -> &str {
    if cell.style.hidden || cell.text.is_empty() || cell.text.chars().any(char::is_control) {
        " "
    } else {
        &cell.text
    }
}

fn capture(screen: &ScreenSnapshot, settings: &Compiled) -> Vec<Match> {
    let mut end = screen.rows;
    let mut scanned = 0;
    let mut hits = Vec::new();
    let mut matching = settings.matching();
    let max_rows = (MAX_LINE_BYTES / usize::from(screen.columns.max(1))).max(1);
    while end > 0 && scanned < MAX_VIEWPORT_BYTES && hits.len() < MAX_OCCURRENCES {
        let mut start = end - 1;
        while start > 0
            && usize::from(end - start) < max_rows
            && screen.wrapped_columns(start - 1).is_some()
        {
            start -= 1;
        }
        let before = if start == 0 {
            screen.starts_with_continuation()
        } else {
            screen.wrapped_columns(start - 1).is_some()
        };
        let (line, cells, cut) = line_text(screen, start..end);
        let after = cut || screen.wrapped_columns(end - 1).is_some();
        scanned += line.len();
        let front = if before {
            BOUNDARY.find(&line).map_or(line.len(), |hit| hit.end())
        } else {
            0
        };
        let back = if after {
            BOUNDARY
                .find_iter(&line)
                .last()
                .map_or(0, |hit| hit.start())
        } else {
            line.len()
        };
        if let Some(scan) = line
            .get(front..back)
            .filter(|text| text.bytes().any(|byte| byte != b' '))
        {
            let aligned = |bytes: &Range<usize>| -> Option<Span> {
                let first = cells
                    .get(cells.partition_point(|cell| cell.bytes.start < front + bytes.start))?;
                let last =
                    cells.get(cells.partition_point(|cell| cell.bytes.end < front + bytes.end))?;
                (first.bytes.start == front + bytes.start && last.bytes.end == front + bytes.end)
                    .then_some(Span {
                        start: first.span.start,
                        end: last.span.end,
                    })
            };
            let mut custom = matching
                .as_mut()
                .map_or_else(Vec::new, |matching| matching.spans(scan));
            custom.retain(|(bytes, _)| aligned(bytes).is_some());
            let mut visible: Vec<_> = matches(scan, custom)
                .into_iter()
                .filter_map(|(bytes, kind)| {
                    let span = aligned(&bytes)?;
                    Some(Match {
                        kind,
                        text: scan[bytes].to_owned(),
                        span,
                    })
                })
                .collect();
            visible
                .sort_unstable_by_key(|hit| (Reverse(hit.span.start.row), hit.span.start.column));
            hits.extend(visible.into_iter().take(MAX_OCCURRENCES - hits.len()));
        }
        end = start;
    }
    hits
}

fn line_text(screen: &ScreenSnapshot, rows: Range<u16>) -> (String, Vec<CellText>, bool) {
    let mut text = String::new();
    let mut cells = Vec::new();
    for row in rows {
        let Some(line) = screen.row(row) else { break };
        let limit = screen
            .wrapped_columns(row)
            .unwrap_or(screen.columns)
            .min(screen.columns);
        for (column, cell) in (0..limit).zip(line).filter(|(_, cell)| cell.width > 0) {
            let value = cell_text(cell);
            let start = text.len();
            if start + value.len() > MAX_LINE_BYTES {
                return (text, cells, true);
            }
            text.push_str(value);
            cells.push(CellText {
                bytes: start..text.len(),
                span: Span {
                    start: Position { row, column },
                    end: Position {
                        row,
                        column: (column + u16::from(cell.width) - 1).min(limit - 1),
                    },
                },
            });
        }
    }
    (text, cells, false)
}

type Spans = BTreeMap<usize, (usize, Kind)>;

fn matches(line: &str, custom: Vec<(Range<usize>, usize)>) -> Vec<(Range<usize>, Kind)> {
    let mut candidates = Vec::<(Range<usize>, Kind)>::new();
    candidates.extend(
        terminal_links::address_candidates(line).filter_map(|(text, start)| {
            (text.len() <= MAX_LINK_BYTES && terminal_links::printed_link(text, false).is_some())
                .then_some((start..start + text.len(), Kind::Url))
        }),
    );
    if line.contains(['/', '\\']) {
        candidates.extend(
            PATH.captures_iter(line)
                .filter_map(|hit| path(line, &hit).map(|range| (range, Kind::Path))),
        );
    }
    candidates.extend(
        terminal_links::email_candidates(line).filter_map(|(text, start)| {
            valid_email(text).then_some((start..start + text.len(), Kind::Email))
        }),
    );
    for (kind, regex) in TOKENS.iter() {
        candidates.extend(regex.find_iter(line).filter_map(|hit| {
            token_end(*kind, line, hit.range()).map(|end| (hit.start()..end, *kind))
        }));
    }
    candidates.sort_unstable_by_key(|(bytes, kind)| (*kind, Reverse(bytes.len()), bytes.start));
    let mut accepted = Spans::new();
    for (bytes, kind) in candidates {
        if overlapping(&accepted, bytes.clone()).next().is_none() {
            accepted.insert(bytes.start, (bytes.end, kind));
        }
    }
    merge_custom(line, custom, &mut accepted);
    accepted
        .into_iter()
        .map(|(start, (end, kind))| (start..end, kind))
        .collect()
}

fn overlapping(
    spans: &Spans,
    candidate: Range<usize>,
) -> impl Iterator<Item = (usize, usize)> + '_ {
    let before = spans
        .range(..candidate.start)
        .next_back()
        .filter(|(_, (end, _))| *end > candidate.start);
    before
        .into_iter()
        .chain(spans.range(candidate))
        .map(|(&start, &(end, _))| (start, end))
}

fn merge_custom(line: &str, mut candidates: Vec<(Range<usize>, usize)>, accepted: &mut Spans) {
    let length = |range: Range<usize>| line[range].encode_utf16().count();
    candidates.sort_by_cached_key(|(bytes, pattern)| {
        (Reverse(length(bytes.clone())), *pattern, bytes.start)
    });
    let mut custom = Spans::new();
    for (bytes, _) in candidates {
        if overlapping(&custom, bytes.clone()).next().is_none() {
            custom.insert(bytes.start, (bytes.end, Kind::Custom));
        }
    }
    for (start, (end, kind)) in custom {
        let candidate_length = length(start..end);
        let beaten: Vec<_> = overlapping(accepted, start..end).collect();
        if beaten
            .iter()
            .all(|&(from, to)| length(from..to) <= candidate_length)
        {
            for (from, _) in beaten {
                accepted.remove(&from);
            }
            accepted.insert(start, (end, kind));
        }
    }
}

fn valid_email(text: &str) -> bool {
    let Some((local, domain)) = text.split_once('@') else {
        return false;
    };
    !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && domain.len() <= 253
        && domain
            .split('.')
            .all(|label| !label.starts_with('-') && !label.ends_with('-'))
        && domain.rsplit_once('.').is_some_and(|(_, last)| {
            last.len() >= 2 && last.bytes().all(|byte| byte.is_ascii_alphabetic())
        })
}

fn path(line: &str, hit: &Captures<'_>) -> Option<Range<usize>> {
    let range = hit.get(0)?.range();
    if line[..range.start]
        .chars()
        .next_back()
        .is_some_and(|value| PATH_CHARACTER.is_match(value.encode_utf8(&mut [0; 4])))
    {
        return None;
    }
    let suffix = hit.name("suffix");
    if suffix.is_some() && line[range.end..].starts_with(|value: char| value.is_ascii_digit()) {
        return None;
    }
    let body = hit.name("body")?;
    let value = if suffix.is_none() {
        body.as_str()
            .trim_end_matches(['.', ',', ';', ':', '!', '?', '\'', '"'])
    } else {
        body.as_str()
    };
    if !value.chars().any(char::is_alphanumeric) {
        return None;
    }
    if hit.name("root").is_some() {
        if value.starts_with(['/', '\\']) {
            return None;
        }
    } else {
        let (_, tail) = value.rsplit_once('/')?;
        if value.starts_with(['/', '\\'])
            || suffix.is_none()
                && !tail
                    .split_once('.')
                    .is_some_and(|(_, extension)| extension.chars().any(char::is_alphabetic))
        {
            return None;
        }
    }
    Some(range.start..suffix.map_or(body.start() + value.len(), |suffix| suffix.end()))
}

fn token_end(kind: Kind, line: &str, range: Range<usize>) -> Option<usize> {
    let value = &line[range.clone()];
    let previous = line[..range.start].chars().next_back();
    let word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let boundary = |end, forbidden: &str| {
        !previous.is_some_and(|ch| word(ch) || forbidden.contains(ch))
            && !line[end..].chars().next().is_some_and(word)
    };
    match kind {
        Kind::Uuid | Kind::Hash => (boundary(range.end, "-")
            && !line[range.end..].starts_with('-')
            && (kind == Kind::Uuid
                || (value.bytes().any(|byte| byte.is_ascii_digit())
                    && value.bytes().any(|byte| byte.is_ascii_alphabetic()))))
        .then_some(range.end),
        Kind::Ipv6 => {
            let mut text = value.trim_end_matches('.');
            if text.ends_with(':') && !text.ends_with("::") {
                text = text.trim_end_matches(':');
            }
            let (address, zone) = text.split_once('%').unwrap_or((text, ""));
            let end = range.start + text.len();
            (boundary(range.end, ":.%")
                && zone.len() <= 64
                && address.contains(':')
                && address.bytes().any(|byte| byte.is_ascii_digit())
                && address.parse::<Ipv6Addr>().is_ok())
            .then_some(end)
        }
        Kind::Ipv4 | Kind::Number => {
            let end = range.end;
            if !boundary(end, ".")
                || line.as_bytes()[end..]
                    .get(..2)
                    .is_some_and(|bytes| bytes[0] == b'.' && bytes[1].is_ascii_digit())
            {
                return None;
            }
            if kind == Kind::Ipv4 {
                let (address, port) = value.split_once(':').unwrap_or((value, ""));
                if address.split('.').any(|octet| octet.parse::<u8>().is_err())
                    || !port.is_empty() && port.parse::<u16>().is_err()
                {
                    return None;
                }
            }
            Some(end)
        }
        _ => None,
    }
}

pub(crate) fn view<'a>(
    base: Element<'a, Message>,
    labels: Option<&'a Labels>,
    screen: &'a ScreenSnapshot,
    font_size: u8,
    font: Font,
    appearance: Appearance,
) -> Element<'a, Message> {
    let Some(labels) = labels else { return base };
    stack![
        base,
        Canvas::new(Overlay {
            labels,
            screen,
            font_size,
            font,
            appearance
        })
        .width(Length::Fill)
        .height(Length::Fill)
    ]
    .into()
}

struct Overlay<'a> {
    labels: &'a Labels,
    screen: &'a ScreenSnapshot,
    font_size: u8,
    font: Font,
    appearance: Appearance,
}

impl canvas::Program<Message> for Overlay<'_> {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let metrics = Metrics::with_font(self.font_size, self.font);
        let foreground = self.appearance.foreground.color();
        let background = self.appearance.background.color();
        let accent = |a| Color { a, ..foreground };
        let visible = self
            .labels
            .targets
            .iter()
            .filter(|target| target.label.starts_with(&self.labels.prefix));
        for target in visible.clone() {
            for span in &target.occurrences {
                for (row, columns) in span.rows(self.screen) {
                    let origin = metrics.point(row, columns.start);
                    let size = Size::new(
                        metrics.width * f32::from(columns.end - columns.start),
                        metrics.height,
                    );
                    frame.fill_rectangle(origin, size, accent(0.18));
                    // Tiny Skia computes stroke damage from the path without its outline.
                    let damage = Rectangle::new(origin, size).expand(2.0);
                    frame.fill_rectangle(damage.position(), damage.size(), Color::TRANSPARENT);
                    frame.stroke(
                        &canvas::Path::rectangle(origin, size),
                        canvas::Stroke::default().with_color(accent(0.7)),
                    );
                }
            }
        }
        for target in visible {
            for span in &target.occurrences {
                let origin = metrics.point(span.start.row, span.start.column);
                let width = metrics.width * (target.label.len() as f32 + 1.0);
                let origin = Point::new(origin.x.min((bounds.width - width).max(0.0)), origin.y);
                frame.fill_rectangle(origin, Size::new(width, metrics.height), foreground);
                frame.fill_text(canvas::Text {
                    content: target.label.clone(),
                    position: Point::new(origin.x + metrics.width * 0.4, origin.y),
                    color: background,
                    size: Pixels(f32::from(self.font_size)),
                    font: Font {
                        weight: Weight::Bold,
                        ..self.font
                    },
                    ..canvas::Text::default()
                });
            }
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use kortty_terminal::Terminal;

    use super::*;

    fn default_labels(screen: &ScreenSnapshot) -> Option<Labels> {
        Labels::capture(screen, &Preferences::default().compile())
    }

    #[test]
    fn quick_select_terminal_files_parse_java_positions_without_touching_copied_text() {
        let mut terminal = Terminal::new(80, 8);
        terminal.advance(b"./one.rs:4:6\r\n../two.py(3,9)\r\nC:\\src\\three.c:0\r\nhttps://example.test/file.rs\r\n");
        let labels = default_labels(&terminal.snapshot()).unwrap();
        assert_eq!(
            labels
                .targets
                .iter()
                .filter_map(|target| {
                    let file = target.file()?;
                    Some((target.text.as_str(), file.path, file.line, file.column))
                })
                .collect::<Vec<_>>(),
            [
                (r"C:\src\three.c:0", r"C:\src\three.c".into(), None, None),
                ("../two.py(3,9)", "../two.py".into(), Some(3), Some(9)),
                ("./one.rs:4:6", "./one.rs".into(), Some(4), Some(6)),
            ]
        );
    }

    #[test]
    fn quick_select_joins_wrapped_tokens_deduplicates_and_orders_bottom_first() {
        let mut terminal = Terminal::new(24, 7);
        terminal.advance(b"https://example.test/long-path\r\n12345 12345\r\nsource/dir.rs:17\r\n");
        let labels = default_labels(&terminal.snapshot()).unwrap();
        let labels: Vec<_> = labels
            .targets
            .iter()
            .map(|target| (&*target.label, &*target.text, target.occurrences.len()))
            .collect();
        assert_eq!(
            labels,
            [
                ("a", "source/dir.rs:17", 1),
                ("s", "12345", 2),
                ("d", "https://example.test/long-path", 1),
            ]
        );
    }

    #[test]
    fn quick_select_respects_both_viewport_cuts_and_hidden_cells() {
        let mut terminal = Terminal::new(12, 2);
        terminal.advance(b"https://example.test/suffix 12345\r\n\x1b[8m54321\x1b[0m");
        let screen = terminal.snapshot();
        assert!(screen.starts_with_continuation());
        let labels = default_labels(&screen).unwrap();
        assert_eq!(labels.targets.len(), 1);
        assert_eq!(labels.targets[0].text, "12345");

        let mut terminal = Terminal::new(12, 3);
        terminal.advance(b"https://example.test/a-very-long-terminal-target\r\n\r\n");
        assert!(default_labels(&terminal.viewport_snapshot(usize::MAX)).is_none());
    }

    #[test]
    fn quick_select_keeps_stable_prefix_free_labels_after_output_changes() {
        let mut terminal = Terminal::new(80, 6);
        for row in 0..4 {
            for index in 0..8 {
                terminal.advance(format!("{} ", 1000 + row * 8 + index).as_bytes());
            }
            terminal.advance(b"\r\n");
        }
        let mut labels = default_labels(&terminal.snapshot()).unwrap();
        assert_eq!(labels.targets[0].label, "aa");
        assert!(labels.letter('a').is_none());
        terminal.advance(b"\x1b[4;1H98765\x1b[K");
        assert!(labels.retain_visible(&terminal.snapshot()));
        assert!(labels.letter('a').is_none());
        labels.backspace();
        assert_eq!(labels.targets[0].label, "az");
    }

    #[test]
    fn quick_select_custom_patterns_follow_overlap_and_alphabet_order() {
        let preferences = Preferences {
            alphabet: "zx".into(),
            patterns: [r"\s*TICKET-[0-9]+\s*", "abc def", "def ghi", "文界"]
                .map(str::to_owned)
                .to_vec(),
        };
        preferences.validate().unwrap();
        let mut terminal = Terminal::new(100, 4);
        terminal.advance(
            "  TICKET-12345 https://example.test/TICKET-67890 abc def ghi 文界\r\n".as_bytes(),
        );
        let mut labels = Labels::capture(&terminal.snapshot(), &preferences.compile()).unwrap();
        let actual: Vec<_> = labels
            .targets
            .iter()
            .map(|target| (target.label.as_str(), target.text.as_str()))
            .collect();
        assert_eq!(
            actual,
            [
                ("zz", "TICKET-12345"),
                ("zx", "https://example.test/TICKET-67890"),
                ("xz", "abc def"),
                ("xx", "文界"),
            ]
        );
        assert!(labels.letter('x').is_none());
        assert_eq!(labels.letter('x').unwrap().text, "文界");
    }

    #[test]
    fn quick_select_custom_settings_validate_exact_limits_and_fall_back_safely() {
        assert!(Preferences::validate_alphabet("\t ").is_ok());
        assert!(Preferences::validate_alphabet("zx").is_ok());
        for alphabet in ["z", "zz", "zX", "z x", "z文"] {
            assert!(Preferences::validate_alphabet(alphabet).is_err());
        }
        assert!(Preferences::validate_pattern(&"😀".repeat(256)).is_ok());
        for pattern in [
            "😀".repeat(257),
            "a*".into(),
            "[".into(),
            "x\0".into(),
            r"(x)\1".into(),
        ] {
            assert!(Preferences::validate_pattern(&pattern).is_err());
        }
        let mut preferences = Preferences {
            alphabet: "zz".into(),
            patterns: vec!["[".into(), "TICKET-[0-9]+".into()],
        };
        assert!(preferences.validate().is_err());
        let mut terminal = Terminal::new(40, 3);
        terminal.advance(b"TICKET-12345\r\n");
        let labels = Labels::capture(&terminal.snapshot(), &preferences.compile()).unwrap();
        assert_eq!(labels.targets[0].label, "a");
        assert_eq!(labels.targets[0].text, "TICKET-12345");

        preferences.alphabet = DEFAULT_ALPHABET.into();
        preferences.patterns = vec!["ticket".into(); MAX_PATTERNS + 1];
        assert!(preferences.validate().is_err());
    }
}
