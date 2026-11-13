use std::fmt::Write as _;

use kortty_terminal::{Cell, Color, KeyModifiers, Terminal, TerminalKey, TerminalProfile};

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
const ANSI_PROFILES: [TerminalProfile; 10] = [
    TerminalProfile::Xterm,
    TerminalProfile::Vt100,
    TerminalProfile::Vt102,
    TerminalProfile::Vt220,
    TerminalProfile::Vt320,
    TerminalProfile::Vt420,
    TerminalProfile::Vt520,
    TerminalProfile::SunCde,
    TerminalProfile::ScoAnsi,
    TerminalProfile::Cterm,
];
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

#[test]
fn ansi_erase_display_archives_the_primary_page_as_in_the_pinned_reference() {
    check_ansi_screen_and_history(
        include_bytes!("fixtures/sithtermfx/dec-erase-display.bin"),
        include_str!("fixtures/sithtermfx/dec-erase-display.expected.txt"),
    );
}

#[test]
fn ansi_partial_erase_display_uses_the_pinned_row_boundaries() {
    check_ansi_screen_and_history(
        include_bytes!("fixtures/sithtermfx/dec-partial-erase-display.bin"),
        include_str!("fixtures/sithtermfx/dec-partial-erase-display.expected.txt"),
    );
}

#[test]
fn ansi_erase_saved_lines_clears_the_active_page() {
    check_ansi_screen_and_history(
        include_bytes!("fixtures/sithtermfx/dec-erase-saved-lines.bin"),
        include_str!("fixtures/sithtermfx/dec-erase-saved-lines.expected.txt"),
    );
}

#[test]
fn ansi_terminal_resets_keep_the_saved_cursor_and_tab_stops() {
    check_ansi_screen_and_history(
        include_bytes!("fixtures/sithtermfx/dec-soft-reset-state.bin"),
        include_str!("fixtures/sithtermfx/dec-soft-reset-state.expected.txt"),
    );
    check_ansi_screen_and_history(
        include_bytes!("fixtures/sithtermfx/dec-hard-reset-state.bin"),
        include_str!("fixtures/sithtermfx/dec-hard-reset-state.expected.txt"),
    );
}

#[test]
fn ansi_scroll_commands_use_the_first_numeric_count() {
    check_ansi_screen_and_history(
        include_bytes!("fixtures/sithtermfx/dec-scroll-parameters.bin"),
        include_str!("fixtures/sithtermfx/dec-scroll-parameters.expected.txt"),
    );
}

fn check_ansi_screen_and_history(input: &[u8], expected: &str) {
    let expected = expected.replace("\r\n", "\n");
    for profile in ANSI_PROFILES {
        for chunk_size in [1, 7, input.len()] {
            let mut terminal = Terminal::with_profile(12, 4, profile);
            for chunk in input.chunks(chunk_size) {
                terminal.advance(chunk);
            }
            let screen = terminal.snapshot();
            let mut actual = format!(
                "cursor={},{}\nalternate={}\nreply=",
                screen.cursor.column, screen.cursor.row, screen.alternate_screen
            );
            append_hex(&mut actual, &terminal.take_responses());
            actual.push_str("\nhistory:\n");
            for row in 0..screen.scrollback_lines {
                append_styled_row(&mut actual, &terminal.history_line(row).unwrap());
            }
            actual.push_str("screen:\n");
            for row in 0..screen.rows {
                append_styled_row(&mut actual, screen.row(row).unwrap());
            }
            assert_eq!(actual, expected, "{profile:?}, chunk size {chunk_size}");
        }
    }
}

#[test]
fn legacy_keypad_arrows_have_independent_modifier_bindings() {
    let ansi = [
        TerminalProfile::Hp2392,
        TerminalProfile::Hp70092,
        TerminalProfile::Tvi910,
        TerminalProfile::Tvi920,
    ];
    let native = [
        TerminalProfile::Tvi925,
        TerminalProfile::Wy50,
        TerminalProfile::Wy60,
        TerminalProfile::Wy160,
    ];
    for line in include_str!("fixtures/sithtermfx/legacy-keypad-arrows.tsv").lines() {
        let [family, key, flags, expected] =
            <[_; 4]>::try_from(line.split('\t').collect::<Vec<_>>()).unwrap();
        let profiles = match family {
            "ansi" => ansi,
            "native" => native,
            _ => panic!("Unknown keypad family {family}"),
        };
        let key = match key {
            "left" => TerminalKey::KeypadLeft,
            "right" => TerminalKey::KeypadRight,
            "up" => TerminalKey::KeypadUp,
            "down" => TerminalKey::KeypadDown,
            _ => panic!("Unknown keypad key {key}"),
        };
        let modifiers = KeyModifiers {
            alt: flags.contains('a'),
            control: flags.contains('c'),
            shift: flags.contains('s'),
        };
        for profile in profiles {
            let screen = Terminal::with_profile(12, 4, profile).snapshot();
            let mut actual = String::new();
            append_hex(&mut actual, &screen.encode_key(key, modifiers).unwrap());
            assert_eq!(actual, expected, "{profile:?}: {line}");
        }
    }
}

#[test]
fn ansi_conformance_selectors_match_the_pinned_reference() {
    let input = include_bytes!("fixtures/sithtermfx/ansi-conformance.bin");
    let expected =
        include_str!("fixtures/sithtermfx/ansi-conformance.expected.txt").replace("\r\n", "\n");
    for profile in ANSI_PROFILES {
        for strict in [false, true] {
            for chunk_size in [1, 7, input.len()] {
                let mut terminal = Terminal::with_profile(12, 3, profile);
                terminal.set_strict_escape_filter(strict);
                for chunk in input.chunks(chunk_size) {
                    terminal.advance(chunk);
                }
                let screen = terminal.snapshot();
                let mut actual = format!(
                    "size={}x{}\ncursor={},{}\ninverse={}",
                    screen.columns,
                    screen.rows,
                    screen.cursor.column,
                    screen.cursor.row,
                    screen.row(0).unwrap()[2].style.inverse,
                );
                let modifiers = KeyModifiers::default();
                let cursor_key = screen
                    .encode_key(TerminalKey::Up, modifiers)
                    .unwrap_or_else(|| {
                        assert_eq!(profile, TerminalProfile::Xterm);
                        // The Xterm front end uses this mode to choose CSI or SS3.
                        let prefix = if terminal.application_cursor_keys() {
                            b'O'
                        } else {
                            b'['
                        };
                        vec![0x1b, prefix, b'A']
                    });
                actual.push_str("\ncursorKey=");
                append_hex(&mut actual, &cursor_key);
                actual.push_str("\nkeypadKey=");
                append_hex(
                    &mut actual,
                    &screen.encode_key(TerminalKey::KeypadUp, modifiers).unwrap(),
                );
                actual.push_str("\nreply=");
                append_hex(&mut actual, &terminal.take_responses());
                actual.push_str("\nscreen:\n");
                for row in 0..screen.rows {
                    writeln!(actual, "{}", row_text(screen.row(row).unwrap()).trim_end()).unwrap();
                }
                assert_eq!(
                    actual, expected,
                    "{profile:?}, strict={strict}, chunk size {chunk_size}"
                );
            }
        }
    }
}

#[test]
fn ansi_conformance_selectors_preserve_dec_control_width_and_non_ansi_dispatch() {
    let mut dec = Terminal::with_profile(12, 3, TerminalProfile::Vt220);
    for &byte in b"\x1b*0\x1b}\xf1\x1b L\xf1\x1b}\x1b N\xf1\x1b M\xf1\x1b G\x1b L\x1b[6n" {
        dec.advance(&[byte]);
    }
    assert_eq!(row_text(dec.snapshot().row(0).unwrap()).trim_end(), "─ñ─ñ");
    assert_eq!(dec.take_responses(), b"\x9b1;5R");

    for profile in [
        TerminalProfile::Hp2392,
        TerminalProfile::Hp70092,
        TerminalProfile::Wy50,
        TerminalProfile::Tvi910,
    ] {
        let mut terminal = Terminal::with_profile(12, 3, profile);
        for &byte in b"A\x1b LB\x1b MC\x1b ND" {
            terminal.advance(&[byte]);
        }
        assert_eq!(
            row_text(terminal.snapshot().row(0).unwrap()).trim_end(),
            "ALBMCND",
            "{profile:?}"
        );
    }
}

#[test]
fn alternate_page_lifecycle_matches_the_pinned_reference() {
    let steps: Vec<_> = include_str!("fixtures/sithtermfx/ansi-alternate-pages.tsv")
        .lines()
        .map(|step| {
            let (label, hex) = step.split_once('\t').unwrap();
            let bytes: Vec<_> = hex
                .as_bytes()
                .chunks(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect();
            (label, bytes)
        })
        .collect();
    let expected =
        include_str!("fixtures/sithtermfx/ansi-alternate-pages.expected.txt").replace("\r\n", "\n");
    for profile in ANSI_PROFILES {
        for chunk_size in [1, 7, usize::MAX] {
            let mut terminal = Terminal::with_profile(12, 3, profile);
            let mut actual = String::new();
            for (label, bytes) in &steps {
                for chunk in bytes.chunks(chunk_size) {
                    terminal.advance(chunk);
                }
                let screen = terminal.snapshot();
                let history = (0..screen.scrollback_lines)
                    .map(|row| {
                        row_text(&terminal.history_line(row).unwrap())
                            .trim_end()
                            .to_owned()
                    })
                    .collect::<Vec<_>>()
                    .join("|");
                let visible = (0..screen.rows)
                    .map(|row| row_text(screen.row(row).unwrap()).trim_end().to_owned())
                    .collect::<Vec<_>>()
                    .join("|");
                writeln!(
                    actual,
                    "{label} alternate={} inverse={} history=[{history}] screen=[{visible}]",
                    screen.alternate_screen,
                    screen.row(0).unwrap()[0].style.inverse,
                )
                .unwrap();
            }
            assert_eq!(actual, expected, "{profile:?}, chunk size {chunk_size}");
        }
    }
}

fn append_hex(output: &mut String, bytes: &[u8]) {
    for byte in bytes {
        write!(output, "{byte:02x}").unwrap();
    }
}

fn row_text(cells: &[Cell]) -> String {
    cells
        .iter()
        .map(|cell| {
            let text = cell.text.as_str();
            if text.is_empty() { " " } else { text }
        })
        .collect()
}

fn append_styled_row(output: &mut String, cells: &[Cell]) {
    let text = row_text(cells);
    output.push_str(text.trim_end_matches(' '));
    output.push('|');
    for cell in cells {
        output.push(match (cell.style.inverse, cell.style.underline) {
            (false, false) => '.',
            (true, false) => 'I',
            (false, true) => 'U',
            (true, true) => 'B',
        });
    }
    output.push('\n');
}
