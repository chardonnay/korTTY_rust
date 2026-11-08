use super::*;

#[test]
fn x11_clipboard_rejects_stale_and_synthetic_selection_events() {
    assert!(current_timestamp(x11rb::CURRENT_TIME, 800));
    assert!(current_timestamp(800, 800));
    assert!(current_timestamp(803, 800));
    assert!(!current_timestamp(799, 800));
    assert!(current_timestamp(2, u32::MAX - 3));
    assert!(!current_timestamp(u32::MAX - 3, 2));
    assert!(genuine(31));
    assert!(!genuine(31 | SYNTHETIC));
}

#[test]
fn x11_clipboard_incremental_transfer_chunks_have_one_empty_terminator() {
    let now = Instant::now();
    let mut transfer = Transfer::new(Arc::from(&b"1234567"[..]), now);
    assert_eq!(transfer.next(3, now), b"123");
    assert_eq!(transfer.next(3, now), b"456");
    assert_eq!(transfer.next(3, now), b"7");
    // The owner removes the completed slot after sending the empty property.
    assert_eq!(transfer.next(3, now + TRANSFER_AGE), b"");
    assert_eq!(transfer.deadline, transfer.expires);
}

#[test]
#[ignore = "requires KORTTY_TEST_X11_DISPLAY set to a private X11 test server"]
fn x11_clipboard_live_direct_incr_and_text_takeover() {
    let name = std::env::var("KORTTY_TEST_X11_DISPLAY").expect("select a private test display");
    let (reader, screen) = x11rb::connect(Some(&name)).unwrap();
    let client = Client::start(Arc::new(Server { name, screen })).unwrap();
    let atoms = Atoms::new(&reader).unwrap().reply().unwrap();
    let root = reader.setup().roots[screen].root;
    let window = reader.generate_id().unwrap();
    checked(reader.create_window(
        0,
        window,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    ))
    .unwrap();
    let property = reader
        .intern_atom(false, b"_KORTTY_X11_CLIPBOARD_TEST")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let text = reader
        .intern_atom(false, b"UTF8_STRING")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    // Fixed byte fixtures exercise the native clipboard protocol without requiring an image decoder.
    let small: Arc<[u8]> = Arc::from(&b"\x89PNG\r\n\x1a\nsmall clipboard payload"[..]);
    write_png(&client, Arc::clone(&small));
    assert_eq!(
        convert(&reader, &atoms, window, property, atoms.targets),
        property
    );
    let targets = reader
        .get_property(true, window, property, AtomEnum::ATOM, 0, 8)
        .unwrap()
        .reply()
        .unwrap();
    assert!(targets.value32().unwrap().any(|target| target == atoms.png));
    assert_eq!(
        convert(&reader, &atoms, window, property, text),
        x11rb::NONE
    );
    assert_eq!(
        convert(&reader, &atoms, window, property, atoms.png),
        property
    );
    let data = reader
        .get_property(true, window, property, atoms.png, 0, 32)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(data.value, small.as_ref());

    let mut large = PNG_SIGNATURE.to_vec();
    large.extend((0..MAX_CHUNK * 2).map(|index| (index % 239) as u8));
    write_png(&client, Arc::from(large.as_slice()));
    assert_eq!(
        convert(&reader, &atoms, window, property, atoms.png),
        property
    );
    let header = reader
        .get_property(false, window, property, atoms.incr, 0, 1)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(header.value32().unwrap().next(), Some(large.len() as u32));

    // Another real X11 owner takes CLIPBOARD while the admitted INCR is outstanding.
    checked(reader.set_selection_owner(window, atoms.clipboard, x11rb::CURRENT_TIME)).unwrap();
    checked(reader.delete_property(window, property)).unwrap();
    let mut result = Vec::new();
    loop {
        wait_for(&reader, |event| {
            matches!(event, Event::PropertyNotify(event)
            if event.window == window && event.atom == property && event.state == Property::NEW_VALUE)
        });
        let chunk = reader
            .get_property(true, window, property, atoms.png, 0, (MAX_CHUNK / 4) as u32)
            .unwrap()
            .reply()
            .unwrap();
        assert_eq!(chunk.type_, atoms.png);
        assert_eq!(chunk.bytes_after, 0);
        if chunk.value.is_empty() {
            break;
        }
        result.extend(chunk.value);
        assert!(result.len() <= large.len());
    }
    assert_eq!(result, large);
    assert_eq!(
        reader
            .get_selection_owner(atoms.clipboard)
            .unwrap()
            .reply()
            .unwrap()
            .owner,
        window
    );
    let Client {
        requests,
        wake,
        worker,
        ..
    } = client;
    drop(requests);
    drop(wake);
    worker.join().unwrap();
}

fn write_png(client: &Client, png: Arc<[u8]>) {
    let (send, receive) = mpsc::sync_channel(1);
    let request = Request {
        png,
        complete: Some(Box::new(move |result| {
            let _ = send.try_send(result);
        })),
    };
    assert!(client.requests.try_send(request).is_ok());
    client.notify();
    receive
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
}

fn convert(
    connection: &RustConnection,
    atoms: &Atoms,
    window: Window,
    property: Atom,
    target: Atom,
) -> Atom {
    checked(connection.convert_selection(
        window,
        atoms.clipboard,
        target,
        property,
        x11rb::CURRENT_TIME,
    ))
    .unwrap();
    let event = wait_for(connection, |event| {
        matches!(event, Event::SelectionNotify(event)
        if event.requestor == window && event.target == target)
    });
    let Event::SelectionNotify(event) = event else {
        unreachable!()
    };
    event.property
}

fn wait_for(connection: &RustConnection, matches: impl Fn(&Event) -> bool) -> Event {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = connection.poll_for_event().unwrap() {
            if matches(&event) {
                return event;
            }
            continue;
        }
        let timeout =
            Timespec::try_from(deadline.saturating_duration_since(Instant::now())).unwrap();
        let mut fds = [PollFd::new(connection.stream(), PollFlags::IN)];
        assert_ne!(
            poll(&mut fds, Some(&timeout)).unwrap(),
            0,
            "X11 event timed out"
        );
    }
}
