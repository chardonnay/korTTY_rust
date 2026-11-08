use std::env;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc::{self, SyncSender};
use std::thread;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::xdg::system_bell::v1::client::xdg_system_bell_v1::XdgSystemBellV1;
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::ConnectionExt as _;

#[derive(Clone, Copy)]
pub(super) enum Display {
    X11,
    Wayland,
}

pub(super) fn request(display: Display) {
    static REQUESTS: OnceLock<Option<SyncSender<Display>>> = OnceLock::new();
    if let Some(requests) = REQUESTS.get_or_init(spawn) {
        let _ = requests.try_send(display);
    }
}

fn spawn() -> Option<SyncSender<Display>> {
    let (send, requests) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("kortty-system-bell".into())
        .spawn(move || {
            let mut x11 = None;
            let mut wayland = None;
            for display in requests {
                match display {
                    Display::X11 => {
                        let connection = x11.get_or_insert_with(|| x11rb::connect(None).ok());
                        if let Some((connection, _)) = connection
                            && (connection.bell(0).is_err() || connection.flush().is_err())
                        {
                            x11 = None;
                        }
                    }
                    Display::Wayland => {
                        if let Some(bell) = wayland.get_or_insert_with(WaylandBell::connect)
                            && !bell.ring()
                        {
                            wayland = None;
                        }
                    }
                }
            }
        })
        .ok()?;
    Some(send)
}

struct Registry;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Registry {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Registry: ignore XdgSystemBellV1);

struct WaylandBell {
    connection: Connection,
    bell: XdgSystemBellV1,
    events: EventQueue<Registry>,
}

impl WaylandBell {
    fn connect() -> Option<Self> {
        // Open a separate display socket. WAYLAND_SOCKET may name an inherited
        // descriptor owned by the windowing toolkit and must remain untouched.
        let display = PathBuf::from(env::var_os("WAYLAND_DISPLAY")?);
        let path = if display.is_absolute() {
            display
        } else {
            let runtime = PathBuf::from(env::var_os("XDG_RUNTIME_DIR")?);
            if !runtime.is_absolute() {
                return None;
            }
            runtime.join(display)
        };
        let connection = Connection::from_socket(UnixStream::connect(path).ok()?).ok()?;
        let (globals, events) = registry_queue_init::<Registry>(&connection).ok()?;
        let bell = globals.bind(&events.handle(), 1..=1, ()).ok()?;
        Some(Self {
            connection,
            bell,
            events,
        })
    }

    fn ring(&mut self) -> bool {
        // This connection has no application surface. The protocol expressly
        // supports an unassociated alert and lets the compositor choose feedback.
        self.bell.ring(None);
        self.connection.flush().is_ok() && self.events.dispatch_pending(&mut Registry).is_ok()
    }
}
