//! A persistent session-bus sender for LauncherEntry counters.

use std::env;
use std::ffi::{CStr, OsStr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;

use super::{dbus, usable_session_bus_address};

pub const DESKTOP_APP_ID: &str = "kortty";

struct DesktopEntry {
    id: &'static str,
    file: &'static str,
    launcher_uri: &'static CStr,
    launcher_path: &'static CStr,
}

// libunity uses the decimal CRC32 of the desktop filename in the object path.
const NATIVE: DesktopEntry = DesktopEntry {
    id: DESKTOP_APP_ID,
    file: "kortty.desktop",
    launcher_uri: c"application://kortty.desktop",
    launcher_path: c"/com/canonical/unity/launcherentry/1711539872",
};
const FLATPAK: DesktopEntry = DesktopEntry {
    id: "io.github.chardonnay.korTTY",
    file: "io.github.chardonnay.korTTY.desktop",
    launcher_uri: c"application://io.github.chardonnay.korTTY.desktop",
    launcher_path: c"/com/canonical/unity/launcherentry/2830777151",
};

fn desktop_entry(flatpak_id: Option<&OsStr>, flatpak_info: bool) -> &'static DesktopEntry {
    if flatpak_info && flatpak_id == Some(OsStr::new(FLATPAK.id)) {
        &FLATPAK
    } else {
        &NATIVE
    }
}

fn installed_entry() -> &'static DesktopEntry {
    desktop_entry(
        env::var_os("FLATPAK_ID").as_deref(),
        Path::new("/.flatpak-info").is_file(),
    )
}

/// Returns the ID of the desktop file used by the running Linux package.
pub fn desktop_app_id() -> &'static str {
    installed_entry().id
}

struct Update {
    count: i64,
    urgent: bool,
    finished: Box<dyn FnOnce(bool) + Send>,
}

#[derive(Default)]
struct Pending {
    latest: Option<Update>,
    stopped: bool,
}

#[derive(Default)]
struct Queue {
    pending: Mutex<Pending>,
    ready: Condvar,
}

/// Keeps the D-Bus name alive while the desktop displays its launcher entry.
pub struct LauncherBadge(Arc<Queue>);

impl LauncherBadge {
    pub fn start() -> Option<Self> {
        let address = env::var_os("DBUS_SESSION_BUS_ADDRESS")?;
        let desktop = installed_entry();
        if !usable_session_bus_address(&address) || !installed_desktop_file(desktop.file) {
            return None;
        }
        let queue = Arc::new(Queue::default());
        let worker = Arc::clone(&queue);
        thread::Builder::new()
            .name("kortty-launcher-badge".into())
            .spawn(move || run(&worker, desktop))
            .ok()?;
        Some(Self(queue))
    }

    /// Coalesces updates while the bus is busy. Superseded callbacks are dropped.
    pub fn set(&self, count: usize, urgent: bool, finished: impl FnOnce(bool) + Send + 'static) {
        let mut pending = self
            .0
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        pending.latest = Some(Update {
            count: i64::try_from(count).unwrap_or(i64::MAX),
            urgent,
            finished: Box::new(finished),
        });
        self.0.ready.notify_one();
    }
}

impl Drop for LauncherBadge {
    fn drop(&mut self) {
        let mut pending = self
            .0
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        pending.latest = None;
        pending.stopped = true;
        self.0.ready.notify_one();
    }
}

fn run(queue: &Queue, desktop: &DesktopEntry) {
    let api = dbus::Api::load();
    let mut connection = None;
    loop {
        let update = {
            let pending = queue.pending.lock().unwrap_or_else(PoisonError::into_inner);
            let mut pending = queue
                .ready
                .wait_while(pending, |pending| {
                    !pending.stopped && pending.latest.is_none()
                })
                .unwrap_or_else(PoisonError::into_inner);
            if pending.stopped {
                break;
            }
            pending.latest.take().expect("badge update is ready")
        };
        if connection.is_none() {
            connection = api.as_ref().and_then(dbus::Api::connect_private);
        }
        let sent = connection.as_ref().is_some_and(|bus| {
            bus.launcher_badge(
                desktop.launcher_path,
                desktop.launcher_uri,
                update.count,
                update.urgent,
            )
        });
        if !sent {
            connection = None;
        }
        (update.finished)(sent);
    }
}

fn installed_desktop_file(file: &str) -> bool {
    let user = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            let home = PathBuf::from(env::var_os("HOME")?);
            home.is_absolute().then(|| home.join(".local/share"))
        });
    let system = env::var_os("XDG_DATA_DIRS")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    user.into_iter()
        .chain(env::split_paths(&system))
        .filter(|path| path.is_absolute())
        .any(|path| path.join("applications").join(file).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatpak_desktop_identity_requires_both_the_expected_id_and_the_marker() {
        let correct = Some(OsStr::new("io.github.chardonnay.korTTY"));
        assert_eq!(
            desktop_entry(correct, true).id,
            "io.github.chardonnay.korTTY"
        );
        assert_eq!(desktop_entry(correct, false).id, DESKTOP_APP_ID);
        assert_eq!(desktop_entry(None, true).id, DESKTOP_APP_ID);
        let other = Some(OsStr::new("io.github.chardonnay.kortty"));
        assert_eq!(desktop_entry(other, true).id, DESKTOP_APP_ID);
    }

    #[test]
    #[ignore = "requires an isolated D-Bus session and temporary desktop entry"]
    fn native_launcher_badge_uses_one_persistent_sender_and_clears() {
        let badge = LauncherBadge::start().expect("private session bus and desktop entry");
        let update = |count, urgent| {
            let (send, receive) = std::sync::mpsc::sync_channel(1);
            badge.set(count, urgent, move |sent| {
                let _ = send.send(sent);
            });
            assert!(
                receive
                    .recv_timeout(std::time::Duration::from_secs(4))
                    .unwrap()
            );
        };
        update(2, true);
        update(1, false);
        update(0, false);
    }
}
