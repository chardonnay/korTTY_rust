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
fn ansi_identity_and_cursor_queries_preserve_graphics_and_key_modes() {
    let input = b"\x1b(0lqk\x1b(B\x1b[2;2H\x1b[?1h\x1b>\x1b[c\x1b[>c\x1b[6n\x1b[>1c\x1b[>0c";
    for (profile, identity) in [
        (TerminalProfile::Xterm, "\x1b[?6c"),
        (TerminalProfile::Vt100, "\x1b[?1;2c"),
        (TerminalProfile::Vt102, "\x1b[?6c"),
        (TerminalProfile::Vt220, "\x1b[?62;1;2;6;7;8;9c"),
        (TerminalProfile::Vt320, "\x1b[?63;1;2;6;7;8;9c"),
        (TerminalProfile::Vt420, "\x1b[?64;1;2;6;7;8;9;15;18;21c"),
        (TerminalProfile::Vt520, "\x1b[?65;1;2;6;7;8;9;15;18;21c"),
        (TerminalProfile::SunCde, "\x1b[?62;1;2;6;7;8;9c"),
        (TerminalProfile::ScoAnsi, "\x1b[?62;1;2;6;7;8;9c"),
        (TerminalProfile::Cterm, "\x1b[?63;1;2;6;7;8;9c"),
    ] {
        for chunk_size in [1, 7, input.len()] {
            let mut terminal = Terminal::with_profile(12, 4, profile);
            for chunk in input.chunks(chunk_size) {
                terminal.advance(chunk);
            }
            let context = format!("{profile} chunk={chunk_size}");
            let replies = format!("{identity}{identity}\x1b[2;2R{identity}");
            assert_eq!(terminal.take_responses(), replies.as_bytes(), "{context}");
            let screen = terminal.snapshot();
            let graphics: String = screen.row(0).unwrap()[..3]
                .iter()
                .map(|cell| cell.text.as_str())
                .collect();
            assert_eq!(graphics, "┌─┐", "{context} graphics");
            assert_eq!(
                (screen.cursor.column, screen.cursor.row),
                (1, 1),
                "{context}"
            );
            assert!(terminal.application_cursor_keys(), "{context}");
            assert!(!terminal.application_keypad(), "{context}");
            assert_eq!(
                screen.encode_key(TerminalKey::KeypadUp, KeyModifiers::default()),
                Some(b"\x1b[A".to_vec()),
                "{context} keypad Up"
            );
        }
    }
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
fn sco_and_sun_keys_preserve_reference_modifiers_and_keypad_mode() {
    for line in include_str!("fixtures/sithtermfx/sco-sun-keys.tsv").lines() {
        let [profile, mode, key, modifiers, expected] =
            line.split('\t').collect::<Vec<_>>().try_into().unwrap();
        let profile = TerminalProfile::from_term_name(profile).unwrap();
        let mut terminal = Terminal::with_profile(12, 3, profile);
        let input: &[u8] = match mode {
            "normal" => b"",
            "application" => b"\x1b=",
            "restored" => b"\x1b=\x1b>",
            _ => panic!("unexpected keypad mode {mode}"),
        };
        for byte in input {
            terminal.advance(std::slice::from_ref(byte));
        }
        let key = match key {
            "home" => TerminalKey::Home,
            "end" => TerminalKey::End,
            "insert" => TerminalKey::Insert,
            _ => TerminalKey::Function(key.strip_prefix('f').unwrap().parse().unwrap()),
        };
        let modifiers = KeyModifiers {
            shift: modifiers.contains('s'),
            alt: modifiers.contains('a'),
            control: modifiers.contains('c'),
        };
        let (pairs, remainder) = expected.as_bytes().as_chunks::<2>();
        assert!(remainder.is_empty());
        let expected: Vec<_> = pairs
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        assert_eq!(
            terminal.snapshot().encode_key(key, modifiers),
            Some(expected),
            "{line}"
        );
    }

    for profile in [TerminalProfile::SunCde, TerminalProfile::ScoAnsi] {
        let mut terminal = Terminal::with_profile(12, 3, profile);
        terminal.advance(b"\x1b G\x1b=");
        let screen = terminal.snapshot();
        let alt = KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        };
        let control = KeyModifiers {
            control: true,
            ..KeyModifiers::default()
        };
        assert_eq!(
            screen.encode_key(TerminalKey::End, alt).as_deref(),
            Some(b"\x1b\x8fF".as_slice())
        );
        assert_eq!(
            screen.encode_key(TerminalKey::Home, control).as_deref(),
            Some(b"\x9b1;5H".as_slice())
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
