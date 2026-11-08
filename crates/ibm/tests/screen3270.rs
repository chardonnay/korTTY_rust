use kortty_ibm::screen3270::{
    Aid3270, Color3270, Edit3270, Error3270, Highlight3270, Keyboard3270, Position3270,
    Tn3270Screen,
};
use kortty_ibm::{Record, Tn3270eData};

fn record(payload: &[u8]) -> Record {
    Record::Tn3270e {
        kind: Tn3270eData::Display,
        payload: payload.to_vec(),
    }
}

fn row_zero(column: u16) -> Position3270 {
    Position3270 { column, row: 0 }
}

#[test]
fn field_input_redacts_hidden_data_and_sends_exact_read_modified_bytes() {
    let mut screen = Tn3270Screen::new();
    // Write a fixture with a protected label, an input field, a hidden
    // numeric field, and a protected boundary after that field.
    let login = [
        0xf5, 0xc3, 0x1d, 0xf0, 0xe4, 0xe2, 0xc5, 0xd9, 0x7a, 0x11, 0x40, 0xc8, 0x1d, 0x40, 0x11,
        0x40, 0x50, 0x1d, 0x60, 0xd7, 0xc9, 0xd5, 0x7a, 0x11, 0x40, 0xd8, 0x1d, 0x5c, 0x11, 0x40,
        0x5e, 0x1d, 0x60, 0x11, 0x40, 0xc9, 0x13,
    ];
    screen.apply_record(&record(&login)).unwrap();
    let initial = screen.snapshot();
    assert_eq!((initial.columns, initial.rows), (80, 24));
    assert_eq!(initial.cursor, row_zero(9));
    assert_eq!(
        initial.cells[1..6]
            .iter()
            .map(|cell| cell.character)
            .collect::<String>(),
        "USER:"
    );
    assert!(initial.cells[0].field_marker);
    assert!(initial.cells[1].field.unwrap().protected);

    screen.set_cursor(row_zero(1)).unwrap();
    assert_eq!(screen.insert_text("X"), Err(Error3270::ProtectedField));
    assert_eq!(screen.snapshot().keyboard, Keyboard3270::OperatorError);
    screen.edit(Edit3270::Reset).unwrap();
    screen.edit(Edit3270::Home).unwrap();
    screen.insert_text("A B ").unwrap();
    screen.set_cursor(row_zero(14)).unwrap();
    screen.insert_text("C").unwrap();

    screen.edit(Edit3270::Tab).unwrap();
    assert_eq!(screen.snapshot().cursor, row_zero(25));
    assert_eq!(screen.insert_text("1x"), Err(Error3270::NumericField));
    screen.edit(Edit3270::Reset).unwrap();
    screen.insert_text("12").unwrap();
    let snapshot = screen.snapshot();
    assert_eq!(snapshot.cursor, row_zero(27));
    assert!(snapshot.cells[25].field.unwrap().hidden);
    assert!(snapshot.cells[25].field.unwrap().modified);
    assert_eq!(snapshot.cells[25].character, ' ');
    assert_eq!(snapshot.cells[26].character, ' ');

    let reply = screen.send_aid(Aid3270::Enter).unwrap();
    assert_eq!(
        reply,
        record(&[
            0x7d, 0x40, 0x5b, 0x11, 0x40, 0xc9, 0xc1, 0x40, 0xc2, 0x40, 0xc3, 0x11, 0x40, 0xd9,
            0xf1, 0xf2,
        ])
    );
    assert_eq!(screen.snapshot().keyboard, Keyboard3270::HostLocked);
    screen.edit(Edit3270::Reset).unwrap();
    assert_eq!(screen.insert_text("2"), Err(Error3270::KeyboardLocked));

    // Reset MDT and restore keyboard are independent WCC bits.
    screen.apply_record(&record(&[0xf1, 0xc1])).unwrap();
    assert_eq!(screen.snapshot().keyboard, Keyboard3270::HostLocked);
    assert_eq!(
        screen.apply_record(&record(&[0xf6])).unwrap().reply,
        Some(record(&[0x7d, 0x40, 0x5b]))
    );
    screen.apply_record(&record(&[0xf1, 0xc2])).unwrap();
    assert_eq!(screen.snapshot().keyboard, Keyboard3270::Ready);
    assert_eq!(
        screen.apply_record(&record(&[0xf6])).unwrap().reply,
        Some(record(&[0x60, 0x40, 0x5b]))
    );
}

#[test]
fn edit_insert_delete_and_erase_never_cross_a_field_boundary() {
    let mut screen = Tn3270Screen::new();
    screen
        .apply_record(&record(&[
            0xf5, 0xc2, 0x1d, 0x40, 0x13, 0xc1, 0xc2, 0x11, 0x40, 0xc5, 0x1d, 0x60, 0xe7,
        ]))
        .unwrap();
    screen.edit(Edit3270::ToggleInsert).unwrap();
    screen.insert_text("é").unwrap();
    let cells = screen.snapshot().cells;
    assert_eq!(
        cells[1..4]
            .iter()
            .map(|cell| cell.character)
            .collect::<String>(),
        "éAB"
    );

    screen.edit(Edit3270::Reset).unwrap();
    screen.edit(Edit3270::Delete).unwrap();
    assert_eq!(screen.snapshot().cells[2].character, 'B');
    screen.edit(Edit3270::Backspace).unwrap();
    assert_eq!(screen.snapshot().cells[1].character, 'B');
    screen.edit(Edit3270::EraseEof).unwrap();
    let snapshot = screen.snapshot();
    assert_eq!(snapshot.cells[1].character, ' ');
    assert_eq!(snapshot.cells[6].character, 'X');
    assert!(snapshot.cells[1].field.unwrap().modified);
}

#[test]
fn write_orders_support_wrapped_addresses_style_and_selective_erasure() {
    let mut screen = Tn3270Screen::new();
    screen
        .apply_record(&record(&[
            0xf5, 0xc6, 0x29, 2, 0xc0, 0x68, 0x42, 0xf2, 0xc1, 0x28, 0x41, 0xf4, 0xc2, 0x11, 0x40,
            0xc4, 0x1d, 0x40, 0xc3, 0x11, 0x07, 0x7e, 0x3c, 0x40, 0x40, 0xe7,
        ]))
        .map(|update| assert!(update.alarm))
        .unwrap();
    let snapshot = screen.snapshot();
    assert_eq!(snapshot.cells[1].style.foreground, Some(Color3270::Red));
    assert_eq!(
        snapshot.cells[2].style.highlighting,
        Some(Highlight3270::Underline)
    );
    assert!(snapshot.cells[1].field.unwrap().intensified);
    assert_eq!(snapshot.cells[1918].character, 'X');
    assert_eq!(snapshot.cells[1919].character, 'X');

    screen
        .apply_record(&record(&[
            0xf1, 0x40, 0x11, 0x40, 0x40, 0x2c, 1, 0x42, 0xf1, 0x11, 0x40, 0xc1, 0x12, 0x40, 0xc6,
        ]))
        .unwrap();
    let snapshot = screen.snapshot();
    assert_eq!(snapshot.cells[1].character, 'A');
    assert_eq!(snapshot.cells[1].style.foreground, Some(Color3270::Blue));
    assert_eq!(snapshot.cells[5].character, ' ');
    assert!(snapshot.cells[4].field_marker);

    screen.apply_record(&record(&[0x6f])).unwrap();
    let snapshot = screen.snapshot();
    assert_eq!(snapshot.cursor, row_zero(5));
    assert_eq!(snapshot.cells[1919].character, ' ');
    assert_eq!(snapshot.cells[1].character, 'A');
}

#[test]
fn host_buffer_read_short_aids_and_all_modified_follow_wire_formats() {
    let mut screen = Tn3270Screen::new();
    // Host preset MDT on a protected field. The read must include it.
    screen
        .apply_record(&record(&[0xf5, 0xc2, 0x1d, 0x61, 0xc1, 0x00, 0x40]))
        .unwrap();
    assert_eq!(screen.send_aid(Aid3270::Pa(1)).unwrap(), record(&[0x6c]));
    assert_eq!(
        screen.apply_record(&record(&[0xf6])).unwrap().reply,
        Some(record(&[0x6c]))
    );
    assert_eq!(
        screen.apply_record(&record(&[0x6e])).unwrap().reply,
        Some(record(&[0x6c, 0x40, 0x40, 0x11, 0x40, 0xc1, 0xc1, 0x40]))
    );
    let response = screen
        .apply_record(&record(&[0xf2]))
        .unwrap()
        .reply
        .unwrap();
    let Record::Tn3270e { payload, .. } = response else {
        unreachable!()
    };
    assert_eq!(payload.len(), 3 + 1920 + 1);
    assert_eq!(
        &payload[..8],
        &[0x6c, 0x40, 0x40, 0x1d, 0x61, 0xc1, 0, 0x40]
    );
    screen.apply_record(&record(&[0xf1, 0xc2])).unwrap();
    assert_eq!(screen.send_aid(Aid3270::Clear).unwrap(), record(&[0x6d]));
    assert!(
        screen
            .snapshot()
            .cells
            .iter()
            .all(|cell| cell.character == ' ' && !cell.field_marker)
    );

    screen.apply_record(&record(&[0xf1, 0xc2])).unwrap();
    screen.insert_text("H I").unwrap();
    assert_eq!(
        screen.send_aid(Aid3270::Pf(24)).unwrap(),
        record(&[0x4c, 0x40, 0xc3, 0xc8, 0x40, 0xc9])
    );
}

#[test]
fn queries_are_bounded_and_malformed_updates_do_not_change_the_screen() {
    let mut screen = Tn3270Screen::new();
    screen.apply_record(&record(&[0xf5, 0xc2, 0xc1])).unwrap();
    let original = screen.snapshot();
    for invalid in [
        &[0xf5, 0xc2, 0xc2, 0x11, 0x40][..],
        &[0xf1, 0xc2, 0x11, 0x80, 0x40],
        &[0xf1, 0xc2, 0x11, 0x07, 0x80],
        &[0xf1, 0xc2, 0x29, 2, 0xc0, 0x40],
        &[0xf1, 0xc2, 0x2c, 0],
        &[0xf3, 0, 9, 1, 0xff, 2],
    ] {
        assert!(screen.apply_record(&record(invalid)).is_err());
        assert_eq!(screen.snapshot(), original);
    }
    let query = screen
        .apply_record(&record(&[0xf3, 0, 5, 1, 0xff, 2]))
        .unwrap();
    let mut expected = vec![
        0x88, 0, 9, 0x81, 0x80, 0x80, 0x81, 0x86, 0x87, 0x88, 0, 21, 0x81, 0x81, 1, 0x40, 0, 80, 0,
        24,
    ];
    expected.extend([0; 11]);
    expected.extend([
        0, 44, 0x81, 0x86, 0, 17, 0, 0xf4, 0xf0, 0xf0, 0xf1, 0xf1, 0xf2, 0xf2, 0xf3, 0xf3, 0xf4,
        0xf4, 0xf5, 0xf5, 0xf6, 0xf6, 0xf7, 0xf7, 0xf8, 0xf8, 0xf9, 0xf9, 0xfa, 0xfa, 0xfb, 0xfb,
        0xfc, 0xfc, 0xfd, 0xfd, 0xfe, 0xfe, 0xff, 0xff, 4, 2, 0, 0xf0, 0, 15, 0x81, 0x87, 5, 0,
        0xf0, 0xf0, 0xf0, 0xf1, 0xf1, 0xf2, 0xf2, 0xf4, 0xf4, 0, 7, 0x81, 0x88, 0, 1, 2,
    ]);
    assert_eq!(query.reply, Some(record(&expected)));
    let query = screen
        .apply_record(&record(&[0xf3, 0, 7, 1, 0xff, 3, 0, 0xfe]))
        .unwrap();
    assert_eq!(query.reply, Some(record(&[0x88, 0, 4, 0x81, 0xff])));
}

#[test]
fn partition_zero_structured_fields_are_ordered_and_atomic() {
    let mut screen = Tn3270Screen::new();
    let update = screen
        .apply_record(&record(&[
            0xf3, 0, 4, 3, 0x80, // Erase/Reset, alternate size
            0, 8, 0x40, 0, 0x7e, 0xc6, 0xc1, 0x13, // EWA with alarm; A and IC
            0, 7, 0x40, 0, 0xf1, 0xc2, 0xc2, // Write B at the cursor
        ]))
        .unwrap();
    assert!(update.alarm && update.reply.is_none());
    let original = screen.snapshot();
    assert_eq!((original.columns, original.rows), (80, 24));
    assert_eq!(original.cursor.column, 1);
    assert_eq!(original.cells[0].character, 'A');
    assert_eq!(original.cells[1].character, 'B');
    assert_eq!(original.keyboard, Keyboard3270::Ready);

    for invalid in [
        &[0xf3, 0, 4, 3, 0, 0, 5, 0x40, 1, 0xf1][..], // Nonzero partition
        &[0xf3, 0, 4, 3, 0, 0, 2],                    // Field without an ID
        &[0xf3, 0, 5, 1, 0xff, 2, 0, 4, 3, 0],        // Read Partition before the end
        &[0xf3, 0, 5, 9, 0, 3],                       // Reserved reply mode
        &[0xf3, 0, 6, 9, 0, 2, 0x43],                 // Unsupported character attribute
    ] {
        assert!(screen.apply_record(&record(invalid)).is_err());
        assert_eq!(screen.snapshot(), original);
    }
}

#[test]
fn reply_mode_query_and_reset_return_exact_wire_values() {
    let mut screen = Tn3270Screen::new();
    let modes = screen
        .apply_record(&record(&[0xf3, 0, 7, 1, 0xff, 3, 0, 0x88]))
        .unwrap();
    assert_eq!(
        modes.reply,
        Some(record(&[0x88, 0, 7, 0x81, 0x88, 0, 1, 2]))
    );
    screen
        .apply_record(&record(&[
            0xf3, 0, 5, 9, 0, 1, 0, 10, 0x40, 0, 0xf5, 0x02, 0x29, 1, 0x42, 0xf2,
        ]))
        .unwrap();
    let payload = |screen: &mut Tn3270Screen| {
        let update = screen.apply_record(&record(&[0xf2])).unwrap();
        let Record::Tn3270e { payload, .. } = update.reply.unwrap() else {
            unreachable!()
        };
        payload
    };
    assert_eq!(&payload(&mut screen)[3..7], &[0x29, 1, 0x42, 0xf2]);
    screen
        .apply_record(&record(&[0xf5, 0xc2, 0x29, 1, 0x42, 0xf2]))
        .unwrap();
    assert_eq!(&payload(&mut screen)[3..6], &[0x1d, 0x40, 0]);
}

#[test]
fn changing_to_nvt_and_back_clears_the_display_and_gates_editing() {
    let mut screen = Tn3270Screen::new();
    screen.apply_record(&record(&[0xf5, 0xc2, 0xc1])).unwrap();
    screen
        .apply_record(&Record::Tn3270e {
            kind: Tn3270eData::Nvt,
            payload: b"Host NVT data".to_vec(),
        })
        .unwrap();
    assert!(screen.snapshot().nvt_mode);
    assert_eq!(screen.snapshot().cells[0].character, ' ');
    assert_eq!(screen.insert_text("x"), Err(Error3270::NvtMode));
    screen.apply_record(&record(&[0xf1, 0xc2, 0xc2])).unwrap();
    assert!(!screen.snapshot().nvt_mode);
    assert_eq!(screen.snapshot().cells[0].character, 'B');
}
