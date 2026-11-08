use kortty_terminal::{Terminal, TerminalProfile};

const RED: &[u8] = b"\x1b_Ga=T,i=1,f=24,s=1,v=1,q=2,C=1;/wAA\x1b\\";

#[test]
fn kitty_transfer_uses_byte_stream_order_and_respects_graphics_policy() {
    let mut terminal = Terminal::new(20, 10);
    let bytes = b"ab\x1b_Ga=T,i=7,f=32,s=1,v=1,m=1,C=1;AP8A\x1b\\\x1b_Gm=0;gA==\x1b\\\x1b[6n";
    for byte in bytes {
        terminal.advance(std::slice::from_ref(byte));
    }
    assert_eq!(terminal.take_responses(), b"\x1b_Gi=7;OK\x1b\\\x1b[1;3R");
    let screen = terminal.snapshot();
    let placement = &screen.images[0].placement;
    assert_eq!((screen.images[0].row, placement.column), (0, 2));
    assert_eq!(placement.image.rgba.as_ref(), &[0, 255, 0, 128]);

    terminal.set_kitty_graphics_enabled(false);
    terminal.advance(b"\x1bc");
    terminal.advance(bytes);
    assert_eq!(terminal.take_responses(), b"\x1b[1;3R");
    assert!(terminal.snapshot().images.is_empty());

    let mut vt220 = Terminal::with_profile(20, 10, TerminalProfile::Vt220);
    vt220.advance(bytes);
    assert!(vt220.snapshot().images.is_empty());
    assert_eq!(vt220.take_responses(), b"\x1b[1;3R");
}

#[test]
fn kitty_anchor_survives_scrollback_reflow_and_archive_display() {
    let mut terminal = Terminal::new(10, 4);
    terminal.advance(b"0123456");
    terminal.advance(RED);
    terminal.advance(b"789");
    terminal.resize(5, 4);
    let screen = terminal.snapshot();
    assert_eq!(
        (screen.images[0].row, screen.images[0].placement.column),
        (1, 2)
    );
    assert_eq!(screen.row(1).unwrap()[2].text, "7");

    terminal.advance(b"\x1b[2J");
    let screen = terminal.snapshot();
    assert!(screen.images.is_empty());
    assert_eq!(screen.scrollback_lines, 2);
    let past = terminal.viewport_snapshot(2);
    assert_eq!(
        (past.images[0].row, past.images[0].placement.column),
        (1, 2)
    );
    assert_eq!(past.row(1).unwrap()[2].text, "7");
    assert!(!past.cursor.visible);

    terminal.advance(b"\x1b[3J");
    assert!(terminal.viewport_snapshot(2).images.is_empty());
}

#[test]
fn kitty_placement_moves_cursor_and_remains_visible_below_its_anchor() {
    let mut terminal = Terminal::new(8, 4);
    terminal.advance(b"\x1b[4;1H\x1b_Ga=T,f=24,s=1,v=1,c=2,r=3,q=2;/wAA\x1b\\");
    let screen = terminal.snapshot();
    assert_eq!((screen.cursor.row, screen.cursor.column), (3, 2));
    assert_eq!(screen.scrollback_lines, 2);
    assert_eq!(screen.images[0].row, 1);

    terminal.advance(b"\r\n\r\n");
    assert_eq!(terminal.snapshot().images[0].row, -1);
    assert_eq!(terminal.viewport_snapshot(1).images[0].row, 0);
}

#[test]
fn kitty_delete_reaches_the_hidden_primary_and_overlong_apc_recovers() {
    let mut terminal = Terminal::new(10, 4);
    terminal.advance(RED);
    terminal.advance(b"\x1b[?1049h\x1b_Ga=T,i=2,f=24,s=1,v=1,q=2,C=1;AP8A\x1b\\");
    assert_eq!(terminal.snapshot().images[0].placement.image.id, 2);
    terminal.advance(b"\x1b_Ga=d,d=I,i=1\x1b\\\x1b[?1049l");
    assert!(terminal.snapshot().images.is_empty());

    terminal.advance(b"\x1b_Ga=T,i=3,f=24,s=1,v=1;");
    for _ in 0..256 {
        terminal.advance(&[b'A'; 4096]);
    }
    terminal.advance(b"\x1b\\after");
    assert_eq!(terminal.snapshot().row(0).unwrap()[0].text, "a");
    assert!(terminal.snapshot().images.is_empty());
    assert!(terminal.take_responses().is_empty());
}

#[test]
fn kitty_discarded_packet_cancels_a_partial_transfer() {
    let mut terminal = Terminal::new(10, 4);
    terminal.advance(b"\x1b_Ga=T,i=3,f=24,s=1,v=1,m=1;/wAA\x1b\\");
    terminal.advance(b"\x1b_Gm=1;");
    for _ in 0..256 {
        terminal.advance(&[b'A'; 4096]);
    }
    terminal.advance(b"\x1b\\\x1b_Gm=0;\x1b\\");
    assert!(terminal.snapshot().images.is_empty());
    assert!(!terminal.take_responses().ends_with(b"i=3;OK\x1b\\"));
}
