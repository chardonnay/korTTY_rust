use std::borrow::Cow;

use kortty_terminal::{Color, Terminal};

const RED: &[u8] = b"\x1b_Ga=T,i=1,f=24,s=1,v=1,q=2,C=1;/wAA\x1b\\";

fn finish_idle_scan(terminal: &mut Terminal, scroll_offset: usize) {
    for _ in 0..512 {
        if !terminal.compress_idle_scrollback(scroll_offset) {
            return;
        }
    }
    panic!("idle scrollback scan did not finish");
}

fn append_rows(terminal: &mut Terminal, count: usize) {
    for row in 0..count {
        terminal.advance(format!("row {row:02}\r\n").as_bytes());
    }
}

#[test]
fn idle_packing_protects_viewport_and_latest_screenful() {
    let mut terminal = Terminal::new(16, 4);
    append_rows(&mut terminal, 28);
    let history_rows = terminal.snapshot().scrollback_lines;
    let viewport_offset = history_rows - 8;
    let viewport = terminal.viewport_snapshot(viewport_offset);
    finish_idle_scan(&mut terminal, viewport_offset);

    assert!(matches!(terminal.history_line(0), Some(Cow::Owned(_))));
    assert!(matches!(terminal.history_line(4), Some(Cow::Borrowed(_))));
    assert!(matches!(terminal.history_line(8), Some(Cow::Borrowed(_))));
    assert!(matches!(terminal.history_line(15), Some(Cow::Borrowed(_))));
    assert!(matches!(terminal.history_line(16), Some(Cow::Owned(_))));
    assert!(matches!(
        terminal.history_line(history_rows - 1),
        Some(Cow::Borrowed(_))
    ));
    assert_eq!(terminal.viewport_snapshot(viewport_offset), viewport);

    // The hidden primary history can be packed while the alternate is active.
    terminal.advance(b"\x1b[?1049h");
    finish_idle_scan(&mut terminal, viewport_offset);
    assert!(terminal.history_line(8).is_none());
    terminal.advance(b"\x1b[?1049l");
    assert!(matches!(terminal.history_line(8), Some(Cow::Owned(_))));
    assert_eq!(terminal.viewport_snapshot(viewport_offset), viewport);
}

#[test]
fn scrollback_compression_toggle_preserves_existing_rows_and_controls_new_packing() {
    let mut terminal = Terminal::new(16, 4);
    assert!(terminal.set_scrollback_compression_enabled(false));
    append_rows(&mut terminal, 28);
    finish_idle_scan(&mut terminal, 0);
    assert!(matches!(terminal.history_line(0), Some(Cow::Borrowed(_))));

    assert!(terminal.set_scrollback_compression_enabled(true));
    finish_idle_scan(&mut terminal, 0);
    assert!(matches!(terminal.history_line(0), Some(Cow::Owned(_))));
    let recent = terminal.snapshot().scrollback_lines - 1;

    assert!(terminal.set_scrollback_compression_enabled(false));
    append_rows(&mut terminal, 10);
    finish_idle_scan(&mut terminal, 0);
    assert!(matches!(terminal.history_line(0), Some(Cow::Owned(_))));
    assert!(matches!(
        terminal.history_line(recent),
        Some(Cow::Borrowed(_))
    ));

    terminal.resize(12, 4);
    let oldest = terminal.history_line(0).unwrap();
    assert!(matches!(oldest, Cow::Borrowed(_)));
    assert_eq!(oldest[0].text, "r");
    terminal.advance(b"\x1bc");
    assert!(!terminal.set_scrollback_compression_enabled(false));
}

#[test]
fn packed_wrapped_row_retains_styles_links_glyphs_and_kitty_anchors() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(
        "\x1b[1;2;3;4;5;7;8;9;38;2;1;2;3;48;5;42m\x1b]8;;https://example.org\x1b\\a\u{301}界3456"
            .as_bytes(),
    );
    terminal.advance(RED);
    terminal.advance(b"789XY\x1b[0m\x1b]8;;\x1b\\\r\n");
    append_rows(&mut terminal, 14);
    let offset = terminal.snapshot().scrollback_lines;
    let before = terminal.viewport_snapshot(offset);
    assert_eq!(before.row(0).unwrap()[0].text, "a\u{301}");
    assert_eq!(before.row(0).unwrap()[1].width, 2);
    assert_eq!(before.row(0).unwrap()[2].width, 0);
    assert_eq!(
        before.row(0).unwrap()[0].style.foreground,
        Color::Rgb(1, 2, 3)
    );
    assert_eq!(
        before.row(0).unwrap()[2].hyperlink.as_deref(),
        Some("https://example.org")
    );
    assert_eq!(
        (before.images[0].row, before.images[0].placement.column),
        (0, 7)
    );

    finish_idle_scan(&mut terminal, 0);
    assert!(matches!(terminal.history_line(0), Some(Cow::Owned(_))));
    assert_eq!(terminal.viewport_snapshot(offset), before);

    terminal.resize(5, 3);
    let reflow = terminal.viewport_snapshot(terminal.snapshot().scrollback_lines);
    assert_eq!(
        (reflow.images[0].row, reflow.images[0].placement.column),
        (1, 2)
    );
    assert_eq!(reflow.row(1).unwrap()[2].text, "7");
    assert_eq!(reflow.row(2).unwrap()[0].text, "X");
    assert_eq!(
        reflow.row(1).unwrap()[2].hyperlink.as_deref(),
        Some("https://example.org")
    );

    terminal.advance(b"\x1b[?1049h\x1b_Ga=d,d=I,i=1\x1b\\\x1b[?1049l");
    assert!(
        terminal
            .viewport_snapshot(terminal.snapshot().scrollback_lines)
            .images
            .is_empty()
    );
}

#[test]
fn width_reflow_keeps_the_cursor_visible_when_one_row_expands_beyond_a_screen() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(b"abcdefgh\r\nijklmnop\r\nqrstuvw");
    terminal.resize(1, 2);

    let snapshot = terminal.snapshot();
    assert_eq!(snapshot.rows, 2);
    assert_eq!(snapshot.cursor.row, 1);
    assert_eq!(snapshot.cursor.column, 0);
    assert_eq!(snapshot.row(0).unwrap()[0].text, "w");
    assert_eq!(snapshot.row(1).unwrap()[0].text, "");
    assert_eq!(snapshot.scrollback_lines, 22);
    assert_eq!(terminal.history_line(0).unwrap()[0].text, "a");
}
