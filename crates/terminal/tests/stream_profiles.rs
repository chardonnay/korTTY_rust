use kortty_terminal::{Color, KeyModifiers, Terminal, TerminalKey, TerminalProfile};

#[test]
fn stream_metadata_has_unique_wire_identifiers() {
    let mut names = std::collections::HashSet::new();
    for profile in TerminalProfile::ALL {
        assert!(names.insert(profile.term_name()));
        assert_eq!(
            TerminalProfile::from_term_name(profile.term_name()),
            Some(profile)
        );
    }
    assert_eq!(names.len(), 19);
    assert_eq!(TerminalProfile::Petscii.default_size(), (40, 25));
    assert_eq!(TerminalProfile::ScoAnsi.default_size(), (80, 25));
}

#[test]
fn sun_sco_and_cterm_select_their_reference_identity_color_and_keys() {
    for (profile, expected_da, color, function) in [
        (
            TerminalProfile::SunCde,
            b"\x1b[?62;1;2;6;7;8;9c".as_slice(),
            Color::Indexed(1),
            b"\x1b[235z".as_slice(),
        ),
        (
            TerminalProfile::ScoAnsi,
            b"\x1b[?62;1;2;6;7;8;9c",
            Color::Indexed(1),
            b"\x1b[X",
        ),
        (
            TerminalProfile::Cterm,
            b"\x1b[?63;1;2;6;7;8;9c",
            Color::Default,
            b"\x1b[24~",
        ),
    ] {
        let mut terminal = Terminal::with_profile(12, 3, profile);
        for byte in b"\x9b2;3H\x1b[31mA\x1b[c" {
            terminal.advance(std::slice::from_ref(byte));
        }
        assert_eq!(terminal.take_responses(), expected_da);
        let snapshot = terminal.snapshot();
        let cell = &snapshot.row(1).unwrap()[2];
        assert_eq!((&*cell.text, cell.style.foreground), ("A", color));
        assert_eq!(
            snapshot
                .encode_key(TerminalKey::Function(12), KeyModifiers::default())
                .unwrap(),
            function
        );
    }
}

#[test]
fn keypad_state_and_legacy_cursor_encodings_reach_the_public_api() {
    let modifiers = KeyModifiers::default();
    let mut xterm = Terminal::new(80, 24);
    assert_eq!(
        xterm
            .snapshot()
            .encode_key(TerminalKey::Keypad(7), modifiers)
            .unwrap(),
        b"7"
    );
    xterm.advance(b"\x1b=");
    assert_eq!(
        xterm
            .snapshot()
            .encode_key(TerminalKey::Keypad(7), modifiers)
            .unwrap(),
        b"\x1bOw"
    );
    assert_eq!(
        xterm
            .snapshot()
            .encode_key(TerminalKey::KeypadEnter, modifiers)
            .unwrap(),
        b"\x1bOM"
    );
    assert_eq!(
        xterm
            .snapshot()
            .encode_key(TerminalKey::KeypadOperator('/'), modifiers)
            .unwrap(),
        b"\x1bOo"
    );

    for (profile, expected) in [
        (TerminalProfile::Tvi910, b"\x1b[B".as_slice()),
        (TerminalProfile::Tvi920, b"\x1b[B"),
        (TerminalProfile::Tvi925, b"\x0a"),
    ] {
        let terminal = Terminal::with_profile(80, 24, profile);
        assert_eq!(
            terminal
                .snapshot()
                .encode_key(TerminalKey::KeypadDown, modifiers)
                .unwrap(),
            expected
        );
    }
}

#[test]
fn keypad_arrows_use_keypad_mode_and_keep_dec_c1_encoding() {
    let modifiers = KeyModifiers::default();
    for (profile, application, normal) in [
        (
            TerminalProfile::Xterm,
            b"\x1bOD".as_slice(),
            b"\x1b[D".as_slice(),
        ),
        (
            TerminalProfile::Vt220,
            b"\x8fD".as_slice(),
            b"\x9bD".as_slice(),
        ),
    ] {
        let mut terminal = Terminal::with_profile(80, 24, profile);
        terminal.advance(b"\x1b G\x1b=");
        assert!(!terminal.application_cursor_keys());
        assert_eq!(
            terminal
                .snapshot()
                .encode_key(TerminalKey::KeypadLeft, modifiers)
                .as_deref(),
            Some(application)
        );

        terminal.advance(b"\x1b[?1h\x1b>");
        assert!(terminal.application_cursor_keys());
        assert_eq!(
            terminal
                .snapshot()
                .encode_key(TerminalKey::KeypadLeft, modifiers)
                .as_deref(),
            Some(normal)
        );
    }
}
