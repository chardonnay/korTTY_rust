use super::*;
use crate::terminal_clipboard::TerminalClipboard;

const ENABLE_PASTE: &[u8] = b"\x1b[?5522h";
const DISABLE_PASTE: &[u8] = b"\x1b[?5522l";
const LEGACY_WRITE: &[u8] = b"\x1b]52;c;Y2xpcGJvYXJk\x1b\\";
const TEXT_TYPE: &str = "dGV4dC9wbGFpbg==";
const PASTE_NAME: &str = "UGFzdGUgZXZlbnQ=";

fn paste_mode() -> (Terminal, ClipboardBridge) {
    let mut terminal = Terminal::new(12, 3);
    terminal.set_clipboard_protocol_enabled(true);
    terminal.advance(ENABLE_PASTE);
    let mut bridge = ClipboardBridge::default();
    assert!(bridge.capture(&mut terminal).is_empty());
    assert!(bridge.paste_mode);
    (terminal, bridge)
}

fn read(terminal: &mut Terminal, id: &str, password: Option<&str>) {
    let credentials = password.map_or_else(String::new, |password| {
        format!(":name={PASTE_NAME}:pw={password}")
    });
    let command = format!("\x1b]5522;type=read:id={id}{credentials};{TEXT_TYPE}\x1b\\");
    // A real byte stream, including an ST that arrives across reads.
    for byte in command.bytes() {
        terminal.advance(&[byte]);
    }
}

fn queue_paste(bridge: &mut ClipboardBridge, text: String, seed: u8) {
    bridge.paste = Some(Paste {
        // Deterministic nonce for this test fixture only.
        nonce: [seed; 16],
        text,
        fallback: b"manual paste".to_vec(),
    });
}

fn announcement(
    bridge: &mut ClipboardBridge,
    terminal: &mut Terminal,
    text: String,
    seed: u8,
) -> String {
    queue_paste(bridge, text, seed);
    let output = bridge.apply(terminal);
    assert!(output.fallback.is_none());
    let packet = std::str::from_utf8(&output.bytes).unwrap();
    let first = packet.split("\x1b\\").next().unwrap();
    assert!(first.starts_with("\x1b]5522;type=read:status=OK:pw="));
    let (_, password) = first.split_once(":pw=").unwrap();
    let (password, name) = password.split_once(":name=").unwrap();
    assert_eq!(name, PASTE_NAME);
    assert!(packet.contains(&format!(":mime=Lg==:pw={password};dGV4dC9wbGFpbgo=")));
    password.to_owned()
}

fn ungranted_request(bridge: &mut ClipboardBridge) -> ClipboardRequestId {
    let Some(ClipboardEvent::Protocol(request)) = bridge.actions.pop_front() else {
        panic!("expected the regular consent path");
    };
    assert!(matches!(
        request.kind,
        ClipboardRequestKind::Read {
            granted_by_paste: false,
            ..
        }
    ));
    assert!(bridge.pending.contains(&request.id));
    request.id
}

fn deny(
    bridge: &mut ClipboardBridge,
    terminal: &mut Terminal,
    request: ClipboardRequestId,
    wire_id: &str,
) {
    // Test host decision. The real workspace sends this same completion
    // after applying its policy or a user decision.
    bridge.pending.retain(|&pending| pending != request);
    bridge.completions.push_back((
        request,
        ClipboardResult::Error(ClipboardError::PermissionDenied),
    ));
    let applied = bridge.apply(terminal);
    let expected = format!("\x1b]5522;type=read:status=EPERM:id={wire_id}\x1b\\");
    assert_eq!(applied.bytes, expected.as_bytes());
    assert!(applied.fallback.is_none());
}

#[test]
fn clipboard_defaults_deny_program_access_and_strict_revokes_pending_protocol() {
    let (mut terminal, mut bridge) = paste_mode();
    let policy = TerminalClipboard::default();
    assert!(!policy.read_allowed() && !policy.write_allowed());
    assert!(!bridge.paste_allowed);

    queue_paste(&mut bridge, "original".into(), 7);
    let applied = bridge.apply(&mut terminal);
    assert_eq!(
        applied.fallback.as_deref(),
        Some(b"manual paste".as_slice())
    );
    assert!(applied.bytes.is_empty() && bridge.grants.is_empty());

    read(&mut terminal, "default", None);
    assert!(bridge.capture(&mut terminal).is_empty());
    assert!(terminal.take_responses().is_empty());
    let request = ungranted_request(&mut bridge);
    deny(&mut bridge, &mut terminal, request, "default");

    read(&mut terminal, "strict-pending", None);
    assert!(bridge.capture(&mut terminal).is_empty());
    let request = ungranted_request(&mut bridge);
    bridge.strict = true;
    bridge.strict_dirty = true;
    bridge.revoke_protocol = true;
    bridge.revoke();
    assert!(bridge.apply(&mut terminal).bytes.is_empty());
    assert!(!terminal.clipboard_request_pending(request));

    terminal.advance(ENABLE_PASTE);
    terminal.advance(LEGACY_WRITE);
    read(&mut terminal, "strict", None);
    assert!(bridge.capture(&mut terminal).is_empty());
    assert!(bridge.actions.is_empty());
    assert!(terminal.take_responses().is_empty());
    assert!(!bridge.paste_mode);
}

#[test]
fn paste_grant_returns_original_snapshot_only_once() {
    let (mut terminal, mut bridge) = paste_mode();
    bridge.paste_allowed = true;
    let host = TerminalClipboard::default();
    host.buffer.set("original".into());
    let password = announcement(&mut bridge, &mut terminal, host.buffer.get().unwrap(), 7);
    host.buffer.set("replacement".into());

    read(&mut terminal, "first", Some(&password));
    let bytes = bridge.capture(&mut terminal);
    let wire = std::str::from_utf8(&bytes).unwrap();
    assert_eq!(
        wire,
        concat!(
            "\x1b]5522;type=read:status=OK:id=first\x1b\\",
            "\x1b]5522;type=read:status=DATA:id=first:mime=dGV4dC9wbGFpbg==;b3JpZ2luYWw=\x1b\\",
            "\x1b]5522;type=read:status=DONE:id=first\x1b\\"
        )
    );
    assert!(bridge.grants.is_empty() && bridge.actions.is_empty());

    read(&mut terminal, "again", Some(&password));
    assert!(bridge.capture(&mut terminal).is_empty());
    let request = ungranted_request(&mut bridge);
    deny(&mut bridge, &mut terminal, request, "again");
    assert_eq!(host.buffer.get().as_deref(), Some("replacement"));
}

#[test]
fn expired_and_revoked_paste_grants_cannot_reuse_a_snapshot() {
    let (mut terminal, mut bridge) = paste_mode();
    bridge.paste_allowed = true;
    let password = announcement(&mut bridge, &mut terminal, "original".into(), 7);
    bridge.grants[0].until = Instant::now();

    read(&mut terminal, "expired", Some(&password));
    assert_eq!(
        bridge.capture(&mut terminal),
        b"\x1b]5522;type=read:status=EPERM:id=expired\x1b\\"
    );
    assert!(bridge.grants.is_empty() && bridge.actions.is_empty());

    let password = announcement(&mut bridge, &mut terminal, "new snapshot".into(), 8);
    terminal.advance(DISABLE_PASTE);
    assert!(bridge.capture(&mut terminal).is_empty());
    assert!(!bridge.paste_mode && bridge.grants.is_empty());
    terminal.advance(ENABLE_PASTE);
    assert!(bridge.capture(&mut terminal).is_empty());
    assert!(bridge.paste_mode);

    read(&mut terminal, "revoked", Some(&password));
    assert!(bridge.capture(&mut terminal).is_empty());
    let request = ungranted_request(&mut bridge);
    deny(&mut bridge, &mut terminal, request, "revoked");
}
