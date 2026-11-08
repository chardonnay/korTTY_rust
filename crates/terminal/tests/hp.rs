use std::fmt::Write;

use kortty_terminal::{
    Color, Hp2392Input, KeyModifiers, ScreenSnapshot, Terminal, TerminalKey, TerminalProfile,
};

fn feed(terminal: &mut Terminal, bytes: &[u8]) {
    for byte in bytes {
        terminal.advance(std::slice::from_ref(byte));
    }
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
                cell.text.as_str()
            }
        })
        .collect()
}

#[test]
fn hp2392_addressing_enhancements_and_softkey_replay_survive_split_input() {
    let mut terminal = Terminal::with_profile(16, 5, TerminalProfile::Hp2392);
    feed(&mut terminal, include_bytes!("fixtures/hp/hp2392.bin"));
    let screen = terminal.snapshot();
    assert_eq!(line(&screen, 0).trim_end(), "header");
    assert_eq!(line(&screen, 1).trim_end(), "  Under");
    assert_eq!(line(&screen, 3).trim_end(), " KEY! !      ?");
    assert!(
        screen.row(1).unwrap()[2..7]
            .iter()
            .all(|cell| cell.style.underline)
    );
    assert!(
        screen.row(3).unwrap()[7..13]
            .iter()
            .all(|cell| cell.style.hidden && cell.text == " ")
    );
    assert!(!screen.row(3).unwrap()[13].style.hidden);
    assert!(terminal.take_responses().is_empty());
    feed(&mut terminal, b"\x1b&a0R\x1b&a2C3");
    assert_eq!(line(&terminal.snapshot(), 0).trim_end(), "he3der");
}

#[test]
fn hp70092_switches_ansi_mode_and_answers_device_status() {
    let mut terminal = Terminal::with_profile(16, 4, TerminalProfile::Hp70092);
    feed(&mut terminal, include_bytes!("fixtures/hp/hp70092.bin"));
    let screen = terminal.snapshot();
    assert_eq!(line(&screen, 0).trim_end(), "HP");
    assert_eq!(line(&screen, 1).trim_end(), "   ANSIN!");
    let row = screen.row(1).unwrap();
    for cell in &row[3..7] {
        assert_eq!(cell.style.foreground, Color::Indexed(1));
        assert_eq!(cell.style.background, Color::Indexed(4));
        assert!(cell.style.bold);
    }
    assert!(row[7].style.underline);
    assert!(!row[8].style.underline);
    assert!(!screen.cursor.visible);
    assert_eq!(terminal.take_responses(), b"\x1b[0n\x1b[2;8R");
    feed(&mut terminal, b"\x1b[5n\x1b[3;1HQ");
    assert!(terminal.take_responses().is_empty());
    assert_eq!(line(&terminal.snapshot(), 1).trim_end(), "   ANSIN!Q");
}

#[test]
fn hp2392_memory_lock_limits_scrolling_and_unlock_resets_it() {
    let mut terminal = Terminal::with_profile(6, 4, TerminalProfile::Hp2392);
    feed(
        &mut terminal,
        b"head\r\nlock\r\none\r\ntwo\x1b&a1R\x1b`\x1b&a3R\r\nnew",
    );
    let screen = terminal.snapshot();
    assert_eq!(line(&screen, 0).trim_end(), "head");
    assert_eq!(line(&screen, 1).trim_end(), "lock");
    assert_eq!(line(&screen, 2).trim_end(), "two");
    assert_eq!(line(&screen, 3).trim_end(), "new");
    assert_eq!(terminal.hp2392_modes().unwrap().memory_lock_row, Some(1));
    feed(&mut terminal, b"\x1bb");
    assert_eq!(terminal.hp2392_modes().unwrap().memory_lock_row, None);
    let cursor = terminal.snapshot().cursor;
    assert_eq!((cursor.row, cursor.column), (0, 0));
    feed(&mut terminal, b"\x1bS");
    assert_eq!(line(&terminal.snapshot(), 0).trim_end(), "lock");
}

#[test]
fn hp_character_and_line_edits_remain_stream_operations() {
    let mut terminal = Terminal::with_profile(8, 4, TerminalProfile::Hp2392);
    feed(&mut terminal, b"ABCD\x1b&a1C\x1bP\x1bQX");
    assert_eq!(line(&terminal.snapshot(), 0).trim_end(), "AXCD");
    feed(&mut terminal, b"\x1b&a1R\rone\r\ntwo\x1b&a1R\x1bL");
    assert_eq!(line(&terminal.snapshot(), 2).trim_end(), "one");
    feed(&mut terminal, b"\x1bM");
    assert_eq!(line(&terminal.snapshot(), 1).trim_end(), "one");

    feed(
        &mut terminal,
        b"\x1bh\x1b1\x1bcKEEP\x1b2gone\x1b&a0C\x1b1\x1bK",
    );
    assert!(line(&terminal.snapshot(), 0).trim_end().is_empty());
    let modes = terminal.hp2392_modes().unwrap();
    assert!(modes.block_mode && modes.protect_mode);
    feed(&mut terminal, b"\x1b2\x1bd");
    let modes = terminal.hp2392_modes().unwrap();
    assert!(!modes.block_mode && !modes.protect_mode);
}

#[test]
fn hp_mode_streams_match_pinned_java_captures() {
    for (profile, input, expected) in [
        (
            TerminalProfile::Hp2392,
            include_bytes!("fixtures/hp/hp2392-modes.bin").as_slice(),
            include_str!("fixtures/hp/hp2392-modes.after.txt"),
        ),
        (
            TerminalProfile::Hp70092,
            include_bytes!("fixtures/hp/hp70092-modes.bin").as_slice(),
            include_str!("fixtures/hp/hp70092-modes.after.txt"),
        ),
    ] {
        let mut terminal = Terminal::with_profile(12, 4, profile);
        feed(&mut terminal, input);
        let snapshot = terminal.snapshot();
        let mut result = String::new();
        for row in 0..snapshot.rows {
            writeln!(&mut result, "row{row}={}", line(&snapshot, row).trim_end()).unwrap();
        }
        writeln!(
            &mut result,
            "cursor={},{}",
            snapshot.cursor.row, snapshot.cursor.column
        )
        .unwrap();
        if let Some(modes) = terminal.hp2392_modes() {
            let lock_row = modes
                .memory_lock_row
                .map_or_else(|| "-".into(), |row| row.to_string());
            writeln!(
                &mut result,
                "modes={},{},{lock_row}",
                modes.block_mode, modes.protect_mode,
            )
            .unwrap();
        } else {
            result.push_str("modes=-\n");
        }
        assert!(!terminal.hp2392_local_input(b"a", Hp2392Input::Text));
        writeln!(&mut result, "replies={}", hex(&terminal.take_responses())).unwrap();
        let keys = [
            TerminalKey::Enter,
            TerminalKey::Tab,
            TerminalKey::Backspace,
            TerminalKey::Function(1),
        ]
        .map(|key| hex(&snapshot.encode_key(key, KeyModifiers::default()).unwrap()));
        writeln!(&mut result, "keys={}", keys.join(",")).unwrap();
        assert_eq!(result, expected, "{profile:?}");
    }
}

const ENTER: Hp2392Input = Hp2392Input::Key { enter: true };
const KEY: Hp2392Input = Hp2392Input::Key { enter: false };

#[test]
fn hp2392a_format_edits_only_fields_and_submits_hidden_data_without_showing_it() {
    let mut terminal = Terminal::with_profile(24, 4, TerminalProfile::Hp2392);
    feed(
        &mut terminal,
        b"\x1b&k1B\x1b&s1HNAME:\x1b[    \x1b] KEEP\r\nPIN: \x1b[\x1b&dS   \x1b&d@\x1b] LOCK\x1bW",
    );
    let screen = terminal.snapshot();
    assert_eq!((screen.cursor.row, screen.cursor.column), (0, 5));
    assert!(terminal.hp2392_local_input(b"Al", Hp2392Input::Text));
    assert!(terminal.hp2392_local_input(b"\x1bD", KEY));
    assert!(terminal.hp2392_local_input(b"\x1bQ", KEY));
    assert!(terminal.hp2392_local_input(b"X", KEY));
    assert!(terminal.hp2392_local_input(b"\x1bQ", KEY));
    assert!(terminal.hp2392_local_input(b"\t", KEY));
    assert!(terminal.hp2392_local_input(b"42", Hp2392Input::Text));
    assert!(terminal.hp2392_local_input(b"\r", Hp2392Input::Text));
    assert!(terminal.take_responses().is_empty());

    let screen = terminal.snapshot();
    assert_eq!(line(&screen, 0).trim_end(), "NAME:AXl  KEEP");
    assert_eq!(line(&screen, 1).trim_end(), "PIN:     LOCK");
    assert!(
        screen.row(1).unwrap()[5..7]
            .iter()
            .all(|cell| cell.style.hidden && cell.text == " ")
    );
    assert!(terminal.search("42", false).matches.is_empty());
    assert!(terminal.hp2392_local_input(b"\x1b[Z", KEY));
    assert!(terminal.hp2392_local_input(b"\r", ENTER));
    assert_eq!(terminal.take_responses(), b"42 \r");

    assert!(terminal.hp2392_local_input(b"\x1bh", KEY));
    assert!(terminal.hp2392_local_input(b"Q", KEY));
    assert_eq!(line(&terminal.snapshot(), 0).trim_end(), "NAME:QXl  KEEP");
    feed(&mut terminal, b"\x1b&k0B\x1bX");
    assert!(!terminal.hp2392_local_input(b"a", KEY));
}

#[test]
fn hp2392a_block_page_and_line_use_documented_host_handshake() {
    let mut terminal = Terminal::with_profile(20, 4, TerminalProfile::Hp2392);
    feed(
        &mut terminal,
        b"\x1b&k1B\x1b&s1DA:\x1b[FIRST\x1b] N\r\nB:\x1b{SECOND\x1b] M\x1bW",
    );
    assert!(terminal.hp2392_local_input(b"\r", ENTER));
    assert!(terminal.hp2392_local_input(b"x", KEY));
    assert!(terminal.take_responses().is_empty());
    feed(&mut terminal, b"\x11");
    assert_eq!(terminal.take_responses(), b"\x12");
    feed(&mut terminal, b"\x11");
    assert_eq!(terminal.take_responses(), b"FIRST\x1fSECOND\x1e");

    feed(&mut terminal, b"\x1b&s0D\x1bW\x11");
    assert!(terminal.take_responses().is_empty());
    assert!(terminal.hp2392_local_input(b"\r", ENTER));
    assert_eq!(terminal.take_responses(), b"\x12\r");
    feed(&mut terminal, b"\x11");
    assert_eq!(terminal.take_responses(), b"FIRST\r");
}

#[test]
fn hp2392a_clear_resize_and_mode_exit_do_not_submit_stale_form_data() {
    let mut terminal = Terminal::with_profile(20, 4, TerminalProfile::Hp2392);
    feed(&mut terminal, b"\x1b&k1BP:\x1b[abc\x1b] KEEP\x1bW\x1bK");
    assert_eq!(line(&terminal.snapshot(), 0).trim_end(), "P:    KEEP");
    assert!(terminal.hp2392_local_input(b"\r", ENTER));
    feed(&mut terminal, b"\x11\x1b&k0B\x11");
    assert_eq!(terminal.take_responses(), b"\x12\r");
    assert!(!terminal.hp2392_local_input(b"\r", ENTER));
    feed(&mut terminal, b"\x1b&k1B\x1b&s1H");
    terminal.resize(19, 4);
    assert!(terminal.hp2392_local_input(b"\r", ENTER));
    assert_eq!(terminal.take_responses(), b"\x1e\r");
}

#[test]
fn hp2392a_field_wrap_and_record_terminator_follow_the_manual() {
    let mut terminal = Terminal::with_profile(6, 2, TerminalProfile::Hp2392);
    feed(
        &mut terminal,
        b"\x1b&k1B\x1b&s1HF:\x1b[  \x1b] !\r\nLAST:\x1b[\x1bW",
    );
    assert!(terminal.hp2392_local_input(b"12X3", Hp2392Input::Text));
    assert_eq!(line(&terminal.snapshot(), 0), "F:32 !");
    assert_eq!(line(&terminal.snapshot(), 1), "LAST:X");

    feed(&mut terminal, b"\x1bW\x1bC\x1e\x1bW\x1b&k1A");
    assert!(terminal.hp2392_local_input(b"\r", ENTER));
    assert_eq!(terminal.take_responses(), b"3\x1e\r\n");
    let cursor = terminal.snapshot().cursor;
    assert_eq!((cursor.row, cursor.column), (0, 4));
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(2 * bytes.len());
    for byte in bytes {
        write!(&mut output, "{byte:02x}").unwrap();
    }
    output
}

#[test]
fn malformed_sequences_and_recursive_softkeys_recover_without_host_side_effects() {
    let mut terminal = Terminal::with_profile(16, 4, TerminalProfile::Hp70092);
    terminal.set_clipboard_protocol_enabled(true);
    feed(&mut terminal, b"\x1b&f1a2L\x1bp\x1bpOK");
    feed(
        &mut terminal,
        b"\x1b]52;c;Zm9v\x07\x1b]5522;type=read\x1b\\",
    );
    feed(&mut terminal, b"\x1b)\x1b[");
    for _ in 0..300 {
        terminal.advance(b"9");
    }
    feed(&mut terminal, b"m\x1b&a1c1R\xc3\xa9!\x1b&a12\x18X");
    let screen = terminal.snapshot();
    assert_eq!(line(&screen, 0).trim_end(), "OK");
    assert_eq!(line(&screen, 1).trim_end(), " é!X");
    assert!(!screen.row(1).unwrap()[1].style.bold);
    assert!(terminal.take_responses().is_empty());
    assert!(terminal.take_clipboard_requests().is_empty());
}

#[test]
fn hp_keys_are_model_specific_and_do_not_fall_back_to_xterm_bytes() {
    let hp2392 = Terminal::with_profile(80, 24, TerminalProfile::Hp2392).snapshot();
    let hp70092 = Terminal::with_profile(80, 24, TerminalProfile::Hp70092).snapshot();
    let shift = KeyModifiers {
        shift: true,
        ..KeyModifiers::default()
    };
    for (key, expected) in [
        (TerminalKey::Up, b"\x1bA".as_slice()),
        (TerminalKey::End, b"\x1bF"),
        (TerminalKey::PageDown, b"\x1bT"),
        (TerminalKey::Enter, b"\r"),
        (TerminalKey::Backspace, b"\x08"),
        (TerminalKey::Keypad(2), b"2"),
        (TerminalKey::Function(8), b"\x1bw"),
    ] {
        assert_eq!(
            hp2392.encode_key(key, KeyModifiers::default()).as_deref(),
            Some(expected)
        );
        assert_eq!(
            hp70092.encode_key(key, KeyModifiers::default()).as_deref(),
            Some(expected)
        );
    }
    assert_eq!(
        hp2392
            .encode_key(TerminalKey::Function(8), shift)
            .as_deref(),
        Some(b"\x1bw".as_slice())
    );
    assert_eq!(
        hp70092
            .encode_key(TerminalKey::Function(8), shift)
            .as_deref(),
        Some(b"\x1bW".as_slice())
    );
    assert_eq!(
        hp70092.encode_key(TerminalKey::Tab, shift).as_deref(),
        Some(b"\x1b[Z".as_slice())
    );
    assert!(
        hp2392
            .encode_key(TerminalKey::Function(13), KeyModifiers::default())
            .is_none()
    );
}
