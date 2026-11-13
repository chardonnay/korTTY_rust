use std::fmt::Write as _;

use kortty_terminal::{Color, KeyModifiers, Terminal, TerminalKey, TerminalProfile};

const STREAMS: &str = include_str!("fixtures/petscii/streams.txt");
const EXPECTED: &str = include_str!("fixtures/petscii/expected.txt");
const KEYS: [TerminalKey; 17] = [
    TerminalKey::Up,
    TerminalKey::Down,
    TerminalKey::Left,
    TerminalKey::Right,
    TerminalKey::Home,
    TerminalKey::Backspace,
    TerminalKey::Delete,
    TerminalKey::Insert,
    TerminalKey::Enter,
    TerminalKey::Function(1),
    TerminalKey::Function(2),
    TerminalKey::Function(3),
    TerminalKey::Function(4),
    TerminalKey::Function(5),
    TerminalKey::Function(6),
    TerminalKey::Function(7),
    TerminalKey::Function(8),
];

#[test]
fn raw_streams_and_keys_match_the_pinned_petscii_core() {
    let expected = EXPECTED.replace("\r\n", "\n");
    let profile = TerminalProfile::Petscii;
    let (columns, rows) = profile.default_size();
    let mut header = format!("profile={}:{columns}x{rows}\nkeys=", profile.term_name());
    let screen = Terminal::with_profile(columns, rows, profile).snapshot();
    for key in KEYS {
        let bytes = screen.encode_key(key, KeyModifiers::default()).unwrap();
        append_hex(&mut header, &bytes);
    }
    header.push('\n');
    assert!(
        expected.starts_with(&header),
        "unexpected profile or key bytes"
    );

    for line in STREAMS.lines() {
        let (name, hex) = line.split_once('\t').unwrap();
        let (pairs, remaining) = hex.as_bytes().as_chunks::<2>();
        assert!(remaining.is_empty(), "{name} has incomplete hex");
        let bytes = pairs
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect::<Vec<_>>();
        let marker = format!("[{name}]\n");
        let (_, remainder) = expected.split_once(&marker).unwrap();
        let snapshot = remainder
            .split_once("\n[")
            .map_or(remainder, |(case, _)| case);

        for chunk_size in [bytes.len(), 7, 1] {
            let mut terminal = Terminal::with_profile(columns, rows, profile);
            for chunk in bytes.chunks(chunk_size) {
                terminal.advance(chunk);
            }
            assert_eq!(
                capture(&mut terminal).trim_end_matches('\n'),
                snapshot.trim_end_matches('\n'),
                "{name}, {chunk_size}-byte chunks"
            );
        }
    }
}

fn capture(terminal: &mut Terminal) -> String {
    let screen = terminal.snapshot();
    let mode = match screen.encode_text("Aa").as_slice() {
        b"AA" => "graphics",
        b"aA" => "lowercase",
        bytes => panic!("unexpected PETSCII mode: {bytes:?}"),
    };
    let mut output = format!(
        "cursor={},{}\nmode={mode}\nbells={}\nreplies=",
        screen.cursor.row,
        screen.cursor.column,
        terminal.take_bell_events()
    );
    append_hex(&mut output, &terminal.take_responses());
    output.push('\n');
    if screen.scrollback_lines > 0 {
        writeln!(output, "history={}", screen.scrollback_lines).unwrap();
    }
    for row in 0..screen.rows {
        let mut glyphs = String::new();
        let mut ink = String::new();
        for cell in screen.row(row).unwrap() {
            assert_eq!(cell.width, 1, "PETSCII glyph must occupy one cell");
            match cell.text.as_str() {
                "" => {
                    assert_eq!(cell.style.background, Color::Default);
                    assert!(!cell.style.inverse);
                    glyphs.push(' ');
                }
                " " => glyphs.push(' '),
                glyph => {
                    glyphs.push_str(glyph);
                    let Color::Indexed(index) = cell.style.foreground else {
                        panic!("unexpected PETSCII color: {:?}", cell.style.foreground);
                    };
                    let value = index + u8::from(cell.style.inverse) * 16;
                    write!(ink, "{value:02x}").unwrap();
                }
            }
        }
        let glyphs = glyphs.trim_end_matches(' ');
        if !glyphs.is_empty() {
            writeln!(output, "row{row}={glyphs}\nink{row}={ink}").unwrap();
        }
    }
    output
}

fn append_hex(output: &mut String, bytes: &[u8]) {
    for byte in bytes {
        write!(output, "{byte:02x}").unwrap();
    }
}
