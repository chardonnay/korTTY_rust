//! Native FreeDesktop desktop notifications on a private session-bus connection.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

mod activation;
mod dbus;
mod launcher;

pub use activation::activate_notification_window;
pub use launcher::{DESKTOP_APP_ID, LauncherBadge};

const QUEUE_LIMIT: usize = 16;
const PENDING_LIMIT: usize = 128;
const CLICK_LIFETIME: Duration = Duration::from_secs(10 * 60);

type Activation = Box<dyn FnOnce(Option<String>) + Send>;
type Permission = Box<dyn Fn() -> bool + Send + Sync>;

struct Request {
    title: String,
    body: String,
    allowed: Permission,
    activate: Activation,
}

/// Queues a notification for the desktop's session-bus service.
///
/// Returns false if the bus address, worker, permission, or queue is unavailable.
/// The worker contacts the service asynchronously after accepting a request.
/// The callback runs once on the worker if the service invokes the default
/// action, with a compositor activation token when the service provides one.
pub fn send_desktop_notification(
    title: String,
    body: String,
    still_allowed: impl Fn() -> bool + Send + Sync + 'static,
    on_activate: impl FnOnce(Option<String>) + Send + 'static,
) -> bool {
    static REQUESTS: OnceLock<Option<SyncSender<Request>>> = OnceLock::new();

    // Never trigger libdbus's legacy X11 bus autolaunch from a window event.
    if !std::env::var_os("DBUS_SESSION_BUS_ADDRESS")
        .as_deref()
        .is_some_and(usable_session_bus_address)
    {
        return false;
    }
    let title = clean_text(&title, 160, false);
    if title.trim().is_empty() || !still_allowed() {
        return false;
    }
    REQUESTS.get_or_init(spawn).as_ref().is_some_and(|queue| {
        queue
            .try_send(Request {
                title,
                body: clean_text(&body, 600, true),
                allowed: Box::new(still_allowed),
                activate: Box::new(on_activate),
            })
            .is_ok()
    })
}

fn usable_session_bus_address(address: &OsStr) -> bool {
    !address.is_empty()
        && !address
            .as_bytes()
            .split(|byte| *byte == b';')
            .any(|entry| entry.starts_with(b"autolaunch:"))
}

fn clean_text(text: &str, limit: usize, multiline: bool) -> String {
    text.chars()
        .filter_map(|character| match character {
            '\n' if multiline => Some('\n'),
            '\n' | '\r' | '\t' => Some(' '),
            control if control.is_control()
                || matches!(control, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') => None,
            character => Some(character),
        })
        .take(limit)
        .collect()
}

fn escape_markup(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            character => escaped.push(character),
        }
    }
    escaped
}

fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 4096 && !token.chars().any(char::is_control)
}

fn spawn() -> Option<SyncSender<Request>> {
    // Do not accept a request until the worker has a usable native API.
    let api = dbus::Api::load()?;
    let (send, receive) = mpsc::sync_channel(QUEUE_LIMIT);
    thread::Builder::new()
        .name("kortty-notifications".into())
        .spawn(move || run(&api, &receive))
        .ok()?;
    Some(send)
}

struct Pending {
    notification: dbus::Notification,
    created: Instant,
    token: Option<String>,
    allowed: Permission,
    activate: Activation,
}

fn run(api: &dbus::Api, requests: &Receiver<Request>) {
    let mut connection = None;
    let mut pending: VecDeque<Pending> = VecDeque::new();
    loop {
        pending.retain(|entry| entry.created.elapsed() < CLICK_LIFETIME && (entry.allowed)());
        let request = if pending.is_empty() {
            match requests.recv() {
                Ok(request) => Some(request),
                Err(_) => break,
            }
        } else {
            match requests.try_recv() {
                Ok(request) => Some(request),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => break,
            }
        };
        if let Some(request) = request.filter(|request| (request.allowed)()) {
            if let Some(bus) = connection.as_ref()
                && !dbus::Bus::read(bus, 0)
            {
                pending.clear();
                connection = None;
            }
            if connection.is_none() {
                connection = api.connect();
            }
            if let Some(bus) = &connection
                && let Some(notification) =
                    bus.notify(&request.title, &request.body, &request.allowed)
            {
                // A conforming server cannot reuse an active ID without a
                // replacement request. Drop both callbacks on a collision.
                if let Some(index) = pending
                    .iter()
                    .position(|entry| entry.notification == notification)
                {
                    pending.remove(index);
                } else {
                    if pending.len() == PENDING_LIMIT {
                        pending.pop_front();
                    }
                    pending.push_back(Pending {
                        notification,
                        created: Instant::now(),
                        token: None,
                        allowed: request.allowed,
                        activate: request.activate,
                    });
                }
            }
        }
        let Some(bus) = &connection else {
            continue;
        };
        if !bus.read(if pending.is_empty() { 0 } else { 50 }) {
            pending.clear();
            connection = None;
            continue;
        }
        for _ in 0..PENDING_LIMIT {
            let Some(message) = bus.pop() else {
                break;
            };
            let Some((sender, id, signal)) = message.signal() else {
                continue;
            };
            let Some(index) = pending.iter().position(|entry| {
                entry.notification.id == id && entry.notification.sender.as_c_str() == sender
            }) else {
                continue;
            };
            match signal {
                dbus::Signal::Token(token) => {
                    if valid_token(token) {
                        pending[index].token = Some(token.to_owned());
                    }
                }
                dbus::Signal::Closed => {
                    pending.remove(index);
                }
                dbus::Signal::Activated => {
                    let entry = pending.remove(index).expect("matched notification");
                    if (entry.allowed)() {
                        (entry.activate)(entry.token);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_text_does_not_supply_markup_or_control_characters() {
        let text = clean_text("a\0<b> & c\r\n😀d", 13, true);
        assert_eq!(escape_markup(&text), "a&lt;b&gt; &amp; c \n😀d");
        assert_eq!(clean_text("long\nline", 6, false), "long l");
        assert!(valid_token("test_4/click"));
        assert!(!valid_token("\nclick"));
        assert!(!valid_token(&"x".repeat(4097)));
    }

    #[test]
    fn an_explicit_session_bus_address_cannot_trigger_x11_autolaunch() {
        for address in [
            "",
            "autolaunch:",
            "unix:path=/run/user/1000/bus;autolaunch:",
        ] {
            assert!(!usable_session_bus_address(OsStr::new(address)));
        }
        for address in ["unix:path=/run/user/1000/bus", "unix:abstract=fixture"] {
            assert!(usable_session_bus_address(OsStr::new(address)));
        }
    }
}
