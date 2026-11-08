use kortty_terminal::{Color, KeyModifiers, Terminal, TerminalKey, TerminalProfile};

struct Capture {
    name: &'static str,
    input: &'static str,
    expected: &'static str,
    columns: u16,
    cursor: (u16, u16),
}

macro_rules! capture {
    ($name:literal, $columns:literal, $column:literal, $row:literal) => {
        Capture {
            name: $name,
            input: include_str!(concat!("fixtures/sithtermfx/", $name, ".txt")),
            expected: include_str!(concat!("fixtures/sithtermfx/", $name, ".after.txt")),
            columns: $columns,
            cursor: ($column, $row),
        }
    };
}

const CHARACTERS: &str = "vttest/Test3_Characters/1";
const MC_VT100: &str = "testMidnightCommanderOnVT100";
const MC_XTERM: &str = "testMidnightCommanderOnXTerm";
const CAPTURES: &[Capture] = &[
    capture!("vttest/Test2_Screen/1", 80, 14, 8),
    capture!("vttest/Test2_Screen/2", 80, 1, 6),
    capture!("vttest/Test2_Screen/3", 132, 75, 20),
    capture!("vttest/Test2_Screen/4", 80, 74, 20),
    capture!("vttest/Test2_Screen/5", 132, 74, 20),
    capture!("vttest/Test2_Screen/6", 80, 73, 20),
    capture!("vttest/Test2_Screen/7", 80, 1, 13),
    capture!("vttest/Test2_Screen/8", 80, 1, 2),
    capture!("vttest/Test2_Screen/9", 80, 1, 13),
    capture!("vttest/Test2_Screen/10", 80, 1, 2),
    capture!("vttest/Test2_Screen/11", 80, 74, 23),
    capture!("vttest/Test2_Screen/12", 80, 60, 1),
    capture!("vttest/Test2_Screen/13", 80, 31, 23),
    capture!("vttest/Test2_Screen/14", 80, 32, 23),
    capture!("vttest/Test2_Screen/15", 80, 14, 24),
    capture!("vttest/Test3_Characters/1", 80, 54, 24),
    capture!("testMidnightCommanderOnVT100", 80, 42, 23),
    capture!("testMidnightCommanderOnXTerm", 80, 42, 23),
];

#[test]
fn split_reference_streams_match_java_screen_cursor_modes_and_replies() {
    for capture in CAPTURES {
        let input = capture.input.replace("\r\n", "\n").replace('\n', "\r\n");
        let expected = capture.expected.replace("\r\n", "\n").replace('\r', "\n");
        let expected = if capture.name == CHARACTERS {
            // Java stores DEL in this fixture; the Rust VT parser ignores it.
            expected.replace('\x7f', " ")
        } else {
            expected
        };
        let expected: Vec<_> = expected.lines().collect();
        assert_eq!(expected.len(), 24, "{} capture height", capture.name);
        for profile in [TerminalProfile::Xterm, TerminalProfile::Vt100] {
            for chunk_size in [1, 7, input.len().max(1)] {
                let context = format!("{} {profile:?} chunk={chunk_size}", capture.name);
                let mut terminal = Terminal::with_profile(capture.columns, 24, profile);
                for chunk in input.as_bytes().chunks(chunk_size) {
                    terminal.advance(chunk);
                }
                let screen = terminal.snapshot();
                assert_eq!(
                    (screen.columns, screen.rows),
                    (capture.columns, 24),
                    "{context}"
                );
                for (line, expected) in (0..24).zip(&expected) {
                    let actual: String = screen
                        .row(line)
                        .unwrap()
                        .iter()
                        .filter(|cell| cell.width != 0)
                        .map(|cell| {
                            if cell.text.is_empty() {
                                " "
                            } else {
                                &cell.text
                            }
                        })
                        .collect();
                    assert_eq!(actual.as_str(), *expected, "{context} line {}", line + 1);
                }
                assert_eq!(
                    (screen.cursor.column + 1, screen.cursor.row + 1),
                    capture.cursor,
                    "{context} cursor"
                );

                assert_eq!(
                    screen.alternate_screen,
                    capture.name == MC_XTERM,
                    "{context} alternate screen"
                );
                assert_eq!(
                    terminal.application_cursor_keys(),
                    matches!(capture.name, MC_VT100 | MC_XTERM),
                    "{context} cursor keys mode"
                );
                let application_keypad = capture.name == MC_VT100;
                assert_eq!(
                    terminal.application_keypad(),
                    application_keypad,
                    "{context} keypad mode"
                );
                let keypad_up = if application_keypad {
                    b"\x1bOA"
                } else {
                    b"\x1b[A"
                };
                assert_eq!(
                    screen
                        .encode_key(TerminalKey::KeypadUp, KeyModifiers::default())
                        .as_deref(),
                    Some(keypad_up.as_slice()),
                    "{context} keypad Up"
                );
                let reply: &[u8] = match (capture.name, profile) {
                    (CHARACTERS, TerminalProfile::Vt100) => b"\x1b[?1;2c",
                    (CHARACTERS, _) => b"\x1b[?6c",
                    _ => b"",
                };
                assert_eq!(
                    terminal.take_responses(),
                    reply,
                    "{context} terminal replies"
                );

                if capture.name == MC_XTERM && profile == TerminalProfile::Xterm {
                    // The same color samples are checked in the upstream EmulatorTest.
                    for (column, row, foreground, background) in
                        [(8, 2, 3, 4), (23, 4, 7, 4), (2, 0, 0, 6)]
                    {
                        let style = screen.row(row).unwrap()[column].style;
                        assert_eq!(
                            style.foreground,
                            Color::Indexed(foreground),
                            "{context} foreground at {column},{row}"
                        );
                        assert_eq!(
                            style.background,
                            Color::Indexed(background),
                            "{context} background at {column},{row}"
                        );
                    }
                }
            }
        }
    }
}
