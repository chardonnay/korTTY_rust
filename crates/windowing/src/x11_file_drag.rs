//! Copy-only Xdnd source on an independent connection to the window's X server.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use raw_window_handle::{DisplayHandle, RawWindowHandle, WindowHandle};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use x11rb::connection::{Connection as _, RequestConnection as _};
use x11rb::protocol::Event;
use x11rb::protocol::res::{ClientIdMask, ClientIdSpec, ConnectionExt as _};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, InputFocus,
    KeyButMask, PropMode, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent,
    Timestamp, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::wayland_file_drag::file_uris;
use crate::x11_clipboard::Server;

const VERSION: u32 = 5;
const SAMPLE: Duration = Duration::from_millis(24);
const STATUS_WAIT: Duration = Duration::from_millis(450);
const TRANSFER_WAIT: Duration = Duration::from_secs(8);
const GESTURE_WAIT: Duration = Duration::from_secs(30);
const START_WAIT: Duration = Duration::from_millis(500);
const MAX_DEPTH: usize = 24;
const EVENTS_PER_TURN: usize = 48;
const ESCAPE: u32 = 0xff1b;
const SYNTHETIC_EVENT: u8 = 0x80;

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        XdndAware,
        XdndProxy,
        XdndEnter,
        XdndLeave,
        XdndPosition,
        XdndStatus,
        XdndDrop,
        XdndFinished,
        XdndSelection,
        XdndActionCopy,
        XdndActionList,
        TARGETS,
        TIMESTAMP,
        URI_LIST: b"text/uri-list",
        KORTTY_XDND_TIME: b"_KORTTY_XDND_TIME",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Copied,
    Cancelled,
    Unavailable,
}

pub struct Surface {
    server: Server,
    window: Window,
}

impl std::fmt::Debug for Surface {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("X11 file drag surface")
    }
}

pub fn identify(window: WindowHandle<'_>, display: DisplayHandle<'_>) -> Option<Surface> {
    let RawWindowHandle::Xlib(window) = window.as_raw() else {
        return None;
    };
    Some(Surface {
        server: Server::from_display(display).ok()?,
        window: window.window.try_into().ok()?,
    })
}

pub struct Operation(Arc<AtomicBool>);

impl Operation {
    pub fn is_live(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub fn start(
    surface: Arc<Surface>,
    paths: &[PathBuf],
    permitted: impl Fn() -> bool + Send + 'static,
    notify: impl FnOnce(Outcome) + Send + 'static,
) -> Option<Operation> {
    let payload = file_uris(paths)?;
    let operation = Operation(Arc::new(AtomicBool::new(true)));
    let live = Arc::clone(&operation.0);
    let started = Instant::now();
    std::thread::Builder::new()
        .name("kortty-x11-file-drag".into())
        .spawn(move || {
            let authorized = || live.load(Ordering::Acquire) && permitted();
            let outcome =
                run(&surface, &payload, started, &authorized).unwrap_or(Outcome::Unavailable);
            if live.swap(false, Ordering::AcqRel) {
                notify(outcome);
            }
        })
        .ok()?;
    Some(operation)
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Target {
    window: Window,
    recipient: Window,
    version: u32,
    client: u32,
}

struct Hover {
    target: Target,
    position: (i16, i16),
    sent: Instant,
    accepted: Option<bool>,
}

struct Released {
    at: Instant,
    time: Timestamp,
    position: (i16, i16),
}

struct Worker<'a> {
    connection: RustConnection,
    atoms: Atoms,
    root: Window,
    window: Window,
    original: Window,
    timestamp: Timestamp,
    payload: &'a [u8],
    escape: Vec<u8>,
    deferred: RefCell<VecDeque<Event>>,
}

fn run(
    surface: &Surface,
    payload: &[u8],
    started: Instant,
    permitted: &impl Fn() -> bool,
) -> Result<Outcome> {
    let (connection, _) = x11rb::connect(Some(&surface.server.name))?;
    let version = connection.res_query_version(1, 2)?.reply()?;
    if version.server_major < 1 || version.server_major == 1 && version.server_minor < 2 {
        return Ok(Outcome::Unavailable);
    }
    let root = connection
        .setup()
        .roots
        .get(surface.server.screen)
        .ok_or("unknown X11 screen")?
        .root;
    let pointer = connection.query_pointer(root)?.reply()?;
    if !pointer.same_screen
        || !pointer.mask.contains(KeyButMask::BUTTON1)
        || !source_focused(&connection, surface.window, root)?
        || started.elapsed() >= START_WAIT
        || !permitted()
    {
        return Ok(Outcome::Cancelled);
    }
    // The automatic pointer grab from Winit's original press stays with Winit.
    // This worker observes the pointer without trying to steal the grab.
    let atoms = Atoms::new(&connection)?.reply()?;
    let window = connection.generate_id()?;
    connection
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?
        .check()?;
    connection
        .change_property32(
            PropMode::REPLACE,
            window,
            atoms.XdndActionList,
            AtomEnum::ATOM,
            &[atoms.XdndActionCopy],
        )?
        .check()?;
    let mut worker = Worker {
        escape: escape_keycodes(&connection)?,
        connection,
        atoms,
        root,
        window,
        original: surface.window,
        timestamp: 0,
        payload,
        deferred: RefCell::default(),
    };
    worker.timestamp = worker.server_time()?;
    if !permitted()
        || started.elapsed() >= START_WAIT
        || !worker
            .connection
            .query_pointer(root)?
            .reply()?
            .mask
            .contains(KeyButMask::BUTTON1)
        || worker.escape_pressed()?
        || worker
            .connection
            .get_selection_owner(worker.atoms.XdndSelection)?
            .reply()?
            .owner
            != x11rb::NONE
    {
        return Ok(Outcome::Cancelled);
    }
    worker
        .connection
        .set_selection_owner(window, worker.atoms.XdndSelection, worker.timestamp)?
        .check()?;
    if worker
        .connection
        .get_selection_owner(worker.atoms.XdndSelection)?
        .reply()?
        .owner
        != window
    {
        return Ok(Outcome::Cancelled);
    }
    if worker.payload.len().saturating_add(32) > worker.connection.maximum_request_bytes() {
        return Ok(Outcome::Unavailable);
    }
    worker.gesture(started, permitted)
}

impl Worker<'_> {
    fn gesture(&self, started: Instant, permitted: &impl Fn() -> bool) -> Result<Outcome> {
        let mut hover: Option<Hover> = None;
        let mut released: Option<Released> = None;
        loop {
            if started.elapsed() >= GESTURE_WAIT || !permitted() || self.escape_pressed()? {
                self.leave(hover.as_ref());
                return Ok(Outcome::Cancelled);
            }
            for _ in 0..EVENTS_PER_TURN {
                match self.event()? {
                    Some(Event::ClientMessage(event)) => {
                        if let Some(current) = hover.as_mut()
                            && event.window == self.window
                            && event.format == 32
                            && event.type_ == self.atoms.XdndStatus
                        {
                            let data = event.data.as_data32();
                            if data[0] == current.target.window {
                                current.accepted =
                                    Some(data[1] & 1 != 0 && data[4] == self.atoms.XdndActionCopy);
                            }
                        }
                    }
                    Some(Event::SelectionRequest(event)) => self.selection(
                        event,
                        hover.as_ref().map(|hover| hover.target),
                        false,
                        permitted,
                    )?,
                    Some(Event::SelectionClear(event))
                        if event.selection == self.atoms.XdndSelection =>
                    {
                        self.leave(hover.as_ref());
                        return Ok(Outcome::Cancelled);
                    }
                    Some(_) => {}
                    None => break,
                }
            }

            let pointer = self.connection.query_pointer(self.root)?.reply()?;
            let point = (pointer.root_x, pointer.root_y);
            if !pointer.mask.contains(KeyButMask::BUTTON1) && released.is_none() {
                released = Some(Released {
                    at: Instant::now(),
                    time: self.server_time()?,
                    position: point,
                });
            }
            let target = if pointer.same_screen {
                self.target(pointer.child)?
            } else {
                None
            };
            if hover.as_ref().map(|value| value.target) != target {
                self.leave(hover.as_ref());
                if released.is_some() {
                    return Ok(Outcome::Cancelled);
                }
                hover = target
                    .map(|target| {
                        self.send(
                            target,
                            self.atoms.XdndEnter,
                            [self.window, target.version << 24, self.atoms.URI_LIST, 0, 0],
                        )?;
                        self.position(target, point, self.server_time()?)?;
                        Ok::<_, Box<dyn std::error::Error>>(Hover {
                            target,
                            position: point,
                            sent: Instant::now(),
                            accepted: None,
                        })
                    })
                    .transpose()?;
            }
            if let Some(current) = hover.as_mut() {
                let position = released.as_ref().map_or(point, |release| release.position);
                if current.position != position && current.accepted.is_some() {
                    let time = released
                        .as_ref()
                        .map_or_else(|| self.server_time(), |release| Ok(release.time))?;
                    self.position(current.target, position, time)?;
                    current.position = position;
                    current.sent = Instant::now();
                    current.accepted = None;
                }
                if let Some(release) = &released {
                    if current.position == release.position
                        && current.accepted == Some(true)
                        && permitted()
                    {
                        self.send(
                            current.target,
                            self.atoms.XdndDrop,
                            [self.window, 0, release.time, 0, 0],
                        )?;
                        return self.finish(current.target, permitted);
                    }
                    if current.accepted == Some(false) || release.at.elapsed() >= STATUS_WAIT {
                        self.leave(hover.as_ref());
                        return Ok(Outcome::Cancelled);
                    }
                } else if current.accepted.is_none() && current.sent.elapsed() >= STATUS_WAIT {
                    self.leave(hover.as_ref());
                    return Ok(Outcome::Cancelled);
                }
            } else if released.is_some() {
                return Ok(Outcome::Cancelled);
            }
            self.wait()?;
        }
    }

    fn finish(&self, target: Target, permitted: &impl Fn() -> bool) -> Result<Outcome> {
        let deadline = Instant::now() + TRANSFER_WAIT;
        while Instant::now() < deadline && permitted() {
            for _ in 0..EVENTS_PER_TURN {
                match self.event()? {
                    Some(Event::SelectionRequest(event)) => {
                        self.selection(event, Some(target), true, permitted)?
                    }
                    Some(Event::ClientMessage(event))
                        if event.window == self.window
                            && event.format == 32
                            && event.type_ == self.atoms.XdndFinished =>
                    {
                        let data = event.data.as_data32();
                        if data[0] == target.window {
                            return Ok(
                                if target.version < VERSION
                                    || data[1] & 1 != 0 && data[2] == self.atoms.XdndActionCopy
                                {
                                    Outcome::Copied
                                } else {
                                    Outcome::Cancelled
                                },
                            );
                        }
                    }
                    Some(Event::SelectionClear(event))
                        if event.selection == self.atoms.XdndSelection =>
                    {
                        return Ok(Outcome::Cancelled);
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            self.wait()?;
        }
        Ok(Outcome::Cancelled)
    }

    fn target(&self, mut window: Window) -> Result<Option<Target>> {
        for _ in 0..MAX_DEPTH {
            if window == x11rb::NONE || window == self.original {
                return Ok(None);
            }
            let proxy = self.property(window, self.atoms.XdndProxy, AtomEnum::WINDOW)?;
            let recipient = match proxy {
                Some(proxy)
                    if self.property(proxy, self.atoms.XdndProxy, AtomEnum::WINDOW)?
                        == Some(proxy) =>
                {
                    proxy
                }
                _ => window,
            };
            if let Some(version) = self.property(recipient, self.atoms.XdndAware, AtomEnum::ATOM)?
                && version >= 3
                && let Some(client) = self.client(window)?
            {
                return Ok(Some(Target {
                    window,
                    recipient,
                    version: version.min(VERSION),
                    client,
                }));
            }
            window = match self.connection.query_pointer(window)?.reply() {
                Ok(pointer) => pointer.child,
                Err(_) => return Ok(None),
            };
        }
        Ok(None)
    }

    fn property(&self, window: Window, property: Atom, kind: AtomEnum) -> Result<Option<u32>> {
        let Ok(reply) = self
            .connection
            .get_property(false, window, property, kind, 0, 1)?
            .reply()
        else {
            return Ok(None);
        };
        Ok(reply.value32().and_then(|mut values| values.next()))
    }

    fn client(&self, window: Window) -> Result<Option<u32>> {
        let Ok(reply) = self
            .connection
            .res_query_client_ids(&[ClientIdSpec {
                client: window,
                mask: ClientIdMask::CLIENT_XID,
            }])?
            .reply()
        else {
            return Ok(None);
        };
        Ok(reply.ids.into_iter().find_map(|entry| {
            (entry.spec.mask == ClientIdMask::CLIENT_XID).then_some(entry.spec.client)
        }))
    }

    fn selection(
        &self,
        event: SelectionRequestEvent,
        target: Option<Target>,
        dropped: bool,
        permitted: &impl Fn() -> bool,
    ) -> Result<()> {
        let property = if event.property == x11rb::NONE {
            event.target
        } else {
            event.property
        };
        let time_ok = event.time == x11rb::CURRENT_TIME
            || event.time.wrapping_sub(self.timestamp) < (1 << 31);
        let mut offered = x11rb::NONE;
        if event.owner == self.window
            && event.selection == self.atoms.XdndSelection
            && event.response_type & SYNTHETIC_EVENT == 0
            && time_ok
            && permitted()
            && let Some(target) = target
            && self.client(event.requestor)? == Some(target.client)
        {
            let wrote = if event.target == self.atoms.TARGETS {
                let types = [
                    self.atoms.TARGETS,
                    self.atoms.TIMESTAMP,
                    self.atoms.URI_LIST,
                ];
                self.connection
                    .change_property32(
                        PropMode::REPLACE,
                        event.requestor,
                        property,
                        AtomEnum::ATOM,
                        &types,
                    )?
                    .check()
                    .is_ok()
            } else if event.target == self.atoms.TIMESTAMP {
                self.connection
                    .change_property32(
                        PropMode::REPLACE,
                        event.requestor,
                        property,
                        AtomEnum::INTEGER,
                        &[self.timestamp],
                    )?
                    .check()
                    .is_ok()
            } else if dropped && event.target == self.atoms.URI_LIST && permitted() {
                self.connection
                    .change_property8(
                        PropMode::REPLACE,
                        event.requestor,
                        property,
                        self.atoms.URI_LIST,
                        self.payload,
                    )?
                    .check()
                    .is_ok()
            } else {
                false
            };
            if wrote {
                offered = property;
            }
        }
        let _ = self
            .connection
            .send_event(
                false,
                event.requestor,
                EventMask::NO_EVENT,
                SelectionNotifyEvent {
                    response_type: SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: event.time,
                    requestor: event.requestor,
                    selection: event.selection,
                    target: event.target,
                    property: offered,
                },
            )?
            .check();
        Ok(())
    }

    fn position(&self, target: Target, point: (i16, i16), time: Timestamp) -> Result<()> {
        let packed = (u32::from(point.0 as u16) << 16) | u32::from(point.1 as u16);
        self.send(
            target,
            self.atoms.XdndPosition,
            [self.window, 0, packed, time, self.atoms.XdndActionCopy],
        )
    }

    fn leave(&self, hover: Option<&Hover>) {
        if let Some(hover) = hover {
            let _ = self.send(
                hover.target,
                self.atoms.XdndLeave,
                [self.window, 0, 0, 0, 0],
            );
        }
    }

    fn send(&self, target: Target, kind: Atom, data: [u32; 5]) -> Result<()> {
        self.connection
            .send_event(
                false,
                target.recipient,
                EventMask::NO_EVENT,
                ClientMessageEvent::new(32, target.window, kind, data),
            )?
            .check()?;
        Ok(())
    }

    fn server_time(&self) -> Result<Timestamp> {
        self.connection
            .change_property8(
                PropMode::REPLACE,
                self.window,
                self.atoms.KORTTY_XDND_TIME,
                AtomEnum::STRING,
                b"",
            )?
            .check()?;
        let deadline = Instant::now() + START_WAIT;
        while Instant::now() < deadline {
            match self.connection.poll_for_event()? {
                Some(Event::PropertyNotify(event))
                    if event.window == self.window && event.atom == self.atoms.KORTTY_XDND_TIME =>
                {
                    return Ok(event.time);
                }
                Some(event) => {
                    let mut deferred = self.deferred.borrow_mut();
                    if deferred.len() >= EVENTS_PER_TURN {
                        return Err("too many queued X11 events".into());
                    }
                    deferred.push_back(event);
                }
                None => self.wait()?,
            }
        }
        Err("X11 server did not provide a timestamp".into())
    }

    fn event(&self) -> Result<Option<Event>> {
        if let Some(event) = self.deferred.borrow_mut().pop_front() {
            return Ok(Some(event));
        }
        Ok(self.connection.poll_for_event()?)
    }

    fn escape_pressed(&self) -> Result<bool> {
        let keys = self.connection.query_keymap()?.reply()?.keys;
        Ok(self
            .escape
            .iter()
            .any(|&key| keys[usize::from(key / 8)] & (1 << (key % 8)) != 0))
    }

    fn wait(&self) -> Result<()> {
        let mut fds = [PollFd::new(self.connection.stream(), PollFlags::IN)];
        let timeout = Timespec {
            tv_sec: 0,
            tv_nsec: SAMPLE.as_nanos() as _,
        };
        let _ = poll(&mut fds, Some(&timeout))?;
        Ok(())
    }
}

fn source_focused(connection: &RustConnection, source: Window, root: Window) -> Result<bool> {
    let mut window = connection.get_input_focus()?.reply()?.focus;
    for _ in 0..MAX_DEPTH {
        if window == source {
            return Ok(true);
        }
        if window <= u32::from(InputFocus::POINTER_ROOT) || window == root {
            break;
        }
        let Ok(tree) = connection.query_tree(window)?.reply() else {
            break;
        };
        window = tree.parent;
    }
    Ok(false)
}

fn escape_keycodes(connection: &RustConnection) -> Result<Vec<u8>> {
    let setup = connection.setup();
    let first = setup.min_keycode;
    let count = setup.max_keycode.saturating_sub(first).saturating_add(1);
    let map = connection.get_keyboard_mapping(first, count)?.reply()?;
    let width = usize::from(map.keysyms_per_keycode).max(1);
    Ok(map
        .keysyms
        .chunks(width)
        .enumerate()
        .filter_map(|(index, keys)| keys.contains(&ESCAPE).then_some(first + index as u8))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a private X server with XTEST and X-Resource"]
    fn native_xdnd_copy_requires_the_selected_target_and_drop() -> Result<()> {
        use x11rb::protocol::xproto::{BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT};
        use x11rb::protocol::xtest::ConnectionExt as _;

        let (source, screen) = x11rb::connect(None)?;
        let (target, _) = x11rb::connect(None)?;
        let (stranger, _) = x11rb::connect(None)?;
        let root = source.setup().roots[screen].root;
        let source_window = mapped_window(&source, root, 10)?;
        let target_window = mapped_window(&target, root, 210)?;
        let stranger_window = mapped_window(&stranger, root, 410)?;
        let atoms = Atoms::new(&target)?.reply()?;
        target
            .change_property32(
                PropMode::REPLACE,
                target_window,
                atoms.XdndAware,
                AtomEnum::ATOM,
                &[VERSION],
            )?
            .check()?;
        source
            .set_input_focus(InputFocus::PARENT, source_window, x11rb::CURRENT_TIME)?
            .check()?;
        source
            .warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, 30, 30)?
            .check()?;
        source
            .xtest_fake_input(BUTTON_PRESS_EVENT, 1, 0, root, 0, 0, 0)?
            .check()?;
        let surface = Arc::new(Surface {
            server: Server {
                name: std::env::var("DISPLAY")?,
                screen,
            },
            window: source_window,
        });
        let fixture_dir = std::env::temp_dir();
        let paths = [fixture_dir.join("ü #.txt"), fixture_dir.join("second.txt")];
        std::fs::write(&paths[0], b"first fixture")?;
        std::fs::write(&paths[1], b"second fixture")?;
        let (done, outcome) = std::sync::mpsc::sync_channel(1);
        let operation = start(
            surface,
            &paths,
            || true,
            move |outcome| {
                let _ = done.send(outcome);
            },
        )
        .ok_or("could not start source")?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let selection_owner = loop {
            let owner = target
                .get_selection_owner(atoms.XdndSelection)?
                .reply()?
                .owner;
            if owner != x11rb::NONE {
                break owner;
            }
            if Instant::now() >= deadline {
                return Err(format!("selection was never owned: {:?}", outcome.try_recv()).into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        source
            .warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, 230, 30)?
            .check()?;
        let enter = message(&target, atoms.XdndEnter)?;
        assert_eq!(enter.window, target_window);
        assert_eq!(
            enter.data.as_data32(),
            [selection_owner, VERSION << 24, atoms.URI_LIST, 0, 0]
        );
        let position = message(&target, atoms.XdndPosition)?;
        assert_eq!(position.data.as_data32()[4], atoms.XdndActionCopy);
        let timestamp = position.data.as_data32()[3];
        assert_ne!(timestamp, 0);
        assert!(convert(&target, &atoms, target_window, timestamp)?.is_none());
        target
            .send_event(
                false,
                selection_owner,
                EventMask::NO_EVENT,
                ClientMessageEvent::new(
                    32,
                    selection_owner,
                    atoms.XdndStatus,
                    [target_window, 1, 0, 0, atoms.XdndActionCopy],
                ),
            )?
            .check()?;
        source
            .xtest_fake_input(BUTTON_RELEASE_EVENT, 1, 0, root, 0, 0, 0)?
            .check()?;
        let dropped = message(&target, atoms.XdndDrop)?;
        assert_eq!(dropped.data.as_data32()[0], selection_owner);
        let timestamp = dropped.data.as_data32()[2];
        assert!(convert(&stranger, &atoms, stranger_window, timestamp)?.is_none());
        let files = convert(&target, &atoms, target_window, timestamp)?.ok_or("no URI data")?;
        assert_eq!(files.as_slice(), file_uris(&paths).unwrap().as_ref());
        assert_eq!(std::fs::read(&paths[0])?, b"first fixture");
        assert_eq!(std::fs::read(&paths[1])?, b"second fixture");
        target
            .send_event(
                false,
                selection_owner,
                EventMask::NO_EVENT,
                ClientMessageEvent::new(
                    32,
                    selection_owner,
                    atoms.XdndFinished,
                    [target_window, 1, atoms.XdndActionCopy, 0, 0],
                ),
            )?
            .check()?;
        assert_eq!(
            outcome.recv_timeout(Duration::from_secs(2))?,
            Outcome::Copied
        );
        assert!(!operation.is_live());
        Ok(())
    }

    fn mapped_window(connection: &RustConnection, root: Window, x: i16) -> Result<Window> {
        let id = connection.generate_id()?;
        connection
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                id,
                root,
                x,
                10,
                150,
                100,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new()
                    .event_mask(EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE),
            )?
            .check()?;
        connection.map_window(id)?.check()?;
        Ok(id)
    }

    fn message(connection: &RustConnection, kind: Atom) -> Result<ClientMessageEvent> {
        event_until(connection, |event| {
            if let Event::ClientMessage(message) = event
                && message.type_ == kind
            {
                return Some(message);
            }
            None
        })
    }

    fn convert(
        connection: &RustConnection,
        atoms: &Atoms,
        window: Window,
        time: Timestamp,
    ) -> Result<Option<Vec<u8>>> {
        connection
            .convert_selection(
                window,
                atoms.XdndSelection,
                atoms.URI_LIST,
                atoms.KORTTY_XDND_TIME,
                time,
            )?
            .check()?;
        let notify = event_until(connection, |event| match event {
            Event::SelectionNotify(notify) => Some(notify),
            _ => None,
        })?;
        if notify.property == x11rb::NONE {
            return Ok(None);
        }
        let reply = connection
            .get_property(true, window, notify.property, atoms.URI_LIST, 0, 1024)?
            .reply()?;
        Ok(Some(reply.value))
    }

    fn event_until<T>(
        connection: &RustConnection,
        accept: impl Fn(Event) -> Option<T>,
    ) -> Result<T> {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Some(event) = connection.poll_for_event()? {
                if let Some(value) = accept(event) {
                    return Ok(value);
                }
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Err("X11 event timed out".into())
    }
}
