use kortty_terminal::{
    Color, KeyModifiers, ScreenSnapshot, Terminal, TerminalKey, TerminalProfile, TextEncoding,
};

fn fragmented(terminal: &mut Terminal, bytes: &[u8]) {
    for byte in bytes {
        terminal.advance(std::slice::from_ref(byte));
    }
}

fn row(screen: &ScreenSnapshot, row: u16) -> String {
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
fn wy50_fixture_exercises_native_editing_and_status() {
    let mut terminal = Terminal::with_profile(12, 5, TerminalProfile::Wy50);
    fragmented(&mut terminal, include_bytes!("fixtures/wyse/wy50.bin"));

    let snapshot = terminal.snapshot();
    assert_eq!(row(&snapshot, 0), "AXYDE       ");
    assert_eq!(row(&snapshot, 2), " M P        ");
    assert_eq!((snapshot.cursor.row, snapshot.cursor.column), (2, 4));
    assert!(snapshot.cursor.visible);
    assert_eq!(snapshot.title, "Wyse 50 status");
    assert!(terminal.take_responses().is_empty());
}

#[test]
fn wy60_fixture_exercises_bold_wrap_modes_and_identification() {
    let mut terminal = Terminal::with_profile(6, 4, TerminalProfile::Wy60);
    fragmented(&mut terminal, include_bytes!("fixtures/wyse/wy60.bin"));

    let snapshot = terminal.snapshot();
    assert_eq!(row(&snapshot, 0), "ABCDEZ");
    assert_eq!(row(&snapshot, 1), "!     ");
    assert!(snapshot.cells[0].style.bold && snapshot.cells[6].style.bold);
    assert_eq!(terminal.take_responses(), b"60\r");
}

#[test]
fn wy160_fixture_uses_the_full_byte_address_and_color_palette() {
    let mut terminal = Terminal::with_profile(80, 6, TerminalProfile::Wy160);
    fragmented(&mut terminal, include_bytes!("fixtures/wyse/wy160.bin"));

    let snapshot = terminal.snapshot();
    assert_eq!(snapshot.columns, 132);
    let cell = &snapshot.row(1).unwrap()[130];
    assert_eq!(cell.text, "C");
    assert_eq!(cell.style.foreground, Color::Indexed(12));
    assert_eq!(cell.style.background, Color::Indexed(3));
    assert_eq!(terminal.take_responses(), b"160\r");

    fragmented(&mut terminal, b"\x1b~!");
    assert_eq!(terminal.snapshot().columns, 80);
}

#[test]
fn wy160_raw_coordinates_preserve_following_text() {
    let mut native = Terminal::with_profile(132, 2, TerminalProfile::Wy160);
    fragmented(&mut native, b"\x1b= \xc2X");
    assert_eq!(native.snapshot().row(0).unwrap()[131].text, "X");

    let mut serial =
        Terminal::with_profile_and_encoding(132, 2, TerminalProfile::Wy160, TextEncoding::Latin1);
    fragmented(&mut serial, b"\x1b= \xc2\xa2");
    assert_eq!(serial.snapshot().row(0).unwrap()[131].text, "¢");
}

#[test]
fn wy160_scroll_region_and_line_edits_leave_outside_rows_alone() {
    let mut terminal = Terminal::with_profile(8, 6, TerminalProfile::Wy160);
    terminal.advance(b"header\r\none\r\ntwo\r\nthree\r\nfooter");
    fragmented(&mut terminal, b"\x1br!#\x1b=# \n");
    let snapshot = terminal.snapshot();
    assert_eq!(row(&snapshot, 0), "header  ");
    assert_eq!(row(&snapshot, 1), "two     ");
    assert_eq!(row(&snapshot, 2), "three   ");
    assert_eq!(row(&snapshot, 3), "        ");
    assert_eq!(row(&snapshot, 4), "footer  ");
    assert_eq!(snapshot.scrollback_lines, 0);

    fragmented(&mut terminal, b"\x1b=\" \x1bE");
    assert_eq!(row(&terminal.snapshot(), 2), "        ");
    assert_eq!(row(&terminal.snapshot(), 3), "three   ");
    fragmented(&mut terminal, b"\x1bR");
    assert_eq!(row(&terminal.snapshot(), 2), "three   ");
    assert_eq!(row(&terminal.snapshot(), 4), "footer  ");
}

#[test]
fn wy50_control_z_reset_clears_scrollback_and_returns_to_normal_display() {
    let mut terminal = Terminal::with_profile(8, 2, TerminalProfile::Wy50);
    terminal.advance(b"one\r\ntwo\r\nthree\x1b/\x1bG4");
    assert_eq!(terminal.snapshot().scrollback_lines, 1);
    assert!(!terminal.snapshot().cursor.visible);
    fragmented(&mut terminal, b"\x1b\x1aA");
    let snapshot = terminal.snapshot();
    assert_eq!(snapshot.scrollback_lines, 0);
    assert!(snapshot.cursor.visible);
    assert_eq!(row(&snapshot, 0), "A       ");
    assert!(!snapshot.cells[0].style.inverse);
}

#[test]
fn wyse_status_limits_and_control_string_filter_prevent_nested_commands() {
    let mut terminal = Terminal::with_profile(16, 4, TerminalProfile::Wy60);
    terminal.advance(b"Keep\x1bz0initial\r");
    terminal.advance(b"\x1bz0");
    terminal.advance(&[b'x'; 1025]);
    fragmented(&mut terminal, b"\x1b+\x1bZ\r\x1bA1\x1b+\x1bZ\r");
    assert_eq!(terminal.snapshot().title, "initial");
    assert!(terminal.take_responses().is_empty());
    assert!(row(&terminal.snapshot(), 0).starts_with("Keep"));

    terminal.set_strict_escape_filter(true);
    fragmented(
        &mut terminal,
        b"\x1b]2;spoof\xc4\x9c\x1b+\x1bZ\x1b\\\xc2\x9d2;spoof\x1b+\xc2\x9c\x1bP\x1bZ\x1b\\\x1b_G\x1b+\x1b\\\x1b[2J\x1bz0spoof\r!",
    );
    let snapshot = terminal.snapshot();
    assert_eq!(snapshot.title, "initial");
    assert!(row(&snapshot, 0).starts_with("Keep!"));
    assert!(terminal.take_responses().is_empty());
}

#[test]
fn wyse_retains_the_graphics_shift_and_streamed_utf8_text() {
    let mut terminal = Terminal::with_profile(12, 3, TerminalProfile::Wy50);
    fragmented(&mut terminal, b"\x0eqx\x0f");
    fragmented(&mut terminal, "qé".as_bytes());
    terminal.advance(b"\x1b.\x1b^!");
    assert_eq!(row(&terminal.snapshot(), 0), "─│qé!       ");
}

#[test]
fn wyse_encodes_native_navigation_and_model_specific_function_ranges() {
    let modifiers = KeyModifiers::default();
    for (profile, max_function, last) in [
        (TerminalProfile::Wy50, 12, b'K'),
        (TerminalProfile::Wy60, 24, b'W'),
        (TerminalProfile::Wy160, 24, b'W'),
    ] {
        let screen = Terminal::with_profile(8, 3, profile).snapshot();
        for (key, expected) in [
            (TerminalKey::Left, b"\x08".as_slice()),
            (TerminalKey::Right, b"\x0c"),
            (TerminalKey::Up, b"\x0b"),
            (TerminalKey::Down, b"\x0a"),
            (TerminalKey::Home, b"\x1e"),
            (TerminalKey::End, b"\x19"),
            (TerminalKey::Insert, b"\x1bq"),
            (TerminalKey::Delete, b"\x1bW"),
            (TerminalKey::PageUp, b"\x1bJ"),
            (TerminalKey::PageDown, b"\x1bK"),
            (TerminalKey::Enter, b"\r"),
            (TerminalKey::Backspace, b"\x08"),
            (TerminalKey::Escape, b"\x1b"),
            (TerminalKey::Tab, b"\t"),
            (TerminalKey::Keypad(9), b"9"),
            (TerminalKey::Function(1), b"\x01@"),
        ] {
            assert_eq!(screen.encode_key(key, modifiers).as_deref(), Some(expected));
        }
        assert_eq!(
            screen.encode_key(TerminalKey::Function(max_function), modifiers),
            Some(vec![1, last])
        );
        assert_eq!(
            screen.encode_key(TerminalKey::Function(max_function + 1), modifiers),
            None
        );
        assert_eq!(screen.encode_key(TerminalKey::Keypad(10), modifiers), None);
        assert_eq!(
            screen.encode_key(
                TerminalKey::Tab,
                KeyModifiers {
                    shift: true,
                    ..modifiers
                }
            ),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(
            screen.encode_key(
                TerminalKey::Function(2),
                KeyModifiers {
                    alt: true,
                    ..modifiers
                }
            ),
            Some(b"\x1b\x01A".to_vec())
        );
    }
}
