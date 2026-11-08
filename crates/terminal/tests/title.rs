use kortty_terminal::Terminal;

#[test]
fn title_stack_matches_the_reference_across_resets_and_alternate_screen() {
    for chunk_size in [1, 2, 3, 7, 1024] {
        let mut terminal = Terminal::new(40, 5);
        for (input, expected_title, alternate) in [
            (
                b"\x1b[22;0t\x1b]2;editor\x07\x1b[23;0t".as_slice(),
                "",
                false,
            ),
            (
                b"\x1b]2;outer\x07\x1b[22;2t\x1b[?1049h\x1b]2;inner\x07",
                "inner",
                true,
            ),
            (b"\x1b[?1049l", "inner", false),
            (b"\x1b[?1049h\x1b[!p", "inner", false),
            (b"\x1b[?1049h\x1bc", "inner", false),
            (b"\x1b[23;2t\x1b[23;0t", "outer", false),
        ] {
            for chunk in input.chunks(chunk_size) {
                terminal.advance(chunk);
            }
            let screen = terminal.snapshot();
            assert_eq!(screen.title, expected_title, "chunk size {chunk_size}");
            assert_eq!(
                screen.alternate_screen, alternate,
                "chunk size {chunk_size}"
            );
            assert!(terminal.take_responses().is_empty());
        }
    }
}

#[test]
fn window_title_stack_ignores_icon_only_and_omitted_selectors() {
    let mut terminal = Terminal::new(40, 5);
    terminal.advance(b"\x1b]0;start\x07\x1b[22t\x1b[22;1t\x1b]1;icon-only\x07\x1b[23;0t");
    assert_eq!(terminal.snapshot().title, "icon-only");
    terminal.advance(b"\x1b[22;t\x1b]2;changed\x07\x1b[23t\x1b[23;1t");
    assert_eq!(terminal.snapshot().title, "changed");
    terminal.advance(b"\x1b[23;0t");
    assert_eq!(terminal.snapshot().title, "icon-only");
}

#[test]
fn title_stack_keeps_the_most_recent_32_sanitized_titles() {
    let mut terminal = Terminal::new(40, 5);
    for index in 0..40 {
        terminal.advance(format!("\x1b]2;title{index}\u{202e}\x07\x1b[22;0t").as_bytes());
    }
    for index in (8..40).rev() {
        terminal.advance(b"\x1b[23;0t");
        assert_eq!(terminal.snapshot().title, format!("title{index}"));
    }
    terminal.advance(b"\x1b]2;after\x07\x1b[23;0t");
    assert_eq!(terminal.snapshot().title, "after");
}

#[test]
fn title_sanitization_matches_reference_before_trimming_and_capping() {
    let spaces = " ".repeat(90);
    let input =
        format!("\x1b]0;{spaces}prod\u{202e}db\u{85}after\r\nnext\u{2028}line\tend;ignored\x07");
    for chunk_size in [1, 2, 3, 7, 1024] {
        let mut terminal = Terminal::new(40, 5);
        for chunk in input.as_bytes().chunks(chunk_size) {
            terminal.advance(chunk);
        }
        assert_eq!(terminal.snapshot().title, "proddb after next line end");
        assert_eq!(terminal.take_bell_events(), 0);
        assert!(
            terminal
                .snapshot()
                .cells
                .iter()
                .all(|cell| cell.text.is_empty())
        );
    }

    let mut terminal = Terminal::new(40, 5);
    terminal.advance(format!("\x1b]2;{} more\x1b\\", "😀".repeat(79)).as_bytes());
    assert_eq!(terminal.snapshot().title, "😀".repeat(79));
    terminal.advance(format!("\x1b]2; {} \x07", "😀".repeat(90)).as_bytes());
    assert_eq!(terminal.snapshot().title, "😀".repeat(80));
}

#[test]
fn strict_and_nested_osc_title_sequences_stay_inert() {
    let mut terminal = Terminal::new(40, 5);
    terminal.advance(b"\x1b]2;first\x07");
    terminal.advance(b"\x1b]2;discard\x1b]2;nested\x07");
    assert_eq!(terminal.snapshot().title, "first");
    terminal.set_strict_escape_filter(true);
    terminal.advance(b"\x1b]2;raw\ncontrol\x07");
    assert_eq!(terminal.snapshot().title, "first");
    terminal.advance("\x1b]2; safe\u{2029}line \x07".as_bytes());
    assert_eq!(terminal.snapshot().title, "safe line");
}
