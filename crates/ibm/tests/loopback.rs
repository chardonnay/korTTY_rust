use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use kortty_ibm::screen3270::{Aid3270, Error3270, Keyboard3270, Tn3270Screen};
use kortty_ibm::screen5250::{Aid5250, Edit5250, Error5250, Tn5250Screen};
use kortty_ibm::{Client, DeviceName5250, Plaintext, Profile, Record, Tn3270eData};

const TIMEOUT: Duration = Duration::from_secs(3);
const IAC: u8 = 255;
const EOR: u8 = 239;
const SE: u8 = 240;
const SB: u8 = 250;
const WILL: u8 = 251;
const WONT: u8 = 252;
const DO: u8 = 253;
const DONT: u8 = 254;

#[test]
fn denied_plaintext_does_not_open_a_tcp_socket() {
    let listener = listener();
    listener.set_nonblocking(true).unwrap();
    let error = Client::connect(
        listener.local_addr().unwrap(),
        Profile::Tn3270e,
        Plaintext::default(),
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn cancelling_stalled_negotiation_closes_the_client() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let (done, finished) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = Client::connect_cancellable(
            address,
            Profile::Tn3270e,
            Plaintext::ExplicitlyAllowed,
            TIMEOUT,
            || worker_cancelled.load(Ordering::Relaxed),
        );
        done.send(result.err().map(|error| error.kind())).unwrap();
    });
    let mut peer = accept(listener);
    cancelled.store(true, Ordering::Relaxed);
    assert_eq!(
        finished.recv_timeout(TIMEOUT).unwrap(),
        Some(io::ErrorKind::Interrupted)
    );
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    worker.join().unwrap();
}

#[test]
fn tn3270e_negotiates_device_and_functions_then_exchanges_records() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        negotiate_3270(&mut peer, true);

        // Fragment each part of an escaped byte and the EOR independently.
        peer.write_all(&[0, 0, 0, 0, 0, 0xf5, IAC]).unwrap();
        peer.write_all(&[IAC, 0x11, IAC]).unwrap();
        peer.write_all(&[EOR, 5, 0, 0, 0, 0, b'X', IAC, EOR])
            .unwrap();

        expect(&mut peer, &[0, 0, 0, 0, 0, 0x7d, IAC, IAC, IAC, EOR]);
        peer.shutdown(Shutdown::Write).unwrap();
    });
    let mut client =
        Client::connect(address, Profile::Tn3270e, Plaintext::ExplicitlyAllowed).unwrap();
    assert_eq!(
        client.read_record(TIMEOUT).unwrap(),
        Some(Record::Tn3270e {
            kind: Tn3270eData::Display,
            payload: vec![0xf5, IAC, 0x11]
        })
    );
    assert_eq!(
        client.read_record(TIMEOUT).unwrap(),
        Some(Record::Tn3270e {
            kind: Tn3270eData::Nvt,
            payload: vec![b'X']
        })
    );
    let error = client
        .send_record(&Record::Tn5250e(Vec::new()))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    client
        .send_record(&Record::Tn3270e {
            kind: Tn3270eData::Display,
            payload: vec![0x7d, IAC],
        })
        .unwrap();
    assert_eq!(client.read_record(TIMEOUT).unwrap(), None);
    server.join().unwrap();
}

#[test]
fn traditional_tn3270_renders_and_returns_aid_without_an_enhanced_header() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        negotiate_classic_3270(&mut peer);
        // The selected framing must survive an attempt to upgrade later.
        exchange(&mut peer, &[IAC, DO, 40], &[IAC, WONT, 40]);
        peer.write_all(&[0xf5, 0xc2, 0x1d, 0x60, 0xe7, IAC])
            .unwrap();
        peer.write_all(&[IAC, 0x11, 0x40, 0xc5, 0x1d, 0x40, 0x13, IAC])
            .unwrap();
        peer.write_all(&[EOR]).unwrap();
        expect(
            &mut peer,
            &[0x7d, 0x40, 0xc7, 0x11, 0x40, 0xc6, 0xe9, IAC, EOR],
        );
        peer.write_all(&[IAC, WONT, 0]).unwrap();
    });
    let mut client =
        Client::connect(address, Profile::Tn3270e, Plaintext::ExplicitlyAllowed).unwrap();
    let record = client.read_record(TIMEOUT).unwrap().unwrap();
    assert_eq!(
        record,
        Record::Tn3270e {
            kind: Tn3270eData::Display,
            payload: vec![
                0xf5, 0xc2, 0x1d, 0x60, 0xe7, IAC, 0x11, 0x40, 0xc5, 0x1d, 0x40, 0x13
            ],
        }
    );
    let mut screen = Tn3270Screen::new();
    screen.apply_record(&record).unwrap();
    assert_eq!(screen.snapshot().cells[1].character, 'X');
    screen.insert_text("Z").unwrap();
    assert_eq!(
        client
            .send_record(&Record::Tn3270e {
                kind: Tn3270eData::Nvt,
                payload: vec![b'X'],
            })
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput,
    );
    client
        .send_record(&screen.send_aid(Aid3270::Enter).unwrap())
        .unwrap();
    assert_eq!(
        client.read_record(TIMEOUT).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        client.read_record(TIMEOUT).unwrap_err().kind(),
        io::ErrorKind::NotConnected
    );
    server.join().unwrap();
}

#[test]
fn tn3270e_device_rejection_can_negotiate_traditional_framing() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        request_3270e_device(&mut peer);
        exchange(
            &mut peer,
            &[IAC, SB, 40, 2, 6, 5, 4, IAC, SE],
            &[IAC, WONT, 40],
        );
        negotiate_classic_3270(&mut peer);
        exchange(
            &mut peer,
            &[0xf1, IAC, IAC, IAC, EOR],
            &[0x7d, IAC, IAC, IAC, EOR],
        );
    });
    let mut client =
        Client::connect(address, Profile::Tn3270e, Plaintext::ExplicitlyAllowed).unwrap();
    assert_eq!(
        client.read_record(TIMEOUT).unwrap(),
        Some(Record::Tn3270e {
            kind: Tn3270eData::Display,
            payload: vec![0xf1, IAC]
        })
    );
    client
        .send_record(&Record::Tn3270e {
            kind: Tn3270eData::Display,
            payload: vec![0x7d, IAC],
        })
        .unwrap();
    server.join().unwrap();
}

#[test]
fn tn3270e_rejection_with_incomplete_classic_negotiation_still_fails() {
    for send_record in [false, true] {
        let listener = listener();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut peer = accept(listener);
            request_3270e_device(&mut peer);
            exchange(
                &mut peer,
                &[IAC, SB, 40, 2, 6, 5, 4, IAC, SE],
                &[IAC, WONT, 40],
            );
            if send_record {
                negotiate_3270_terminal_type(&mut peer);
                // Only the server's WILL EOR is missing. Display data must
                // still be rejected before the complete option exchange.
                exchange(
                    &mut peer,
                    &[IAC, DO, 25, IAC, DO, 0, IAC, WILL, 0],
                    &[IAC, WILL, 25, IAC, WILL, 0, IAC, DO, 0],
                );
                peer.write_all(&[0xf1, 0xc2, IAC, EOR]).unwrap();
            }
            peer.shutdown(Shutdown::Write).unwrap();
        });
        let error = Client::connect(address, Profile::Tn3270e, Plaintext::ExplicitlyAllowed)
            .err()
            .unwrap();
        if send_record {
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        } else {
            assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
            assert!(error.to_string().contains("reason 4"));
        }
        server.join().unwrap();
    }
}

#[test]
fn tn3270e_takes_priority_while_its_device_and_functions_are_pending() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        request_3270e_device(&mut peer);
        negotiate_classic_3270(&mut peer);
        finish_3270e(&mut peer, false);
        peer.write_all(&[0, 0, 0, 0, 0, 0xf1, 0xc2, IAC, EOR])
            .unwrap();
    });
    let mut client =
        Client::connect(address, Profile::Tn3270e, Plaintext::ExplicitlyAllowed).unwrap();
    assert_eq!(
        client.read_record(TIMEOUT).unwrap(),
        Some(Record::Tn3270e {
            kind: Tn3270eData::Display,
            payload: vec![0xf1, 0xc2],
        })
    );
    server.join().unwrap();
}

#[test]
fn tn5250_negotiates_binary_eor_type_and_its_display_environment() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let gds = [0, 12, 0x12, 0xa0, 0, 0, 4, 0, 0, 3, 0x11, IAC];
    let wire = [
        0, 12, 0x12, 0xa0, 0, 0, 4, 0, 0, 3, 0x11, IAC, IAC, IAC, EOR,
    ];
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        exchange(
            &mut peer,
            &[IAC, DO, 39, IAC, DO, 24],
            &[IAC, WILL, 39, IAC, WILL, 24],
        );
        exchange(
            &mut peer,
            &[IAC, SB, 39, 1, 0, IAC, SE],
            b"\xff\xfa\x27\x00\x03KBDTYPE\x01USB\x03CODEPAGE\x0137\x03CHARSET\x01697\xff\xf0",
        );
        // A named variable is returned undefined. No process environment is read.
        exchange(
            &mut peer,
            &[IAC, SB, 39, 1, 0, b'U', b'S', b'E', b'R', IAC, SE],
            &[IAC, SB, 39, 0, 0, b'U', b'S', b'E', b'R', IAC, SE],
        );
        exchange(
            &mut peer,
            &[IAC, SB, 24, 1, IAC, SE],
            &[&[IAC, SB, 24, 0], b"IBM-3179-2".as_slice(), &[IAC, SE]].concat(),
        );
        exchange(
            &mut peer,
            &[IAC, DO, 25, IAC, WILL, 25, IAC, DO, 0, IAC, WILL, 0],
            &[IAC, WILL, 25, IAC, DO, 25, IAC, WILL, 0, IAC, DO, 0],
        );
        exchange(
            &mut peer,
            &[IAC, WILL, 1, IAC, DO, 1],
            &[IAC, DONT, 1, IAC, WONT, 1],
        );
        peer.write_all(&wire).unwrap();
        expect(&mut peer, &wire);
    });
    let mut client =
        Client::connect(address, Profile::Tn5250e, Plaintext::ExplicitlyAllowed).unwrap();
    let record = Record::Tn5250e(gds.to_vec());
    assert_eq!(client.read_record(TIMEOUT).unwrap(), Some(record.clone()));
    let mut malformed = gds;
    malformed[1] -= 1;
    let error = client
        .send_record(&Record::Tn5250e(malformed.to_vec()))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    client.send_record(&record).unwrap();
    server.join().unwrap();
}

#[test]
fn tn5250_environment_uses_only_negotiated_requested_display_settings() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        exchange(
            &mut peer,
            b"\xff\xfa\x27\x01\xff\xf0\xff\xfd\x27",
            b"\xff\xfb\x27",
        );
        let defaults = b"\xff\xfa\x27\x00\x03KBDTYPE\x01USB\x03CODEPAGE\x0137\x03CHARSET\x01697\x03DEVNAME\x01TERM_01\xff\xf0";
        for request in [b"".as_slice(), b"\x03", b"\x00\x03"] {
            exchange(
                &mut peer,
                &[b"\xff\xfa\x27\x01", request, b"\xff\xf0"].concat(),
                defaults,
            );
        }
        // Variable type, order, undefined values and both escaping layers are preserved.
        exchange(
            &mut peer,
            b"\xff\xfa\x27\x01\x03DEVNAME\x00CODEPAGE\x03CHARSET\x03X\x02\x03\xff\xff\x00USER\x03IBMRSEED\x03IBMSUBSPW\x03KBDTYPE\xff\xf0",
            b"\xff\xfa\x27\x00\x03DEVNAME\x01TERM_01\x00CODEPAGE\x03CHARSET\x01697\x03X\x02\x03\xff\xff\x00USER\x03IBMRSEED\x03IBMSUBSPW\x03KBDTYPE\x01USB\xff\xf0",
        );
        negotiate_binary_display(&mut peer, b"IBM-3180-2");
        exchange(
            &mut peer,
            b"\xff\xfa\x27\x01\x03DEVNAME\xff\xf0",
            b"\xff\xfa\x27\x00\x03DEVNAME\x01TERM_01\xff\xf0",
        );
        peer.write_all(&[0, 10, 0x12, 0xa0, 0, 0, 4, 0, 0, 0, IAC, EOR])
            .unwrap();
    });
    let mut client = Client::connect_cancellable_with_device_name(
        address,
        Profile::Tn5250eWide,
        Plaintext::ExplicitlyAllowed,
        Some("term_01".parse().unwrap()),
        TIMEOUT,
        || false,
    )
    .unwrap();
    assert!(matches!(
        client.read_record(TIMEOUT).unwrap(),
        Some(Record::Tn5250e(_))
    ));
    server.join().unwrap();
}

#[test]
fn tn5250_rejects_malformed_and_oversized_environment_requests() {
    let oversized = vec![3; 1024]; // Repeated wildcards would expand to far more than 1 KiB.
    for data in [b"\x03BAD\x02".as_slice(), b"\x03KBDTYPE\x01USB", &oversized] {
        let listener = listener();
        let address = listener.local_addr().unwrap();
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut peer = accept(listener);
                exchange(&mut peer, &[IAC, DO, 39], &[IAC, WILL, 39]);
                peer.write_all(&[b"\xff\xfa\x27\x01", data, b"\xff\xf0"].concat())
                    .unwrap();
                assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);
            });
            let error = Client::connect(address, Profile::Tn5250e, Plaintext::ExplicitlyAllowed)
                .err()
                .unwrap();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        });
    }
}

#[test]
fn tn5250_device_name_rejects_invalid_names_and_other_profiles_before_dial() {
    for name in [
        "",
        "123A",
        "_TERM",
        "TOO_LONG_01",
        "TERM.1",
        "TÉRM",
        "A\x00",
    ] {
        assert!(name.parse::<DeviceName5250>().is_err(), "name {name:?}");
    }
    let listener = listener();
    listener.set_nonblocking(true).unwrap();
    let error = Client::connect_cancellable_with_device_name(
        listener.local_addr().unwrap(),
        Profile::Tn3270e,
        Plaintext::ExplicitlyAllowed,
        Some("T#ER$M@01".parse().unwrap()),
        TIMEOUT,
        || false,
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn tn5250_wide_model_negotiates_and_exchanges_input_in_the_last_column() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        negotiate_binary_display(&mut peer, b"IBM-3180-2");
        peer.write_all(&[
            0, 17, 0x12, 0xa0, 0, 0, 4, 0, 0, 3, 4, 0xf3, 0, 5, 0xd9, 0x70, 0, IAC, EOR,
        ])
        .unwrap();
        let mut query = [0; 73];
        peer.read_exact(&mut query).unwrap();
        assert_eq!(
            &query[..18],
            &[
                0, 71, 0x12, 0xa0, 0, 0, 4, 0, 0, 0, 0, 0, 0x88, 0, 0x3a, 0xd9, 0x70, 0x80
            ]
        );
        assert_eq!(
            &query[39..48],
            &[1, 0xf3, 0xf1, 0xf8, 0xf0, 0xf0, 0xf0, 0xf2, 2]
        );
        assert_eq!(&query[59..61], &[0x63, 0x30]);
        assert_eq!(&query[71..], &[IAC, EOR]);
        exchange(
            &mut peer,
            &[
                0, 33, 0x12, 0xa0, 0, 0, 4, 0, 0, 3, 4, 0x20, 0, 4, 0x11, 0, 8, 0x11, 27, 130,
                0x1d, 0x40, 0, 0x28, 0, 2, 0x13, 27, 131, 4, 0x52, 0, 0, IAC, EOR,
            ],
            &[
                0, 18, 0x12, 0xa0, 0, 0, 4, 0, 0, 0, 27, 131, 0xf1, 0x11, 27, 131, 0xc1, 0xc2, IAC,
                EOR,
            ],
        );
        peer.write_all(&[0, 12, 0x12, 0xa0, 0, 0, 4, 0, 0, 2, 4, 0x40, IAC, EOR])
            .unwrap();
    });
    let profile = Profile::Tn5250eWide;
    let mut client = Client::connect(address, profile, Plaintext::ExplicitlyAllowed).unwrap();
    let mut screen = Tn5250Screen::with_model(profile.model_5250().unwrap());
    let query = screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    assert_eq!(screen.snapshot().columns, 80);
    client.send_record(&query.reply.unwrap()).unwrap();

    screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    let wide = screen.snapshot();
    assert_eq!((wide.columns, wide.rows), (132, 27));
    assert_eq!((wide.cursor.column, wide.cursor.row), (130, 26));
    screen.insert_text("AB").unwrap();
    let typed = screen.snapshot();
    let last = typed.cells.last().unwrap();
    assert_eq!(last.character, 'B');
    assert_eq!(last.color, kortty_ibm::screen5250::Color5250::Green);
    client
        .send_record(&screen.send_aid(Aid5250::Enter).unwrap().reply.unwrap())
        .unwrap();

    screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    let cleared = screen.snapshot();
    assert_eq!(
        (cleared.columns, cleared.rows, cleared.cells.len()),
        (80, 24, 1920)
    );
    assert!(!cleared.invited);
    assert!(
        cleared
            .cells
            .iter()
            .all(|cell| cell.character == ' ' && !cell.editable)
    );
    server.join().unwrap();
}

#[test]
fn tn3270e_loopback_renders_host_fields_and_returns_aid() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        negotiate_3270(&mut peer, false);
        exchange(
            &mut peer,
            &[
                0, 0, 0, 0, 0, 0xf5, 0xc2, 0x1d, 0x60, 0xe7, 0x11, 0x40, 0xc5, 0x1d, 0x40, 0x13,
                IAC, EOR,
            ],
            &[
                0, 0, 0, 0, 0, 0x7d, 0x40, 0xc7, 0x11, 0x40, 0xc6, 0xe9, IAC, EOR,
            ],
        );
        peer.write_all(&[0, 0, 0, 0, 0, 0xf1, 0xc3, IAC, EOR])
            .unwrap();
    });
    let mut client =
        Client::connect(address, Profile::Tn3270e, Plaintext::ExplicitlyAllowed).unwrap();
    let mut screen = Tn3270Screen::new();
    screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    assert_eq!(screen.snapshot().cells[1].character, 'X');
    assert_eq!(screen.snapshot().cursor.column, 6);
    screen.insert_text("Z").unwrap();
    client
        .send_record(&screen.send_aid(Aid3270::Enter).unwrap())
        .unwrap();
    assert_eq!(screen.snapshot().keyboard, Keyboard3270::HostLocked);
    screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    assert_eq!(screen.snapshot().keyboard, Keyboard3270::Ready);
    assert!(!screen.snapshot().cells[6].field.unwrap().modified);
    server.join().unwrap();
}

#[test]
fn tn3270e_loopback_returns_extended_fields_and_selected_character_attributes() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        request_3270e_type(&mut peer, b"IBM-3278-2-E");
        accept_3270e_type(&mut peer, b"IBM-3278-2-E", &[2]);
        exchange(
            &mut peer,
            &[IAC, SB, 40, 3, 7, IAC, SE],
            &[IAC, SB, 40, 3, 4, IAC, SE],
        );
        exchange(
            &mut peer,
            &[0, 0, 0, 0, 0, 0xf3, 0, 7, 1, IAC, IAC, 3, 0, 0x86, IAC, EOR],
            &[
                0, 0, 0, 0, 0, 0x88, 0, 44, 0x81, 0x86, 0, 17, 0, 0xf4, 0xf0, 0xf0, 0xf1, 0xf1,
                0xf2, 0xf2, 0xf3, 0xf3, 0xf4, 0xf4, 0xf5, 0xf5, 0xf6, 0xf6, 0xf7, 0xf7, 0xf8, 0xf8,
                0xf9, 0xf9, 0xfa, 0xfa, 0xfb, 0xfb, 0xfc, 0xfc, 0xfd, 0xfd, 0xfe, 0xfe, IAC, IAC,
                IAC, IAC, 4, 2, 0, 0xf0, IAC, EOR,
            ],
        );
        let mut read_buffer = vec![
            0, 0, 0, 0, 0, 0x61, 0x40, 0xc1, 0x29, 1, 0x42, 0xf2, 0xc1, 0xc2, 0x29, 1, 0xc0, 0x60,
        ];
        read_buffer.extend([0; 1920 - 4]);
        read_buffer.extend([IAC, EOR]);
        exchange(
            &mut peer,
            &[
                0, 0, 0, 0, 0, 0xf3, 0, 4, 3, 0, 0, 26, 0x40, 0, 0xf5, 0xc2, 0x29, 2, 0xc0, 0x40,
                0x42, 0xf2, 0x13, 0x28, 0x42, 0xf4, 0x28, 0x41, 0xf2, 0xc1, 0x28, 0, 0, 0xc2, 0x1d,
                0x60, 0, 5, 9, 0, 1, // Extended Field Mode
                0, 5, 1, 0, 0xf2, // Read Partition / Read Buffer
                IAC, EOR,
            ],
            &read_buffer,
        );
        exchange(
            &mut peer,
            &[
                0, 0, 0, 0, 0, 0xf3, 0, 6, 0x40, 0, 0xf1, 0xc2, 0, 6, 9, 0, 2,
                0x42, // Character Mode, foreground only
                IAC, EOR,
            ],
            &[
                0, 0, 0, 0, 0, 0x7d, 0x40, 0xc2, 0x11, 0x40, 0xc1, 0x28, 0x42, 0xf4, 0xe9, 0x28,
                0x42, 0, 0xc2, IAC, EOR,
            ],
        );
        peer.write_all(&[0, 0, 0, 0, 0, 0xf3, 0, 0, 0x40, 0, 0x6f, IAC, EOR])
            .unwrap();
    });
    let mut client = Client::connect(
        address,
        Profile::Tn3270eExtended,
        Plaintext::ExplicitlyAllowed,
    )
    .unwrap();
    let mut screen = Tn3270Screen::new();
    let query = screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    client.send_record(&query.reply.unwrap()).unwrap();
    let update = screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    let display = screen.snapshot();
    assert_eq!(display.keyboard, Keyboard3270::HostLocked);
    assert_eq!(display.cells[1].character, 'A');
    assert_eq!(
        display.cells[1].style.foreground,
        Some(kortty_ibm::screen3270::Color3270::Green)
    );
    client.send_record(&update.reply.unwrap()).unwrap();

    screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    screen.insert_text("Z").unwrap();
    client
        .send_record(&screen.send_aid(Aid3270::Enter).unwrap())
        .unwrap();

    screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    let erased = screen.snapshot();
    assert_eq!(erased.keyboard, Keyboard3270::Ready);
    assert_eq!(erased.cells[1].character, ' ');
    assert!(erased.cells[3].field.unwrap().protected);
    server.join().unwrap();
}

#[test]
fn tn3270e_responses_follow_processing_and_preserve_independent_sequences() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        request_3270e_type(&mut peer, b"IBM-3278-2-E");
        accept_3270e_type(&mut peer, b"IBM-3278-2-E", &[2]);
        exchange(
            &mut peer,
            &[IAC, SB, 40, 3, 7, 2, 4, IAC, SE],
            &[IAC, SB, 40, 3, 7, 2, IAC, SE],
        );
        peer.write_all(&[IAC, SB, 40, 3, 4, 2, IAC, SE]).unwrap();
        let prelude: Vec<_> = (0..IAC)
            .flat_map(|sequence| [0, 0, 0, 0, sequence, 0xf1, 0x40, IAC, EOR])
            .collect();
        peer.write_all(&prelude).unwrap();
        exchange(
            &mut peer,
            &[
                0, 0, 2, 0, IAC, IAC, 0xf3, 0, 7, 1, IAC, IAC, 3, 0, 0x88, IAC, EOR,
            ],
            &[2, 0, 0, 0, IAC, IAC, 0, IAC, EOR],
        );
        expect(
            &mut peer,
            &[0, 0, 0, 0, 0, 0x88, 0, 7, 0x81, 0x88, 0, 1, 2, IAC, EOR],
        );

        exchange(
            &mut peer,
            &[
                0, 0, 1, 1, 0, 0xf5, 0xc2, 0xc1, IAC, EOR, // ERROR-RESPONSE, valid A
                0, 0, 2, 1, 1, 0x7f, IAC, EOR, // ALWAYS-RESPONSE, invalid command
            ],
            &[2, 0, 1, 1, 1, 0, IAC, EOR],
        );
        exchange(
            &mut peer,
            &[
                0, 0, 1, 1, 2, 0xf1, 0xc2, 0x11, 0x07, 0x80, IAC, EOR, // Illegal address
            ],
            &[2, 0, 1, 1, 2, 2, IAC, EOR],
        );

        exchange(
            &mut peer,
            &[
                0, 0, 0, 1, 3, 0xf1, 0xc2, 0xc2, IAC, EOR, // NO-RESPONSE, valid B
                0, 0, 2, 1, 4, 0xf3, 0, 7, 1, IAC, IAC, 3, 0, 0x88, IAC, EOR,
            ],
            &[2, 0, 0, 1, 4, 0, IAC, EOR],
        );
        expect(
            &mut peer,
            &[0, 0, 0, 0, 1, 0x88, 0, 7, 0x81, 0x88, 0, 1, 2, IAC, EOR],
        );
    });
    let mut client = Client::connect(
        address,
        Profile::Tn3270eExtended,
        Plaintext::ExplicitlyAllowed,
    )
    .unwrap();
    let mut screen = Tn3270Screen::new();
    for _ in 0..IAC {
        let update = screen
            .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
            .unwrap();
        assert!(update.reply.is_none() && !update.alarm);
    }
    let query = screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap()
        .reply
        .unwrap();
    assert_eq!(
        client.read_record(TIMEOUT).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        client.send_record(&query).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(client.complete_3270_record(Ok(())).unwrap());
    assert!(!client.complete_3270_record(Ok(())).unwrap());
    client.send_record(&query).unwrap();

    screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    assert!(!client.complete_3270_record(Ok(())).unwrap());
    let original = screen.snapshot();
    assert_eq!(original.cells[0].character, 'A');
    for expected in [
        Error3270::UnsupportedCommand(0x7f),
        Error3270::Malformed("3270 buffer address is outside the screen"),
    ] {
        let result = screen.apply_record(&client.read_record(TIMEOUT).unwrap().unwrap());
        assert_eq!(result.err(), Some(expected));
        assert_eq!(screen.snapshot(), original);
        assert!(client.complete_3270_record(Err(expected)).unwrap());
    }

    screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    assert!(!client.complete_3270_record(Ok(())).unwrap());
    assert_eq!(screen.snapshot().cells[0].character, 'B');
    let query = screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap()
        .reply
        .unwrap();
    assert!(client.complete_3270_record(Ok(())).unwrap());
    client.send_record(&query).unwrap();
    assert_eq!(client.read_record(TIMEOUT).unwrap(), None);
    server.join().unwrap();
}

#[test]
fn extended_device_keeps_its_terminal_type_on_traditional_framing() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        request_3270e_type(&mut peer, b"IBM-3278-2-E");
        exchange(
            &mut peer,
            &[IAC, SB, 40, 2, 6, 5, 4, IAC, SE],
            &[IAC, WONT, 40],
        );
        negotiate_binary_display(&mut peer, b"IBM-3278-2-E");
        exchange(
            &mut peer,
            &[0xf3, 0, 7, 1, IAC, IAC, 3, 0, 0x88, IAC, EOR],
            &[0x88, 0, 7, 0x81, 0x88, 0, 1, 2, IAC, EOR],
        );
    });
    let mut client = Client::connect(
        address,
        Profile::Tn3270eExtended,
        Plaintext::ExplicitlyAllowed,
    )
    .unwrap();
    let mut screen = Tn3270Screen::new();
    let query = screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    client.send_record(&query.reply.unwrap()).unwrap();
    server.join().unwrap();
}

#[test]
fn tn5250_loopback_attention_preserves_read_mdt_and_cancel_invite() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        negotiate_5250(&mut peer);
        let attention = [0, 10, 0x12, 0xa0, 0, 0, 4, 0x40, 0, 0, IAC, EOR];
        expect(&mut peer, &attention);
        peer.write_all(&[
            0, 32, 0x12, 0xa0, 0, 0, 4, 0, 0, 3, 4, 0x40, 4, 0x11, 0, 8, 0x11, 1, 1, 0x1d, 0x40, 0,
            0x20, 0, 3, 0x13, 1, 2, 4, 0x52, 0, 0, IAC, EOR,
        ])
        .unwrap();
        expect(&mut peer, &attention);
        expect(
            &mut peer,
            &[
                0, 17, 0x12, 0xa0, 0, 0, 4, 0, 0, 0, 1, 3, 0xf1, 0x11, 1, 2, 0xc1, IAC, EOR,
            ],
        );
        let cancel = [0, 10, 0x12, 0xa0, 0, 0, 4, 0, 0, 0x0a, IAC, EOR];
        exchange(&mut peer, &cancel, &cancel);
        expect(&mut peer, &attention);
    });
    let mut client =
        Client::connect(address, Profile::Tn5250e, Plaintext::ExplicitlyAllowed).unwrap();
    let mut screen = Tn5250Screen::new();
    let send_attention = |client: &mut Client, screen: &mut Tn5250Screen| {
        let before = screen.snapshot();
        let update = screen.send_aid(Aid5250::Attention).unwrap();
        assert!(!update.alarm);
        assert_eq!(screen.snapshot(), before);
        client.send_record(&update.reply.unwrap()).unwrap();
    };
    send_attention(&mut client, &mut screen);
    screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    assert!(screen.snapshot().invited);
    assert!(screen.snapshot().cells[1].editable);
    screen.insert_text("A").unwrap();
    send_attention(&mut client, &mut screen);
    let reply = screen.send_aid(Aid5250::Enter).unwrap().reply.unwrap();
    client.send_record(&reply).unwrap();
    let cancel = screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    client.send_record(&cancel.reply.unwrap()).unwrap();
    assert!(!screen.snapshot().invited);
    assert!(screen.snapshot().keyboard_locked);
    send_attention(&mut client, &mut screen);
    server.join().unwrap();
}

#[test]
fn tn5250_loopback_system_request_restores_the_original_read_and_hidden_field() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        negotiate_5250(&mut peer);
        exchange(
            &mut peer,
            &[
                0, 32, 0x12, 0xa0, 0, 0, 4, 0, 0, 3, 4, 0x40, 4, 0x11, 0, 8, 0x11, 1, 1, 0x1d,
                0x40, 0, 0x27, 0, 3, 0x13, 1, 2, 4, 0x52, 0, 0, IAC, EOR,
            ],
            &[0, 10, 0x12, 0xa0, 0, 0, 4, 4, 0, 0, IAC, EOR],
        );
        let cancel = [0, 10, 0x12, 0xa0, 0, 0, 4, 0, 0, 0x0a, IAC, EOR];
        exchange(&mut peer, &cancel, &cancel);
        peer.write_all(&[0, 12, 0x12, 0xa0, 0, 0, 4, 0, 0, 4, 4, 2, IAC, EOR])
            .unwrap();
        let mut restore = [0; 12];
        peer.read_exact(&mut restore).unwrap();
        assert_eq!(restore, [0, 28, 0x12, 0xa0, 0, 0, 4, 0, 0, 4, 4, 0x12]);
        restore[9] = 5;
        let mut restore = restore.to_vec();
        for _ in 0..16 {
            let mut byte = [0];
            peer.read_exact(&mut byte).unwrap();
            restore.extend(byte);
            if byte == [IAC] {
                expect(&mut peer, &[IAC]);
                restore.push(IAC);
            }
        }
        expect(&mut peer, &[IAC, EOR]);
        restore.extend([IAC, EOR]);

        exchange(
            &mut peer,
            &[
                0, 32, 0x12, 0xa0, 0, 0, 4, 0, 0, 3, 4, 0x40, 4, 0x11, 0, 8, 0x11, 2, 1, 0x1d,
                0x40, 0, 0x20, 0, 3, 0x13, 2, 2, 4, 0x52, 0, 0, IAC, EOR,
            ],
            &[
                0, 17, 0x12, 0xa0, 0, 0, 4, 0, 0, 0, 2, 3, 0xf1, 0x11, 2, 2, 0xf1, IAC, EOR,
            ],
        );
        peer.write_all(&[0, 10, 0x12, 0xa0, 0, 0, 4, 0, 0, 0x0b, IAC, EOR])
            .unwrap();
        peer.write_all(&restore).unwrap();
        exchange(
            &mut peer,
            &[0, 14, 0x12, 0xa0, 0, 0, 4, 0, 0, 1, 4, 0x52, 0, 0, IAC, EOR],
            &[
                0, 17, 0x12, 0xa0, 0, 0, 4, 0, 0, 0, 1, 3, 0xf1, 0x11, 1, 2, 0xc1, IAC, EOR,
            ],
        );
    });
    let mut client =
        Client::connect(address, Profile::Tn5250e, Plaintext::ExplicitlyAllowed).unwrap();
    let mut screen = Tn5250Screen::new();
    let receive = |client: &mut Client, screen: &mut Tn5250Screen| {
        screen
            .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
            .unwrap()
    };
    let send = |client: &mut Client, screen: &mut Tn5250Screen, aid| {
        client
            .send_record(&screen.send_aid(aid).unwrap().reply.unwrap())
            .unwrap();
    };

    assert!(receive(&mut client, &mut screen).reply.is_none());
    screen.insert_text("A").unwrap();
    screen.edit(Edit5250::ToggleInsert).unwrap();
    let mut original = screen.snapshot();
    assert!(original.cells[1].hidden && original.cells[1].character == ' ');
    send(&mut client, &mut screen, Aid5250::SystemRequest);
    assert_eq!(screen.snapshot(), original);

    let cancel = receive(&mut client, &mut screen);
    client.send_record(&cancel.reply.unwrap()).unwrap();
    assert!(!screen.snapshot().invited && screen.snapshot().keyboard_locked);
    assert_eq!(screen.insert_text("X"), Err(Error5250::KeyboardLocked));
    let save = receive(&mut client, &mut screen);
    client.send_record(&save.reply.unwrap()).unwrap();

    assert!(receive(&mut client, &mut screen).reply.is_none());
    assert!(screen.snapshot().cells[81].editable);
    screen.insert_text("1").unwrap();
    send(&mut client, &mut screen, Aid5250::Enter);

    assert!(receive(&mut client, &mut screen).reply.is_none()); // Message light
    assert!(receive(&mut client, &mut screen).reply.is_none()); // Restore
    original.invited = false;
    original.message_waiting = true;
    assert_eq!(screen.snapshot(), original);
    assert_eq!(
        screen.send_aid(Aid5250::Enter).err(),
        Some(Error5250::NotInvited)
    );
    assert!(receive(&mut client, &mut screen).reply.is_none()); // Invite
    original.invited = true;
    assert_eq!(screen.snapshot(), original);
    send(&mut client, &mut screen, Aid5250::Enter);
    server.join().unwrap();
}

#[test]
fn tn5250_loopback_rolls_display_rows_and_preserves_the_active_field() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        negotiate_5250(&mut peer);
        peer.write_all(&[
            0, 53, 0x12, 0xa0, 0, 0, 4, 0, 0, 3, // Invite Operation
            4, 0x40, 4, 0x11, 0, 8, // Clear Unit, Write to Display
            0x11, 1, 1, 0x20, 0xc1, // Row 1: A
            0x11, 2, 1, 0x1d, 0x40, 0, 0x20, 0, 3, 0xc2, // Row 2: input field B
            0x11, 3, 1, 0x20, 0xc3, // Row 3: C
            0x11, 4, 1, 0x20, 0xc4, // Row 4: D
            0x11, 5, 1, 0x20, 0xc5, // Row 5: E
            0x13, 2, 2, 4, 0x52, 0, 0, IAC, EOR, // Insert Cursor, Read MDT Fields
            0, 15, 0x12, 0xa0, 0, 0, 4, 0, 0, 2, 4, 0x23, 0x01, 2, 4, IAC, EOR, 0, 15, 0x12, 0xa0,
            0, 0, 4, 0, 0, 2, 4, 0x23, 0x81, 2, 4, IAC, EOR,
        ])
        .unwrap();
        expect(
            &mut peer,
            &[
                0, 17, 0x12, 0xa0, 0, 0, 4, 0, 0, 0, 2, 3, 0xf1, 0x11, 2, 2, 0xe7, IAC, EOR,
            ],
        );
    });
    let mut client =
        Client::connect(address, Profile::Tn5250e, Plaintext::ExplicitlyAllowed).unwrap();
    let mut screen = Tn5250Screen::new();
    screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    let initial = screen.snapshot();

    for expected in [['A', 'C', 'D', 'D', 'E'], ['A', 'C', 'C', 'D', 'E']] {
        let roll = screen
            .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
            .unwrap();
        assert!(roll.reply.is_none() && !roll.alarm);
        let view = screen.snapshot();
        assert_eq!(
            [1, 81, 161, 241, 321].map(|i| view.cells[i].character),
            expected
        );
        assert_eq!(view.cursor, initial.cursor);
        assert!(view.invited && !view.keyboard_locked);
        assert!(view.cells[81].editable);
        assert!(!view.cells[161].editable);
    }
    screen.insert_text("X").unwrap();
    client
        .send_record(&screen.send_aid(Aid5250::Enter).unwrap().reply.unwrap())
        .unwrap();
    server.join().unwrap();
}

#[test]
fn tn5250_loopback_requests_packed_help_in_the_host_error_state() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        negotiate_5250(&mut peer);
        exchange(
            &mut peer,
            &[
                0, 18, 0x12, 0xa0, 0, 0, 4, 0, 0, 2, 4, 0x21, 0x2a, 0xf0, 0xf0, 0xf0, 0xf5, 0x27,
                IAC, EOR,
            ],
            &[0, 12, 0x12, 0xa0, 0, 0, 4, 1, 0, 0, 0, 5, IAC, EOR],
        );
        peer.write_all(&[
            0, 17, 0x12, 0xa0, 0, 0, 4, 0, 0, 2, 4, 0x21, 0x20, 0xc8, 0xc5, 0xd3, 0xd7, IAC, EOR,
        ])
        .unwrap();
    });
    let mut client =
        Client::connect(address, Profile::Tn5250e, Plaintext::ExplicitlyAllowed).unwrap();
    let mut screen = Tn5250Screen::new();
    screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    assert!(screen.snapshot().keyboard_locked);
    assert!(!screen.snapshot().invited);
    let help = screen.send_aid(Aid5250::Help).unwrap().reply.unwrap();
    client.send_record(&help).unwrap();
    screen
        .apply_record(&client.read_record(TIMEOUT).unwrap().unwrap())
        .unwrap();
    assert_eq!(screen.snapshot().cells[23 * 80 + 1].character, 'H');
    screen.edit(Edit5250::Reset).unwrap();
    assert!(!screen.snapshot().keyboard_locked);
    assert_eq!(screen.snapshot().cells[23 * 80 + 1].character, ' ');
    server.join().unwrap();
}

#[test]
fn partial_record_survives_idle_timeout_and_reports_truncated_eof() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let (resume, wait) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        negotiate_3270(&mut peer, false);
        peer.write_all(&[0, 0, 0, 0, 0, IAC]).unwrap();
        wait.recv_timeout(TIMEOUT).unwrap();
        peer.write_all(&[IAC, IAC, EOR, 0, IAC]).unwrap();
        peer.shutdown(Shutdown::Write).unwrap();
    });
    let mut client =
        Client::connect(address, Profile::Tn3270e, Plaintext::ExplicitlyAllowed).unwrap();
    assert_eq!(
        client
            .read_record(Duration::from_millis(60))
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    resume.send(()).unwrap();
    assert_eq!(
        client.read_record(TIMEOUT).unwrap(),
        Some(Record::Tn3270e {
            kind: Tn3270eData::Display,
            payload: vec![IAC]
        })
    );
    assert_eq!(
        client.read_record(TIMEOUT).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    assert_eq!(
        client.read_record(TIMEOUT).unwrap_err().kind(),
        io::ErrorKind::NotConnected
    );
    server.join().unwrap();
}

#[test]
fn oversized_record_is_rejected_without_eor() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        negotiate_3270(&mut peer, false);
        peer.write_all(&vec![0; 1 + usize::from(u16::MAX)]).unwrap();
    });
    let mut client =
        Client::connect(address, Profile::Tn3270e, Plaintext::ExplicitlyAllowed).unwrap();
    assert_eq!(
        client.read_record(TIMEOUT).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    server.join().unwrap();
}

#[test]
fn oversized_subnegotiation_is_rejected_without_se() {
    let listener = listener();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut peer = accept(listener);
        exchange(&mut peer, &[IAC, DO, 40], &[IAC, WILL, 40]);
        peer.write_all(&[IAC, SB, 40]).unwrap();
        peer.write_all(&[0; 4097]).unwrap();
    });
    let error = Client::connect(address, Profile::Tn3270e, Plaintext::ExplicitlyAllowed)
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    server.join().unwrap();
}

fn listener() -> TcpListener {
    TcpListener::bind(("127.0.0.1", 0)).unwrap()
}

fn accept(listener: TcpListener) -> TcpStream {
    let (peer, _) = listener.accept().unwrap();
    peer.set_read_timeout(Some(TIMEOUT)).unwrap();
    peer.set_write_timeout(Some(TIMEOUT)).unwrap();
    peer.set_nodelay(true).unwrap();
    peer
}

fn expect(peer: &mut TcpStream, expected: &[u8]) {
    let mut reply = vec![0; expected.len()];
    peer.read_exact(&mut reply).unwrap();
    assert_eq!(reply, expected);
}

fn exchange(peer: &mut TcpStream, request: &[u8], expected: &[u8]) {
    peer.write_all(request).unwrap();
    expect(peer, expected);
}

// Drives the production TCP client with a local protocol fixture.
fn negotiate_3270(peer: &mut TcpStream, counter_functions: bool) {
    request_3270e_device(peer);
    finish_3270e(peer, counter_functions);
}

fn request_3270e_device(peer: &mut TcpStream) {
    request_3270e_type(peer, b"IBM-3278-2");
}

fn request_3270e_type(peer: &mut TcpStream, device: &[u8]) {
    exchange(peer, &[IAC, DO, 40], &[IAC, WILL, 40]);
    exchange(
        peer,
        &[IAC, SB, 40, 8, 2, IAC, SE],
        &[&[IAC, SB, 40, 2, 7], device, &[IAC, SE]].concat(),
    );
}

fn finish_3270e(peer: &mut TcpStream, counter_functions: bool) {
    accept_3270e_type(peer, b"IBM-3278-2", &[]);
    if counter_functions {
        exchange(
            peer,
            &[IAC, SB, 40, 3, 7, 2, IAC, SE],
            &[IAC, SB, 40, 3, 7, IAC, SE],
        );
    }
    peer.write_all(&[IAC, SB, 40, 3, 4, IAC, SE]).unwrap();
}

fn accept_3270e_type(peer: &mut TcpStream, device: &[u8], functions: &[u8]) {
    exchange(
        peer,
        &[&[IAC, SB, 40, 2, 4], device, &[1], b"LU01", &[IAC, SE]].concat(),
        &[&[IAC, SB, 40, 3, 7], functions, &[IAC, SE]].concat(),
    );
}

fn negotiate_classic_3270(peer: &mut TcpStream) {
    negotiate_binary_display(peer, b"IBM-3278-2");
}

fn negotiate_3270_terminal_type(peer: &mut TcpStream) {
    negotiate_terminal_type(peer, b"IBM-3278-2");
}

fn negotiate_5250(peer: &mut TcpStream) {
    negotiate_binary_display(peer, b"IBM-3179-2");
}

fn negotiate_terminal_type(peer: &mut TcpStream, device: &[u8]) {
    exchange(peer, &[IAC, DO, 24], &[IAC, WILL, 24]);
    exchange(
        peer,
        &[IAC, SB, 24, 1, IAC, SE],
        &[&[IAC, SB, 24, 0], device, &[IAC, SE]].concat(),
    );
}

fn negotiate_binary_display(peer: &mut TcpStream, device: &[u8]) {
    negotiate_terminal_type(peer, device);
    exchange(
        peer,
        &[IAC, DO, 25, IAC, WILL, 25, IAC, DO, 0, IAC, WILL, 0],
        &[IAC, WILL, 25, IAC, DO, 25, IAC, WILL, 0, IAC, DO, 0],
    );
}
