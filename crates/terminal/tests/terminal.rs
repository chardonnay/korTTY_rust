use kortty_terminal::{Color, CursorShape, ScreenSnapshot, Terminal, TerminalProfile};

fn text(cells: &[kortty_terminal::Cell]) -> String {
    cells
        .iter()
        .filter(|cell| cell.width != 0)
        .map(|cell| {
            if cell.text.is_empty() {
                " "
            } else {
                &cell.text
            }
        })
        .collect()
}

fn row(snapshot: &ScreenSnapshot, index: u16) -> String {
    text(snapshot.row(index).expect("row in grid"))
}

#[test]
fn split_utf8_and_escape_sequences_keep_state_across_reads() {
    let mut terminal = Terminal::new(8, 2);
    let data = "\x1b[1;38;2;12;34;56;48:5:201m界a\u{301}\x1b[0m!".as_bytes();
    for byte in data {
        terminal.advance(std::slice::from_ref(byte));
    }

    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "界a\u{301}!    ");
    assert_eq!(screen.cells[0].width, 2);
    assert_eq!(screen.cells[1].width, 0);
    assert!(screen.cells[0].style.bold);
    assert_eq!(screen.cells[0].style.foreground, Color::Rgb(12, 34, 56));
    assert_eq!(screen.cells[0].style.background, Color::Indexed(201));
    assert_eq!(screen.cells[2].text, "a\u{301}");
    assert_eq!(screen.cells[3].style.foreground, Color::Default);
}

#[test]
fn alternate_screen_restores_primary_cells_cursor_and_input_modes() {
    let mut terminal = Terminal::new(10, 2);
    terminal.advance(b"hello\x1b[?1049h\x1b[?1;2004hworld\x1b[?25l\x1b[6 q");

    let alternate = terminal.snapshot();
    assert!(alternate.alternate_screen);
    assert!(terminal.application_cursor_keys());
    assert!(terminal.bracketed_paste());
    assert!(!alternate.cursor.visible);
    assert_eq!(alternate.cursor_shape, CursorShape::Bar);
    assert!(!alternate.cursor_blinking);
    assert_eq!(row(&alternate, 0), "     world");
    assert_eq!(alternate.scrollback_lines, 0);

    terminal.advance(b"\x1b[?1049l\x1b[?25h\x1b[?1;2004l!");
    let primary = terminal.snapshot();
    assert!(!primary.alternate_screen);
    assert!(primary.cursor.visible);
    assert!(!terminal.application_cursor_keys());
    assert!(!terminal.bracketed_paste());
    assert_eq!(row(&primary, 0), "hello!    ");
}

#[test]
fn ansi_alternate_pages_carry_position_and_pending_wrap() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-alternate-cursor.bin"),
        include_str!("fixtures/sithtermfx/dec-alternate-cursor.expected.txt"),
    );
}

#[test]
fn ansi_alternate_pages_share_the_saved_cursor() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-alternate-saved-cursor.bin"),
        include_str!("fixtures/sithtermfx/dec-alternate-saved-cursor.expected.txt"),
    );
}

#[test]
fn dec_cursor_restore_round_trips_autowrap_and_pending_wrap() {
    let cases: [(&[u8], [&str; 2]); 2] = [
        (b"ABCD\x1b7\x1b[?7l\x1b8!", ["ABCD", "!   "]),
        (b"\x1b[?7lABCD\x1b7\x1b[?7h\x1b8!", ["!BCD", "    "]),
    ];
    for profile in [TerminalProfile::Xterm, TerminalProfile::Vt220] {
        for (bytes, expected) in cases {
            for split in 0..=bytes.len() {
                let mut terminal = Terminal::with_profile(4, 2, profile);
                terminal.advance(&bytes[..split]);
                terminal.advance(&bytes[split..]);
                let screen = terminal.snapshot();
                assert_eq!(
                    [row(&screen, 0), row(&screen, 1)],
                    expected,
                    "profile {profile:?}, split at {split}"
                );
            }
        }
    }
}

#[test]
fn dec_cursor_restore_preserves_autowrap_without_a_save_and_shares_the_saved_slot() {
    let mut terminal = Terminal::new(4, 2);
    terminal.advance(b"\x1b[?7l\x1b[2;2H\x1b8ABCDE");
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "EBCD");
    assert_eq!(row(&screen, 1), "    ");

    let mut terminal = Terminal::new(4, 2);
    terminal.advance(b"ABCD\x1b[?1049h\x1b[?7l\x1b7\x1b[?1049l!");
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "!BCD");
    assert_eq!(row(&screen, 1), "    ");
}

#[test]
fn scroll_regions_restrict_scrolling_without_restricting_absolute_cursor() {
    let mut terminal = Terminal::new(6, 5);
    terminal.advance(b"top\x1b[5;1Hbottom\x1b[2;4r\x1b[1;1HHEAD\x1b[4;1Hlower\r\nnew");
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "HEAD  ");
    assert_eq!(row(&screen, 2), "lower ");
    assert_eq!(row(&screen, 3), "new   ");
    assert_eq!(row(&screen, 4), "bottom");

    terminal.advance(b"\x1b[?6h\x1b[1;1Horigin\x1b[20;1H");
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 1), "origin");
    assert_eq!(screen.cursor.row, 3);
}

#[test]
fn horizontal_margin_scroll_keeps_its_count_rules() {
    let mut terminal = Terminal::with_profile(8, 4, TerminalProfile::Vt420);
    terminal.advance(
        b"ABCDEFGH\x1b[2;1HIJKLMNOP\x1b[3;1HQRSTUVWX\x1b[4;1HYZabcdef\x1b[?69h\x1b[2;5s\x1b[2;3r\x1b[0S",
    );
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 1), "IRSTUNOP");
    assert_eq!(row(&screen, 2), "Q    VWX");

    terminal.advance(b"\x1b[;2T");
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "ABCDEFGH");
    assert_eq!(row(&screen, 1), "I    NOP");
    assert_eq!(row(&screen, 2), "QRSTUVWX");
    assert_eq!(row(&screen, 3), "YZabcdef");
    assert_eq!(screen.scrollback_lines, 0);
}

#[test]
fn escape_f_moves_to_the_lower_left_and_cancels_pending_wrap() {
    let cases: [(&[u8], u16, &[u8]); 4] = [
        (b"\x1b[2;3H\x1bF\x1b[6nZ", 3, b"\x1b[4;1R"),
        (b"\x1b[2;3r\x1b[1;4H\x1bF\x1b[6nZ", 3, b"\x1b[4;1R"),
        (b"\x1b[2;3r\x1b[?6h\x1b[1;4H\x1bFZ", 2, b""),
        (b"ABCDE\x1bF\x1b[6nZ", 3, b"\x1b[4;1R"),
    ];
    for profile in [TerminalProfile::Xterm, TerminalProfile::Vt220] {
        for (bytes, target_row, response) in cases {
            for split in 0..=bytes.len() {
                let mut terminal = Terminal::with_profile(5, 4, profile);
                terminal.advance(&bytes[..split]);
                terminal.advance(&bytes[split..]);
                let screen = terminal.snapshot();
                let context = format!("{profile:?}, {bytes:?}, split at {split}");
                assert_eq!(row(&screen, target_row), "Z    ", "{context}");
                assert_eq!(screen.cursor.row, target_row, "{context}");
                assert_eq!(screen.cursor.column, 1, "{context}");
                assert_eq!(screen.scrollback_lines, 0, "{context}");
                assert_eq!(terminal.take_responses(), response, "{context}");
            }
        }
    }
}

#[test]
fn screen_alignment_fills_the_visible_grid_independently_of_wrap_and_origin_modes() {
    let cases: [(&[u8], usize); 2] = [
        (b"\x1b[?7l\x1b[1m\x1b[3;4H\x1b#8", 0),
        (
            b"hist\r\nA\r\nB\r\nC\r\nD\x1b[2;3r\x1b[?6h\x1b[2;3H\x1b[1m\x1b#8",
            1,
        ),
    ];
    for profile in [TerminalProfile::Xterm, TerminalProfile::Vt220] {
        for (bytes, history) in cases {
            for split in 0..=bytes.len() {
                let mut terminal = Terminal::with_profile(5, 4, profile);
                terminal.advance(&bytes[..split]);
                terminal.advance(&bytes[split..]);
                let screen = terminal.snapshot();
                assert!(
                    screen
                        .cells
                        .iter()
                        .all(|cell| cell.text == "E" && cell.width == 1 && cell.style.bold),
                    "{profile:?}, split at {split}, history={history}"
                );
                assert_eq!(screen.scrollback_lines, history);
                if history != 0 {
                    assert_eq!(row(&terminal.viewport_snapshot(history), 0), "hist ");
                }
            }
        }
    }
}

#[test]
fn screen_alignment_preserves_the_cursor_and_pending_wrap_without_repeating_the_pattern() {
    let cases = [
        (
            b"Q\x1b[3;4H\x1b#8\x1b[2b\x1b[6n".as_slice(),
            (2, 3),
            b"\x1b[3;4R",
            2,
            "EEE!E",
        ),
        (
            b"ABCDE\x1b#8\x1b[2b\x1b[6n".as_slice(),
            (0, 4),
            b"\x1b[1;6R",
            1,
            "!EEEE",
        ),
        (
            b"\x1b[?7lABCDE\x1b#8\x1b[2b\x1b[6n".as_slice(),
            (0, 4),
            b"\x1b[1;6R",
            0,
            "!EEEE",
        ),
    ];
    for profile in [TerminalProfile::Xterm, TerminalProfile::Vt220] {
        for (bytes, cursor, reply, next_row, expected) in cases {
            for split in 0..=bytes.len() {
                let mut terminal = Terminal::with_profile(5, 4, profile);
                terminal.advance(&bytes[..split]);
                terminal.advance(&bytes[split..]);
                let screen = terminal.snapshot();
                assert!(screen.cells.iter().all(|cell| cell.text == "E"));
                assert_eq!(
                    (screen.cursor.row, screen.cursor.column),
                    cursor,
                    "{profile:?}, split at {split}"
                );
                assert_eq!(terminal.take_responses(), reply);

                terminal.advance(b"!");
                assert_eq!(row(&terminal.snapshot(), next_row), expected);
            }
        }
    }

    let mut terminal = Terminal::new(5, 4);
    terminal.advance(b"\x1b#8Q\x1b[2b");
    assert_eq!(row(&terminal.snapshot(), 0), "QQQEE");
}

#[test]
fn screen_alignment_pattern_rows_stay_independent_when_reflowed() {
    let mut terminal = Terminal::new(5, 4);
    terminal.advance(b"A\r\nB\x1b#8");
    terminal.resize(6, 4);
    let screen = terminal.snapshot();
    assert_eq!(screen.scrollback_lines, 0);
    for index in 0..screen.rows {
        assert_eq!(row(&screen, index), "EEEEE ");
    }
}

#[test]
fn screen_alignment_preserves_existing_soft_wraps_and_fills_their_padding() {
    let mut terminal = Terminal::new(5, 4);
    terminal.advance("1234界\x1b#8".as_bytes());
    let result = terminal.search("EEEEEEEEEE", false);
    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].start.row, 0);
    assert_eq!(result.matches[0].end.row, 1);
}

#[test]
fn resize_reflows_soft_wrap_and_keeps_the_cursor_after_the_content() {
    let mut terminal = Terminal::new(4, 3);
    terminal.advance(b"abcdefg");
    terminal.resize(8, 3);
    terminal.advance(b"h!");
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "abcdefgh");
    assert_eq!(row(&screen, 1), "!       ");

    let mut pending_wrap = Terminal::new(4, 3);
    pending_wrap.advance(b"abcd");
    pending_wrap.resize(5, 3);
    pending_wrap.advance(b"!");
    assert_eq!(row(&pending_wrap.snapshot(), 0), "abcd!");
}

#[test]
fn resizing_drops_padding_before_a_wrapped_wide_glyph() {
    let mut terminal = Terminal::new(5, 3);
    terminal.advance("abcd界".as_bytes());
    terminal.resize(8, 3);
    terminal.advance(b"!");
    assert_eq!(row(&terminal.snapshot(), 0), "abcd界! ");
}

#[test]
fn resizing_the_alternate_screen_reflows_the_primary_cursor() {
    let mut terminal = Terminal::new(4, 3);
    terminal.advance(b"abcde\x1b[?1049h");
    terminal.resize(8, 3);
    terminal.advance(b"\x1b[?1049l!");
    assert_eq!(row(&terminal.snapshot(), 0), "abcde!  ");
}

#[test]
fn resizing_keeps_a_later_cursor_saved_in_the_alternate_page() {
    let mut terminal = Terminal::new(4, 3);
    terminal.advance(b"abcde\x1b[?1049h\x1b[3;2H\x1b7");
    terminal.resize(8, 3);
    terminal.advance(b"\x1b[?1049l!");
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "abcde   ");
    assert_eq!(row(&screen, 2), " !      ");
}

#[test]
fn writing_over_a_continuation_or_erasing_it_clears_the_entire_glyph() {
    let mut terminal = Terminal::new(5, 2);
    terminal.advance("A界Z\x1b[3GX".as_bytes());
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "A XZ ");
    assert_eq!(screen.cells[1].width, 1);
    assert_eq!(screen.cells[2].width, 1);

    terminal.advance("\r界\x1b[2G\x1b[X".as_bytes());
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "  XZ ");
    assert_eq!(screen.cells[0].width, 1);
    assert_eq!(screen.cells[1].width, 1);
}

#[test]
fn history_is_oldest_first_and_preserved_by_soft_reset() {
    let mut terminal = Terminal::new(4, 2);
    terminal.advance(b"one\r\ntwo\r\ntri\r\nfour");
    assert_eq!(terminal.snapshot().scrollback_lines, 2);
    assert_eq!(
        text(
            terminal
                .history_line(0)
                .expect("first history line")
                .as_ref()
        ),
        "one "
    );
    assert_eq!(
        text(
            terminal
                .history_line(1)
                .expect("second history line")
                .as_ref()
        ),
        "two "
    );

    terminal.advance(b"\x1b[!p");
    assert_eq!(terminal.snapshot().scrollback_lines, 2);
    assert_eq!(row(&terminal.snapshot(), 0), "    ");
    terminal.advance(b"\x1bc");
    assert_eq!(terminal.snapshot().scrollback_lines, 0);
}

#[test]
fn explicit_dec_modes_keep_their_reset_rules() {
    let mut terminal = Terminal::with_profile(10, 3, TerminalProfile::Vt420);
    terminal.advance(b"visible\x1b[?69h\x1b[2;9s\x1b[!p\x1b[1;10HX");
    assert_eq!(row(&terminal.snapshot(), 0), "visible  X");

    let mut terminal = Terminal::with_profile(10, 3, TerminalProfile::Vt220);
    terminal.advance(b"\x1b[61;1\"pvisible\x1b[!p");
    assert_eq!(row(&terminal.snapshot(), 0), "visible   ");

    let mut terminal = Terminal::with_profile(10, 3, TerminalProfile::Vt320);
    terminal.advance(b"\x1b[62;2\"pvisible\x9b!p\x9b6n");
    assert_eq!(row(&terminal.snapshot(), 0), "          ");
    assert_eq!(terminal.take_responses(), b"\x9b1;1R");
}

#[test]
fn malformed_control_strings_are_bounded_and_output_resumes() {
    let mut terminal = Terminal::new(10, 2);
    terminal.advance(b"\x1b]2;old\x07\x1b]2;");
    for _ in 0..75 {
        terminal.advance(&[b'a'; 1024]);
    }
    terminal.advance(b"\x07\x1b[");
    for _ in 0..100 {
        terminal.advance(b"99999;");
    }
    terminal.advance(b"0mnext");

    let screen = terminal.snapshot();
    assert_eq!(screen.title, "old");
    assert_eq!(row(&screen, 0), "next      ");
}

#[test]
fn reports_and_metadata_are_separate_from_visible_output() {
    let mut terminal = Terminal::new(6, 4);
    terminal
        .advance("\x1b[2;3H\x1b[5n\x1b[6n\x1b[c\x1b[18t\x1b]2;title\u{202e};safe\x07".as_bytes());
    terminal.advance(b"\x1b]8;;https://example.test/a;b\x1b\\x\x1b]8;;\x1b\\y");
    let screen = terminal.snapshot();
    // SithTermFX treats the next semicolon as the end of the title argument.
    assert_eq!(screen.title, "title");
    assert_eq!(row(&screen, 1), "  xy  ");
    assert_eq!(
        screen.row(1).unwrap()[2].hyperlink.as_deref(),
        Some("https://example.test/a;b")
    );
    assert_eq!(screen.row(1).unwrap()[3].hyperlink, None);
    assert_eq!(
        terminal.take_responses(),
        b"\x1b[0n\x1b[2;3R\x1b[?6c\x1b[8;4;6t"
    );
    assert!(terminal.take_responses().is_empty());
}

fn replay_fixture(input: &str, expected: &str, columns: u16, rows: u16) -> ScreenSnapshot {
    replay_profile_fixture(TerminalProfile::Xterm, input, expected, columns, rows)
}

fn replay_profile_fixture(
    profile: TerminalProfile,
    input: &str,
    expected: &str,
    columns: u16,
    rows: u16,
) -> ScreenSnapshot {
    let mut terminal = Terminal::with_profile(columns, rows, profile);
    let input = input.replace("\r\n", "\n").replace('\n', "\r\n");
    for chunk in input.as_bytes().chunks(7) {
        terminal.advance(chunk);
    }

    let screen = terminal.snapshot();
    let expected = expected.replace("\r\n", "\n").replace('\r', "\n");
    let expected_rows = expected.lines().map(str::trim_end).collect::<Vec<_>>();
    let actual_rows = (0..rows)
        .map(|index| row(&screen, index))
        .collect::<Vec<_>>();
    let actual_rows = actual_rows
        .iter()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>();
    assert_eq!(actual_rows, expected_rows);
    screen
}

#[test]
fn replays_sithtermfx_cursor_positioning() {
    replay_fixture(
        include_str!("fixtures/sithtermfx/testSetCursorPosition.txt"),
        "X00\n0X \nX X\n   \n",
        3,
        4,
    );
}

#[test]
fn erase_and_delete_counts_are_clamped_to_the_right_margin() {
    let mut terminal = Terminal::new(4, 2);
    terminal.advance(b"ABCD\r\nWXYZ\x1b[1;3H\x1b[65535X\x1b[2;3H\x1b[65535P");
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "AB  ");
    assert_eq!(row(&screen, 1), "WX  ");
}

#[test]
fn replays_sithtermfx_system_commands() {
    replay_fixture(
        include_str!("fixtures/sithtermfx/testSystemCommands.txt"),
        include_str!("fixtures/sithtermfx/testSystemCommands.after.txt"),
        30,
        3,
    );
}

#[test]
fn replays_sithtermfx_midnight_commander() {
    let snapshot = replay_fixture(
        include_str!("fixtures/sithtermfx/testMidnightCommanderOnXTerm.txt"),
        include_str!("fixtures/sithtermfx/testMidnightCommanderOnXTerm.after.txt"),
        80,
        24,
    );
    let check_style = |column: usize, row: u16, foreground, background| {
        let cell = &snapshot.row(row).unwrap()[column];
        assert_eq!(cell.style.foreground, Color::Indexed(foreground));
        assert_eq!(cell.style.background, Color::Indexed(background));
    };
    check_style(8, 2, 3, 4);
    check_style(23, 4, 7, 4);
    check_style(2, 0, 0, 6);
}

#[test]
fn petscii_decodes_raw_graphics_and_switches_character_modes() {
    let profile = TerminalProfile::Petscii;
    let (columns, rows) = profile.default_size();
    let mut terminal = Terminal::with_profile(columns, rows, profile);
    terminal.advance(b"AZ\x60\x61\x5c\x7e\xbc\xff\x0eAZaz\x8eAZ");

    let screen = terminal.snapshot();
    assert_eq!((screen.columns, screen.rows), (40, 25));
    assert_eq!(profile.term_name(), "petscii");
    assert_eq!(screen.profile, profile);
    assert_eq!(row(&screen, 0).trim_end(), "AZ─♠£π█πazAZAZ");
    assert_eq!(screen.cells[2].width, 1);
    assert_eq!(screen.cells[6].style.foreground, Color::Indexed(14));
    assert!(terminal.take_responses().is_empty());
}

#[test]
fn petscii_applies_editing_cursor_color_and_reverse_controls() {
    let mut terminal = Terminal::with_profile(10, 3, TerminalProfile::Petscii);
    terminal.advance(b"ABC\x9d\x14\x94\x12\x1cX\x92\x9fY\x0d\x11Z");
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "AXY       ");
    assert_eq!(row(&screen, 2), "Z         ");
    assert_eq!(screen.cells[1].style.foreground, Color::Indexed(2));
    assert!(screen.cells[1].style.inverse);
    assert_eq!(screen.cells[2].style.foreground, Color::Indexed(3));
    assert!(!screen.cells[2].style.inverse);

    terminal.advance(b"\x12\x93Q");
    let cleared = terminal.snapshot();
    assert_eq!(row(&cleared, 0), "Q         ");
    assert_eq!(row(&cleared, 2), "          ");
    assert!(!cleared.cells[0].style.inverse);
}

#[test]
fn petscii_keys_and_text_are_encoded_for_the_current_mode() {
    use kortty_terminal::TerminalKey;

    let profile = TerminalProfile::Petscii;
    assert_eq!(profile.encode_legacy_key(TerminalKey::Up), Some(0x91));
    assert_eq!(profile.encode_legacy_key(TerminalKey::Left), Some(0x9d));
    assert_eq!(profile.encode_legacy_key(TerminalKey::Enter), Some(0x0d));
    assert_eq!(
        profile.encode_legacy_key(TerminalKey::Function(8)),
        Some(0x8c)
    );
    assert_eq!(profile.encode_legacy_key(TerminalKey::Function(9)), None);
    assert_eq!(
        TerminalProfile::Xterm.encode_legacy_key(TerminalKey::Left),
        None
    );

    let mut terminal = Terminal::with_profile(10, 3, profile);
    assert_eq!(terminal.snapshot().encode_text("Aa£\r\n界"), b"AA\\\r?");
    terminal.advance(b"\x0e");
    assert_eq!(terminal.snapshot().encode_text("Aa£\n界"), b"aA\\\r?");
    assert_eq!(
        Terminal::new(10, 3).snapshot().encode_text("Aa£"),
        "Aa£".as_bytes()
    );
}

#[test]
fn shell_marks_capture_the_cursor_at_their_exact_stream_position() {
    use kortty_terminal::{ShellEventKind, ShellPosition};

    let data = b"\x1b]133;A;aid=shell\x1b\\p> \x1b]133;B\x07ls\r\n\x9d133;C\x9cdone\x1b]133;D;cl=x;-7\x07\x1b]7;file://Web01.Example.com/srv/My%20App\x1b\\";
    let expected = [
        (ShellEventKind::PromptStart, 0, 0),
        (ShellEventKind::CommandStart, 0, 3),
        (ShellEventKind::OutputStart, 1, 0),
        (
            ShellEventKind::CommandFinished {
                exit_status: Some(-7),
            },
            1,
            4,
        ),
        (
            ShellEventKind::WorkingDirectory {
                host: "web01.example.com".to_owned(),
                path: "/srv/My App".to_owned(),
            },
            1,
            4,
        ),
    ];

    for chunk_size in [1, 7, data.len()] {
        let mut terminal = Terminal::new(10, 3);
        for bytes in data.chunks(chunk_size) {
            terminal.advance(bytes);
        }
        let events = terminal.take_shell_events();
        assert_eq!(events.len(), expected.len());
        for (event, (kind, row, column)) in events.iter().zip(&expected) {
            assert_eq!(&event.kind, kind);
            assert_eq!(
                event.position,
                ShellPosition {
                    row: *row,
                    column: *column,
                    scrollback_lines: 0,
                    alternate_screen: false,
                }
            );
        }
        assert_eq!(row(&terminal.snapshot(), 0), "p> ls     ");
        assert_eq!(row(&terminal.snapshot(), 1), "done      ");
        assert!(terminal.take_shell_events().is_empty());
        assert!(terminal.take_responses().is_empty());
    }
}

#[test]
fn canceled_and_nested_osc_do_not_forge_events_or_metadata() {
    use kortty_terminal::ShellEventKind;

    let mut terminal = Terminal::new(20, 3);
    let data = b"\x1b]133;A\x18a\x1b]133;B\x1ab\x1b]133;C\x1b\x07c\x1b]2;bad\x1b]133;D;2\x07d\x1bPtmux;\x1b\x1b]133;A\x1b\x1b\\\x1b]133;B\x07\x1b\\e\x1b_\x1b]133;C\x07\x1b\\f\xc2\x9d133;D;aid=1;2147483648;3\xc2\x9c\xc3\x9c\xc3\x9d";
    for bytes in data.chunks(2) {
        terminal.advance(bytes);
    }
    let events = terminal.take_shell_events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].kind,
        ShellEventKind::CommandFinished {
            exit_status: Some(3)
        }
    );
    assert_eq!(events[0].position.column, 6);
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0).trim_end(), "abcdefÜÝ");
    assert!(screen.title.is_empty());
}

#[test]
fn osc_directory_rejects_dangerous_syntax_and_keeps_encoded_path_characters() {
    use kortty_terminal::ShellEventKind;

    let mut terminal = Terminal::new(10, 3);
    terminal.advance(
        b"\x1b]7;https://example.test/tmp\x07\x1b]7;file://user@host/tmp\x07\x1b]7;file:///bad%00\x07\x1b]7;file:///bad%9x\x07\x1b]7;file:///bad?query\x07\x1b]7;file://[::1]/a%23b%3F%20%c3%a4\x07",
    );
    let events = terminal.take_shell_events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].kind,
        ShellEventKind::WorkingDirectory {
            host: "[::1]".to_owned(),
            path: "/a#b? ä".to_owned(),
        }
    );
    assert_eq!(row(&terminal.snapshot(), 0), "          ");
}

#[test]
fn oversized_payloads_and_shell_event_floods_are_bounded() {
    use kortty_terminal::ShellEventKind;

    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b]133;A;");
    terminal.advance(&[b'x'; 4096]);
    terminal.advance(b"\x07");
    assert!(terminal.take_shell_events().is_empty());

    for status in 0..200 {
        terminal.advance(format!("\x1b]133;D;{status}\x07").as_bytes());
    }
    let events = terminal.take_shell_events();
    assert_eq!(events.len(), 128);
    assert_eq!(
        events[0].kind,
        ShellEventKind::CommandFinished {
            exit_status: Some(72)
        }
    );
    assert_eq!(
        events[127].kind,
        ShellEventKind::CommandFinished {
            exit_status: Some(199)
        }
    );
}

#[test]
fn replays_sithtermfx_line_feed_below_and_at_the_scroll_margin() {
    // Captures the assertions in SithTermFX v1.2.7 ScrollingTest. The status
    // line outside the region must survive both line feeds.
    let mut terminal = Terminal::new(5, 4);
    terminal.advance(b"\x1b[1;3r\x1b[1;1Ha\x1b[2;1Hb\x1b[3;1Hc\x1b[4;1Hz\n");
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "a    ");
    assert_eq!(row(&screen, 1), "b    ");
    assert_eq!(row(&screen, 2), "c    ");
    assert_eq!(row(&screen, 3), "z    ");
    assert_eq!(screen.scrollback_lines, 0);
    assert_eq!((screen.cursor.column, screen.cursor.row), (1, 3));

    terminal.advance(b"\x1b[3;2H\n");
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "b    ");
    assert_eq!(row(&screen, 1), "c    ");
    assert_eq!(row(&screen, 2), "     ");
    assert_eq!(row(&screen, 3), "z    ");
    assert_eq!((screen.cursor.column, screen.cursor.row), (1, 2));
}

#[test]
fn reports_measured_pixel_sizes_without_inventing_unknown_values() {
    let mut terminal = Terminal::new(80, 25);
    terminal.advance(b"\x1b[14t\x1b[16t\x1b[18t");
    assert_eq!(terminal.take_responses(), b"\x1b[8;25;80t");

    // A fractional pitch after DPI scaling may round differently for the cell
    // and its whole row, so the measured area takes precedence.
    terminal.set_pixel_size(9, 18, 725, 451);
    terminal.advance(b"\x1b[14t\x1b[16t\x1b[18t");
    assert_eq!(
        terminal.take_responses(),
        b"\x1b[4;451;725t\x1b[6;18;9t\x1b[8;25;80t"
    );

    terminal.set_pixel_size(10, 20, 0, 0);
    terminal.advance(b"\x1bc\x1b[14t\x1b[16t");
    assert_eq!(terminal.take_responses(), b"\x1b[6;20;10t");
}

#[test]
fn vt100_reports_its_identity_and_renders_dec_graphics_in_monochrome() {
    let profile = TerminalProfile::Vt100;
    let (columns, rows) = profile.default_size();
    let mut terminal = Terminal::with_profile(columns, rows, profile);
    terminal.advance(b"\x1b[0c\x1bZ\x1b[1;31;44m\x1b(0lqk\x1b(B!\x1b[0m");

    let screen = terminal.snapshot();
    assert_eq!(screen.profile, TerminalProfile::Vt100);
    assert_eq!(profile.term_name(), "vt100");
    assert_eq!(row(&screen, 0).trim_end(), "┌─┐!");
    assert!(screen.cells[0].style.bold);
    assert_eq!(screen.cells[0].style.foreground, Color::Default);
    assert_eq!(screen.cells[0].style.background, Color::Default);
    assert_eq!(terminal.take_responses(), b"\x1b[?1;2c\x1b[?1;2c");
    assert_eq!(profile.vt_function_key(1), Some(b"\x1bOP".as_slice()));
    assert_eq!(profile.vt_function_key(4), Some(b"\x1bOS".as_slice()));
    assert_eq!(profile.vt_function_key(5), None);
    assert_eq!(TerminalProfile::Xterm.vt_function_key(1), None);
}

#[test]
fn vt100_replays_sithtermfx_scrolling_and_origin_mode_captures() {
    replay_profile_fixture(
        TerminalProfile::Vt100,
        include_str!("fixtures/sithtermfx/vttest/Test2_Screen/7.txt"),
        include_str!("fixtures/sithtermfx/vttest/Test2_Screen/7.after.txt"),
        80,
        24,
    );
    replay_profile_fixture(
        TerminalProfile::Vt100,
        include_str!("fixtures/sithtermfx/vttest/Test2_Screen/11.txt"),
        include_str!("fixtures/sithtermfx/vttest/Test2_Screen/11.after.txt"),
        80,
        24,
    );
}

#[test]
fn dec_column_mode_clears_only_the_active_display_at_the_host_grid_size() {
    for profile in [TerminalProfile::Xterm, TerminalProfile::Vt100] {
        let mut terminal = Terminal::with_profile(12, 4, profile);
        terminal.advance(b"history\r\none\r\ntwo\r\nthree\r\nfour\x1b[2;3r\x1b[?6h\x1b[2;3H");
        assert_eq!(terminal.snapshot().scrollback_lines, 1);

        for mode in [b"\x1b[?3h", b"\x1b[?3l"] {
            for byte in mode {
                terminal.advance(std::slice::from_ref(byte));
            }
            let cleared = terminal.snapshot();
            assert_eq!((cleared.columns, cleared.rows), (12, 4));
            assert_eq!((cleared.cursor.column, cleared.cursor.row), (0, 0));
            assert!((0..4).all(|line| row(&cleared, line).trim_end().is_empty()));
            assert_eq!(cleared.scrollback_lines, 1);
            assert_eq!(row(&terminal.viewport_snapshot(1), 0).trim_end(), "history");
            terminal.advance(b"\x1b[4;1Hset");
            assert_eq!(row(&terminal.snapshot(), 3).trim_end(), "set");
        }

        terminal.advance(b"\x1b[?1049hALT\x1b[?3h");
        assert!(terminal.snapshot().alternate_screen);
        assert!(row(&terminal.snapshot(), 0).trim_end().is_empty());
        terminal.advance(b"\x1b[?1049l");
        assert_eq!(row(&terminal.snapshot(), 3).trim_end(), "set");
    }
}

#[test]
fn vt102_implements_insert_mode_and_identity_beyond_vt100() {
    let mut vt100 = Terminal::with_profile(6, 3, TerminalProfile::Vt100);
    let mut vt102 = Terminal::with_profile(6, 3, TerminalProfile::Vt102);
    let data = b"ABCD\x1b[2G\x1b[4h!\x1b[4l\x1b[c";
    vt100.advance(data);
    vt102.advance(data);
    assert_eq!(row(&vt100.snapshot(), 0), "A!CD  ");
    assert_eq!(row(&vt102.snapshot(), 0), "A!BCD ");
    assert_eq!(vt100.take_responses(), b"\x1b[?1;2c");
    assert_eq!(vt102.take_responses(), b"\x1b[?6c");
    assert_eq!(
        TerminalProfile::Vt102.vt_function_key(4),
        Some(b"\x1bOS".as_slice())
    );
    assert_eq!(TerminalProfile::Vt102.vt_function_key(6), None);
}

#[test]
fn ansi_line_edit_distinguishes_c1_counts_without_accepting_private_commands() {
    let mut terminal = Terminal::with_profile(6, 4, TerminalProfile::Vt220);
    for &byte in b"AAAAAA\x9b2;1HBBBBBB\x9b3;1HCCCCCC\x9b4;1HDDDDDD\x9b2;3H" {
        terminal.advance(&[byte]);
    }
    terminal.advance(b"\x9b;0L\x9b?L");
    assert_eq!(row(&terminal.snapshot(), 1), "BBBBBB");

    terminal.advance(b"\x9bL\x9b;0M\x9b?M");
    let snapshot = terminal.snapshot();
    assert_eq!(row(&snapshot, 1), "      ");
    assert_eq!(row(&snapshot, 2), "BBBBBB");
    assert_eq!(row(&snapshot, 3), "CCCCCC");

    terminal.advance(b"\x9bM");
    let snapshot = terminal.snapshot();
    assert_eq!(row(&snapshot, 1), "BBBBBB");
    assert_eq!(row(&snapshot, 3), "      ");
    assert_eq!((snapshot.cursor.column, snapshot.cursor.row), (2, 1));
}

#[test]
fn vt220_selects_its_conformance_level_and_c1_key_encoding() {
    use kortty_terminal::TerminalKey;

    let mut terminal = Terminal::with_profile(12, 3, TerminalProfile::Vt220);
    for &byte in b"\x9b2;3H\x9b6n\x1b G\x9b?1h\x9b0c" {
        terminal.advance(&[byte]);
    }
    assert_eq!(terminal.take_responses(), b"\x1b[2;3R\x9b?62;1;2;6;7;8;9c");
    let snapshot = terminal.snapshot();
    assert_eq!(
        snapshot.encode_dec_key(TerminalKey::Up),
        Some(b"\x8fA".to_vec())
    );
    assert_eq!(
        snapshot.encode_dec_key(TerminalKey::Function(1)),
        Some(b"\x8fP".to_vec())
    );
    assert_eq!(
        snapshot.encode_dec_key(TerminalKey::Function(20)),
        Some(b"\x9b34~".to_vec())
    );
    assert_eq!(snapshot.encode_text("aäŒ"), b"a\xe4\xd7");

    terminal.advance(b"\x1b[61\"p\x1b G\x1b[c");
    assert_eq!(terminal.take_responses(), b"\x1b[?6c");
    let snapshot = terminal.snapshot();
    assert_eq!(
        snapshot.encode_dec_key(TerminalKey::Up),
        Some(b"\x1b[A".to_vec())
    );
    assert_eq!(snapshot.encode_dec_key(TerminalKey::Function(6)), None);
    assert_eq!(snapshot.encode_text("aäŒ"), b"a??");

    terminal.advance(b"\x1b[62;2\"p\x1b[c\x1b F\x1b[c");
    assert_eq!(
        terminal.take_responses(),
        b"\x9b?62;1;2;6;7;8;9c\x1b[?62;1;2;6;7;8;9c"
    );
}

#[test]
fn vt220_uses_dec_multinational_gl_gr_and_single_shifts() {
    let mut terminal = Terminal::with_profile(20, 3, TerminalProfile::Vt220);
    terminal.advance(b"\xe4\xd7\x1b*0\x8eq\x1b+K\x1bO[\x1b}\xf1x\x1b)R\x0e[\x0f[");
    let snapshot = terminal.snapshot();
    assert_eq!(row(&snapshot, 0).trim_end(), "äŒ─Ä─x°[");
}

#[test]
fn vt220_selective_erase_preserves_protected_characters() {
    let mut terminal = Terminal::with_profile(8, 3, TerminalProfile::Vt220);
    terminal.advance(b"AA\x1b[1\"qXX\x1b[0m\x1b[0\"qBB\x1b[?2K");
    assert_eq!(row(&terminal.snapshot(), 0), "  XX    ");
    terminal.advance(b"\x1b[2K");
    assert_eq!(row(&terminal.snapshot(), 0), "        ");
}

#[test]
fn vt320_supports_its_own_identity_conformance_query_and_latin1_designation() {
    let mut terminal = Terminal::with_profile(20, 3, TerminalProfile::Vt320);
    terminal.advance(b"\x1b[c\x1bP$q\"p\x1b\\\xd7\x1b/A\x1b|\xd7");
    assert_eq!(
        terminal.take_responses(),
        b"\x1b[?63;1;2;6;7;8;9c\x1bP1$r63;1\"p\x1b\\"
    );
    assert_eq!(row(&terminal.snapshot(), 0).trim_end(), "Œ×");
    terminal.advance(b"\x1b[62;2\"p\x90$q\"p\x9c\x9b0c");
    assert_eq!(
        terminal.take_responses(),
        b"\x901$r63;0\"p\x9c\x9b?63;1;2;6;7;8;9c"
    );
    assert_eq!(row(&terminal.snapshot(), 0).trim_end(), "Œ×");

    let mut vt220 = Terminal::with_profile(20, 3, TerminalProfile::Vt220);
    vt220.advance(b"\x1bP$q\"p\x1b\\");
    assert!(vt220.take_responses().is_empty());
}

#[test]
fn newer_dec_profiles_replay_sithtermfx_origin_mode_capture() {
    for profile in [
        TerminalProfile::Vt102,
        TerminalProfile::Vt220,
        TerminalProfile::Vt320,
    ] {
        replay_profile_fixture(
            profile,
            include_str!("fixtures/sithtermfx/vttest/Test2_Screen/11.txt"),
            include_str!("fixtures/sithtermfx/vttest/Test2_Screen/11.after.txt"),
            80,
            24,
        );
    }
}

#[test]
fn ansi_horizontal_tabs_match_the_pinned_reference() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-tabs.bin"),
        include_str!("fixtures/sithtermfx/dec-tabs.expected.txt"),
    );
}

#[test]
fn ansi_csi_tab_movement_preserves_position_and_pending_wrap() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-csi-tab-movement.bin"),
        include_str!("fixtures/sithtermfx/dec-csi-tab-movement.expected.txt"),
    );
}

#[test]
fn vt420_csi_tab_movement_remains_available_with_horizontal_margins() {
    let mut terminal = Terminal::with_profile(16, 5, TerminalProfile::Vt420);
    terminal.advance(b"\x1b[?69h\x1b[2;12s\x1b[3g\x1b[1;4H\x1bH\x1b[1;9H\x1bH\x1b[1;2H");
    terminal.advance(b"\x1b[2I\x1b[6n\x1b[1Z\x1b[6n");
    assert_eq!(terminal.take_responses(), b"\x1b[1;9R\x1b[1;4R");
}

#[test]
fn ansi_backward_motion_counts_from_the_virtual_right_margin() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-backward-motion.bin"),
        include_str!("fixtures/sithtermfx/dec-backward-motion.expected.txt"),
    );
}

#[test]
fn ansi_cursor_left_right_uses_the_first_numeric_count() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-cursor-left-right-parameters.bin"),
        include_str!("fixtures/sithtermfx/dec-cursor-left-right-parameters.expected.txt"),
    );
}

#[test]
fn ansi_relative_position_commands_preserve_position_and_pending_wrap() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-relative-position.bin"),
        include_str!("fixtures/sithtermfx/dec-relative-position.expected.txt"),
    );
}

#[test]
fn vt420_relative_position_commands_remain_available_with_horizontal_margins() {
    let mut terminal = Terminal::with_profile(8, 5, TerminalProfile::Vt420);
    terminal.advance(b"\x1b[?69h\x1b[2;6s\x1b[2;4r\x1b[3;2H\x1b[9a\x1b[6n\x1b[9e\x1b[6n");
    assert_eq!(terminal.take_responses(), b"\x1b[3;6R\x1b[4;6R");

    terminal.advance(b"\x1b[?69l\x1b[3;2H\x1b[9a\x1b[9e\x1b[6n");
    assert_eq!(terminal.take_responses(), b"\x1b[3;2R");
}

#[test]
fn ansi_vertical_motion_retains_the_virtual_right_margin() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-vertical-motion.bin"),
        include_str!("fixtures/sithtermfx/dec-vertical-motion.expected.txt"),
    );
}

#[test]
fn ansi_cursor_up_down_preserves_the_pending_wrap() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-cursor-up-down-wrap.bin"),
        include_str!("fixtures/sithtermfx/dec-cursor-up-down-wrap.expected.txt"),
    );
}

#[test]
fn ansi_cursor_up_down_uses_directional_origin_bounds() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-cursor-up-down-bounds.bin"),
        include_str!("fixtures/sithtermfx/dec-cursor-up-down-bounds.expected.txt"),
    );
}

#[test]
fn ansi_origin_mode_changes_preserve_position_and_pending_wrap() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-origin-mode-cursor.bin"),
        include_str!("fixtures/sithtermfx/dec-origin-mode-cursor.expected.txt"),
    );
}

#[test]
fn ansi_cursor_up_down_preserves_existing_horizontal_margin_behavior() {
    let mut terminal = Terminal::with_profile(8, 5, TerminalProfile::Vt420);
    terminal.advance(b"\x1b[?69h\x1b[2;6s\x1b[2;4r\x1b[3;2HABCDE\x1b[9A\x1b[6nX");
    let screen = terminal.snapshot();
    assert_eq!(terminal.take_responses(), b"\x1b[2;6R");
    assert_eq!(row(&screen, 1), "     X  ");
    assert_eq!(row(&screen, 2), " ABCDE  ");
}

#[test]
fn ansi_soft_reset_retains_custom_and_cleared_tab_stops() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-soft-reset-tabs.bin"),
        include_str!("fixtures/sithtermfx/dec-soft-reset-tabs.expected.txt"),
    );
}

#[test]
fn ansi_erase_to_end_respects_the_virtual_right_margin() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-erase-right-margin.bin"),
        include_str!("fixtures/sithtermfx/dec-erase-right-margin.expected.txt"),
    );
}

#[test]
fn ansi_erase_saved_lines_keeps_the_hidden_primary_page() {
    let mut terminal = Terminal::new(12, 4);
    terminal.advance(b"history\r\nA\r\nB\r\nC\r\nD\x1b[?47h\x1b[1;1Halt\x1b[3J");
    let alternate = terminal.snapshot();
    assert!(alternate.alternate_screen);
    assert!(alternate.cells.iter().all(|cell| cell.text.is_empty()));
    assert_eq!(alternate.scrollback_lines, 0);

    terminal.advance(b"\x1b[?47l");
    let primary = terminal.snapshot();
    assert_eq!(primary.scrollback_lines, 1);
    assert_eq!(row(&primary, 0).trim_end(), "A");
    assert_eq!(row(&primary, 3).trim_end(), "D");
}

#[test]
fn ansi_erase_saved_lines_keeps_visible_text_on_one_row() {
    let mut terminal = Terminal::new(12, 1);
    terminal.advance(b"history\r\nA");
    assert_eq!(terminal.snapshot().scrollback_lines, 1);

    terminal.advance(b"\x1b[3J\x1b[6nB");
    let screen = terminal.snapshot();
    assert_eq!(screen.scrollback_lines, 0);
    assert_eq!(row(&screen, 0).trim_end(), "AB");
    assert_eq!(terminal.take_responses(), b"\x1b[1;2R");
}

#[test]
fn ansi_character_edits_respect_the_virtual_right_margin() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-character-edit-right-margin.bin"),
        include_str!("fixtures/sithtermfx/dec-character-edit-right-margin.expected.txt"),
    );
}

#[test]
fn ansi_character_edits_at_an_explicit_last_column_are_applied() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-character-edit-last-column.bin"),
        include_str!("fixtures/sithtermfx/dec-character-edit-last-column.expected.txt"),
    );
}

#[test]
fn ansi_character_edits_use_the_first_numeric_count() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-character-edit-parameters.bin"),
        include_str!("fixtures/sithtermfx/dec-character-edit-parameters.expected.txt"),
    );
}

#[test]
fn horizontal_margin_character_edits_keep_their_count_rules() {
    let mut terminal = Terminal::with_profile(8, 4, TerminalProfile::Vt420);
    terminal.advance(
        b"ABCDEFGH\x1b[2;1HIJKLMNOP\x1b[3;1HQRSTUVWX\x1b[?69h\x1b[2;6s\x1b[1;3H\x1b[0@\x1b[2;3H\x1b[;2P\x1b[3;3H\x1b[0X",
    );
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "AB CDEGH");
    assert_eq!(row(&screen, 1), "IJLMN OP");
    assert_eq!(row(&screen, 2), "QR TUVWX");
}

#[test]
fn ansi_character_edits_preserve_dec_conformance_downgrade() {
    let mut terminal = Terminal::with_profile(16, 5, TerminalProfile::Vt220);
    terminal.advance(b"\x1b[61\"p");
    terminal.advance(include_bytes!(
        "fixtures/sithtermfx/dec-character-edit-last-column.bin"
    ));
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "0123456789ABCDEI");
    assert_eq!(row(&screen, 1), "0123456789ABCDE ");
    assert_eq!(row(&screen, 2), "0123456789ABCDEE");
    assert_eq!((screen.cursor.column, screen.cursor.row), (15, 2));
}

#[test]
fn ansi_autowrap_off_restarts_the_row_until_reenabled() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-autowrap-toggle.bin"),
        include_str!("fixtures/sithtermfx/dec-autowrap-toggle.expected.txt"),
    );
}

#[test]
fn ansi_autowrap_off_preserves_existing_horizontal_margin_behavior() {
    let mut terminal = Terminal::with_profile(6, 3, TerminalProfile::Vt420);
    terminal.advance(b"\x1b[?69h\x1b[2;4s\x1b[?7l\x1b[1;2HABCD\x1b[2;6HAB");
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), " ABD  ");
    assert_eq!(row(&screen, 1), "     B");
    assert_eq!((screen.cursor.column, screen.cursor.row), (5, 1));
}

#[test]
fn ansi_cursor_report_distinguishes_the_pending_and_addressed_columns() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-cursor-report-wrap.bin"),
        include_str!("fixtures/sithtermfx/dec-cursor-report-wrap.expected.txt"),
    );
}

#[test]
fn ansi_cursor_report_uses_the_physical_row_in_origin_mode() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-cursor-report-origin.bin"),
        include_str!("fixtures/sithtermfx/dec-cursor-report-origin.expected.txt"),
    );
}

#[test]
fn ansi_device_status_replies_only_to_standard_requests() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-device-status.bin"),
        include_str!("fixtures/sithtermfx/dec-device-status.expected.txt"),
    );
}

#[test]
fn ansi_device_status_uses_the_first_numeric_parameter() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-device-status-parameters.bin"),
        include_str!("fixtures/sithtermfx/dec-device-status-parameters.expected.txt"),
    );
}

#[test]
fn vt420_cursor_reports_preserve_private_and_horizontal_margin_behavior() {
    let mut terminal = Terminal::with_profile(8, 4, TerminalProfile::Vt420);
    terminal.advance(b"\x1b[2;4r\x1b[?6h\x1b[2;1HABCDEFGH\x1b[6n\x1b[?6n");
    assert_eq!(terminal.take_responses(), b"\x1b[3;9R");

    terminal.advance(b"\x1b[?69h\x1b[2;6s\x1b[2;1HABCDE\x1b[6n\x1b[?6n");
    assert_eq!(terminal.take_responses(), b"\x1b[2;5R\x1b[?2;5R");

    terminal.advance(b"\x1b[?69l\x1b G\x9b?6n\x9b;6n");
    assert_eq!(terminal.take_responses(), b"\x9b3;6R");
}

#[test]
fn ansi_index_and_reverse_index_scroll_only_the_selected_row() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-single-row-region.bin"),
        include_str!("fixtures/sithtermfx/dec-single-row-region.expected.txt"),
    );
}

#[test]
fn ansi_vpa_uses_physical_rows_and_preserves_pending_wrap() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-absolute-row.bin"),
        include_str!("fixtures/sithtermfx/dec-absolute-row.expected.txt"),
    );
}

#[test]
fn ansi_vpa_bounds_rows_to_the_physical_grid() {
    for profile in ANSI_CURSOR_PROFILES {
        let mut terminal = Terminal::with_profile(16, 5, profile);
        terminal.advance(b"\x1b[2;4r\x1b[?6h\x1b[1;5H\x1b[65535d");
        let cursor = terminal.snapshot().cursor;
        assert_eq!((cursor.column, cursor.row), (4, 4), "{profile:?}");
        terminal.advance(b"\x1b[0d");
        let cursor = terminal.snapshot().cursor;
        assert_eq!((cursor.column, cursor.row), (4, 0), "{profile:?}");
    }
}

#[test]
fn ansi_column_address_and_line_steps_reapply_the_origin() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-absolute-column.bin"),
        include_str!("fixtures/sithtermfx/dec-absolute-column.expected.txt"),
    );
}

#[test]
fn ansi_column_address_clears_wrap_and_line_steps_use_origin_bounds() {
    for profile in ANSI_CURSOR_PROFILES {
        let mut terminal = Terminal::with_profile(16, 5, profile);
        terminal.advance(
            b"\x1b[2;4r\x1b[2;1H0123456789ABCDEF\x1b[3GX\
              \x1b[4;5H\x1b[EY\x1b[2;5H\x1b[FZ\x1b[?6h\x1b[3;5H\x1b[9FV",
        );
        let screen = terminal.snapshot();
        assert_eq!(
            (screen.cursor.column, screen.cursor.row),
            (1, 2),
            "{profile:?}"
        );
        for (index, expected) in ["Z", "01X3456789ABCDEF", "V", "", "Y"]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                row(&screen, index as u16).trim_end(),
                expected,
                "{profile:?}, row {index}"
            );
        }
    }
}

#[test]
fn ansi_line_edit_preserves_the_cursor_column() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-line-edit-columns.bin"),
        include_str!("fixtures/sithtermfx/dec-line-edit-columns.expected.txt"),
    );
}

#[test]
fn ansi_line_edit_preserves_pending_wrap() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-line-edit-wrap.bin"),
        include_str!("fixtures/sithtermfx/dec-line-edit-wrap.expected.txt"),
    );
}

#[test]
fn ansi_line_edit_respects_the_bottom_margin_and_explicit_zero() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-line-edit-bounds.bin"),
        include_str!("fixtures/sithtermfx/dec-line-edit-bounds.expected.txt"),
    );
}

#[test]
fn ansi_line_edit_uses_the_first_numeric_parameter() {
    check_ansi_cursor_capture(
        include_bytes!("fixtures/sithtermfx/dec-line-edit-parameters.bin"),
        include_str!("fixtures/sithtermfx/dec-line-edit-parameters.expected.txt"),
    );
}

const ANSI_CURSOR_PROFILES: [TerminalProfile; 10] = [
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

#[test]
fn ansi_resize_preserves_the_vertical_scrolling_region() {
    let input = include_bytes!("fixtures/sithtermfx/dec-resize-margins.bin");
    let expected =
        include_str!("fixtures/sithtermfx/dec-resize-margins.expected.txt").replace("\r\n", "\n");
    for profile in ANSI_CURSOR_PROFILES {
        for (columns, rows) in [(16, 5), (20, 4), (20, 7)] {
            let mut terminal = Terminal::with_profile(columns, rows, profile);
            terminal.advance(format!("\x1b[2;{}r", rows - 1).as_bytes());
            terminal.resize(20, 5);
            terminal.advance(input);
            assert_eq!(
                capture_ansi_cursor(&mut terminal),
                expected,
                "{profile:?}, initial size {columns}x{rows}"
            );
        }
    }
}

fn check_ansi_cursor_capture(input: &[u8], expected: &str) {
    let expected = expected.replace("\r\n", "\n");
    for profile in ANSI_CURSOR_PROFILES {
        for chunk_size in [1, 7, input.len()] {
            let mut terminal = Terminal::with_profile(16, 5, profile);
            for chunk in input.chunks(chunk_size) {
                terminal.advance(chunk);
            }
            assert_eq!(
                capture_ansi_cursor(&mut terminal),
                expected,
                "{profile:?}, chunk size {chunk_size}"
            );
        }
    }
}

fn capture_ansi_cursor(terminal: &mut Terminal) -> String {
    use std::fmt::Write as _;

    let screen = terminal.snapshot();
    let mut output = format!(
        "cursor={},{}\nreply=",
        screen.cursor.column, screen.cursor.row
    );
    for byte in terminal.take_responses() {
        write!(output, "{byte:02x}").unwrap();
    }
    output.push_str("\nscreen:\n");
    for index in 0..screen.rows {
        writeln!(output, "{}", row(&screen, index).trim_end()).unwrap();
    }
    output.push_str("attributes:\n");
    for index in 0..screen.rows {
        for cell in screen.row(index).unwrap() {
            output.push(match (cell.style.inverse, cell.style.underline) {
                (false, false) => '.',
                (true, false) => 'I',
                (false, true) => 'U',
                (true, true) => 'B',
            });
        }
        output.push('\n');
    }
    output
}

#[test]
fn vt420_fills_copies_and_erases_a_rectangle_without_moving_the_cursor() {
    let mut terminal = Terminal::with_profile(8, 4, TerminalProfile::Vt420);
    terminal.advance(b"\x1b[4;8H\x1b[1m\x1b[65;1;2;2;4$x\x1b[1;2;1;4;1;2;4;1$v\x1b[1;3;1;4$z");
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), " A      ");
    assert_eq!(row(&screen, 1), " AAAAA  ");
    assert!(screen.row(1).unwrap()[5].style.bold);
    assert_eq!((screen.cursor.row, screen.cursor.column), (3, 7));

    let mut vt320 = Terminal::with_profile(8, 4, TerminalProfile::Vt320);
    vt320.advance(b"\x1b[65;1;2;2;4$x");
    assert_eq!(row(&vt320.snapshot(), 0), "        ");
}

#[test]
fn vt420_rectangular_attributes_and_protection_stay_in_their_area() {
    let mut terminal = Terminal::with_profile(8, 4, TerminalProfile::Vt420);
    terminal.advance(b"ABCDEFGH\r\nABCDEFGH\x1b[2*x\x1b[1;2;2;3;4$r");
    let screen = terminal.snapshot();
    assert!(screen.row(0).unwrap()[1].style.underline);
    assert!(screen.row(1).unwrap()[2].style.underline);
    assert!(!screen.row(0).unwrap()[3].style.underline);
    terminal.advance(b"\x1b[1;2;2;3;4$t\x1b[1\"q\x1b[90;1;3;2;3$x\x1b[1;2;2;4${");
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "A Z EFGH");
    assert_eq!(row(&screen, 1), "A Z EFGH");
    assert!(!screen.row(0).unwrap()[2].style.underline);
}

#[test]
fn vt420_scrolls_and_wraps_between_both_pairs_of_margins() {
    let mut terminal = Terminal::with_profile(8, 4, TerminalProfile::Vt420);
    terminal.advance(b"L11111R!\r\nL22222R!\r\nL33333R!\r\nL44444R!");
    terminal.advance(b"\x1b[?69h\x1b[2;6s\x1b[2;3r\x1b[?6h\x1b[2;5H\n\x1b[6n");
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 0), "L11111R!");
    assert_eq!(row(&screen, 1), "L33333R!");
    assert_eq!(row(&screen, 2), "L     R!");
    assert_eq!(row(&screen, 3), "L44444R!");
    assert_eq!(screen.scrollback_lines, 0);
    assert_eq!(terminal.take_responses(), b"\x1b[2;5R");

    terminal.advance(b"\x1b[1;1H\x1b6ABCDEZ");
    let screen = terminal.snapshot();
    assert_eq!(row(&screen, 1), "LABCDER!");
    assert_eq!(row(&screen, 2), "LZ    R!");
}

#[test]
fn vt520_reports_its_conformance_extent_and_selectable_identity() {
    let mut terminal = Terminal::with_profile(8, 4, TerminalProfile::Vt520);
    terminal.advance(b"\x1b[c\x1b[\"v\x1b[5,q\x1b[c\x1b[10,q\x1b[65;2\"p\x90$q\"p\x9c\x9bc");
    assert_eq!(
        terminal.take_responses(),
        b"\x1b[?65;1;2;6;7;8;9;15;18;21c\x1b[4;8;1;1;1\"w\x1b[?62;1;2;6;7;8;9c\x901$r65;0\"p\x9c\x9b?65;1;2;6;7;8;9;15;18;21c"
    );
}
#[test]
fn desktop_cursor_defaults_preserve_explicit_host_shape_and_blink() {
    let mut terminal = Terminal::new(12, 3);
    terminal.set_default_cursor_style(CursorShape::Bar, false);
    let screen = terminal.snapshot();
    assert_eq!(screen.cursor_shape, CursorShape::Bar);
    assert!(!screen.cursor_blinking);

    terminal.advance(b"\x1b[?12h");
    terminal.set_default_cursor_style(CursorShape::Underline, false);
    let screen = terminal.snapshot();
    assert_eq!(screen.cursor_shape, CursorShape::Underline);
    assert!(screen.cursor_blinking);

    terminal.advance(b"\x1b[2 q");
    terminal.set_default_cursor_style(CursorShape::Bar, true);
    let screen = terminal.snapshot();
    assert_eq!(screen.cursor_shape, CursorShape::Block);
    assert!(!screen.cursor_blinking);

    terminal.advance(b"\x1bc");
    let screen = terminal.snapshot();
    assert_eq!(screen.cursor_shape, CursorShape::Bar);
    assert!(screen.cursor_blinking);
}
