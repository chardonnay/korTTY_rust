use std::fmt::Write;

use kortty_terminal::{
    KeyModifiers, ScreenSnapshot, Terminal, TerminalKey, TerminalProfile, TextEncoding,
};

#[test]
fn reference_streams_match_across_all_input_boundaries() {
    for (profile, input, expected) in [
        (
            TerminalProfile::Tvi910,
            include_bytes!("fixtures/tvi/tvi910.bin").as_slice(),
            include_str!("fixtures/tvi/tvi910.expected.txt"),
        ),
        (
            TerminalProfile::Tvi920,
            include_bytes!("fixtures/tvi/tvi920.bin").as_slice(),
            include_str!("fixtures/tvi/tvi920.expected.txt"),
        ),
        (
            TerminalProfile::Tvi925,
            include_bytes!("fixtures/tvi/tvi925.bin").as_slice(),
            include_str!("fixtures/tvi/tvi925.expected.txt"),
        ),
    ] {
        for split in 0..=input.len() {
            let mut terminal = Terminal::with_profile(16, 6, profile);
            terminal.advance(&input[..split]);
            terminal.advance(&input[split..]);
            assert_eq!(
                capture(&mut terminal),
                expected,
                "{profile:?}, split {split}"
            );
        }
        let mut terminal = Terminal::with_profile(16, 6, profile);
        for byte in input {
            terminal.advance(std::slice::from_ref(byte));
        }
        assert_eq!(capture(&mut terminal), expected, "{profile:?}, bytewise");
    }
}

#[test]
fn horizontal_tabs_preserve_existing_cells_and_use_the_active_attributes() {
    let input = include_bytes!("fixtures/tvi/tvi-tabs.bin");
    let expected = include_str!("fixtures/tvi/tvi-tabs.expected.txt");
    for profile in [
        TerminalProfile::Tvi910,
        TerminalProfile::Tvi920,
        TerminalProfile::Tvi925,
    ] {
        for chunk_size in [1, 7, input.len()] {
            let mut terminal = Terminal::with_profile(16, 6, profile);
            for chunk in input.chunks(chunk_size) {
                terminal.advance(chunk);
            }
            assert_eq!(
                capture_with_attributes(&mut terminal),
                expected,
                "{profile:?}, chunk size {chunk_size}"
            );
        }
    }
}

#[test]
fn factory_reset_preserves_saved_cursor_and_attributes() {
    let input = include_bytes!("fixtures/tvi/tvi-factory-reset-cursor.bin");
    let expected = include_str!("fixtures/tvi/tvi-factory-reset-cursor.expected.txt");
    for profile in [TerminalProfile::Tvi920, TerminalProfile::Tvi925] {
        for chunk_size in [1, 7, input.len()] {
            let mut terminal = Terminal::with_profile(16, 6, profile);
            for chunk in input.chunks(chunk_size) {
                terminal.advance(chunk);
            }
            assert_eq!(
                capture_with_attributes(&mut terminal),
                expected,
                "{profile:?}, chunk size {chunk_size}"
            );
        }
    }
}

fn capture_with_attributes(terminal: &mut Terminal) -> String {
    let mut capture = capture(terminal);
    capture.push_str("attributes:\n");
    let screen = terminal.snapshot();
    for row in 0..screen.rows {
        for cell in screen.row(row).unwrap() {
            capture.push(match (cell.style.inverse, cell.style.underline) {
                (false, false) => '.',
                (true, false) => 'I',
                (false, true) => 'U',
                (true, true) => 'B',
            });
        }
        capture.push('\n');
    }
    capture
}

#[test]
fn utf8_text_preserves_latin_characters_and_ascii_graphics_shift() {
    let input = include_bytes!("fixtures/tvi/tvi-utf8-text.bin");
    let expected = include_str!("fixtures/tvi/tvi-utf8-text.expected.txt");
    for profile in [
        TerminalProfile::Tvi910,
        TerminalProfile::Tvi920,
        TerminalProfile::Tvi925,
    ] {
        for chunk_size in [1, 7, input.len()] {
            let mut terminal = Terminal::with_profile(16, 6, profile);
            for chunk in input.chunks(chunk_size) {
                terminal.advance(chunk);
            }
            assert_eq!(
                capture(&mut terminal),
                expected,
                "{profile:?}, chunk size {chunk_size}"
            );
        }
    }
}

#[test]
fn wide_bmp_insertion_uses_the_utf16_length() {
    let input = include_bytes!("fixtures/tvi/tvi-wide-insert.bin");
    let expected =
        include_str!("fixtures/tvi/tvi-wide-insert.expected.txt").replace('\u{e000}', " ");
    for profile in [
        TerminalProfile::Tvi910,
        TerminalProfile::Tvi920,
        TerminalProfile::Tvi925,
    ] {
        for chunk_size in [1, 7, input.len()] {
            let mut terminal = Terminal::with_profile(16, 6, profile);
            for chunk in input.chunks(chunk_size) {
                terminal.advance(chunk);
            }
            assert_eq!(
                capture(&mut terminal),
                expected,
                "{profile:?}, chunk size {chunk_size}"
            );
        }
    }
}

#[test]
fn wide_utf8_cursor_addresses_and_reports_match_the_pinned_reference() {
    let input = include_bytes!("fixtures/tvi/tvi-wide-address.bin");
    let expected = include_str!("fixtures/tvi/tvi-wide-address.expected.txt").replace("\r\n", "\n");
    for profile in [
        TerminalProfile::Tvi910,
        TerminalProfile::Tvi920,
        TerminalProfile::Tvi925,
    ] {
        for split in 0..=input.len() {
            let mut terminal = Terminal::with_profile(132, 3, profile);
            terminal.advance(&input[..split]);
            terminal.advance(&input[split..]);
            assert_eq!(
                capture(&mut terminal),
                expected,
                "{profile:?}, split {split}"
            );
        }
        let mut terminal = Terminal::with_profile(132, 3, profile);
        for byte in input {
            terminal.advance(std::slice::from_ref(byte));
        }
        assert_eq!(capture(&mut terminal), expected, "{profile:?}, bytewise");
    }
}

#[test]
fn tall_utf8_cursor_addresses_and_scroll_bounds_match_the_pinned_reference() {
    let profiles = [
        TerminalProfile::Tvi910,
        TerminalProfile::Tvi920,
        TerminalProfile::Tvi925,
    ];
    for (input, expected, columns, profiles) in [
        (
            include_bytes!("fixtures/tvi/tvi-tall-address.bin").as_slice(),
            include_str!("fixtures/tvi/tvi-tall-address.expected.txt"),
            132,
            &profiles[..],
        ),
        (
            include_bytes!("fixtures/tvi/tvi-tall-region.bin").as_slice(),
            include_str!("fixtures/tvi/tvi-tall-region.expected.txt"),
            12,
            &profiles[1..],
        ),
    ] {
        let expected = expected.replace("\r\n", "\n");
        for &profile in profiles {
            for split in 0..=input.len() {
                let mut terminal = Terminal::with_profile(columns, 132, profile);
                terminal.advance(&input[..split]);
                terminal.advance(&input[split..]);
                assert_eq!(
                    capture_tall(&mut terminal),
                    expected,
                    "{profile:?}, split {split}"
                );
            }
            let mut terminal = Terminal::with_profile(columns, 132, profile);
            for byte in input {
                terminal.advance(std::slice::from_ref(byte));
            }
            assert_eq!(
                capture_tall(&mut terminal),
                expected,
                "{profile:?}, bytewise"
            );
        }
    }
}

#[test]
fn raw_cursor_addresses_preserve_protocol_bytes_and_invalid_utf8_keeps_the_next_escape() {
    let profile = TerminalProfile::Tvi925;
    for encoding in [
        TextEncoding::Latin1,
        TextEncoding::Latin9,
        TextEncoding::Windows1252,
    ] {
        let mut terminal = Terminal::with_profile_and_encoding(132, 3, profile, encoding);
        terminal.advance(b"\x1b=!\x80\x1bnX\x1b=!\xa1\x1bnY");
        assert_eq!(terminal.take_responses(), b"\x1b=!\x80\x1b=!\xa1");
        let screen = terminal.snapshot();
        assert_eq!(screen.row(1).unwrap()[96].text, "X");
        assert_eq!(screen.row(1).unwrap()[129].text, "Y");
    }

    let mut terminal = Terminal::with_profile(132, 3, profile);
    terminal.advance(b"\x1b= \xa1X\x1b= \xc2");
    terminal.advance(b"\x1bn");
    assert_eq!(terminal.snapshot().row(0).unwrap()[129].text, "X");
    assert_eq!(terminal.take_responses(), b"\x1b= \xc2\xa3");

    let mut larger = Terminal::with_profile(200, 3, profile);
    larger.advance(b"\x1b= \xc2X");
    assert_eq!(larger.snapshot().row(0).unwrap()[162].text, "X");

    let mut larger = Terminal::with_profile(12, 200, profile);
    larger.advance(b"\x1b=\xc2 X");
    assert_eq!(larger.snapshot().row(162).unwrap()[0].text, "X");

    let mut tall = Terminal::with_profile_and_encoding(12, 132, profile, TextEncoding::Latin1);
    tall.advance(b"\x1b=\x80 gone\x1b=\x81 keep\x1b~\x80\x81\x1b=\x81 \nX\x1bn");
    let screen = tall.snapshot();
    assert_eq!(line(&screen, 96).trim_end(), "keep");
    assert_eq!(line(&screen, 97).trim_end(), "X");
    assert_eq!(tall.take_responses(), b"\x1b=\x81!");

    let mut tall = Terminal::with_profile(12, 132, profile);
    tall.advance(b"\x1b=\xc2");
    tall.advance(b"\x1bn");
    assert_eq!(tall.take_responses(), b"\x1b=  ");
}

fn capture_tall(terminal: &mut Terminal) -> String {
    let screen = terminal.snapshot();
    let mut capture = format!(
        "size={}x{}\ncursor={},{}\nhistory={}\nreply=",
        screen.columns,
        screen.rows,
        screen.cursor.column,
        screen.cursor.row,
        screen.scrollback_lines,
    );
    for byte in terminal.take_responses() {
        write!(capture, "{byte:02x}").unwrap();
    }
    capture.push('\n');
    for row in 0..screen.rows {
        let text = line(&screen, row);
        let text = text.trim_end();
        if !text.is_empty() {
            writeln!(capture, "row{row}={text}").unwrap();
        }
    }
    capture
}

fn capture(terminal: &mut Terminal) -> String {
    let screen = terminal.snapshot();
    let mut capture = format!(
        "cursor={},{}\ntitle={}\nreply=",
        screen.cursor.column, screen.cursor.row, screen.title,
    );
    for byte in terminal.take_responses() {
        write!(capture, "{byte:02x}").unwrap();
    }
    capture.push_str("\nscreen:\n");
    for row in 0..screen.rows {
        let text = line(&screen, row);
        capture.push_str(text.trim_end());
        capture.push('\n');
    }
    capture
}

fn line(screen: &ScreenSnapshot, row: u16) -> String {
    screen
        .row(row)
        .unwrap()
        .iter()
        .map(|cell| {
            if cell.text.is_empty() {
                " "
            } else {
                &cell.text
            }
        })
        .collect()
}

#[test]
fn attributes_cursor_and_field_reset_match_reference() {
    let mut tvi = Terminal::with_profile(16, 6, TerminalProfile::Tvi910);
    tvi.advance(include_bytes!("fixtures/tvi/tvi910.bin"));
    let screen = tvi.snapshot();
    assert!(!screen.cursor.visible);
    let row = screen.row(1).unwrap();
    for cell in &row[3..5] {
        let style = cell.style;
        assert!(style.hidden && style.blink && style.inverse && style.underline);
        assert!(!style.bold && !style.dim);
    }
    tvi.advance(b"\x1bA");
    assert!(tvi.snapshot().cursor.visible);

    let mut tvi = Terminal::with_profile(16, 6, TerminalProfile::Tvi925);
    tvi.advance(include_bytes!("fixtures/tvi/tvi925.bin"));
    let screen = tvi.snapshot();
    assert_eq!((screen.columns, screen.rows), (16, 6));
    let row = screen.row(2).unwrap();
    assert!(row[1].style.dim && row[1].style.underline);
    assert_eq!(row[2].style, Default::default());
    assert!(row[3].style.bold);
}

#[test]
fn key_encoding_respects_profile_and_enter_mode() {
    use TerminalKey::{
        Backspace, Delete, Down, End, Enter, Function, Home, Insert, Keypad, Left, PageDown,
        PageUp, Right, Tab, Up,
    };

    for profile in [
        TerminalProfile::Tvi910,
        TerminalProfile::Tvi920,
        TerminalProfile::Tvi925,
    ] {
        let mut tvi = Terminal::with_profile(16, 6, profile);
        let screen = tvi.snapshot();
        for (key, expected) in [
            (Up, b"\x0b".as_slice()),
            (Down, b"\x0a"),
            (Left, b"\x08"),
            (Right, b"\x0c"),
            (Home, b"\x1e"),
            (Backspace, b"\x08"),
            (Enter, b"\r"),
            (Tab, b"\t"),
            (Keypad(8), b"8"),
            (Function(1), b"\x01@"),
            (Function(12), b"\x01K"),
        ] {
            assert_eq!(
                screen.encode_key(key, KeyModifiers::default()).as_deref(),
                Some(expected)
            );
        }
        let shifted = KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        };
        assert_eq!(
            screen.encode_key(Function(5), shifted).as_deref(),
            Some(b"\x01D".as_slice())
        );
        assert_eq!(
            screen.encode_key(Tab, shifted).as_deref(),
            Some(b"\x1b[Z".as_slice())
        );
        for (key, base, extended) in [
            (Insert, b"\x1b[2~".as_slice(), b"\x1bQ".as_slice()),
            (Delete, b"\x1b[3~", b"\x1bW"),
            (PageUp, b"\x1b[5~", b"\x1bJ"),
            (PageDown, b"\x1b[6~", b"\x1bK"),
            (End, b"\x1b[F", b"\x1bT"),
        ] {
            let expected = if profile == TerminalProfile::Tvi910 {
                base
            } else {
                extended
            };
            assert_eq!(
                screen.encode_key(key, KeyModifiers::default()).as_deref(),
                Some(expected)
            );
        }
        tvi.advance(b"ab\x1bK\nc");
        assert_eq!(&line(&tvi.snapshot(), 1)[..3], "  c");
        assert_eq!(
            tvi.snapshot()
                .encode_key(Enter, KeyModifiers::default())
                .unwrap(),
            b"\r\n"
        );
        tvi.advance(b"\x1bk");
        assert_eq!(
            tvi.snapshot()
                .encode_key(Enter, KeyModifiers::default())
                .unwrap(),
            b"\r"
        );
    }
}

#[test]
fn fragmented_utf8_and_oversized_control_strings_remain_bounded_and_inert() {
    let mut tvi = Terminal::with_profile(16, 6, TerminalProfile::Tvi925);
    tvi.set_strict_escape_filter(true);
    tvi.advance(b"\x1blsafe\r\x1bloverride\x1b*\r\x1bp");
    tvi.advance(&[b'x'; 4096]);
    tvi.advance(b"\x1blforged\r\x1b");
    tvi.advance(b"\\");
    tvi.advance(b"\x1bo");
    tvi.advance(&[b'x'; 4096]);
    tvi.advance(b"\x1b*\r");
    tvi.advance(b"\xe2");
    tvi.advance(b"\x82");
    tvi.advance(b"\xac");
    tvi.advance(b"\xf0\x1bn");
    let screen = tvi.snapshot();
    assert_eq!(screen.title, "safe");
    assert_eq!(line(&screen, 0).trim_end(), "€�");
    assert_eq!(tvi.take_responses(), b"\x1b= \x22");
}

#[test]
fn wrap_modes_and_factory_reset_use_the_active_model() {
    let mut tvi = Terminal::with_profile(4, 2, TerminalProfile::Tvi920);
    tvi.advance(b"\x1bwABCDx");
    assert_eq!(line(&tvi.snapshot(), 0), "xBCD");
    tvi.advance(b"\r\x1bv12345\r\n6");
    assert!(tvi.snapshot().scrollback_lines > 0);
    tvi.advance(b"\x1bz");
    let screen = tvi.snapshot();
    assert_eq!((screen.cursor.column, screen.cursor.row), (0, 0));
    assert_eq!(screen.scrollback_lines, 0);
    assert_eq!(line(&screen, 0), "    ");
}

#[test]
fn backward_tab_uses_the_virtual_cursor_at_the_right_edge() {
    let mut tvi = Terminal::with_profile(9, 2, TerminalProfile::Tvi910);
    tvi.advance(b"123456789\x1bI");
    assert_eq!(tvi.snapshot().cursor.column, 8);
    tvi.advance(b"\x1bi");
    assert_eq!(tvi.snapshot().cursor.column, 0);
}
