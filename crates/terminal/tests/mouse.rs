use kortty_terminal::{KeyModifiers, MouseButton, MouseEvent, MousePosition, Terminal};

const POSITION: MousePosition = MousePosition {
    cell: (2, 4),
    pixel: (34, 72),
};

#[test]
fn sgr_reports_click_release_drag_wheel_and_modifiers() {
    let mut terminal = Terminal::new(80, 24);
    terminal.advance(b"\x1b[?1002;1006h\x1b[?1002$p");
    assert_eq!(terminal.take_responses(), b"\x1b[?1002;1$y");
    let snapshot = terminal.snapshot();
    assert!(snapshot.mouse_reporting());
    let encode = |event| {
        snapshot.encode_mouse(
            event,
            POSITION,
            KeyModifiers {
                control: true,
                ..KeyModifiers::default()
            },
        )
    };
    assert_eq!(
        encode(MouseEvent::Press(MouseButton::Left)).unwrap(),
        b"\x1b[<16;3;5M"
    );
    assert_eq!(
        encode(MouseEvent::Release(MouseButton::Left)).unwrap(),
        b"\x1b[<16;3;5m"
    );
    assert_eq!(
        encode(MouseEvent::Move(Some(MouseButton::Right))).unwrap(),
        b"\x1b[<50;3;5M"
    );
    assert_eq!(
        encode(MouseEvent::Press(MouseButton::WheelUp)).unwrap(),
        b"\x1b[<80;3;5M"
    );
    assert_eq!(encode(MouseEvent::Move(None)), None);
    assert_eq!(encode(MouseEvent::Release(MouseButton::WheelUp)), None);
}

#[test]
fn reports_pixel_and_utf8_positions_and_honors_legacy_coordinate_limits() {
    let mut terminal = Terminal::new(350, 25);
    terminal.advance(b"\x1b[?1003;1016h");
    let encode = |terminal: &Terminal, event, position| {
        terminal
            .snapshot()
            .encode_mouse(event, position, KeyModifiers::default())
    };
    assert_eq!(
        encode(&terminal, MouseEvent::Move(None), POSITION).unwrap(),
        b"\x1b[<35;35;73M"
    );

    let far = MousePosition {
        cell: (250, 4),
        ..POSITION
    };
    let left = MouseEvent::Press(MouseButton::Left);
    terminal.advance(b"\x1b[?1016l");
    assert_eq!(encode(&terminal, left, far), None);
    assert_eq!(encode(&terminal, left, POSITION).unwrap(), b"\x1b[M #%");
    terminal.advance(b"\x1b[?1005h");
    assert_eq!(
        encode(&terminal, left, far).unwrap(),
        "\x1b[M ě%".as_bytes()
    );
    terminal.advance(b"\x1b[?1015h");
    assert_eq!(
        encode(&terminal, MouseEvent::Release(MouseButton::Right), far).unwrap(),
        b"\x1b[35;251;5M"
    );
}

#[test]
fn x10_reports_only_button_presses_and_reset_disables_reports() {
    let mut terminal = Terminal::new(80, 24);
    terminal.advance(b"\x1b[?9h");
    let modifiers = KeyModifiers {
        shift: true,
        ..KeyModifiers::default()
    };
    let snapshot = terminal.snapshot();
    assert_eq!(
        snapshot
            .encode_mouse(MouseEvent::Press(MouseButton::Middle), POSITION, modifiers)
            .unwrap(),
        b"\x1b[M!#%"
    );
    assert!(
        snapshot
            .encode_mouse(
                MouseEvent::Release(MouseButton::Middle),
                POSITION,
                modifiers
            )
            .is_none()
    );
    terminal.advance(b"\x1bc\x1b[?9$p");
    assert!(!terminal.snapshot().mouse_reporting());
    assert_eq!(terminal.take_responses(), b"\x1b[?9;2$y");
}
