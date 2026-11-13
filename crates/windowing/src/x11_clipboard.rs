//! Owns an X11 CLIPBOARD PNG on a dedicated protocol connection.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use raw_window_handle::{DisplayHandle, RawDisplayHandle};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use x11rb::connection::{Connection as _, RequestConnection as _};
use x11rb::cookie::VoidCookie;
use x11rb::errors::ConnectionError;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask,
    PropMode, Property, PropertyNotifyEvent, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent,
    SelectionRequestEvent, Timestamp, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const MAX_PNG: usize = 16 * 1024 * 1024;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const MAX_CHUNK: usize = 64 * 1024;
const MAX_TRANSFERS: usize = 8;
const MAX_TRANSFER_BYTES: usize = 2 * MAX_PNG;
const TRANSFER_IDLE: Duration = Duration::from_secs(5);
const TRANSFER_AGE: Duration = Duration::from_secs(30);
const CLAIM_TIMEOUT: Duration = Duration::from_secs(5);
const EVENTS_PER_TURN: usize = 64;
const SYNTHETIC: u8 = 0x80;

type Completion = Box<dyn FnOnce(io::Result<()>) + Send>;
type Slot = (Window, Atom);

static CLIENT: Mutex<Option<Client>> = Mutex::new(None);

/// Copies only the display address off the borrowed UI connection.
pub(super) fn write(display: DisplayHandle<'_>, png: Arc<[u8]>, complete: Completion) {
    let request = Request {
        png,
        complete: Some(complete),
    };
    let server = if request.png.len() > MAX_PNG || !request.png.starts_with(PNG_SIGNATURE) {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid or oversized X11 clipboard PNG",
        ))
    } else {
        Server::from_display(display)
    };
    match server {
        Ok(server) => {
            if let Err((request, error)) = enqueue(server, request) {
                request.finish(Err(error));
            }
        }
        Err(error) => request.finish(Err(error)),
    }
}

/// Closing the wake socket releases the owner connection, its window and outstanding transfers.
pub(super) fn shutdown() {
    CLIENT
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take();
}

struct Request {
    png: Arc<[u8]>,
    complete: Option<Completion>,
}

impl Request {
    fn finish(mut self, result: io::Result<()>) {
        if let Some(complete) = self.complete.take() {
            complete(result);
        }
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        if let Some(complete) = self.complete.take() {
            complete(Err(stopped()));
        }
    }
}

#[derive(Eq, PartialEq)]
pub(super) struct Server {
    pub(super) name: String,
    pub(super) screen: usize,
}

impl Server {
    pub(super) fn from_display(display: DisplayHandle<'_>) -> io::Result<Self> {
        // Winit supplies a live Xlib handle. An XCB address cannot be inferred from its
        // connection pointer or the process environment without risking another X server.
        let RawDisplayHandle::Xlib(display) = display.as_raw() else {
            return Err(unbound_display());
        };
        let pointer = display.display.ok_or_else(unbound_display)?;
        let screen = usize::try_from(display.screen).map_err(|_| unbound_display())?;
        let xlib = XLIB
            .get_or_init(Xlib::load)
            .as_ref()
            .ok_or_else(unbound_display)?;
        // SAFETY: The borrowed DisplayHandle keeps the UI's Xlib Display live for this call.
        // XDisplayString returns a display-owned NUL-terminated string, copied before returning.
        let name = unsafe {
            let name = (xlib.display_string)(pointer.as_ptr());
            if name.is_null() {
                return Err(unbound_display());
            }
            CStr::from_ptr(name)
                .to_str()
                .map_err(|_| unbound_display())?
        };
        if name.is_empty() || name.len() > 1024 {
            return Err(unbound_display());
        }
        Ok(Self {
            name: name.to_owned(),
            screen,
        })
    }
}

type DisplayString = unsafe extern "C" fn(*mut c_void) -> *const c_char;

static XLIB: OnceLock<Option<Xlib>> = OnceLock::new();

struct Xlib {
    _library: libloading::Library,
    display_string: DisplayString,
}

impl Xlib {
    fn load() -> Option<Self> {
        // SAFETY: This is the platform X11 library already used by the borrowed Xlib display.
        // The exported XDisplayString signature is stable, and the Library outlives the symbol.
        unsafe {
            let library = libloading::Library::new("libX11.so.6").ok()?;
            let display_string = *library.get::<DisplayString>(b"XDisplayString\0").ok()?;
            Some(Self {
                _library: library,
                display_string,
            })
        }
    }
}

fn unbound_display() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Could not identify the borrowed X11 clipboard display",
    )
}

struct Client {
    server: Arc<Server>,
    requests: SyncSender<Request>,
    wake: UnixStream,
    worker: thread::JoinHandle<()>,
}

impl Client {
    fn start(server: Arc<Server>) -> io::Result<Self> {
        let (wake, mut reader) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        reader.set_nonblocking(true)?;
        let (requests, inbox) = mpsc::sync_channel(2);
        let address = Arc::clone(&server);
        let worker = thread::Builder::new()
            .name("kortty-x11-clipboard".into())
            .spawn(move || {
                let result =
                    Owner::connect(&address).and_then(|mut owner| owner.run(&inbox, &mut reader));
                if let Err(error) = result {
                    for request in inbox.try_iter() {
                        request.finish(Err(io::Error::new(error.kind(), error.to_string())));
                    }
                }
            })?;
        Ok(Self {
            server,
            requests,
            wake,
            worker,
        })
    }

    fn notify(&self) {
        while (&self.wake)
            .write(&[1])
            .is_err_and(|error| error.kind() == io::ErrorKind::Interrupted)
        {}
    }
}

fn enqueue(server: Server, mut request: Request) -> Result<(), (Request, io::Error)> {
    let mut slot = CLIENT.lock().unwrap_or_else(|poison| poison.into_inner());
    let server = Arc::new(server);
    if slot
        .as_ref()
        .is_some_and(|client| client.server != server || client.worker.is_finished())
    {
        slot.take();
    }
    for _ in 0..2 {
        if slot.is_none() {
            match Client::start(Arc::clone(&server)) {
                Ok(client) => *slot = Some(client),
                Err(error) => return Err((request, error)),
            }
        }
        let Some(client) = slot.as_ref() else {
            return Err((request, stopped()));
        };
        match client.requests.try_send(request) {
            Ok(()) => {
                // A full wake buffer already contains a wakeup. On disconnect the inbox drops
                // any accepted command and completes its callback with a worker-stop error.
                client.notify();
                return Ok(());
            }
            Err(TrySendError::Full(request)) => {
                return Err((
                    request,
                    io::Error::new(io::ErrorKind::WouldBlock, "The X11 clipboard queue is busy"),
                ));
            }
            Err(TrySendError::Disconnected(returned)) => {
                request = returned;
                slot.take();
            }
        }
    }
    Err((request, stopped()))
}

fn stopped() -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "The X11 clipboard worker stopped",
    )
}

x11rb::atom_manager! {
    Atoms: AtomCookies {
        clipboard: b"CLIPBOARD".as_slice(),
        targets: b"TARGETS".as_slice(),
        timestamp: b"TIMESTAMP".as_slice(),
        png: b"image/png".as_slice(),
        incr: b"INCR".as_slice(),
        clock: b"_KORTTY_CLIPBOARD_TIME".as_slice(),
    }
}

struct Selection {
    png: Arc<[u8]>,
    since: Timestamp,
}

struct Claim {
    request: Request,
    sequence: u16,
    deadline: Instant,
}

struct Transfer {
    png: Arc<[u8]>,
    offset: usize,
    expires: Instant,
    deadline: Instant,
}

impl Transfer {
    fn new(png: Arc<[u8]>, now: Instant) -> Self {
        Self {
            png,
            offset: 0,
            expires: now + TRANSFER_AGE,
            deadline: now + TRANSFER_IDLE,
        }
    }

    fn next(&mut self, maximum: usize, now: Instant) -> &[u8] {
        let start = self.offset;
        self.offset = self.png.len().min(start + maximum);
        self.deadline = self.expires.min(now + TRANSFER_IDLE);
        &self.png[start..self.offset]
    }
}

struct Owner {
    connection: RustConnection,
    window: Window,
    atoms: Atoms,
    chunk: usize,
    selection: Option<Selection>,
    claim: Option<Claim>,
    transfers: HashMap<Slot, Transfer>,
}

impl Owner {
    fn connect(server: &Server) -> io::Result<Self> {
        let (connection, _) = x11rb::connect(Some(&server.name)).map_err(io::Error::other)?;
        let root = connection
            .setup()
            .roots
            .get(server.screen)
            .ok_or_else(unbound_display)?
            .root;
        let window = connection.generate_id().map_err(io::Error::other)?;
        checked(connection.create_window(
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
        ))?;
        let atoms = Atoms::new(&connection)
            .map_err(io::Error::other)?
            .reply()
            .map_err(io::Error::other)?;
        let chunk = connection
            .maximum_request_bytes()
            .saturating_sub(32)
            .min(MAX_CHUNK);
        if chunk == 0 {
            return Err(io::Error::other(
                "The X11 clipboard server has no usable request capacity",
            ));
        }
        Ok(Self {
            connection,
            window,
            atoms,
            chunk,
            selection: None,
            claim: None,
            transfers: HashMap::new(),
        })
    }

    fn run(&mut self, inbox: &Receiver<Request>, wake: &mut UnixStream) -> io::Result<()> {
        loop {
            if !drain(wake)? {
                return Ok(());
            }
            self.expire(Instant::now())?;
            let more = self.events()?;
            if self.claim.is_none() {
                match inbox.try_recv() {
                    Ok(request) => {
                        self.begin_claim(request);
                        continue;
                    }
                    Err(TryRecvError::Disconnected) => return Ok(()),
                    Err(TryRecvError::Empty) => {}
                }
            }
            if !more {
                self.wait(wake)?;
            }
        }
    }

    fn wait(&self, wake: &UnixStream) -> io::Result<()> {
        let deadline = self
            .claim
            .iter()
            .map(|claim| claim.deadline)
            .chain(self.transfers.values().map(|transfer| transfer.deadline))
            .min();
        let timeout = deadline
            .map(|deadline| Timespec::try_from(deadline.saturating_duration_since(Instant::now())))
            .transpose()
            .map_err(io::Error::other)?;
        let mut fds = [
            PollFd::new(wake, PollFlags::IN),
            PollFd::new(self.connection.stream(), PollFlags::IN),
        ];
        match poll(&mut fds, timeout.as_ref()) {
            Ok(_)
                if fds[1]
                    .revents()
                    .intersects(PollFlags::ERR | PollFlags::HUP | PollFlags::NVAL) =>
            {
                Err(stopped())
            }
            Ok(_) | Err(rustix::io::Errno::INTR) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn begin_claim(&mut self, request: Request) {
        let clock = (|| {
            let cookie = self
                .connection
                .change_property8(
                    PropMode::REPLACE,
                    self.window,
                    self.atoms.clock,
                    AtomEnum::INTEGER,
                    &[0],
                )
                .map_err(io::Error::other)?;
            let sequence = cookie.sequence_number() as u16;
            cookie.check().map_err(io::Error::other)?;
            Ok(sequence)
        })();
        match clock {
            Ok(sequence) => {
                self.claim = Some(Claim {
                    request,
                    sequence,
                    deadline: Instant::now() + CLAIM_TIMEOUT,
                })
            }
            Err(error) => request.finish(Err(error)),
        }
    }

    fn complete_claim(&mut self, event: PropertyNotifyEvent) {
        if event.state != Property::NEW_VALUE
            || event.time == x11rb::CURRENT_TIME
            || self
                .claim
                .as_ref()
                .is_none_or(|claim| claim.sequence != event.sequence)
        {
            return;
        }
        let Some(claim) = self.claim.take() else {
            return;
        };
        let result = checked(self.connection.set_selection_owner(
            self.window,
            self.atoms.clipboard,
            event.time,
        ))
        .and_then(|()| self.is_owner())
        .and_then(|owned| {
            if !owned {
                return Err(io::Error::other(
                    "X11 declined clipboard selection ownership",
                ));
            }
            self.selection = Some(Selection {
                png: Arc::clone(&claim.request.png),
                since: event.time,
            });
            Ok(())
        });
        claim.request.finish(result);
    }

    fn is_owner(&self) -> io::Result<bool> {
        self.connection
            .get_selection_owner(self.atoms.clipboard)
            .map_err(io::Error::other)?
            .reply()
            .map(|reply| reply.owner == self.window)
            .map_err(io::Error::other)
    }

    fn events(&mut self) -> io::Result<bool> {
        for _ in 0..EVENTS_PER_TURN {
            let Some(event) = self.connection.poll_for_event().map_err(io::Error::other)? else {
                return Ok(false);
            };
            match event {
                Event::SelectionRequest(event) if genuine(event.response_type) => {
                    self.request(event)
                }
                Event::SelectionClear(event)
                    if genuine(event.response_type)
                        && event.owner == self.window
                        && event.selection == self.atoms.clipboard =>
                {
                    if !self.is_owner()? {
                        self.selection = None;
                    }
                }
                Event::PropertyNotify(event) if genuine(event.response_type) => {
                    if event.window == self.window && event.atom == self.atoms.clock {
                        self.complete_claim(event);
                    } else if event.state == Property::DELETE {
                        self.advance_transfer((event.window, event.atom));
                    }
                }
                Event::DestroyNotify(event) if genuine(event.response_type) => {
                    self.transfers
                        .retain(|&(window, _), _| window != event.window);
                }
                _ => {}
            }
        }
        Ok(true)
    }

    fn request(&mut self, event: SelectionRequestEvent) {
        if event.requestor == x11rb::NONE
            || event.requestor == self.window
            || self
                .connection
                .setup()
                .roots
                .iter()
                .any(|screen| screen.root == event.requestor)
            || event.owner != self.window
            || event.selection != self.atoms.clipboard
        {
            return;
        }
        let property = if event.property == x11rb::NONE {
            event.target
        } else {
            event.property
        };
        let slot = (event.requestor, property);
        let accepted = property != x11rb::NONE
            && !self.transfers.contains_key(&slot)
            && self
                .selection
                .as_ref()
                .is_some_and(|selection| current_timestamp(event.time, selection.since))
            && self.convert(slot, event.target).is_ok();
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: event.time,
            requestor: event.requestor,
            selection: event.selection,
            target: event.target,
            property: if accepted { property } else { x11rb::NONE },
        };
        if checked(
            self.connection
                .send_event(false, event.requestor, EventMask::NO_EVENT, notify),
        )
        .is_err()
            && accepted
        {
            self.finish_transfer(slot);
        }
    }

    fn convert(&mut self, slot: Slot, target: Atom) -> io::Result<()> {
        let Some(selection) = &self.selection else {
            return Err(stopped());
        };
        let (window, property) = slot;
        if target == self.atoms.targets {
            return checked(self.connection.change_property32(
                PropMode::REPLACE,
                window,
                property,
                AtomEnum::ATOM,
                &[self.atoms.targets, self.atoms.timestamp, self.atoms.png],
            ));
        }
        if target == self.atoms.timestamp {
            return checked(self.connection.change_property32(
                PropMode::REPLACE,
                window,
                property,
                AtomEnum::INTEGER,
                &[selection.since],
            ));
        }
        if target != self.atoms.png {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unsupported X11 clipboard target",
            ));
        }
        if selection.png.len() <= self.chunk {
            return checked(self.connection.change_property8(
                PropMode::REPLACE,
                window,
                property,
                self.atoms.png,
                &selection.png,
            ));
        }
        if self.transfers.len() == MAX_TRANSFERS
            || self
                .transfers
                .values()
                .map(|transfer| transfer.png.len())
                .sum::<usize>()
                + selection.png.len()
                > MAX_TRANSFER_BYTES
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Too many X11 clipboard transfers",
            ));
        }
        let watch = !self
            .transfers
            .keys()
            .any(|&(requestor, _)| requestor == window);
        if watch {
            checked(
                self.connection.change_window_attributes(
                    window,
                    &ChangeWindowAttributesAux::new()
                        .event_mask(EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY),
                ),
            )?;
        }
        let png = Arc::clone(&selection.png);
        let result = checked(self.connection.change_property32(
            PropMode::REPLACE,
            window,
            property,
            self.atoms.incr,
            &[png.len() as u32],
        ));
        if result.is_ok() {
            // ICCCM permits this admitted transfer to finish after another owner takes over.
            self.transfers
                .insert(slot, Transfer::new(png, Instant::now()));
        } else if watch {
            self.unwatch(window);
        }
        result
    }

    fn advance_transfer(&mut self, slot: Slot) {
        let (window, property) = slot;
        let Some(transfer) = self.transfers.get_mut(&slot) else {
            return;
        };
        let bytes = transfer.next(self.chunk, Instant::now());
        let result = checked(self.connection.change_property8(
            PropMode::REPLACE,
            window,
            property,
            self.atoms.png,
            bytes,
        ));
        if bytes.is_empty() || result.is_err() {
            self.finish_transfer(slot);
        }
    }

    fn expire(&mut self, now: Instant) -> io::Result<()> {
        if self
            .claim
            .as_ref()
            .is_some_and(|claim| now >= claim.deadline)
        {
            let error = || {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "The X11 clipboard server did not supply a timestamp",
                )
            };
            if let Some(claim) = self.claim.take() {
                claim.request.finish(Err(error()));
            }
            return Err(error());
        }
        let expired = self
            .transfers
            .iter()
            .filter_map(|(&slot, transfer)| (now >= transfer.deadline).then_some(slot))
            .collect::<Vec<_>>();
        for slot in expired {
            self.finish_transfer(slot);
        }
        Ok(())
    }

    fn finish_transfer(&mut self, slot: Slot) {
        if self.transfers.remove(&slot).is_some()
            && !self.transfers.keys().any(|&(window, _)| window == slot.0)
        {
            self.unwatch(slot.0);
        }
    }

    fn unwatch(&self, window: Window) {
        if let Ok(cookie) = self.connection.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT),
        ) {
            cookie.ignore_error();
            let _ = self.connection.flush();
        }
    }
}

fn checked(cookie: Result<VoidCookie<'_, RustConnection>, ConnectionError>) -> io::Result<()> {
    cookie
        .map_err(io::Error::other)?
        .check()
        .map_err(io::Error::other)
}

fn genuine(response_type: u8) -> bool {
    response_type & SYNTHETIC == 0
}

fn current_timestamp(requested: Timestamp, owned_since: Timestamp) -> bool {
    requested == x11rb::CURRENT_TIME || (requested.wrapping_sub(owned_since) as i32) >= 0
}

fn drain(wake: &mut UnixStream) -> io::Result<bool> {
    let mut bytes = [0; 64];
    match wake.read(&mut bytes) {
        Ok(0) => Ok(false),
        Ok(_) => Ok(true),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) =>
        {
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
