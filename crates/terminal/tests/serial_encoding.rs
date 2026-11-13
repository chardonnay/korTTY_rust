use kortty_terminal::{Color, Terminal, TerminalProfile, TextEncoding};

#[test]
fn serial_encodings_round_trip_text_across_stream_emulations() {
    for profile in TerminalProfile::ALL
        .into_iter()
        .filter(|&profile| profile != TerminalProfile::Petscii)
    {
        for (encoding, bytes, text) in [
            (TextEncoding::Utf8, "¤é".as_bytes(), "¤é"),
            (TextEncoding::Latin1, b"\xa4\xe9".as_slice(), "¤é"),
            (TextEncoding::Latin9, b"\xa4\xe9".as_slice(), "€é"),
            (TextEncoding::Windows1252, b"\x80\xe9".as_slice(), "€é"),
        ] {
            let mut terminal = Terminal::with_profile_and_encoding(12, 2, profile, encoding);
            for byte in bytes {
                terminal.advance(std::slice::from_ref(byte));
            }
            let screen = terminal.snapshot();
            let row = screen.row(0).unwrap();
            assert_eq!(
                format!("{}{}", row[0].text, row[1].text),
                text,
                "{profile} {encoding}"
            );
            assert_eq!(screen.encode_text(text), bytes);
            let unrepresentable = if encoding == TextEncoding::Utf8 {
                "🙂".as_bytes()
            } else {
                b"?"
            };
            assert_eq!(screen.encode_text("🙂"), unrepresentable);
        }
    }
}

#[test]
fn serial_text_encoding_preserves_escape_and_profile_specific_protocol_bytes() {
    let mut ansi = Terminal::with_profile_and_encoding(
        16,
        2,
        TerminalProfile::Xterm,
        TextEncoding::Windows1252,
    );
    ansi.advance(b"\x1b[31");
    ansi.advance(b"m\x9b\x1b]2;\x9c\xe9\x07\x1b[5n");
    let screen = ansi.snapshot();
    assert_eq!(screen.row(0).unwrap()[0].text, "›");
    assert_eq!(
        screen.row(0).unwrap()[0].style.foreground,
        Color::Indexed(1)
    );
    assert_eq!(screen.title, "œé");
    assert_eq!(ansi.take_responses(), b"\x1b[0n");
    ansi.advance(b"\x1bc\x80");
    assert_eq!(ansi.snapshot().row(0).unwrap()[0].text, "€");

    let mut dec = Terminal::with_profile_and_encoding(
        16,
        2,
        TerminalProfile::Vt220,
        TextEncoding::Windows1252,
    );
    dec.advance(b"\x1b G\x9b2;3H\xa0X");
    assert_eq!(dec.snapshot().row(1).unwrap()[3].text, "X");
    assert_eq!(dec.snapshot().encode_text("›œé"), b"??\xe9");
    dec.advance(b"\x9b5n");
    assert_eq!(dec.take_responses(), b"\x9b0n");

    let mut utf8_dec =
        Terminal::with_profile_and_encoding(16, 2, TerminalProfile::Vt220, TextEncoding::Utf8);
    utf8_dec.advance(b"\x1b G\x9b2;3H\xc3");
    utf8_dec.advance(b"\xa9\x9b5n");
    assert_eq!(utf8_dec.snapshot().row(1).unwrap()[2].text, "é");
    assert_eq!(utf8_dec.take_responses(), b"\x9b0n");
    let mut native_dec = Terminal::with_profile(16, 2, TerminalProfile::Vt220);
    native_dec.advance(b"\xe9");
    assert_eq!(native_dec.snapshot().row(0).unwrap()[0].text, "é");
    assert_eq!(native_dec.snapshot().encode_text("é"), b"\xe9");

    for profile in [TerminalProfile::Wy160, TerminalProfile::Tvi925] {
        let mut legacy =
            Terminal::with_profile_and_encoding(120, 2, profile, TextEncoding::Windows1252);
        legacy.advance(b"\x1b=\x20\x80\x80");
        assert_eq!(legacy.snapshot().row(0).unwrap()[96].text, "€", "{profile}");
        let title: &[u8] = if profile == TerminalProfile::Wy160 {
            b"\x1bz0Caf\xe9\r"
        } else {
            b"\x1blCaf\xe9\r"
        };
        legacy.advance(title);
        assert_eq!(legacy.snapshot().title, "Café", "{profile}");
    }

    let mut wyse = Terminal::with_profile_and_encoding(
        16,
        2,
        TerminalProfile::Wy160,
        TextEncoding::Windows1252,
    );
    wyse.advance(b"\x1bPignored\x9cstill ignored\x1b\\\x80");
    assert_eq!(wyse.snapshot().row(0).unwrap()[0].text, "€");

    let mut petscii = Terminal::with_profile_and_encoding(
        16,
        2,
        TerminalProfile::Petscii,
        TextEncoding::Windows1252,
    );
    let mut native = Terminal::with_profile(16, 2, TerminalProfile::Petscii);
    for terminal in [&mut petscii, &mut native] {
        terminal.advance(b"\x0ehello\x93\x80AB");
    }
    assert_eq!(petscii.snapshot(), native.snapshot());
}
