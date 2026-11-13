use kortty_terminal::Terminal;

#[test]
fn osc_notifications_are_chunk_safe_bounded_and_separate_from_bells() {
    let bytes = b"\x1b]9;Build finished\x07\x1b]777;notify;Deploy;Ready\x1b\\";
    for size in [1, 3, bytes.len()] {
        let mut terminal = Terminal::new(80, 24);
        for chunk in bytes.chunks(size) {
            terminal.advance(chunk);
        }
        let notices = terminal.take_program_notifications();
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[0].body, "Build finished");
        assert_eq!(
            (notices[1].title.as_str(), notices[1].body.as_str()),
            ("Deploy", "Ready")
        );
        assert_eq!(terminal.take_bell_events(), 0);
        assert!(terminal.take_program_notifications().is_empty());
    }
}

#[test]
fn notifications_reject_strict_and_incomplete_messages_and_clean_direction_overrides() {
    let mut terminal = Terminal::new(80, 24);
    terminal.set_strict_escape_filter(true);
    terminal.advance(b"\x1b]9;hidden\x07");
    assert!(terminal.take_program_notifications().is_empty());
    terminal.set_strict_escape_filter(false);
    terminal.advance("\x1b]777;notify;Title\u{202e};Body\u{2066}\x07".as_bytes());
    let notice = terminal.take_program_notifications().remove(0);
    assert_eq!(
        (notice.title.as_str(), notice.body.as_str()),
        ("Title", "Body")
    );
    terminal.advance(b"\x1b]9;incomplete");
    assert!(terminal.take_program_notifications().is_empty());
    terminal.advance(b"\x18plain");
    assert!(terminal.take_program_notifications().is_empty());
    for _ in 0..100 {
        terminal.advance(b"\x1b]9;bounded\x07");
    }
    assert_eq!(terminal.take_program_notifications().len(), 32);
}
