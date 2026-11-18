//! Incoming Xdnd Copy offers on a proxy owned by a separate X11 connection.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use raw_window_handle::{RawWindowHandle, WindowHandle};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use x11rb::connection::Connection;
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    Property, SelectionNotifyEvent, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const TEXT_LIMIT: usize = 64 * 1024;
const FILE_LIMIT: usize = 1024 * 1024;
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(3);

x11rb::atom_manager! {
    Atoms: AtomCookies {
        XdndAware,
        XdndProxy,
        XdndEnter,
        XdndPosition,
        XdndStatus,
        XdndLeave,
        XdndDrop,
        XdndFinished,
        XdndSelection,
        XdndTypeList,
        XdndActionCopy,
        UTF8_STRING,
        TextPlainUtf8: b"text/plain;charset=utf-8",
        TextPlain: b"text/plain",
        UriList: b"text/uri-list",
        INCR,
        DropData: b"_KORTTY_XDND_DATA",
    }
}

pub enum Event {
    Files {
        point: (f64, f64),
        paths: Vec<PathBuf>,
        dropped: bool,
    },
    Left,
}

type TextCallback = dyn Fn((f64, f64), String) -> bool + Send + Sync;

struct Registration {
    enabled: AtomicBool,
    notify: Box<TextCallback>,
}

#[derive(Clone)]
pub struct Watch(Arc<Registration>);

impl Watch {
    pub fn set_enabled(&self, enabled: bool) {
        self.0.enabled.store(enabled, Ordering::Release);
    }
}

#[derive(Default)]
struct Slot {
    closed: AtomicBool,
    text: Mutex<Weak<Registration>>,
}

impl Slot {
    fn text(&self) -> Option<Arc<Registration>> {
        self.text
            .lock()
            .ok()?
            .upgrade()
            .filter(|registration| registration.enabled.load(Ordering::Acquire))
    }
}

thread_local! {
    static TARGETS: RefCell<HashMap<Window, Weak<Slot>>> = RefCell::new(HashMap::new());
}

pub struct Target {
    slot: Arc<Slot>,
    _wake: UnixStream,
}

impl Drop for Target {
    fn drop(&mut self) {
        self.slot.closed.store(true, Ordering::Release);
    }
}

fn xid(window: WindowHandle<'_>) -> Option<Window> {
    match window.as_raw() {
        RawWindowHandle::Xlib(handle) => handle.window.try_into().ok(),
        RawWindowHandle::Xcb(handle) => Some(handle.window.get()),
        _ => None,
    }
}

pub fn register(
    window: WindowHandle<'_>,
    notify: impl Fn(Event) -> bool + Send + 'static,
) -> Option<Target> {
    let window = xid(window)?;
    let (wake, stop) = UnixStream::pair().ok()?;
    let slot = Arc::new(Slot::default());
    let worker_slot = slot.clone();
    std::thread::Builder::new()
        .name("kortty-xdnd-target".into())
        .spawn(move || {
            if let Ok(target) = Receiver::connect(window, worker_slot, notify) {
                let _ = target.run(stop);
            }
        })
        .ok()?;
    TARGETS.with_borrow_mut(|targets| {
        targets.retain(|_, slot| slot.strong_count() != 0);
        targets.insert(window, Arc::downgrade(&slot));
    });
    Some(Target { slot, _wake: wake })
}

pub fn watch_text(
    window: WindowHandle<'_>,
    notify: impl Fn((f64, f64), String) -> bool + Send + Sync + 'static,
) -> Option<Watch> {
    let window = xid(window)?;
    let slot = TARGETS.with_borrow(|targets| targets.get(&window)?.upgrade())?;
    let registration = Arc::new(Registration {
        enabled: AtomicBool::new(false),
        notify: Box::new(notify),
    });
    *slot.text.lock().ok()? = Arc::downgrade(&registration);
    Some(Watch(registration))
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Receiver<F> {
    connection: RustConnection,
    atoms: Atoms,
    root: Window,
    window: Window,
    proxy: Window,
    slot: Arc<Slot>,
    notify: F,
    hover: Option<Offer>,
    read: Option<Transfer>,
}

struct Offer {
    source: Window,
    version: u32,
    target: Atom,
    position: Option<(f64, f64)>,
    paths: Option<Vec<PathBuf>>,
    preview_requested: bool,
    dropped: bool,
}

struct Transfer {
    requestor: Window,
    target: Atom,
    time: u32,
    started: Instant,
    incremental: Option<Vec<u8>>,
}

impl<F: Fn(Event) -> bool> Receiver<F> {
    fn connect(window: Window, slot: Arc<Slot>, notify: F) -> Result<Self> {
        let (connection, _) = x11rb::connect(None)?;
        let root = connection.get_geometry(window)?.reply()?.root;
        let atoms = Atoms::new(&connection)?.reply()?;
        let proxy = Self::input_window(&connection, root)?;
        connection
            .change_window_attributes(
                window,
                &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                    .event_mask(EventMask::STRUCTURE_NOTIFY),
            )?
            .check()?;
        connection
            .change_property32(
                PropMode::REPLACE,
                proxy,
                atoms.XdndAware,
                AtomEnum::ATOM,
                &[5],
            )?
            .check()?;
        connection
            .change_property32(
                PropMode::REPLACE,
                proxy,
                atoms.XdndProxy,
                AtomEnum::WINDOW,
                &[proxy],
            )?
            .check()?;
        connection
            .change_property32(
                PropMode::REPLACE,
                window,
                atoms.XdndProxy,
                AtomEnum::WINDOW,
                &[proxy],
            )?
            .check()?;
        Ok(Self {
            connection,
            atoms,
            root,
            window,
            proxy,
            slot,
            notify,
            hover: None,
            read: None,
        })
    }

    fn input_window(connection: &RustConnection, root: Window) -> Result<Window> {
        let id = connection.generate_id()?;
        connection
            .create_window(
                0,
                id,
                root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                0,
                &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )?
            .check()?;
        Ok(id)
    }

    fn run(mut self, stop: UnixStream) -> Result<()> {
        while !self.slot.closed.load(Ordering::Acquire) {
            let mut drained = false;
            for _ in 0..32 {
                match self.connection.poll_for_event()? {
                    Some(XEvent::ClientMessage(event)) => self.message(event),
                    Some(XEvent::SelectionNotify(event)) => self.selection(event),
                    Some(XEvent::PropertyNotify(event)) if event.state == Property::NEW_VALUE => {
                        self.increment(event.window, event.atom);
                    }
                    Some(XEvent::DestroyNotify(event)) if event.window == self.window => {
                        return Ok(());
                    }
                    Some(_) => {}
                    None => {
                        drained = true;
                        break;
                    }
                }
            }
            if self
                .read
                .as_ref()
                .is_some_and(|read| read.started.elapsed() >= TRANSFER_TIMEOUT)
            {
                self.complete(None);
            }
            self.connection.flush()?;
            if drained {
                let mut fds = [
                    PollFd::new(self.connection.stream(), PollFlags::IN),
                    PollFd::new(&stop, PollFlags::IN),
                ];
                let timeout = self.read.as_ref().map(|read| {
                    let remaining = TRANSFER_TIMEOUT.saturating_sub(read.started.elapsed());
                    Timespec {
                        tv_sec: remaining.as_secs() as _,
                        tv_nsec: remaining.subsec_nanos().into(),
                    }
                });
                poll(&mut fds, timeout.as_ref())?;
            }
        }
        self.clear();
        let proxy = self
            .connection
            .get_property(
                false,
                self.window,
                self.atoms.XdndProxy,
                AtomEnum::WINDOW,
                0,
                1,
            )
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .and_then(|reply| reply.value32()?.next());
        if proxy == Some(self.proxy)
            && let Ok(cookie) = self
                .connection
                .delete_property(self.window, self.atoms.XdndProxy)
        {
            let _ = cookie.check();
        }
        self.connection.flush()?;
        Ok(())
    }

    fn message(&mut self, event: ClientMessageEvent) {
        if event.format != 32 || ![self.window, self.proxy].contains(&event.window) {
            return;
        }
        let data = event.data.as_data32();
        let source = data[0];
        if source == x11rb::NONE {
            return;
        }
        if event.type_ == self.atoms.XdndEnter {
            self.clear();
            let version = data[1] >> 24;
            if (3..=5).contains(&version) {
                self.hover = Some(Offer {
                    source,
                    version,
                    target: self.offered_type(data),
                    position: None,
                    paths: None,
                    preview_requested: false,
                    dropped: false,
                });
            }
        } else if event.type_ == self.atoms.XdndPosition {
            self.position(data);
        } else if event.type_ == self.atoms.XdndDrop {
            self.drop_offer(source, data[2]);
        } else if event.type_ == self.atoms.XdndLeave
            && self
                .hover
                .as_ref()
                .is_some_and(|offer| offer.source == source && !offer.dropped)
        {
            self.clear();
        }
    }

    fn offered_type(&self, data: [u32; 5]) -> Atom {
        let mut types = data[2..].to_vec();
        if data[1] & 1 != 0 {
            types = self
                .connection
                .get_property(
                    false,
                    data[0],
                    self.atoms.XdndTypeList,
                    AtomEnum::ATOM,
                    0,
                    256,
                )
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .filter(|reply| reply.bytes_after == 0)
                .and_then(|reply| Some(reply.value32()?.collect()))
                .unwrap_or_default();
        }
        [
            self.atoms.UriList,
            self.atoms.UTF8_STRING,
            self.atoms.TextPlainUtf8,
            self.atoms.TextPlain,
        ]
        .into_iter()
        .find(|target| types.contains(target))
        .unwrap_or(x11rb::NONE)
    }

    fn client_point(&self, packed: u32) -> Option<(f64, f64)> {
        let x = (packed >> 16) as i16;
        let y = packed as i16;
        let point = self
            .connection
            .translate_coordinates(self.root, self.window, x, y)
            .ok()?
            .reply()
            .ok()?;
        let geometry = self
            .connection
            .get_geometry(self.window)
            .ok()?
            .reply()
            .ok()?;
        let (x, y) = (f64::from(point.dst_x), f64::from(point.dst_y));
        (point.same_screen
            && (0.0..f64::from(geometry.width)).contains(&x)
            && (0.0..f64::from(geometry.height)).contains(&y))
        .then_some((x, y))
    }

    fn position(&mut self, data: [u32; 5]) {
        let source = data[0];
        let point = self.client_point(data[2]);
        let mut accepted = false;
        let mut preview = None;
        if let Some(offer) = self
            .hover
            .as_mut()
            .filter(|offer| offer.source == source && !offer.dropped)
        {
            let files = offer.target == self.atoms.UriList;
            if data[4] == self.atoms.XdndActionCopy
                && offer.target != x11rb::NONE
                && (files || self.slot.text().is_some())
            {
                offer.position = point;
                accepted = point.is_some();
            } else {
                offer.position = None;
            }
            if accepted && files && !offer.preview_requested {
                offer.preview_requested = true;
                preview = Some(offer.target);
            }
        }
        self.send(
            source,
            self.atoms.XdndStatus,
            [
                self.window,
                2 | u32::from(accepted),
                0,
                0,
                if accepted {
                    self.atoms.XdndActionCopy
                } else {
                    x11rb::NONE
                },
            ],
        );
        if let Some(target) = preview {
            let _ = self.request(source, target, data[3]);
        }
    }

    fn drop_offer(&mut self, source: Window, time: u32) {
        let owns_selection = self.owns_selection(source);
        let Some(offer) = self
            .hover
            .as_mut()
            .filter(|offer| offer.source == source && !offer.dropped)
        else {
            return;
        };
        offer.dropped = true;
        if !owns_selection
            || offer.position.is_none()
            || (offer.target != self.atoms.UriList && self.slot.text().is_none())
        {
            self.complete(None);
        } else if let Some(paths) = offer.paths.take() {
            self.publish_files(paths);
        } else if self.read.is_none() {
            let target = offer.target;
            if self.request(source, target, time).is_err() {
                self.complete(None);
            }
        }
    }

    fn request(&mut self, source: Window, target: Atom, time: u32) -> Result<()> {
        if !self.owns_selection(source) {
            return Err("The Xdnd source no longer owns its selection".into());
        }
        let requestor = Self::input_window(&self.connection, self.root)?;
        let result = self
            .connection
            .convert_selection(
                requestor,
                self.atoms.XdndSelection,
                target,
                self.atoms.DropData,
                time,
            )
            .map_err(Box::<dyn std::error::Error>::from)
            .and_then(|cookie| cookie.check().map_err(Into::into));
        if result.is_ok() {
            self.read = Some(Transfer {
                requestor,
                target,
                time,
                started: Instant::now(),
                incremental: None,
            });
        } else if let Ok(cookie) = self.connection.destroy_window(requestor) {
            cookie.ignore_error();
        }
        result
    }

    fn owns_selection(&self, source: Window) -> bool {
        self.connection
            .get_selection_owner(self.atoms.XdndSelection)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some_and(|reply| reply.owner == source)
    }

    fn selection(&mut self, event: SelectionNotifyEvent) {
        let Some(read) = self.read.as_ref().filter(|read| {
            event.requestor == read.requestor
                && event.selection == self.atoms.XdndSelection
                && event.target == read.target
                && (read.time == 0 || event.time == read.time)
                && read.incremental.is_none()
        }) else {
            return;
        };
        if event.property != self.atoms.DropData {
            self.complete(None);
            return;
        }
        let limit = self.limit(read.target);
        let Some((kind, format, bytes)) = self.property(read.requestor, limit) else {
            self.complete(None);
            return;
        };
        if kind == self.atoms.INCR
            && format == 32
            && bytes.len() == 4
            && u32::from_ne_bytes(bytes.as_slice().try_into().unwrap()) as usize <= limit
        {
            if let Some(read) = &mut self.read {
                read.incremental = Some(Vec::new());
            }
        } else {
            let matches = self.payload_type(read.target, kind) && format == 8;
            self.complete(matches.then_some(bytes));
        }
    }

    fn increment(&mut self, window: Window, atom: Atom) {
        let Some(read) = self.read.as_ref().filter(|read| {
            read.requestor == window && atom == self.atoms.DropData && read.incremental.is_some()
        }) else {
            return;
        };
        let target = read.target;
        let limit = self.limit(target);
        let Some((kind, 8, bytes)) = self.property(window, limit) else {
            self.complete(None);
            return;
        };
        if !self.payload_type(target, kind) {
            self.complete(None);
            return;
        }
        if let Some(data) = self
            .read
            .as_mut()
            .and_then(|read| read.incremental.as_mut())
        {
            if bytes.is_empty() {
                let data = std::mem::take(data);
                self.complete(Some(data));
            } else if data.len() + bytes.len() <= limit {
                data.extend(bytes);
            } else {
                self.complete(None);
            }
        }
    }

    fn payload_type(&self, target: Atom, kind: Atom) -> bool {
        kind == target || (target != self.atoms.UriList && kind == self.atoms.UTF8_STRING)
    }

    fn limit(&self, target: Atom) -> usize {
        if target == self.atoms.UriList {
            FILE_LIMIT
        } else {
            TEXT_LIMIT
        }
    }

    fn property(&self, window: Window, limit: usize) -> Option<(Atom, u8, Vec<u8>)> {
        let reply = self
            .connection
            .get_property(
                true,
                window,
                self.atoms.DropData,
                AtomEnum::ANY,
                0,
                (limit / 4 + 1) as u32,
            )
            .ok()?
            .reply()
            .ok()?;
        (reply.bytes_after == 0 && reply.value.len() <= limit).then_some((
            reply.type_,
            reply.format,
            reply.value,
        ))
    }

    fn complete(&mut self, bytes: Option<Vec<u8>>) {
        self.stop_read();
        let Some(offer) = self.hover.as_ref() else {
            return;
        };
        if offer.target == self.atoms.UriList {
            if let Some(paths) = bytes
                .as_deref()
                .and_then(crate::wayland_file_drop::local_paths)
            {
                self.publish_files(paths);
            } else if offer.dropped {
                self.finish(false);
            }
        } else if offer.dropped {
            let accepted = bytes
                .and_then(text)
                .zip(offer.position)
                .zip(self.slot.text())
                .is_some_and(|((text, point), watch)| (watch.notify)(point, text));
            self.finish(accepted);
        }
    }

    fn publish_files(&mut self, paths: Vec<PathBuf>) {
        let Some(offer) = self.hover.as_mut() else {
            return;
        };
        let dropped = offer.dropped;
        let accepted = offer.position.is_some_and(|point| {
            (self.notify)(Event::Files {
                point,
                paths: paths.clone(),
                dropped,
            })
        });
        if dropped {
            self.finish(accepted);
        } else {
            offer.paths = Some(paths);
        }
    }

    fn clear(&mut self) {
        self.stop_read();
        if let Some(offer) = self.hover.take() {
            if offer.dropped {
                self.finished(&offer, false);
            } else if offer.paths.is_some() {
                (self.notify)(Event::Left);
            }
        }
    }

    fn stop_read(&mut self) {
        if let Some(read) = self.read.take()
            && let Ok(cookie) = self.connection.destroy_window(read.requestor)
        {
            cookie.ignore_error();
        }
    }

    fn finish(&mut self, accepted: bool) {
        if let Some(offer) = self.hover.take() {
            if !accepted && offer.paths.is_some() {
                (self.notify)(Event::Left);
            }
            self.finished(&offer, accepted);
        }
    }

    fn finished(&self, offer: &Offer, accepted: bool) {
        let success = offer.version >= 5 && accepted;
        self.send(
            offer.source,
            self.atoms.XdndFinished,
            [
                self.window,
                u32::from(success),
                if success {
                    self.atoms.XdndActionCopy
                } else {
                    x11rb::NONE
                },
                0,
                0,
            ],
        );
    }

    fn send(&self, window: Window, kind: Atom, data: [u32; 5]) {
        if let Ok(cookie) = self.connection.send_event(
            false,
            window,
            EventMask::NO_EVENT,
            ClientMessageEvent::new(32, window, kind, data),
        ) {
            cookie.ignore_error();
        }
    }
}

fn text(bytes: Vec<u8>) -> Option<String> {
    if bytes.is_empty() || bytes.len() > TEXT_LIMIT || bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use super::*;
    use x11rb::protocol::xproto::SelectionNotifyEvent;

    #[test]
    fn rejects_invalid_unicode_nul_and_oversized_payloads() {
        assert_eq!(
            text("Å 日本語".as_bytes().to_vec()).as_deref(),
            Some("Å 日本語")
        );
        for bytes in [Vec::new(), vec![0], vec![0xff], vec![b'a'; TEXT_LIMIT + 1]] {
            assert!(text(bytes).is_none());
        }
    }

    #[test]
    #[ignore = "requires a private X11 server"]
    fn native_proxy_routes_text_and_files_from_an_independent_source() -> Result<()> {
        let (client, screen) = x11rb::connect(None)?;
        let root = client.setup().roots[screen].root;
        let window = client.generate_id()?;
        client
            .create_window(
                0,
                window,
                root,
                31,
                47,
                140,
                120,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new(),
            )?
            .check()?;
        client.map_window(window)?.check()?;

        let (file_tx, file_rx) = mpsc::channel();
        let (text_tx, text_rx) = mpsc::channel();
        let registration = Arc::new(Registration {
            enabled: AtomicBool::new(true),
            notify: Box::new(move |point, text| text_tx.send((point, text)).is_ok()),
        });
        let slot = Arc::new(Slot::default());
        *slot.text.lock().unwrap() = Arc::downgrade(&registration);
        let (wake, stop) = UnixStream::pair()?;
        let target = Target {
            slot: slot.clone(),
            _wake: wake,
        };
        let receiver = Receiver::connect(window, slot, move |event| file_tx.send(event).is_ok())?;
        let proxy = receiver.proxy;
        assert_eq!(
            client
                .get_property(
                    false,
                    window,
                    receiver.atoms.XdndProxy,
                    AtomEnum::WINDOW,
                    0,
                    1
                )?
                .reply()?
                .value32()
                .unwrap()
                .next(),
            Some(proxy)
        );
        let worker = thread::spawn(move || receiver.run(stop).map_err(|error| error.to_string()));

        let atoms = Atoms::new(&client)?.reply()?;
        let source = Receiver::<fn(Event) -> bool>::input_window(&client, root)?;
        client
            .set_selection_owner(source, atoms.XdndSelection, x11rb::CURRENT_TIME)?
            .check()?;
        let send = |kind, data| -> Result<()> {
            client
                .send_event(
                    false,
                    proxy,
                    EventMask::NO_EVENT,
                    ClientMessageEvent::new(32, window, kind, data),
                )?
                .check()?;
            Ok(())
        };
        let enter = |types: [Atom; 3]| {
            send(
                atoms.XdndEnter,
                [source, 5 << 24, types[0], types[1], types[2]],
            )
        };
        let position = |action| -> Result<[u32; 5]> {
            send(atoms.XdndPosition, [source, 0, (55 << 16) | 74, 0, action])?;
            next_event(&client, "XdndStatus", |event| match event {
                XEvent::ClientMessage(event) if event.type_ == atoms.XdndStatus => {
                    Some(event.data.as_data32())
                }
                _ => None,
            })
        };
        let request = || {
            next_event(&client, "SelectionRequest", |event| match event {
                XEvent::SelectionRequest(event) => Some(event),
                _ => None,
            })
        };
        let finish = || {
            next_event(&client, "XdndFinished", |event| match event {
                XEvent::ClientMessage(event) if event.type_ == atoms.XdndFinished => {
                    Some(event.data.as_data32())
                }
                _ => None,
            })
        };

        enter([atoms.UTF8_STRING, 0, 0])?;
        assert_eq!(
            position(atoms.XdndActionCopy)?,
            [window, 3, 0, 0, atoms.XdndActionCopy]
        );
        assert!(text_rx.try_recv().is_err());
        send(atoms.XdndDrop, [source, 0, 0, 0, 0])?;
        let read = request()?;
        assert_eq!(read.target, atoms.UTF8_STRING);
        client
            .change_property32(
                PropMode::REPLACE,
                read.requestor,
                read.property,
                atoms.INCR,
                &[16],
            )?
            .check()?;
        client
            .change_window_attributes(
                read.requestor,
                &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                    .event_mask(EventMask::PROPERTY_CHANGE),
            )?
            .check()?;
        client
            .send_event(
                false,
                read.requestor,
                EventMask::NO_EVENT,
                SelectionNotifyEvent {
                    response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: read.time,
                    requestor: read.requestor,
                    selection: read.selection,
                    target: read.target,
                    property: read.property,
                },
            )?
            .check()?;
        let pending = |phase| {
            next_event(&client, phase, |event| match event {
                XEvent::PropertyNotify(event)
                    if event.window == read.requestor
                        && event.atom == read.property
                        && event.state == Property::DELETE =>
                {
                    Some(())
                }
                _ => None,
            })
        };
        pending("INCR header acknowledgement")?;
        let unicode = "Grüße 世界".as_bytes();
        for (bytes, phase) in [
            (&unicode[..3], "first INCR chunk acknowledgement"),
            (&unicode[3..], "second INCR chunk acknowledgement"),
            (&b""[..], "INCR completion acknowledgement"),
        ] {
            client
                .change_property8(
                    PropMode::REPLACE,
                    read.requestor,
                    read.property,
                    read.target,
                    bytes,
                )?
                .check()?;
            pending(phase)?;
        }
        assert_eq!(
            text_rx.recv_timeout(TRANSFER_TIMEOUT)?,
            ((24.0, 27.0), "Grüße 世界".into())
        );
        assert_eq!(finish()?, [window, 1, atoms.XdndActionCopy, 0, 0]);

        enter([atoms.UTF8_STRING, atoms.UriList, 0])?;
        assert_eq!(position(atoms.XdndActionCopy)?[1], 3);
        let read = request()?;
        assert_eq!(read.target, atoms.UriList);
        client
            .change_property8(
                PropMode::REPLACE,
                read.requestor,
                read.property,
                read.target,
                b"file:///tmp/one%20file\r\nfile:///tmp/two\r\n",
            )?
            .check()?;
        client
            .send_event(
                false,
                read.requestor,
                EventMask::NO_EVENT,
                SelectionNotifyEvent {
                    response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: read.time,
                    requestor: read.requestor,
                    selection: read.selection,
                    target: read.target,
                    property: read.property,
                },
            )?
            .check()?;
        let files = vec![PathBuf::from("/tmp/one file"), PathBuf::from("/tmp/two")];
        assert!(matches!(file_rx.recv_timeout(TRANSFER_TIMEOUT)?,
            Event::Files { paths, dropped: false, .. } if paths == files));
        send(atoms.XdndDrop, [source, 0, 0, 0, 0])?;
        assert!(matches!(file_rx.recv_timeout(TRANSFER_TIMEOUT)?,
            Event::Files { point: (24.0, 27.0), paths, dropped: true } if paths == files));
        assert_eq!(finish()?[1], 1);

        enter([atoms.UTF8_STRING, 0, 0])?;
        assert_eq!(position(atoms.XdndActionCopy)?[1], 3);
        registration.enabled.store(false, Ordering::Release);
        send(atoms.XdndDrop, [source, 0, 0, 0, 0])?;
        assert_eq!(finish()?[1], 0);
        assert!(text_rx.try_recv().is_err());

        drop(target);
        worker.join().unwrap()?;
        assert_eq!(
            client
                .get_property(false, window, atoms.XdndProxy, AtomEnum::WINDOW, 0, 1)?
                .reply()?
                .value_len,
            0
        );
        Ok(())
    }

    fn next_event<T>(
        client: &RustConnection,
        phase: &str,
        select: impl Fn(XEvent) -> Option<T>,
    ) -> Result<T> {
        let end = Instant::now() + TRANSFER_TIMEOUT;
        while Instant::now() < end {
            if let Some(result) = client.poll_for_event()?.and_then(&select) {
                return Ok(result);
            }
            thread::sleep(Duration::from_millis(2));
        }
        Err(format!("Timed out waiting for {phase}").into())
    }
}
