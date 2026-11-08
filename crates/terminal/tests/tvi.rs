use std::fmt::Write;

use kortty_terminal::{KeyModifiers, ScreenSnapshot, Terminal, TerminalKey, TerminalProfile};

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
