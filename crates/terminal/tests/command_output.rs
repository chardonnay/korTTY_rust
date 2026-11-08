use kortty_terminal::{Color, LastOutput, LastOutputRange, Terminal};

const A: &str = "\x1b]133;A\x07";
const B: &str = "\x1b]133;B\x1b\\";
const C: &str = "\x1b]133;C\x07";
const D: &str = "\x1b]133;D;0\x1b\\";

fn feed(terminal: &mut Terminal, chunk: usize, data: &str) {
    for bytes in data.as_bytes().chunks(chunk) {
        terminal.advance(bytes);
    }
}

fn last(terminal: &Terminal) -> (LastOutputRange, String) {
    let LastOutput::Found(range) = terminal.last_output() else {
        panic!("missing last output: {:?}", terminal.last_output());
    };
    (range, terminal.last_output_text(range).unwrap())
}

#[test]
fn shell_output_excludes_command_next_prompt_and_trailing_empty_lines() {
    for chunk in [1, 5, 4096] {
        let mut terminal = Terminal::new(30, 8);
        feed(
            &mut terminal,
            chunk,
            &format!("{A}$ {B}ls{C}\r\nfile1\r\nfile two\r\n\r\n{D}{A}$ "),
        );
        let (range, text) = last(&terminal);
        assert_eq!(
            range,
            LastOutputRange {
                start_row: 1,
                start_column: 0,
                end_row: 2,
                end_column: 8,
                truncated: false,
            }
        );
        assert_eq!(text, "file1\nfile two");
        feed(
            &mut terminal,
            chunk,
            &format!("{B}printf\r\n{C}a\r\nbc{D}{A}$ "),
        );
        assert_eq!(last(&terminal).1, "a\nbc");
        feed(
            &mut terminal,
            chunk,
            &format!("{B}tail\r\n{C}new unfinished output\r\n"),
        );
        assert_eq!(last(&terminal).1, "a\nbc");
        feed(
            &mut terminal,
            chunk,
            &format!("{D}{A}$ {B}true\r\n{C}{D}{A}$ "),
        );
        assert_eq!(terminal.last_output(), LastOutput::NoOutput);
    }
}

#[test]
fn shell_output_joins_soft_wrapping_and_keeps_wide_glyphs_after_reflow() {
    let mut terminal = Terminal::new(20, 8);
    let line = "123456789é".repeat(3);
    feed(
        &mut terminal,
        1,
        &format!("{A}$ {B}cat\r\n{C}{line}\r\n漢e\u{301}{D}{A}$ "),
    );
    let expected = format!("{line}\n漢e\u{301}");
    assert_eq!(last(&terminal).1, expected);
    terminal.resize(8, 8);
    assert_eq!(last(&terminal).1, expected);
    terminal.resize(21, 8);
    assert_eq!(last(&terminal).1, expected);

    let mut terminal = Terminal::new(8, 6);
    feed(
        &mut terminal,
        1,
        &format!("{A}$ {B}printf\r\n{C}1234567x{D}"),
    );
    assert_eq!(last(&terminal).1, "1234567x");
    terminal.resize(5, 6);
    assert_eq!(last(&terminal).1, "1234567x");
}

#[test]
fn shell_output_reports_partial_eviction_and_then_expires() {
    let mut terminal = Terminal::new(1000, 3);
    feed(&mut terminal, 5, &format!("{A}$ {B}seq\r\n{C}"));
    for number in 1..=300 {
        terminal.advance(format!("{number}\r\n").as_bytes());
    }
    feed(&mut terminal, 5, &format!("{D}{A}$ "));
    while terminal.compress_idle_scrollback(0) {}
    let (range, text) = last(&terminal);
    assert!(range.truncated);
    assert_eq!((range.start_row, range.start_column), (0, 0));
    let numbers = text
        .lines()
        .map(|line| line.parse::<usize>().unwrap())
        .collect::<Vec<_>>();
    let first = numbers[0];
    assert!(first > 1);
    assert_eq!(numbers, (first..=300).collect::<Vec<_>>());
    for _ in 0..400 {
        terminal.advance(b"other\r\n");
    }
    assert!(!terminal.has_finished_command());
    assert!(matches!(
        terminal.last_output(),
        LastOutput::NoMarks | LastOutput::NoCommand
    ));
}

#[test]
fn shell_output_is_inactive_when_disabled_or_alternate_and_needs_a_finished_block() {
    let mut terminal = Terminal::new(30, 8);
    feed(&mut terminal, 1, &format!("{C}unrelated{D}"));
    assert_eq!(terminal.last_output(), LastOutput::NoMarks);
    feed(&mut terminal, 1, &format!("\r\n{A}$ {B}echo\r\n{D}"));
    assert_eq!(terminal.last_output(), LastOutput::NoCommand);
    feed(&mut terminal, 1, &format!("{C}done\r\n{D}{A}$ "));
    assert!(terminal.has_finished_command());
    terminal.advance(b"\x1b[?1049h");
    feed(
        &mut terminal,
        1,
        &format!("{A}alternate\r\n{C}not output{D}"),
    );
    assert_eq!(terminal.last_output(), LastOutput::AlternateScreen);
    assert!(!terminal.has_finished_command());
    terminal.advance(b"\x1b[?1049l");
    assert_eq!(last(&terminal).1, "done");
    terminal.set_prompt_navigation_enabled(false);
    assert_eq!(terminal.last_output(), LastOutput::Disabled);
    terminal.set_prompt_navigation_enabled(true);
    assert_eq!(last(&terminal).1, "done");
}

#[test]
fn clear_buffer_keeps_only_cursor_line_appearance_and_local_state() {
    let mut terminal = Terminal::new(30, 3);
    feed(
        &mut terminal,
        1,
        &format!("{A}$ {B}ls\r\n{C}one\r\ntwo\r\n{D}{A}"),
    );
    terminal.advance(b"\x1b[31mPROMPT \x1b[0m\x1b[?2004h");
    let before = terminal.snapshot();
    assert!(before.scrollback_lines > 0);
    assert!(terminal.has_finished_command());
    assert!(terminal.clear_buffer());
    let after = terminal.snapshot();
    assert_eq!(after.scrollback_lines, 0);
    assert_eq!(
        (after.cursor.row, after.cursor.column),
        (0, before.cursor.column)
    );
    assert_eq!(after.row(0), before.row(before.cursor.row));
    assert_eq!(after.row(0).unwrap()[0].style.foreground, Color::Indexed(1));
    assert!(after.cells.iter().skip(30).all(|cell| cell.text.is_empty()));
    assert!(terminal.bracketed_paste());
    assert_eq!(terminal.last_output(), LastOutput::NoMarks);
    assert!(terminal.take_responses().is_empty());
    terminal.advance(b"typed\r\nnext");
    let screen = terminal.snapshot();
    let text = |row| {
        screen
            .row(row)
            .unwrap()
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>()
    };
    assert_eq!(text(0), "PROMPT typed");
    assert_eq!(text(1), "next");
}

#[test]
fn clear_buffer_is_noop_on_alternate_and_moves_only_the_retained_kitty_anchor() {
    let mut terminal = Terminal::new(15, 4);
    terminal.advance(b"old\x1b_Ga=T,i=1,f=24,s=1,v=1,q=2,C=1;/wAA\x1b\\\r\n");
    terminal.advance(b"current\x1b_Ga=T,i=2,f=24,s=1,v=1,q=2,C=1;/wAA\x1b\\");
    terminal.advance(b"\x1b[?1049hhidden");
    let alternate = terminal.snapshot();
    assert!(!terminal.clear_buffer());
    assert_eq!(terminal.snapshot().cells, alternate.cells);
    terminal.advance(b"\x1b[?1049l");
    assert_eq!(terminal.snapshot().images.len(), 2);
    assert!(terminal.clear_buffer());
    let screen = terminal.snapshot();
    assert_eq!(screen.images.len(), 1);
    assert_eq!(
        (screen.images[0].row, screen.images[0].placement.image.id),
        (0, 2)
    );
    assert!(terminal.take_responses().is_empty());
}

#[test]
fn clear_buffer_clamps_the_terminal_cursor_to_the_origin_mode_margin() {
    let mut terminal = Terminal::new(15, 5);
    terminal.advance(b"old\x1b[2;4r\x1b[?6hcurrent");
    assert!(terminal.clear_buffer());
    let snapshot = terminal.snapshot();
    let kept = snapshot
        .row(0)
        .unwrap()
        .iter()
        .map(|cell| cell.text.as_str())
        .collect::<String>();
    assert_eq!(kept, "current");
    assert_eq!((snapshot.cursor.row, snapshot.cursor.column), (1, 7));
    terminal.advance(b"\x1b[6n");
    assert_eq!(terminal.take_responses(), b"\x1b[1;8R");
}

#[test]
fn viewport_wrap_lengths_follow_scrollback_and_distinguish_wide_wrap_padding() {
    let mut terminal = Terminal::new(5, 2);
    terminal.advance("abcd漢\r\nnext\r\n".as_bytes());
    let past = terminal.viewport_snapshot(usize::MAX);
    assert_eq!(past.wrapped_columns(0), Some(4));
    assert_eq!(past.wrapped_columns(1), None);
    assert_eq!(past.wrapped_columns(2), None);
    assert_eq!(terminal.snapshot().wrapped_columns(0), None);
    terminal.advance(b"12345x");
    assert_eq!(terminal.snapshot().wrapped_columns(0), Some(5));
    assert_eq!(terminal.snapshot().wrapped_columns(1), None);
}
