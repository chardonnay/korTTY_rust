//! Native Lua syntax colors and explicit completion for Iced editors.

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use iced::advanced::text::{Highlighter, highlighter::Format};
use iced::widget::text_editor::{self, Content};
use iced::widget::{button, row};
use iced::{Alignment, Color, Element, Font, Theme};

use super::{Appearance, structure};

const KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];
const GLOBALS: &[&str] = &[
    "_G",
    "_VERSION",
    "assert",
    "collectgarbage",
    "coroutine",
    "debug",
    "dofile",
    "error",
    "getmetatable",
    "io",
    "ipairs",
    "load",
    "loadfile",
    "math",
    "next",
    "os",
    "package",
    "pairs",
    "pcall",
    "print",
    "rawequal",
    "rawget",
    "rawlen",
    "rawset",
    "require",
    "select",
    "setmetatable",
    "string",
    "table",
    "tonumber",
    "tostring",
    "type",
    "utf8",
    "warn",
    "xpcall",
];
const MEMBERS: &[(&str, &[&str])] = &[
    (
        "coroutine",
        &[
            "close",
            "create",
            "isyieldable",
            "resume",
            "running",
            "status",
            "wrap",
            "yield",
        ],
    ),
    (
        "io",
        &[
            "close", "flush", "input", "lines", "open", "output", "popen", "read", "stderr",
            "stdin", "stdout", "tmpfile", "type", "write",
        ],
    ),
    (
        "math",
        &[
            "abs",
            "ceil",
            "floor",
            "fmod",
            "huge",
            "log",
            "max",
            "min",
            "pi",
            "random",
            "randomseed",
            "sqrt",
            "tointeger",
            "type",
        ],
    ),
    (
        "os",
        &[
            "clock",
            "date",
            "difftime",
            "execute",
            "exit",
            "getenv",
            "remove",
            "rename",
            "setlocale",
            "time",
            "tmpname",
        ],
    ),
    (
        "string",
        &[
            "byte", "char", "dump", "find", "format", "gmatch", "gsub", "len", "lower", "match",
            "pack", "packsize", "rep", "reverse", "sub", "unpack", "upper",
        ],
    ),
    (
        "table",
        &[
            "concat", "insert", "move", "pack", "remove", "sort", "unpack",
        ],
    ),
    (
        "utf8",
        &["char", "charpattern", "codepoint", "codes", "len", "offset"],
    ),
];
const MAX_COMPLETIONS: usize = 12;
const MAX_HIGHLIGHTS: usize = 4096;
static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);

pub(crate) fn source_id() -> u64 {
    NEXT_SOURCE.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn detects(path: &Path, content: &Content) -> bool {
    let first = content.line(0);
    let first = first.as_ref().map_or("", |line| line.text.as_ref());
    structure::additional_language(path, first) == Some("lua")
}

pub(crate) fn detects_snippet(name: &str, language: &str, content: &Content) -> bool {
    let language = language.trim();
    language.eq_ignore_ascii_case("lua")
        || language.eq_ignore_ascii_case("luajit")
        || ((language.is_empty() || language.eq_ignore_ascii_case("plain"))
            && detects(Path::new(name), content))
}

pub(crate) fn completion_key(event: &text_editor::KeyPress) -> bool {
    matches!(event.status, text_editor::Status::Focused { .. })
        && event.modifiers.control()
        && !event.modifiers.alt()
        && !event.modifiers.shift()
        && !event.modifiers.logo()
        && event.key == iced::keyboard::Key::Named(iced::keyboard::key::Named::Space)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Settings {
    enabled: bool,
    dark: bool,
    source: u64,
}

impl Settings {
    pub(crate) fn new(enabled: bool, appearance: Appearance, source: u64) -> Self {
        let background = appearance.background.color();
        let luma = background.r * 0.2126 + background.g * 0.7152 + background.b * 0.0722;
        Self {
            enabled,
            dark: luma < 0.5,
            source: if enabled { source } else { 0 },
        }
    }
}

pub(crate) struct Lua {
    settings: Settings,
    line: usize,
    beginnings: Vec<Option<Long>>,
    highlights: Vec<(Range<usize>, Color)>,
}

pub(crate) fn format(color: &Color, _: &Theme) -> Format<Font> {
    Format {
        color: Some(*color),
        font: None,
    }
}

impl Highlighter for Lua {
    type Settings = Settings;
    type Highlight = Color;
    type Iterator<'a> = std::iter::Cloned<std::slice::Iter<'a, (Range<usize>, Color)>>;

    fn new(settings: &Settings) -> Self {
        Self {
            settings: *settings,
            line: 0,
            beginnings: vec![None],
            highlights: Vec::new(),
        }
    }

    fn update(&mut self, settings: &Settings) {
        *self = Self::new(settings);
    }

    fn change_line(&mut self, line: usize) {
        self.line = line.min(self.line);
        self.beginnings.truncate(self.line + 1);
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        self.highlights.clear();
        if self.settings.enabled {
            let mut long = self.beginnings[self.line];
            scan(line, self.line == 0, &mut long, |range, kind| {
                if self.highlights.len() < MAX_HIGHLIGHTS {
                    self.highlights
                        .push((range, kind.color(self.settings.dark)));
                }
            });
            self.beginnings.push(long);
        }
        self.line += 1;
        self.highlights.iter().cloned()
    }

    fn current_line(&self) -> usize {
        self.line
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Comment,
    Literal,
    Number,
    Keyword,
    Global,
}

impl Kind {
    fn color(self, dark: bool) -> Color {
        let rgb = match (self, dark) {
            (Self::Comment, true) => [140, 166, 143],
            (Self::Literal, true) => [229, 188, 125],
            (Self::Number, true) => [238, 162, 118],
            (Self::Keyword, true) => [196, 160, 249],
            (Self::Global, true) => [133, 191, 245],
            (Self::Comment, false) => [54, 99, 63],
            (Self::Literal, false) => [127, 77, 0],
            (Self::Number, false) => [153, 59, 10],
            (Self::Keyword, false) => [106, 44, 161],
            (Self::Global, false) => [16, 90, 151],
        };
        Color::from_rgb8(rgb[0], rgb[1], rgb[2])
    }
}

#[derive(Clone, Copy)]
struct Long {
    equals: usize,
    kind: Kind,
}

fn scan(
    line: &str,
    first: bool,
    long: &mut Option<Long>,
    mut emit: impl FnMut(Range<usize>, Kind),
) {
    let bytes = line.as_bytes();
    if first && bytes.starts_with(b"#!") {
        emit(0..line.len(), Kind::Comment);
        return;
    }
    let mut index = 0;
    while index < bytes.len() {
        let start = index;
        let kind = if let Some(block) = *long {
            index = long_end(bytes, index, block.equals, long);
            block.kind
        } else if bytes[index..].starts_with(b"--") {
            if let Some((length, equals)) = bracket(&bytes[index + 2..], b'[') {
                *long = Some(Long {
                    equals,
                    kind: Kind::Comment,
                });
                index = long_end(bytes, index + 2 + length, equals, long);
            } else {
                index = bytes.len();
            }
            Kind::Comment
        } else if let Some((length, equals)) = bracket(&bytes[index..], b'[') {
            *long = Some(Long {
                equals,
                kind: Kind::Literal,
            });
            index = long_end(bytes, index + length, equals, long);
            Kind::Literal
        } else if matches!(bytes[index], b'\'' | b'"') {
            let quote = bytes[index];
            index += 1;
            while index < bytes.len() {
                let byte = bytes[index];
                index += 1;
                if byte == quote {
                    break;
                }
                if byte == b'\\' && index < bytes.len() {
                    index += 1;
                }
            }
            Kind::Literal
        } else if bytes[index].is_ascii_digit()
            || (bytes[index] == b'.' && bytes.get(index + 1).is_some_and(u8::is_ascii_digit))
        {
            index = number_end(bytes, index);
            Kind::Number
        } else if bytes[index].is_ascii_alphabetic() || bytes[index] == b'_' {
            index += 1;
            while bytes.get(index).is_some_and(|byte| identifier(*byte)) {
                index += 1;
            }
            let word = &line[start..index];
            if KEYWORDS.contains(&word) {
                Kind::Keyword
            } else if GLOBALS.contains(&word) {
                Kind::Global
            } else {
                continue;
            }
        } else {
            index += 1;
            continue;
        };
        emit(start..index, kind);
    }
}

fn bracket(bytes: &[u8], boundary: u8) -> Option<(usize, usize)> {
    if bytes.first() != Some(&boundary) {
        return None;
    }
    let equals = bytes[1..].iter().take_while(|&&byte| byte == b'=').count();
    (bytes.get(equals + 1) == Some(&boundary)).then_some((equals + 2, equals))
}

fn long_end(bytes: &[u8], start: usize, equals: usize, long: &mut Option<Long>) -> usize {
    for index in start..bytes.len() {
        if let Some((length, level)) = bracket(&bytes[index..], b']')
            && level == equals
        {
            *long = None;
            return index + length;
        }
    }
    bytes.len()
}

fn identifier(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn number_end(bytes: &[u8], mut index: usize) -> usize {
    let hex = bytes[index..].starts_with(b"0x") || bytes[index..].starts_with(b"0X");
    if hex {
        index += 2;
    }
    let digit = |byte: &u8| {
        if hex {
            byte.is_ascii_hexdigit()
        } else {
            byte.is_ascii_digit()
        }
    };
    while bytes.get(index).is_some_and(digit) {
        index += 1;
    }
    if bytes.get(index) == Some(&b'.') && bytes.get(index + 1) != Some(&b'.') {
        index += 1;
        while bytes.get(index).is_some_and(digit) {
            index += 1;
        }
    }
    let exponent = if hex { b"pP" } else { b"eE" };
    if bytes.get(index).is_some_and(|byte| exponent.contains(byte)) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
    }
    index
}

pub(crate) struct Completion {
    position: text_editor::Position,
    line: String,
    range: Range<usize>,
    items: Vec<&'static str>,
}

impl Completion {
    pub(crate) fn open(content: &Content) -> Option<Self> {
        let cursor = content.cursor();
        if cursor.selection.is_some() {
            return None;
        }
        let position = cursor.position;
        let line = content.line(position.line)?.text.to_string();
        if !line.is_char_boundary(position.column) {
            return None;
        }
        let mut long = None;
        for (index, previous) in content.lines().take(position.line).enumerate() {
            scan(&previous.text, index == 0, &mut long, |_, _| {});
        }
        let mut blocked = long.is_some() && position.column == 0;
        scan(&line, position.line == 0, &mut long, |range, kind| {
            blocked |= matches!(kind, Kind::Comment | Kind::Literal | Kind::Number)
                && range.start < position.column
                && position.column <= range.end;
        });
        if blocked {
            return None;
        }
        let left = &line[..position.column];
        let start = left
            .as_bytes()
            .iter()
            .rposition(|&byte| !identifier(byte))
            .map_or(0, |index| index + 1);
        let end = position.column
            + line.as_bytes()[position.column..]
                .iter()
                .take_while(|&&byte| identifier(byte))
                .count();
        let prefix = &left[start..];
        let library = left[..start].strip_suffix('.').and_then(|base| {
            base.rsplit(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
                .next()
        });
        let items = if let Some(library) = library {
            let members = MEMBERS
                .iter()
                .find_map(|&(name, members)| (library == name).then_some(members))?;
            members
                .iter()
                .copied()
                .filter(|word| word.starts_with(prefix))
                .take(MAX_COMPLETIONS)
                .collect::<Vec<_>>()
        } else {
            KEYWORDS
                .iter()
                .chain(GLOBALS)
                .copied()
                .filter(|word| word.starts_with(prefix))
                .take(MAX_COMPLETIONS)
                .collect::<Vec<_>>()
        };
        (!items.is_empty()).then_some(Self {
            position,
            line,
            range: start..end,
            items,
        })
    }

    pub(crate) fn edit(
        &self,
        content: &mut Content,
        item: &'static str,
    ) -> Option<text_editor::Action> {
        let cursor = content.cursor();
        if cursor.selection.is_some()
            || cursor.position != self.position
            || !self.items.contains(&item)
            || content.line(self.position.line)?.text != self.line
        {
            return None;
        }
        content.move_to(text_editor::Cursor {
            position: text_editor::Position {
                line: self.position.line,
                column: self.range.end,
            },
            selection: (!self.range.is_empty()).then_some(text_editor::Position {
                line: self.position.line,
                column: self.range.start,
            }),
        });
        Some(text_editor::Action::Edit(text_editor::Edit::Paste(
            Arc::new(item.into()),
        )))
    }

    pub(crate) fn view<'a, Message: Clone + 'a>(
        &'a self,
        choose: impl Fn(&'static str) -> Message,
        dismiss: Message,
    ) -> Element<'a, Message> {
        row(self
            .items
            .iter()
            .map(|&item| button(item).on_press(choose(item)).into())
            .chain([button("Dismiss").on_press(dismiss).into()]))
        .align_y(Alignment::Center)
        .spacing(5)
        .wrap()
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_highlights_long_brackets_across_lines_and_keeps_utf8_ranges() {
        let mut long = None;
        let source = "local n = 0x1.fp2 --[=[ café";
        let mut tokens = Vec::new();
        scan(source, false, &mut long, |range, kind| {
            tokens.push((&source[range], kind))
        });
        assert_eq!(
            tokens,
            [
                ("local", Kind::Keyword),
                ("0x1.fp2", Kind::Number),
                ("--[=[ café", Kind::Comment)
            ]
        );
        assert!(long.is_some());
        let next = "]=] print('é')";
        tokens.clear();
        scan(next, false, &mut long, |range, kind| {
            tokens.push((&next[range], kind))
        });
        assert_eq!(
            tokens,
            [
                ("]=]", Kind::Comment),
                ("print", Kind::Global),
                ("'é'", Kind::Literal)
            ]
        );
        assert!(long.is_none());
    }

    #[test]
    fn completion_replaces_a_library_member_and_refuses_a_changed_target() {
        let mut content = Content::with_text("local a = table.insXX");
        content.move_to(text_editor::Cursor {
            position: text_editor::Position {
                line: 0,
                column: 19,
            },
            selection: None,
        });
        let completion = Completion::open(&content).unwrap();
        assert_eq!(completion.items, ["insert"]);
        let action = completion.edit(&mut content, "insert").unwrap();
        content.perform(action);
        assert_eq!(content.text(), "local a = table.insert");
        assert!(completion.edit(&mut content, "insert").is_none());
        assert!(detects(
            Path::new("no-extension"),
            &Content::with_text("#!/usr/bin/env luajit\nprint(1)")
        ));
    }
}
