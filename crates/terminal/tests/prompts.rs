use kortty_terminal::{PromptDirection::*, PromptNavigator, Terminal, TerminalProfile};

fn top(terminal: &Terminal, offset: usize) -> String {
    terminal
        .viewport_snapshot(offset)
        .row(0)
        .unwrap()
        .iter()
        .map(|cell| cell.text.as_str())
        .collect()
}

#[test]
fn prompt_navigation_chains_and_falls_back_with_alternate_or_disabled() {
    let mut terminal = Terminal::with_profile(24, 3, TerminalProfile::ScoAnsi);
    assert!(terminal.prompt_layout().is_none());
    assert!(!terminal.has_prompt_marks());
    terminal.advance(
        b"\x1b]133;A\x07one\x1b]133;B\x07\r\n\x1b]133;C\x07output\r\n\x1b]133;D;0\x07\
          \x1b]133;A\x07two\x1b]133;C\x07\r\noutput\r\n\x1b]133;D;0\x07\
          \x1b]133;A\x07three\r\ninput continuation",
    );
    let layout = terminal.prompt_layout().unwrap();
    let mut nav = PromptNavigator::default();
    let two = nav.navigate(&layout, Previous, 0);
    assert_eq!(top(&terminal, two), "two");
    let one = nav.navigate(&layout, Previous, two);
    assert_eq!(top(&terminal, one), "one");
    assert_eq!(nav.navigate(&layout, Previous, one), one);
    assert_eq!(nav.navigate(&layout, Next, one), two);
    assert_eq!(nav.navigate(&layout, Next, two), 0);
    assert_eq!(nav.navigate(&layout, Next, 0), 0);
    assert_eq!(nav.navigate(&layout, Previous, one - 1), one);

    terminal.advance(b"\x1b[?1049h\x1b]133;A\x07alternate");
    assert!(terminal.prompt_layout().is_none());
    assert!(terminal.has_prompt_marks());
    terminal.advance(b"\x1b[?1049l");
    let layout = terminal.prompt_layout().unwrap();
    assert_eq!(
        PromptNavigator::default().navigate(&layout, Previous, 0),
        two
    );
    terminal.set_prompt_navigation_enabled(false);
    assert!(terminal.prompt_layout().is_none());
    assert!(!terminal.has_prompt_marks());
    terminal.advance(b"\r\n\x1b]133;A\x07disabled marker\r\n");
    terminal.set_prompt_navigation_enabled(true);
    let layout = terminal.prompt_layout().unwrap();
    let previous = PromptNavigator::default().navigate(&layout, Previous, 0);
    assert_eq!(top(&terminal, previous), "two");
    terminal.advance(b"\x1b]133;C\x07");
    let layout = terminal.prompt_layout().unwrap();
    let three = PromptNavigator::default().navigate(&layout, Previous, 0);
    assert_eq!(top(&terminal, three), "three");
}

#[test]
fn prompt_navigation_preserves_columns_through_packing_and_reflow_and_drops_trimmed_marks() {
    let mut terminal = Terminal::new(20, 3);
    terminal.advance(b"12345678\x1b]133;A\x07first\x1b]133;C\x07\r\n");
    for _ in 0..16 {
        terminal.advance(b"filler\r\n");
    }
    while terminal.compress_idle_scrollback(0) {}
    assert!(matches!(
        terminal.history_line(0),
        Some(std::borrow::Cow::Owned(_))
    ));
    terminal.resize(8, 3);
    let layout = terminal.prompt_layout().unwrap();
    let offset = PromptNavigator::default().navigate(&layout, Previous, 0);
    assert_eq!(top(&terminal, offset), "first");

    for _ in 0..10_010 {
        terminal.advance(b"x\r\n");
    }
    assert!(terminal.prompt_layout().is_none());
    assert!(!terminal.has_prompt_marks());
    terminal.advance(b"\x1b]133;A\x07live");
    assert!(terminal.prompt_layout().is_some());
    terminal.advance(b"\x1b[3J");
    assert!(terminal.prompt_layout().is_none());
}
