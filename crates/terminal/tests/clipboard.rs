use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use kortty_terminal::{
    ClipboardError, ClipboardLocation, ClipboardRequestKind, ClipboardResult, Terminal,
    TerminalProfile,
};

const QUERY: &[u8] = b"\x1b[?5522$p";
const READ: &[u8] = b"\x1b]5522;type=read:id=read1;dGV4dC9wbGFpbg==\x1b\\";

fn osc(terminal: &mut Terminal, metadata: &str, payload: &str) {
    let frame = format!("\x1b]5522;{metadata};{payload}\x1b\\");
    for chunk in frame.as_bytes().chunks(23) {
        terminal.advance(chunk);
    }
}

#[test]
fn clipboard_access_requires_host_enablement_and_completion() {
    let mut terminal = Terminal::new(20, 4);
    terminal.advance(READ);
    terminal.advance(QUERY);
    assert!(terminal.take_clipboard_requests().is_empty());
    assert_eq!(terminal.take_responses(), b"\x1b[?5522;0$y");

    terminal.set_clipboard_protocol_enabled(true);
    for byte in READ {
        terminal.advance(std::slice::from_ref(byte));
    }
    let request = terminal.take_clipboard_requests().pop_front().unwrap();
    let ClipboardRequestKind::Read {
        location,
        mime_types,
        granted_by_paste,
        ..
    } = request.kind
    else {
        panic!("expected clipboard read");
    };
    assert_eq!(location, ClipboardLocation::Clipboard);
    assert_eq!(mime_types, ["text/plain"]);
    assert!(!granted_by_paste);
    assert!(terminal.take_responses().is_empty());
    let denied = terminal.complete_clipboard_request(
        request.id,
        ClipboardResult::Error(ClipboardError::PermissionDenied),
    );
    assert!(
        denied
            .windows(b"status=EPERM".len())
            .any(|part| part == b"status=EPERM")
    );
    assert!(
        terminal
            .complete_clipboard_request(request.id, ClipboardResult::Written)
            .is_empty()
    );
}

#[test]
fn strict_mode_revokes_pending_clipboard_operations_and_blocks_sensitive_controls() {
    let mut terminal = Terminal::new(20, 4);
    terminal.set_clipboard_protocol_enabled(true);
    terminal.advance(READ);
    let stale = terminal.take_clipboard_requests().pop_front().unwrap();
    terminal.set_strict_escape_filter(true);
    terminal.advance(b"\x1b[?25;5522h\x1b[?25l");
    terminal.advance(READ);
    terminal.advance(QUERY);
    terminal.advance(b"\x1b_Ga=T,f=24,s=1,v=1,C=1;/wAA\x1b\\");
    terminal.advance(b"\x1b]8;;file:///tmp/test\x1b\\a\x1b]8;;HTTPS://example.org\x1b\\b");
    let screen = terminal.snapshot();
    assert!(!screen.cursor.visible);
    assert!(screen.images.is_empty());
    assert!(screen.cells[0].hyperlink.is_none());
    assert_eq!(
        screen.cells[1].hyperlink.as_deref(),
        Some("HTTPS://example.org")
    );
    assert!(terminal.take_clipboard_requests().is_empty());
    assert!(
        terminal
            .begin_clipboard_paste(ClipboardLocation::Clipboard, [0; 16])
            .is_none()
    );
    assert!(
        terminal
            .complete_clipboard_request(stale.id, ClipboardResult::Types(vec![]))
            .is_empty()
    );
    assert!(terminal.take_responses().is_empty());

    let mut vt320 = Terminal::with_profile(20, 4, TerminalProfile::Vt320);
    vt320.set_strict_escape_filter(true);
    vt320.advance(b"\x1bP$q\"p\x1b\\after");
    assert!(vt320.take_responses().is_empty());
    assert_eq!(vt320.snapshot().cells[0].text, "a");
}

#[test]
fn paste_events_are_local_gestures_and_reset_revokes_the_old_request_id() {
    let mut terminal = Terminal::new(20, 4);
    terminal.set_clipboard_protocol_enabled(true);
    terminal.advance(QUERY);
    terminal.advance(b"\x1b[?5522h");
    terminal.advance(QUERY);
    assert_eq!(terminal.take_responses(), b"\x1b[?5522;2$y\x1b[?5522;1$y");
    // This nonce is deterministic only for this test. Real paste events use OS entropy.
    let paste = terminal
        .begin_clipboard_paste(ClipboardLocation::Clipboard, [7; 16])
        .unwrap();
    assert!(matches!(
        paste.kind,
        ClipboardRequestKind::List {
            user_initiated: true,
            ..
        }
    ));
    terminal.advance(b"\x1bc");
    terminal.advance(READ);
    let fresh = terminal.take_clipboard_requests().pop_front().unwrap();
    assert_ne!(fresh.id, paste.id);
    assert!(
        terminal
            .complete_clipboard_request(paste.id, ClipboardResult::Types(vec!["text/plain".into()]))
            .is_empty()
    );
    let response = terminal.complete_clipboard_request(
        fresh.id,
        ClipboardResult::Data(vec![("text/plain".into(), b"paste".to_vec())]),
    );
    assert!(
        response
            .windows(b"cGFzdGU=".len())
            .any(|part| part == b"cGFzdGU=")
    );
}

#[test]
fn maximum_mime_list_crosses_the_stream_framer_and_keeps_request_order() {
    let mut terminal = Terminal::new(20, 4);
    terminal.set_clipboard_protocol_enabled(true);
    let kinds = "a".repeat(127);
    let subtype = "b".repeat(125);
    let parameter = "c".repeat(255);
    let types: Vec<_> = (0..32)
        .map(|index| format!("{kinds}/{subtype}{index:02};{parameter}"))
        .collect();
    let wire = STANDARD.encode(types.join(" "));
    assert!(wire.len() > 16 * 1024);
    osc(&mut terminal, "type=read:loc=primary:id=mimes", &wire);
    let request = terminal.take_clipboard_requests().pop_front().unwrap();
    let ClipboardRequestKind::Read {
        location,
        mime_types,
        ..
    } = request.kind
    else {
        panic!("expected MIME read");
    };
    assert_eq!(location, ClipboardLocation::Primary);
    assert_eq!(mime_types, types);
    let data = vec![0xa5; 4096];
    let response = terminal.complete_clipboard_request(
        request.id,
        ClipboardResult::Data(vec![
            (types[3].clone(), vec![0, 0xff]),
            ("application/not-requested".into(), b"hidden".to_vec()),
            (types[0].clone(), [data.clone(), vec![0x80]].concat()),
        ]),
    );
    let response = std::str::from_utf8(&response).unwrap();
    let packets: Vec<_> = response
        .strip_suffix("\x1b\\")
        .unwrap()
        .split("\x1b\\")
        .collect();
    assert_eq!(packets.len(), 5);
    assert_eq!(
        packets[0],
        "\x1b]5522;type=read:status=OK:loc=primary:id=mimes"
    );
    assert_eq!(packets[4], "\x1b]5522;type=read:status=DONE:id=mimes");
    for (packet, mime, expected) in [
        (packets[1], &types[0], data.as_slice()),
        (packets[2], &types[0], &[0x80]),
        (packets[3], &types[3], &[0, 0xff]),
    ] {
        let (metadata, payload) = packet
            .strip_prefix("\x1b]5522;")
            .unwrap()
            .split_once(';')
            .unwrap();
        assert_eq!(
            metadata,
            format!(
                "type=read:status=DATA:id=mimes:mime={}",
                STANDARD.encode(mime)
            )
        );
        assert_eq!(STANDARD.decode(payload).unwrap(), expected);
    }
}

#[test]
fn malformed_reads_are_independent_of_writes_but_corrupt_write_chunks_abort() {
    let mut terminal = Terminal::new(20, 4);
    terminal.set_clipboard_protocol_enabled(true);
    let data = format!("type=wdata:mime={}", STANDARD.encode("image/png"));
    osc(&mut terminal, "type=write:id=valid", "");
    osc(&mut terminal, &data, "AA");
    osc(&mut terminal, "type=read:id=invalid", "QUJD\nQUJD");
    osc(&mut terminal, "type=read:id=oversized", &"A".repeat(65_536));
    osc(
        &mut terminal,
        &format!("type=read:id={}", "b".repeat(9000)),
        "Lg==",
    );
    assert!(terminal.take_responses().is_empty());
    osc(&mut terminal, &data, "/w");
    osc(&mut terminal, "type=wdata", "");
    let request = terminal.take_clipboard_requests().pop_front().unwrap();
    let ClipboardRequestKind::Write { data: contents, .. } = request.kind else {
        panic!("expected an unmodified write");
    };
    assert_eq!(contents, [("image/png".into(), vec![0, 0x0f, 0xf0])]);
    let response = terminal.complete_clipboard_request(request.id, ClipboardResult::Written);
    assert_eq!(response, b"\x1b]5522;type=write:status=DONE:id=valid\x1b\\");

    osc(&mut terminal, "type=write:id=corrupt", "");
    osc(&mut terminal, &data, "AAAA");
    osc(&mut terminal, &data, "AAAA\nAAAA");
    osc(&mut terminal, "type=wdata", "");
    assert!(terminal.take_clipboard_requests().is_empty());
    assert_eq!(
        terminal.take_responses(),
        b"\x1b]5522;type=write:status=EINVAL:id=corrupt\x1b\\"
    );
}

#[test]
fn in_progress_clipboard_frames_cannot_cross_enable_and_strict_transitions() {
    let mut terminal = Terminal::new(20, 4);
    terminal.advance(&READ[..4]);
    terminal.set_clipboard_protocol_enabled(true);
    terminal.advance(&READ[4..]);
    assert!(terminal.take_clipboard_requests().is_empty());

    terminal.advance(&READ[..READ.len() - 2]);
    terminal.set_strict_escape_filter(true);
    terminal.set_strict_escape_filter(false);
    terminal.advance(b"\x07");
    assert!(terminal.take_clipboard_requests().is_empty());
    terminal.advance(READ);
    assert_eq!(terminal.take_clipboard_requests().len(), 1);
}

#[test]
fn sithtermfx_ansi_profiles_all_offer_the_same_mime_listing() {
    for profile in [
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
    ] {
        let mut terminal = Terminal::with_profile(40, 4, profile);
        terminal.advance(QUERY);
        assert_eq!(terminal.take_responses(), b"\x1b[?5522;0$y", "{profile}");
        terminal.set_clipboard_protocol_enabled(true);
        terminal.advance(QUERY);
        assert_eq!(terminal.take_responses(), b"\x1b[?5522;2$y", "{profile}");
        osc(&mut terminal, "type=read:id=probe", "Lg==");
        let request = terminal.take_clipboard_requests().pop_front().unwrap();
        let response = terminal.complete_clipboard_request(
            request.id,
            ClipboardResult::Types(vec!["image/png".into()]),
        );
        assert_eq!(
            response,
            concat!(
                "\x1b]5522;type=read:status=OK:id=probe\x1b\\",
                "\x1b]5522;type=read:status=DATA:id=probe:mime=Lg==;aW1hZ2UvcG5nCg==\x1b\\",
                "\x1b]5522;type=read:status=DONE:id=probe\x1b\\"
            )
            .as_bytes(),
            "{profile}"
        );
        terminal.set_strict_escape_filter(true);
        terminal.advance(QUERY);
        osc(&mut terminal, "type=read:id=strict", "Lg==");
        assert!(terminal.take_responses().is_empty(), "{profile}");
        assert!(terminal.take_clipboard_requests().is_empty(), "{profile}");
    }
}

#[test]
fn dec_eight_bit_osc_keeps_the_standard_seven_bit_clipboard_reply() {
    let mut terminal = Terminal::with_profile(40, 4, TerminalProfile::Vt320);
    terminal.set_clipboard_protocol_enabled(true);
    terminal.advance(b"\x1b[63;2\"p\x9d5522;type=write:id=bad\x9c");
    terminal.advance(b"\x9d5522;type=wdata:mime=aW1hZ2UvcG5n;!!!!\x9c");
    assert_eq!(
        terminal.take_responses(),
        b"\x1b]5522;type=write:status=EINVAL:id=bad\x1b\\"
    );
}
