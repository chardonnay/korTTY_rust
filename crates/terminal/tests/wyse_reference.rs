use std::fmt::Write as _;

use kortty_terminal::{Color, KeyModifiers, Terminal, TerminalKey, TerminalProfile};

const STREAMS: &str = include_str!("fixtures/wyse/streams.tsv");
const JAVA: &str = include_str!("fixtures/wyse/java.expected.txt");
const RUST_DIVERGENCES: &str = include_str!("fixtures/wyse/rust-divergences.txt");
const PROFILES: [TerminalProfile; 3] = [
    TerminalProfile::Wy50,
    TerminalProfile::Wy60,
    TerminalProfile::Wy160,
];
const NAVIGATION_AND_EDITING: [TerminalKey; 18] = [
    TerminalKey::Left,
    TerminalKey::Right,
    TerminalKey::Up,
    TerminalKey::Down,
    TerminalKey::KeypadLeft,
    TerminalKey::KeypadRight,
    TerminalKey::KeypadUp,
    TerminalKey::KeypadDown,
    TerminalKey::Home,
    TerminalKey::End,
    TerminalKey::Insert,
    TerminalKey::Delete,
    TerminalKey::PageUp,
    TerminalKey::PageDown,
    TerminalKey::Enter,
    TerminalKey::Backspace,
    TerminalKey::Escape,
    TerminalKey::Tab,
];

#[test]
fn wyse_screen_and_replies_match_the_pinned_reference() {
    let java = JAVA.replace("\r\n", "\n");
    let divergences = RUST_DIVERGENCES.replace("\r\n", "\n");
    for line in STREAMS.lines() {
        let [name, term, columns, rows, source] =
            line.split('\t').collect::<Vec<_>>().try_into().unwrap();
        let profile = TerminalProfile::from_term_name(term).unwrap();
        let columns = columns.parse().unwrap();
        let rows = rows.parse().unwrap();
        let input = match source {
            "wy50.bin" => include_bytes!("fixtures/wyse/wy50.bin").to_vec(),
            "wy60.bin" => include_bytes!("fixtures/wyse/wy60.bin").to_vec(),
            "wy160.bin" => include_bytes!("fixtures/wyse/wy160.bin").to_vec(),
            hex => {
                let hex = hex.strip_prefix("utf8:").unwrap_or(hex);
                let (pairs, remaining) = hex.as_bytes().as_chunks::<2>();
                assert!(remaining.is_empty(), "{name} has incomplete hex");
                pairs
                    .iter()
                    .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                    .collect()
            }
        };
        let java_case = section(&java, name).unwrap();
        let expected = section(&divergences, name).unwrap_or(java_case);
        for chunk_size in [input.len(), 7, 1] {
            let mut terminal = Terminal::with_profile(columns, rows, profile);
            for chunk in input.chunks(chunk_size) {
                terminal.advance(chunk);
            }
            assert_eq!(
                capture(&mut terminal).trim_end_matches('\n'),
                expected,
                "{name}, {chunk_size}-byte chunks"
            );
        }
    }
}

#[test]
fn wyse_keys_and_default_dimensions_match_the_pinned_reference() {
    let java = JAVA.replace("\r\n", "\n");
    let ordinary = KeyModifiers::default();
    for profile in PROFILES {
        let (columns, rows) = profile.default_size();
        let screen = Terminal::with_profile(columns, rows, profile).snapshot();
        let function_count = if profile == TerminalProfile::Wy50 {
            12
        } else {
            24
        };
        let keys = NAVIGATION_AND_EDITING
            .into_iter()
            .chain((1..=function_count).map(TerminalKey::Function))
            .map(|key| (key, ordinary))
            .chain([
                (
                    TerminalKey::Tab,
                    KeyModifiers {
                        shift: true,
                        ..ordinary
                    },
                ),
                (
                    TerminalKey::Function(2),
                    KeyModifiers {
                        alt: true,
                        ..ordinary
                    },
                ),
            ]);
        let bytes = keys
            .map(|(key, modifiers)| hex(&screen.encode_key(key, modifiers).unwrap()))
            .collect::<Vec<_>>()
            .join(",");
        let name = format!("keys-{}", profile.term_name());
        assert_eq!(
            format!("size={columns}x{rows}\nbytes={bytes}"),
            section(&java, &name).unwrap(),
            "{profile:?}"
        );
    }
}

fn capture(terminal: &mut Terminal) -> String {
    let screen = terminal.snapshot();
    let mut output = format!(
        "size={}x{}\ncursor={},{},{}\ntitle={}\nhistory={}\nbells={}\nreplies={}\n",
        screen.columns,
        screen.rows,
        screen.cursor.row,
        screen.cursor.column,
        screen.cursor.visible,
        screen.title,
        screen.scrollback_lines,
        terminal.take_bell_events(),
        hex(&terminal.take_responses())
    );
    for row in 0..screen.rows {
        let mut glyphs = String::new();
        let mut ink = Vec::new();
        for (column, cell) in screen.row(row).unwrap().iter().enumerate() {
            let glyph = cell.text.as_str();
            let glyph = if glyph.is_empty() { " " } else { glyph };
            glyphs.push_str(glyph);
            let style = cell.style;
            if style == Default::default() {
                continue;
            }
            let flags = u8::from(style.hidden)
                | u8::from(style.blink) << 1
                | u8::from(style.inverse) << 2
                | u8::from(style.underline) << 3
                | u8::from(style.dim) << 4
                | u8::from(style.bold) << 5;
            if flags != 0 || glyph != " " {
                let foreground = index(style.foreground);
                let background = index(style.background);
                ink.push(format!("{column}:{flags}/{foreground}/{background}"));
            }
        }
        let glyphs = glyphs.trim_end_matches(' ');
        if !glyphs.is_empty() {
            writeln!(output, "row{row}={glyphs}").unwrap();
        }
        if !ink.is_empty() {
            writeln!(output, "ink{row}={}", ink.join(",")).unwrap();
        }
    }
    output
}

fn index(color: Color) -> String {
    match color {
        Color::Default => "-".into(),
        Color::Indexed(index) => index.to_string(),
        Color::Rgb(..) => panic!("unexpected Wyse RGB color"),
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(result, "{byte:02x}").unwrap();
    }
    result
}

fn section<'a>(fixture: &'a str, name: &str) -> Option<&'a str> {
    let (_, contents) = fixture.split_once(&format!("[{name}]\n"))?;
    let contents = contents
        .split_once("\n[")
        .map_or(contents, |(case, _)| case);
    Some(contents.trim_end_matches('\n'))
}
