use kortty_ibm::Record;
use kortty_ibm::screen5250::{Aid5250, Edit5250, Error5250, Model5250, Position5250, Tn5250Screen};

fn record(opcode: u8, payload: &[u8]) -> Record {
    let [high, low] = u16::try_from(10 + payload.len()).unwrap().to_be_bytes();
    let mut bytes = vec![high, low, 0x12, 0xa0, 0, 0, 4, 0, 0, opcode];
    bytes.extend_from_slice(payload);
    Record::Tn5250e(bytes)
}

const LOGIN: &[u8] = &[
    0x04, 0x40, // Clear Unit
    0x04, 0x11, 0, 0x0c, // Write to Display, unlock and alarm
    0x11, 1, 2, 0x20, 0xe4, 0xe2, 0xc5, 0xd9, 0x7a, // USER:
    0x11, 1, 10, 0x1d, 0x40, 0x20, 0x20, 0, 3, // Monocase input
    0x11, 2, 10, 0x1d, 0x43, 0x08, 0x27, 0, 4, // Mandatory nondisplay numeric field
    0x13, 1, 11, // Home and next unlock cursor
    0x04, 0x52, 0, 0, // Read MDT Fields
];

#[test]
fn gds_screen_fields_invite_and_aid_use_distinct_opcode_and_command_bytes() {
    let mut screen = Tn5250Screen::new();
    let update = screen.apply_record(&record(3, LOGIN)).unwrap();
    assert!(update.alarm);
    assert!(update.reply.is_none());

    let snapshot = screen.snapshot();
    assert_eq!((snapshot.columns, snapshot.rows), (80, 24));
    assert_eq!(snapshot.cursor, Position5250 { column: 10, row: 0 });
    assert!(!snapshot.keyboard_locked);
    assert!(snapshot.invited);
    assert_eq!(
        snapshot.cells[2..7]
            .iter()
            .map(|cell| cell.character)
            .collect::<String>(),
        "USER:"
    );
    assert!(!snapshot.cells[2].editable);
    assert!(snapshot.cells[10].editable);

    screen.insert_text("ab").unwrap();
    screen.edit(Edit5250::Tab).unwrap();
    assert_eq!(
        screen.snapshot().cursor,
        Position5250 { column: 10, row: 1 }
    );
    assert_eq!(
        screen.send_aid(Aid5250::Enter).err(),
        Some(Error5250::FieldValidation)
    );
    screen.edit(Edit5250::Reset).unwrap();
    assert_eq!(screen.insert_text("1x"), Err(Error5250::FieldValidation));
    screen.edit(Edit5250::Reset).unwrap();
    screen.insert_text("1").unwrap();
    screen
        .set_cursor(Position5250 { column: 12, row: 1 })
        .unwrap();
    screen.insert_text("3").unwrap();
    let snapshot = screen.snapshot();
    assert!(
        snapshot.cells[90..94]
            .iter()
            .all(|cell| cell.hidden && cell.character == ' ')
    );
    assert_eq!(
        snapshot.cells[10..12]
            .iter()
            .map(|cell| cell.character)
            .collect::<String>(),
        "AB"
    );

    let reply = screen.send_aid(Aid5250::Enter).unwrap().reply;
    assert_eq!(
        reply,
        Some(record(
            0,
            &[
                2, 14, 0xf1, 0x11, 1, 11, 0xc1, 0xc2, 0x11, 2, 11, 0xf1, 0x40, 0xf3
            ]
        ))
    );
    assert!(screen.snapshot().keyboard_locked);
    assert!(!screen.snapshot().invited);
}

#[test]
fn modified_alternate_and_cancel_invite_are_distinct_from_ordinary_input() {
    let mut screen = Tn5250Screen::new();
    screen.apply_record(&record(3, LOGIN)).unwrap();
    screen.insert_text("a").unwrap();
    screen
        .set_cursor(Position5250 { column: 12, row: 0 })
        .unwrap();
    screen.insert_text("b").unwrap();

    let reply = screen.apply_record(&record(6, &[4, 0x83])).unwrap().reply;
    assert_eq!(
        reply,
        Some(record(0, &[2, 11, 0, 0x11, 1, 11, 0xc1, 0, 0xc2]))
    );
    assert!(screen.snapshot().invited);

    let reply = screen.apply_record(&record(0x0a, &[])).unwrap().reply;
    assert_eq!(reply, Some(record(0x0a, &[])));
    assert!(screen.snapshot().keyboard_locked);
    assert!(!screen.snapshot().invited);
    screen
        .apply_record(&record(2, &[4, 0x11, 0, 0x08]))
        .unwrap();
    assert_eq!(
        screen.send_aid(Aid5250::Pf(1)).err(),
        Some(Error5250::NotInvited)
    );
}

#[test]
fn row_one_column_one_field_and_gds_header_extension_are_supported() {
    let mut screen = Tn5250Screen::new();
    let mut bytes = vec![
        0, 32, 0x12, 0xa0, 0, 0, 5, 0, 0, 3, 0, // Extended variable header
        4, 0x40, 4, 0x11, 0, 8, 0x11, 1, 0, 0x1d, 0x40, 0, 0x20, 0, 2, 4, 0x42, 0,
        0, // Read Input Fields
    ];
    let length = u16::try_from(bytes.len()).unwrap().to_be_bytes();
    bytes[..2].copy_from_slice(&length);
    screen.apply_record(&Record::Tn5250e(bytes)).unwrap();
    assert_eq!(screen.snapshot().cursor, Position5250 { column: 0, row: 0 });
    assert!(screen.snapshot().cells[0].editable);
    assert_eq!(screen.insert_text("ABC"), Err(Error5250::FieldFull));
    screen.edit(Edit5250::Reset).unwrap();
    assert_eq!(screen.snapshot().cells[0].character, ' ');
    screen.insert_text("A").unwrap();
    assert_eq!(
        screen.send_aid(Aid5250::Pf(24)).unwrap().reply,
        Some(record(0, &[1, 2, 0xbc, 0xc1, 0x40]))
    );
}

#[test]
fn query_reply_advertises_only_tested_display_capabilities() {
    let mut screen = Tn5250Screen::new();
    let query = screen
        .apply_record(&record(3, &[4, 0xf3, 0, 5, 0xd9, 0x70, 0]))
        .unwrap();
    let Some(Record::Tn5250e(bytes)) = query.reply else {
        panic!("missing query reply")
    };
    assert_eq!(&bytes[..10], &[0, 71, 0x12, 0xa0, 0, 0, 4, 0, 0, 0]);
    let payload = &bytes[10..];
    assert_eq!(
        &payload[..10],
        &[0, 0, 0x88, 0, 0x3a, 0xd9, 0x70, 0x80, 6, 0]
    );
    assert_eq!(
        &payload[29..38],
        &[1, 0xf3, 0xf1, 0xf7, 0xf9, 0xf0, 0xf0, 0xf2, 2]
    );
    assert_eq!(&payload[44..46], &[1, 0]);
    assert_eq!(&payload[49..54], &[0x63, 0x11, 0, 0, 0]);
}

#[test]
fn write_error_code_uses_the_header_row_and_restores_the_original_screen_on_reset() {
    let mut screen = Tn5250Screen::new();
    screen
        .apply_record(&record(
            3,
            &[
                4, 0x40, 4, 0x11, 0, 8, 1, 4, 0, 0, 0,
                2, // SOH selects the second row as the error line.
                0x11, 2, 1, 0x20, 0xd6, 0xd3, 0xc4, // OLD
                0x11, 3, 1, 0x20, 0xd5, 0xc5, 0xe6, // NEW
                0x11, 1, 10, 0x1d, 0x40, 0x20, 0x20, 0, 3, 0x13, 1, 11, 4, 0x52, 0, 0,
            ],
        ))
        .unwrap();
    screen.edit(Edit5250::ToggleInsert).unwrap();
    let original = screen.snapshot();

    screen
        .apply_record(&record(
            2,
            &[4, 0x21, 0x2a, 0xf1, 0xf2, 0x13, 1, 12, 0xf3, 0xf4, 0x27],
        ))
        .unwrap();
    let error = screen.snapshot();
    assert_eq!(row_text(&error.cells[80..86]), " 1234 ");
    assert_eq!(error.cells[160..165], original.cells[160..165]);
    assert!(error.cells[81].blink);
    assert!(error.keyboard_locked && error.invited);
    assert!(!error.insert_mode);
    assert_eq!(error.cursor, Position5250 { row: 0, column: 11 });
    assert_eq!(screen.insert_text("x"), Err(Error5250::KeyboardLocked));
    assert_eq!(
        screen.send_aid(Aid5250::Enter).err(),
        Some(Error5250::KeyboardLocked)
    );

    let mut expected = record(0, &[0x12, 0x34]);
    let Record::Tn5250e(wire) = &mut expected else {
        unreachable!()
    };
    wire[7] = 1; // RFC 1205 Help in Error State flag.
    assert_eq!(
        screen.send_aid(Aid5250::Help).unwrap().reply,
        Some(expected)
    );
    assert!(!screen.snapshot().cells[81].blink);
    let waiting = screen.snapshot();
    screen.edit(Edit5250::Reset).unwrap();
    assert_eq!(screen.snapshot(), waiting);
    assert_eq!(
        screen.send_aid(Aid5250::Help).err(),
        Some(Error5250::KeyboardLocked)
    );

    screen
        .apply_record(&record(2, &[4, 0x21, 0x20, 0xc8, 0xc5, 0xd3, 0xd7]))
        .unwrap();
    assert_eq!(row_text(&screen.snapshot().cells[80..85]), " HELP");
    screen.edit(Edit5250::Reset).unwrap();
    let reset = screen.snapshot();
    assert_eq!(reset.cells, original.cells);
    assert_eq!(reset.cursor, error.cursor);
    assert!(!reset.keyboard_locked && reset.invited);
    screen.insert_text("B").unwrap();
    assert_eq!(
        screen.send_aid(Aid5250::Enter).unwrap().reply,
        Some(record(0, &[1, 13, 0xf1, 0x11, 1, 11, 0x40, 0xc2]))
    );
}

#[test]
fn malformed_write_error_code_is_atomic_and_an_unpacked_message_can_be_reset() {
    let mut screen = Tn5250Screen::new();
    screen.apply_record(&record(3, LOGIN)).unwrap();
    let original = screen.snapshot();
    let mut oversized = vec![4, 0x40, 4, 0x21];
    oversized.extend([0xc1; 81]);
    for bytes in [&[4, 0x21][..], &[4, 0x21, 0x13, 25, 1], &oversized] {
        assert!(screen.apply_record(&record(2, bytes)).is_err());
        assert_eq!(screen.snapshot(), original);
    }
    screen
        .apply_record(&record(2, &[4, 0x21, 0x20, 0xc1]))
        .unwrap();
    assert_eq!(
        row_text(&screen.snapshot().cells[23 * 80..23 * 80 + 2]),
        " A"
    );
    assert_eq!(
        screen.send_aid(Aid5250::Help).err(),
        Some(Error5250::FieldValidation)
    );
    assert!(screen.apply_record(&record(8, &[4, 0x62])).is_err());
    screen.edit(Edit5250::Reset).unwrap();
    assert_eq!(screen.snapshot(), original);
}

fn row_text(cells: &[kortty_ibm::screen5250::Cell5250]) -> String {
    cells.iter().map(|cell| cell.character).collect()
}

#[test]
fn roll_handles_zero_lines_and_rejects_invalid_ranges_and_host_error_state() {
    let mut screen = Tn5250Screen::new();
    screen.apply_record(&record(3, LOGIN)).unwrap();
    let initial = screen.snapshot();
    screen
        .apply_record(&record(2, &[4, 0x23, 0, 1, 24]))
        .unwrap();
    assert_eq!(screen.snapshot(), initial);

    for parameters in [[1, 0, 24], [1, 1, 25], [1, 2, 2], [2, 1, 2], [0x21, 1, 2]] {
        let mut command = vec![4, 0x23];
        command.extend(parameters);
        assert!(screen.apply_record(&record(2, &command)).is_err());
        assert_eq!(screen.snapshot(), initial);
    }
    assert!(
        screen
            .apply_record(&record(2, &[4, 0x23, 1, 1, 24, 4, 0x23, 1, 1]))
            .is_err()
    );
    assert_eq!(screen.snapshot(), initial);

    screen
        .apply_record(&record(2, &[4, 0x21, 0x20, 0xf1]))
        .unwrap();
    let error = screen.snapshot();
    assert_eq!(
        screen.apply_record(&record(2, &[4, 0x23, 1, 1, 24])).err(),
        Some(Error5250::KeyboardLocked)
    );
    assert_eq!(screen.snapshot(), error);
}

#[test]
fn roll_cannot_expose_input_that_loses_its_nondisplay_attribute() {
    let mut screen = Tn5250Screen::new();
    screen
        .apply_record(&record(
            3,
            &[
                4, 0x40, 4, 0x11, 0, 8, 0x11, 1, 80, 0x1d, 0x40, 0, 0x27, 0,
                2, // Nondisplay field starts on row 2.
                0x13, 2, 1, 4, 0x52, 0, 0,
            ],
        ))
        .unwrap();
    screen.insert_text("Z").unwrap();
    let initial = screen.snapshot();
    assert!(initial.cells[80].hidden);
    assert_eq!(initial.cells[80].character, ' ');

    // Rolling from row 2 would leave the copy visible immediately. Rolling
    // from row 1 would hide it only until the host changes the attribute.
    for top in [2, 1] {
        assert_eq!(
            screen
                .apply_record(&record(2, &[4, 0x23, 0x81, top, 3]))
                .err(),
            Some(Error5250::Malformed(
                "5250 Roll would expose nondisplay data"
            ))
        );
        assert_eq!(screen.snapshot(), initial);
    }
}

#[test]
fn saved_screens_are_session_local_bounded_and_require_an_exact_echo() {
    let mut screen = Tn5250Screen::new();
    screen.apply_record(&record(3, LOGIN)).unwrap();
    let save = |screen: &mut Tn5250Screen| {
        let update = screen.apply_record(&record(4, &[4, 2])).unwrap();
        let Record::Tn5250e(reply) = update.reply.unwrap() else {
            unreachable!();
        };
        assert_eq!(
            &reply[..12],
            &[0, 28, 0x12, 0xa0, 0, 0, 4, 0, 0, 4, 4, 0x12]
        );
        reply[10..].to_vec()
    };
    let first = save(&mut screen);
    let mut foreign = Tn5250Screen::new();
    assert!(foreign.apply_record(&record(5, &first)).is_err());
    let initial = screen.snapshot();
    assert!(screen.apply_record(&record(4, &[4, 2, 4, 0x40])).is_err());
    let mut modified = first.clone();
    modified[2] ^= 1;
    assert!(screen.apply_record(&record(5, &modified)).is_err());
    let mut trailing = first.clone();
    trailing.extend([4, 0x40]);
    assert!(screen.apply_record(&record(5, &trailing)).is_err());
    assert_eq!(screen.snapshot(), initial);

    let mut latest = Vec::new();
    for _ in 0..8 {
        latest = save(&mut screen);
    }
    assert!(screen.apply_record(&record(5, &first)).is_err());
    screen.apply_record(&record(2, &[4, 0x40])).unwrap();
    assert_ne!(screen.snapshot(), initial);
    screen.apply_record(&record(5, &latest)).unwrap();
    assert_eq!(screen.snapshot(), initial);
}

#[test]
fn wide_screen_validates_orders_and_restores_geometry_with_the_saved_screen() {
    let mut screen = Tn5250Screen::with_model(Model5250::Ibm3180Model2);
    let initial = screen.snapshot();
    for invalid in [
        &[4, 0x20][..],
        &[4, 0x20, 1],
        &[4, 0x20, 0, 4, 0x11, 0, 8, 0x11, 27, 133],
    ] {
        assert!(screen.apply_record(&record(2, invalid)).is_err());
        assert_eq!(screen.snapshot(), initial);
    }
    screen
        .apply_record(&record(
            2,
            &[
                4, 0x20, 0, 4, 0x11, 0, 8, 0x11, 27, 132, 0xc1, 0x14, 27, 132,
            ],
        ))
        .unwrap();
    screen
        .apply_record(&record(2, &[4, 0x23, 1, 26, 27]))
        .unwrap();
    let wide = screen.snapshot();
    assert_eq!((wide.columns, wide.rows), (132, 27));
    assert_eq!(wide.cells[26 * 132 - 1].character, 'A');
    assert_eq!(
        wide.cursor,
        Position5250 {
            row: 26,
            column: 131
        }
    );

    let mut error = vec![4, 0x21, 0x20];
    error.extend([0xc2; 131]);
    screen.apply_record(&record(2, &error)).unwrap();
    assert_eq!(screen.snapshot().cells.last().unwrap().character, 'B');
    screen.edit(Edit5250::Reset).unwrap();
    assert_eq!(screen.snapshot(), wide);

    let Record::Tn5250e(reply) = screen
        .apply_record(&record(4, &[4, 2]))
        .unwrap()
        .reply
        .unwrap()
    else {
        unreachable!();
    };
    screen.apply_record(&record(2, &[4, 0x40])).unwrap();
    assert_eq!(screen.snapshot().columns, 80);
    screen.apply_record(&record(5, &reply[10..])).unwrap();
    assert_eq!(screen.snapshot(), wide);
}

#[test]
fn malformed_or_unsupported_host_data_preserves_the_previous_screen() {
    let mut screen = Tn5250Screen::new();
    screen.apply_record(&record(3, LOGIN)).unwrap();
    let previous = screen.snapshot();
    for record in [
        record(3, &[4, 0x40, 4, 0x11, 0, 8, 0x11, 25, 1]),
        record(3, &[4, 0x11, 0, 8, 0x11, 24, 80, 0x10, 0, 2, 0xc1, 0xc2]),
        record(3, &[4, 0x11, 0, 8, 0x11, 1, 20, 0x02, 1, 19, 0xc1]),
        record(3, &[4, 0x11, 0, 8, 0x1d, 0x40, 0, 0x83, 0, 0x20, 0, 1]),
        record(3, &[4, 0x20, 0x00]),
        record(4, &[4]),
        Record::Tn5250e(vec![0; 10]),
    ] {
        assert!(screen.apply_record(&record).is_err());
        assert_eq!(screen.snapshot(), previous);
    }
}
