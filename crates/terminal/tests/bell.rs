use kortty_terminal::{Terminal, TerminalProfile};

#[test]
fn every_character_profile_emits_one_event_per_ground_bel() {
    for profile in TerminalProfile::ALL {
        let mut terminal = Terminal::with_profile(20, 4, profile);
        let initial = terminal.snapshot();
        for input in [b"\x07".as_slice(), b"\x07\x07"] {
            terminal.advance(input);
        }
        assert_eq!(terminal.take_bell_events(), 3, "{profile:?}");
        assert_eq!(terminal.take_bell_events(), 0, "{profile:?}");
        assert_eq!(terminal.snapshot(), initial, "{profile:?}");
        assert!(terminal.take_responses().is_empty(), "{profile:?}");
    }
}

#[test]
fn ansi_osc_terminators_and_opaque_escape_payloads_do_not_ring() {
    let input = b"\x1b]0;title\x07\x1b]133;A\x07\x1bPbad\x07\x1b\\\x1b_bad\x07\x1b\\\x07";
    for profile in [TerminalProfile::Xterm, TerminalProfile::Vt220] {
        for chunk_size in [1, 2, 3, 7, input.len()] {
            let mut terminal = Terminal::with_profile(20, 4, profile);
            for chunk in input.chunks(chunk_size) {
                terminal.advance(chunk);
            }
            assert_eq!(terminal.take_bell_events(), 1, "{profile:?}, {chunk_size}");
            assert_eq!(terminal.snapshot().title, "title");
        }
    }
}

#[test]
fn bell_after_escape_follows_reference_dispatch_and_split_csi_still_rings() {
    for profile in TerminalProfile::ALL {
        let mut terminal = Terminal::with_profile(20, 4, profile);
        terminal.advance(b"\x1b");
        terminal.advance(b"\x07X");
        assert_eq!(
            terminal.take_bell_events(),
            u32::from(profile == TerminalProfile::Petscii),
            "{profile:?}"
        );
    }
    let mut terminal = Terminal::new(20, 4);
    terminal.advance(b"\x1b[3");
    terminal.advance(b"\x07mX");
    assert_eq!(terminal.take_bell_events(), 1);
    assert!(terminal.snapshot().cells[0].style.italic);
}

#[test]
fn resets_preserve_pending_bells_and_local_serial_echo_cannot_ring() {
    let mut terminal = Terminal::new(20, 4);
    terminal.advance(b"\x07\x1bc\x07\x1b[!p\x07");
    terminal.echo_serial_input(b"\x07");
    assert_eq!(terminal.take_bell_events(), 3);
    assert_eq!(terminal.take_bell_events(), 0);
}
