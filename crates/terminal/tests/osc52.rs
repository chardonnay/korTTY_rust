use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use kortty_terminal::{ClipboardError, ClipboardEvent, MAX_CLIPBOARD_TEXT_BYTES, Terminal};

#[test]
fn osc52_only_writes_complete_valid_text_and_never_answers_queries() {
    let mut terminal = Terminal::new(80, 24);
    let stream = b"\x1b]52;c;aGVs\r\nbG8=\x1b\\\x1b]52;c;?\x07\x1b]52;c;\x07\x1b]52;x;aGVsbG8=\x07\x1b]52;c;/w==\x07";
    for byte in stream {
        terminal.advance(std::slice::from_ref(byte));
    }
    let mut events = terminal.take_clipboard_events();
    assert!(
        matches!(events.pop_front(), Some(ClipboardEvent::LegacyWrite(Ok(text))) if text == "hello")
    );
    assert!(matches!(
        events.pop_front(),
        Some(ClipboardEvent::LegacyWrite(Err(ClipboardError::Io)))
    ));
    assert!(events.is_empty());
    assert!(terminal.take_responses().is_empty());

    terminal.set_strict_escape_filter(true);
    terminal.advance(b"\x1b]52;c;aGVsbG8=\x07");
    assert!(terminal.take_clipboard_events().is_empty());
}

#[test]
fn osc52_decoded_limit_is_enforced_above_the_generic_osc_limit() {
    let mut terminal = Terminal::new(80, 24);
    for bytes in [MAX_CLIPBOARD_TEXT_BYTES, MAX_CLIPBOARD_TEXT_BYTES + 1] {
        let encoded = STANDARD.encode(vec![b'a'; bytes]);
        terminal.advance(b"\x1b]52;c;");
        for chunk in encoded.as_bytes().chunks(367) {
            terminal.advance(chunk);
        }
        terminal.advance(b"\x1b\\");
    }
    let mut events = terminal.take_clipboard_events();
    assert!(
        matches!(events.pop_front(), Some(ClipboardEvent::LegacyWrite(Ok(text))) if text.len() == MAX_CLIPBOARD_TEXT_BYTES)
    );
    assert!(matches!(
        events.pop_front(),
        Some(ClipboardEvent::LegacyWrite(Err(ClipboardError::TooLarge)))
    ));
    assert!(events.is_empty());
}
