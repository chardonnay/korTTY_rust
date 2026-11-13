use std::borrow::Cow;

use kortty_terminal::{
    MAX_SEARCH_QUERY_BYTES, MAX_SEARCH_RESULTS, SearchPosition, SearchSpan, Terminal,
};

fn span(row: usize, start_column: u16, end_column: u16) -> SearchSpan {
    SearchSpan {
        row,
        start_column,
        end_column,
    }
}

#[test]
fn search_is_literal_and_find_results_do_not_overlap() {
    let mut terminal = Terminal::new(32, 2);
    terminal.advance(b"aaaa [a.*] abababaca");

    let result = terminal.search("aa", false);
    assert_eq!(result.matches.len(), 2);
    assert_eq!(result.matches[0].spans.as_ref(), [span(0, 0, 2)]);
    assert_eq!(result.matches[1].spans.as_ref(), [span(0, 2, 4)]);
    assert_eq!(
        terminal.search("[a.*]", false).matches[0].spans.as_ref(),
        [span(0, 5, 10)]
    );
    assert_eq!(
        terminal.search("ababaca", false).matches[0].spans.as_ref(),
        [span(0, 13, 20)]
    );
    assert!(terminal.search("a.+", false).matches.is_empty());
}

#[test]
fn style_changes_do_not_split_a_match_and_case_search_handles_utf8() {
    let mut terminal = Terminal::new(24, 2);
    terminal.advance("Äp\x1b[31mFel İ a\u{301}".as_bytes());

    assert!(terminal.search("äPFEL", false).matches.is_empty());
    let result = terminal.search("äPFEL", true);
    assert_eq!(result.matches[0].spans.as_ref(), [span(0, 0, 5)]);

    let result = terminal.search("i\u{307}", true);
    assert_eq!(result.matches[0].spans.as_ref(), [span(0, 6, 7)]);
    let result = terminal.search("\u{301}", false);
    assert_eq!(result.matches[0].spans.as_ref(), [span(0, 8, 9)]);
}

#[test]
fn wide_glyphs_are_highlighted_in_full_and_wrap_padding_is_excluded() {
    let mut terminal = Terminal::new(4, 3);
    terminal.advance("abc界Z\r\n界".as_bytes());

    let result = terminal.search("c界Z", false);
    let found = &result.matches[0];
    assert_eq!(found.start, SearchPosition { row: 0, column: 2 });
    assert_eq!(found.end, SearchPosition { row: 1, column: 3 });
    assert_eq!(found.spans.as_ref(), [span(0, 2, 3), span(1, 0, 3)]);

    let result = terminal.search("界", false);
    assert_eq!(result.matches[0].spans.as_ref(), [span(1, 0, 2)]);
    assert_eq!(result.matches[1].spans.as_ref(), [span(2, 0, 2)]);
    assert!(terminal.search("c 界", false).matches.is_empty());
    assert!(terminal.search("Z界", false).matches.is_empty());
}

#[test]
fn erased_interior_cells_are_spaces_and_unwritten_row_tails_are_ignored() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(b"A\x1b[4GB\r\nC");

    assert!(terminal.search("AB", false).matches.is_empty());
    let result = terminal.search("A  B", false);
    assert_eq!(result.matches[0].spans.as_ref(), [span(0, 0, 4)]);
    assert!(terminal.search("B ", false).matches.is_empty());
    assert!(terminal.search("BC", false).matches.is_empty());
}

#[test]
fn matches_cross_soft_wraps_between_history_and_live_and_survive_packing() {
    let mut terminal = Terminal::new(4, 1);
    terminal.advance(b"abcdef");
    let initial = terminal.search("cdef", false);
    assert_eq!(initial.scrollback_lines, 1);
    assert_eq!(
        initial.matches[0].spans.as_ref(),
        [span(0, 2, 4), span(1, 0, 2)]
    );

    terminal.advance(b"\r\n000\r\n111\r\n222\r\n333");
    let before = terminal.search("cdef", false);
    while terminal.compress_idle_scrollback(0) {}
    assert!(matches!(terminal.history_line(0), Some(Cow::Owned(_))));
    assert!(matches!(terminal.history_line(1), Some(Cow::Owned(_))));
    assert_eq!(terminal.search("cdef", false), before);
    assert!(matches!(terminal.history_line(0), Some(Cow::Owned(_))));
    assert_eq!(before.matches, initial.matches);
}

#[test]
fn alternate_screen_search_does_not_include_hidden_primary_content() {
    let mut terminal = Terminal::new(10, 2);
    terminal.advance(b"primary\r\nline\r\n\x1b[?1049hsecondary");
    assert!(terminal.search("primary", false).matches.is_empty());

    let result = terminal.search("secondary", false);
    assert_eq!(result.scrollback_lines, 0);
    assert_eq!(result.matches[0].spans.as_ref(), [span(1, 0, 9)]);
    terminal.advance(b"\x1b[?1049l");
    assert_eq!(terminal.search("primary", false).matches[0].start.row, 0);
    assert!(terminal.search("secondary", false).matches.is_empty());
}

#[test]
fn query_and_result_limits_are_explicit() {
    let mut terminal = Terminal::new(128, 3);
    terminal.advance(&vec![b'x'; MAX_SEARCH_RESULTS]);
    let result = terminal.search("x", false);
    assert_eq!(result.matches.len(), MAX_SEARCH_RESULTS);
    assert!(!result.truncated);
    terminal.advance(b"x");
    let result = terminal.search("x", false);
    assert_eq!(result.matches.len(), MAX_SEARCH_RESULTS);
    assert!(result.truncated);

    let allowed = "x".repeat(MAX_SEARCH_QUERY_BYTES);
    let beyond = "x".repeat(MAX_SEARCH_QUERY_BYTES + 1);
    assert_eq!(terminal.search(&allowed, false).matches.len(), 1);
    assert!(terminal.search(&beyond, false).matches.is_empty());
    assert!(terminal.search("", true).matches.is_empty());
}
