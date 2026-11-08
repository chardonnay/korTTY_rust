use kortty_terminal::{Terminal, TerminalProfile};

#[test]
fn sithtermfx_display_color_fixtures_match_for_every_split_including_strict_mode() {
    let cases = [
        (
            b"\x1b]10;?\x07".as_slice(),
            b"\x1b]10;rgb:1010/0f0f/0e0e\x07".as_slice(),
        ),
        (b"\x1b]10;?\x1b\\", b"\x1b]10;rgb:1010/0f0f/0e0e\x1b\\"),
        (b"\x1b]11;?\x07", b"\x1b]11;rgb:1010/0f0f/0e0e\x07"),
        (b"\x1b]11;?\x1b\\", b"\x1b]11;rgb:1010/0f0f/0e0e\x1b\\"),
        (
            b"\xc2\x9d10;?\xc2\x9c",
            b"\x1b]10;rgb:1010/0f0f/0e0e\xc2\x9c",
        ),
        (b"\x9d11;?\x9c", b"\x1b]11;rgb:1010/0f0f/0e0e\x9c"),
    ];
    for strict in [false, true] {
        for (request, reply) in cases {
            for split in 0..request.len() {
                let mut terminal = Terminal::new(10, 4);
                terminal.set_display_colors(Some([16, 15, 14]), Some([16, 15, 14]));
                terminal.set_strict_escape_filter(strict);
                terminal.advance(&request[..split]);
                assert!(terminal.take_responses().is_empty());
                terminal.advance(&request[split..]);
                assert_eq!(
                    terminal.take_responses(),
                    reply,
                    "strict={strict} split={split}"
                );
                assert_eq!(terminal.take_bell_events(), 0);
            }
        }
    }
}

#[test]
fn queries_leave_unavailable_colors_and_palette_updates_unanswered() {
    let mut terminal = Terminal::new(10, 4);
    terminal.advance(b"\x1b]10;?\x07\x1b]11;?\x07");
    assert!(terminal.take_responses().is_empty());
    terminal.set_display_colors(Some([0, 128, 255]), None);
    terminal.advance(
        concat!(
            "\x1b]10;rgb:ffff/ffff/ffff\x07\x1b]11;?\x07",
            "\x1b]10;??\x07\x1b]10; ?\x07\x1b]12;?\x07",
            "\x1b]10;?\x18\x1b]10;?\x1a\x1b]10;?\x1b\x07"
        )
        .as_bytes(),
    );
    assert!(terminal.take_responses().is_empty());
    terminal.advance(b"\x1bc\x1b[?1049h\x1b]010;?;trailing-argument\x07\x1b[?1049l");
    assert_eq!(terminal.take_responses(), b"\x1b]10;rgb:0000/8080/ffff\x07");
}

#[test]
fn dec_c1_control_mode_preserves_the_query_terminator() {
    let mut terminal = Terminal::with_profile(10, 4, TerminalProfile::Vt320);
    terminal.set_display_colors(None, Some([1, 2, 3]));
    terminal.advance(b"\x1b[63;2\"p\x1b]11;?\x1b\\\x9d11;?\x9c");
    assert_eq!(
        terminal.take_responses(),
        b"\x1b]11;rgb:0101/0202/0303\x1b\\\x1b]11;rgb:0101/0202/0303\x9c"
    );
}
#[test]
fn reports_configured_palette_and_cursor_without_accepting_remote_color_writes() {
    let mut terminal = Terminal::new(12, 2);
    let mut ansi = [[0; 3]; 16];
    ansi[1] = [0x12, 0x34, 0x56];
    ansi[9] = [0x65, 0x43, 0x21];
    terminal.set_display_palette(
        Some([0xab, 0xcd, 0xef]),
        kortty_terminal::Palette::new(ansi),
    );
    let input = b"\x1b]4;1;#FFFFFF;9;?;1;?;196;?;256;?\x07\x1b]12;#000000\x07\x1bc\x1b]12;?\x1b\\";
    for chunk in input.chunks(3) {
        terminal.advance(chunk);
    }
    assert_eq!(
        terminal.take_responses(),
        concat!(
            "\x1b]4;9;rgb:6565/4343/2121\x07",
            "\x1b]4;1;rgb:1212/3434/5656\x07",
            "\x1b]4;196;rgb:ffff/0000/0000\x07",
            "\x1b]12;rgb:abab/cdcd/efef\x1b\\",
        )
        .as_bytes()
    );
}

#[test]
fn sgr_color_filter_keeps_other_parameters_and_does_not_rewrite_old_cells() {
    use kortty_terminal::Color;

    let mut terminal = Terminal::new(12, 2);
    terminal.advance(b"\x1b[31mA");
    terminal.set_sgr_colors_enabled(false);
    terminal.advance(b"\x1b[39;1;38;2;7;8;9;48:5:5;4mB\x1b[0;94;102mC\x1b[7mD");
    let screen = terminal.snapshot();
    let row = screen.row(0).unwrap();
    assert_eq!(row[0].style.foreground, Color::Indexed(1));
    assert_eq!(row[1].style.foreground, Color::Indexed(1));
    assert_eq!(row[1].style.background, Color::Default);
    assert!(row[1].style.bold && row[1].style.underline);
    assert!(!row[1].style.inverse && !row[1].style.hidden && !row[1].style.strikethrough);
    assert_eq!(row[2].style, Default::default());
    assert!(row[3].style.inverse);

    terminal.advance(b"\x1bc\x1b[91mE");
    assert_eq!(
        terminal.snapshot().row(0).unwrap()[0].style.foreground,
        Color::Default
    );
    terminal.set_sgr_colors_enabled(true);
    terminal.advance(b"\x1b[91mF");
    assert_eq!(
        terminal.snapshot().row(0).unwrap()[1].style.foreground,
        Color::Indexed(9)
    );
}

#[test]
fn cursor_blink_preference_gates_host_requests_without_changing_shape() {
    use kortty_terminal::CursorShape;

    let mut terminal = Terminal::new(12, 2);
    terminal.set_cursor_blink_allowed(false);
    terminal.advance(b"\x1b[5 q");
    let screen = terminal.snapshot();
    assert_eq!(screen.cursor_shape, CursorShape::Bar);
    assert!(!screen.cursor_blinking);
    terminal.set_cursor_blink_allowed(true);
    assert!(terminal.snapshot().cursor_blinking);
}
