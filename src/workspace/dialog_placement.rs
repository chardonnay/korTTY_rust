//! Remember the normal bounds of each native dialog independently.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use iced::{Point, Size, Task, window};

use crate::profile::{SavedPosition, WindowPlacement, dialog_geometry};

use super::{Message, Workspace, window_lifecycle};

#[cfg(any(target_os = "windows", target_os = "linux"))]
mod desktop;

const SAVE_DELAY: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(in crate::workspace) enum Kind {
    About,
    AiManager,
    AsciiArt,
    GlobalSettings,
    QuickConnect,
    TerminalLink,
    ConnectionManager,
    ConnectionOverview,
    OsBadgeManager,
    CredentialVault,
    CloseAll,
    LogfileManager,
    LogfileViewer,
    Backup,
    RestoreBackup,
    Sftp,
    Editor,
    KnownHosts,
    JobScheduler,
    SessionProcesses,
    TerminalEffects,
    OpenPgpRecipients,
    VideoManager,
    VideoExport,
    VideoReplay,
    JournalManager,
    JournalViewer,
    JournalMarkerManager,
    JournalScreenshotEditor,
    JournalExport,
    JournalScreenshotExport,
    JournalEntryDelete,
    JournalActions,
    JournalReplace,
    JournalRename,
    JournalNote,
    JournalScreenshotText,
    ShellIntegration,
    ConnectionImport,
    ConnectionExport,
    TabRename,
    TabGroupName,
    SshKeyManager,
    SshKeyEdit,
    SshKeyDelete,
    Manual,
    SnippetManager,
    SnippetComparison,
    Environments,
    RemoteHide,
    RemoteChooser,
    RemoteTransferConfirm,
    Diagram,
}

impl Kind {
    const fn key(self) -> &'static str {
        match self {
            Self::About => "about",
            Self::AiManager => "ai-manager",
            Self::AsciiArt => "ascii-art",
            Self::GlobalSettings => "global-settings",
            Self::QuickConnect => "quick-connect",
            Self::TerminalLink => "terminal-link",
            Self::ConnectionManager => "connection-manager",
            Self::ConnectionOverview => "connection-overview",
            Self::OsBadgeManager => "os-badge-manager",
            Self::CredentialVault => "credential-vault",
            Self::CloseAll => "close-all",
            Self::LogfileManager => "logfile-manager",
            Self::LogfileViewer => "logfile-viewer",
            Self::Backup => "backup",
            Self::RestoreBackup => "restore-backup",
            Self::Sftp => "sftp",
            Self::Editor => "editor",
            Self::KnownHosts => "known-hosts",
            Self::JobScheduler => "job-scheduler",
            Self::SessionProcesses => "session-processes",
            Self::TerminalEffects => "terminal-effects",
            Self::OpenPgpRecipients => "openpgp-recipients",
            Self::VideoManager => "video-manager",
            Self::VideoExport => "video-export",
            Self::VideoReplay => "video-replay",
            Self::JournalManager => "journal-manager",
            Self::JournalViewer => "journal-viewer",
            Self::JournalMarkerManager => "journal-marker-manager",
            Self::JournalScreenshotEditor => "journal-screenshot-editor",
            Self::JournalExport => "journal-export",
            Self::JournalScreenshotExport => "journal-screenshot-export",
            Self::JournalEntryDelete => "journal-entry-delete",
            Self::JournalActions => "journal-actions",
            Self::JournalReplace => "journal-replace",
            Self::JournalRename => "journal-rename",
            Self::JournalNote => "journal-note",
            Self::JournalScreenshotText => "journal-screenshot-text",
            Self::ShellIntegration => "shell-integration",
            Self::ConnectionImport => "connection-import",
            Self::ConnectionExport => "connection-export",
            Self::TabRename => "tab-rename",
            Self::TabGroupName => "tab-group-name",
            Self::SshKeyManager => "ssh-key-manager",
            Self::SshKeyEdit => "ssh-key-edit",
            Self::SshKeyDelete => "ssh-key-delete",
            Self::Manual => "manual",
            Self::SnippetManager => "snippet-manager",
            Self::SnippetComparison => "snippet-comparison",
            Self::Environments => "environments",
            Self::RemoteHide => "remote-hide",
            Self::RemoteChooser => "remote-chooser",
            Self::RemoteTransferConfirm => "remote-transfer-confirm",
            Self::Diagram => "diagram",
        }
    }
}

#[derive(Default)]
pub(super) struct State {
    writer: Option<Writer>,
    remembered: HashMap<Kind, Option<WindowPlacement>>,
    open: HashMap<window::Id, Live>,
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    focused: Option<window::Id>,
}

struct Live {
    kind: Kind,
    current: Option<WindowPlacement>,
    preferred: WindowPlacement,
    restored_position: Option<Point>,
    revision: u64,
    checking: bool,
    closing: bool,
    #[cfg(target_os = "linux")]
    opening: Option<desktop::Opening>,
}

struct Writer {
    requests: mpsc::Sender<Write>,
    stopped: async_channel::Receiver<()>,
}

enum Write {
    Placement(Kind, WindowPlacement),
    Flush,
}

impl State {
    pub(super) fn initialize() -> Self {
        let (requests, incoming) = mpsc::channel();
        let (stopped_sender, stopped) = async_channel::bounded(1);
        let writer = match std::thread::Builder::new()
            .name("kortty-dialog-geometry".into())
            .spawn(move || {
                write_geometry(incoming);
                let _ = stopped_sender.try_send(());
            }) {
            Ok(_) => Some(Writer { requests, stopped }),
            Err(error) => {
                eprintln!("kortty: could not start dialog geometry writer: {error}");
                None
            }
        };
        Self {
            writer,
            ..Self::default()
        }
    }

    fn remembered(&mut self, kind: Kind) -> Option<WindowPlacement> {
        *self.remembered.entry(kind).or_insert_with(|| {
            self.writer.as_ref()?;
            match dialog_geometry::load(kind.key()) {
                Ok(placement) => Some(placement),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    eprintln!("kortty: could not read {} geometry: {error}", kind.key());
                    None
                }
            }
        })
    }

    fn register(
        &mut self,
        id: window::Id,
        kind: Kind,
        preferred: WindowPlacement,
        restored_position: Option<Point>,
    ) {
        self.open.insert(
            id,
            Live {
                kind,
                current: None,
                preferred,
                restored_position,
                revision: 0,
                checking: false,
                closing: false,
                #[cfg(target_os = "linux")]
                opening: None,
            },
        );
    }

    pub(super) fn observe(&mut self, id: window::Id, event: &window::Event) -> Task<Message> {
        let Some(live) = self.open.get_mut(&id) else {
            return Task::none();
        };
        match *event {
            window::Event::Opened { size, position } => {
                // Opening on another monitor may have constrained the saved bounds.
                // Treat those initial bounds as a baseline, not an edit.
                live.current = Some(WindowPlacement {
                    width: size.width,
                    height: size.height,
                    position: position.and_then(|point| SavedPosition::new(point.x, point.y)),
                });
                return Task::none();
            }
            window::Event::Resized(_) | window::Event::Moved(_) | window::Event::Rescaled(_) => {}
            _ => return Task::none(),
        }
        if self.writer.is_none() || live.current.is_none() || live.closing {
            return Task::none();
        }
        live.revision = live.revision.wrapping_add(1);
        if live.checking {
            return Task::none();
        }
        live.checking = true;
        observe_placement(id, live.revision)
    }

    pub(super) fn checked(
        &mut self,
        id: window::Id,
        revision: u64,
        placement: Option<WindowPlacement>,
    ) -> Task<Message> {
        let Some(live) = self.open.get_mut(&id).filter(|live| !live.closing) else {
            return Task::none();
        };
        if live.revision != revision {
            return observe_placement(id, live.revision);
        }
        live.checking = false;
        self.capture(id, placement);
        Task::none()
    }

    pub(super) fn capture(&mut self, id: window::Id, placement: Option<WindowPlacement>) {
        let Some((live, placement)) = self.open.get_mut(&id).zip(placement) else {
            return;
        };
        let Some(current) = live.current else {
            return;
        };
        if same_bounds(current, placement) {
            return;
        }
        live.current = Some(placement);
        #[cfg(target_os = "linux")]
        if live
            .opening
            .as_ref()
            .is_some_and(desktop::Opening::constrained)
        {
            return;
        }
        if let Some(target) = live.restored_position.take()
            && current.width == placement.width
            && current.height == placement.height
            && placement.position.is_some_and(|point| {
                (point.x - target.x).abs() <= 1.0 && (point.y - target.y).abs() <= 1.0
            })
        {
            // AppKit's after-open correction of the outer frame isn't a user move.
            return;
        }
        // The opening work area may have reduced one dimension of the live window.
        // Only adopt the dimensions that actually changed after opening.
        let previous = self
            .remembered
            .get(&live.kind)
            .copied()
            .flatten()
            .unwrap_or(live.preferred);
        let placement = WindowPlacement {
            width: if current.width != placement.width {
                placement.width
            } else {
                previous.width
            },
            height: if current.height != placement.height {
                placement.height
            } else {
                previous.height
            },
            position: if same_position(current, placement) {
                previous.position
            } else {
                placement.position
            },
        };
        self.remembered.insert(live.kind, Some(placement));
        if let Some(writer) = &self.writer {
            let _ = writer.requests.send(Write::Placement(live.kind, placement));
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn opening_subscription(&self) -> iced::Subscription<Message> {
        iced::Subscription::batch(
            self.open
                .iter()
                .filter(|(_, live)| live.opening.is_some())
                .map(|(&id, _)| desktop::Opening::subscription(id)),
        )
    }

    #[cfg(target_os = "linux")]
    pub(super) fn opening_redraw(&mut self, id: window::Id) -> Task<Message> {
        let Some(live) = self.open.get_mut(&id) else {
            return Task::none();
        };
        if live.opening.as_mut().is_some_and(desktop::Opening::placed) {
            live.opening
                .take()
                .map_or_else(Task::none, |opening| opening.release(id))
        } else {
            Task::none()
        }
    }

    pub(super) fn native_windows(&self) -> impl Iterator<Item = window::Id> + '_ {
        self.open.iter().filter_map(|(&id, live)| {
            (self.writer.is_some() && live.current.is_some()).then_some(id)
        })
    }

    pub(super) fn closed(&mut self, id: window::Id) {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if self.focused == Some(id) {
            self.focused = None;
        }
        if self.open.remove(&id).is_some()
            && let Some(writer) = &self.writer
        {
            let _ = writer.requests.send(Write::Flush);
        }
    }

    pub(super) fn focused(&mut self, id: window::Id) {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        {
            self.focused = Some(id);
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        let _ = id;
    }

    pub(super) fn finish(&mut self) -> Option<async_channel::Receiver<()>> {
        let Writer { requests, stopped } = self.writer.take()?;
        drop(requests);
        Some(stopped)
    }
}

fn same_bounds(a: WindowPlacement, b: WindowPlacement) -> bool {
    a.width == b.width && a.height == b.height && same_position(a, b)
}

fn same_position(a: WindowPlacement, b: WindowPlacement) -> bool {
    a.position.map(|point| (point.x, point.y)) == b.position.map(|point| (point.x, point.y))
}

fn observe_placement(id: window::Id, revision: u64) -> Task<Message> {
    inspect_normal(id)
        .map(move |placement| Message::DialogPlacementChecked(id, revision, placement))
}

fn inspect_normal(id: window::Id) -> Task<Option<WindowPlacement>> {
    window_lifecycle::inspect_placement(id)
        .collect()
        .map(|results| {
            results
                .into_iter()
                .next()
                .and_then(|(_, placement)| placement)
        })
}

fn write_geometry(requests: mpsc::Receiver<Write>) {
    let mut pending = HashMap::new();
    loop {
        let received = if pending.is_empty() {
            requests
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        } else {
            requests.recv_timeout(SAVE_DELAY)
        };
        match received {
            Ok(Write::Placement(kind, placement)) => {
                pending.insert(kind, placement);
            }
            Ok(Write::Flush) | Err(mpsc::RecvTimeoutError::Timeout) => save_pending(&mut pending),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                save_pending(&mut pending);
                return;
            }
        }
    }
}

fn save_pending(pending: &mut HashMap<Kind, WindowPlacement>) {
    for (kind, placement) in pending.drain() {
        if let Err(error) = dialog_geometry::save(kind.key(), placement) {
            eprintln!("kortty: could not save {} geometry: {error}", kind.key());
        }
    }
}

impl Workspace {
    pub(in crate::workspace) fn open_placed_dialog(
        &mut self,
        kind: Kind,
        settings: window::Settings,
    ) -> (window::Id, Task<window::Id>) {
        self.open_placed_dialog_with_fallback(kind, settings, None)
    }

    pub(in crate::workspace) fn open_placed_dialog_with_fallback(
        &mut self,
        kind: Kind,
        mut settings: window::Settings,
        fallback: Option<WindowPlacement>,
    ) -> (window::Id, Task<window::Id>) {
        let remembered = self.dialog_placement.remembered(kind);
        let saved = remembered.or(fallback);
        if let Some(saved) = saved {
            settings.size = Size::new(saved.width, saved.height);
        }
        let position = saved.and_then(|saved| saved.position);
        let preferred = WindowPlacement {
            width: settings.size.width,
            height: settings.size.height,
            position,
        };
        window_lifecycle::fit_dialog_settings(&mut settings, position);
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if self.dialog_placement.writer.is_some()
            && let Some(reference) = self.dialog_placement.focused.or_else(|| {
                self.windows
                    .iter()
                    .max_by_key(|(_, window)| (window.focused, window.number))
                    .map(|(&id, _)| id)
            })
        {
            #[cfg(target_os = "linux")]
            let opening = desktop::Opening::new(remembered.is_some(), &settings);
            let (id, opened) = desktop::open(
                reference,
                settings,
                position,
                #[cfg(target_os = "linux")]
                opening.clone(),
            );
            self.dialog_placement.register(id, kind, preferred, None);
            #[cfg(target_os = "linux")]
            if let Some(live) = self.dialog_placement.open.get_mut(&id) {
                live.opening = opening;
            }
            return (id, opened);
        }
        #[cfg(target_os = "macos")]
        let position = match settings.position {
            window::Position::Specific(point) => Some(point),
            _ => None,
        };
        #[cfg(not(target_os = "macos"))]
        let position = None;
        let (id, opened) = window::open(settings);
        self.dialog_placement
            .register(id, kind, preferred, position);
        #[cfg(target_os = "macos")]
        let opened = if let Some(position) = position {
            opened.then(move |id| window::move_to(id, position).chain(Task::done(id)))
        } else {
            opened
        };
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        let opened = if let Some(position) = saved.and_then(|saved| saved.position) {
            opened.then(move |id| {
                window_lifecycle::restore_native_position(id, Some(position))
                    .discard()
                    .chain(Task::done(id))
            })
        } else {
            opened
        };
        (id, opened)
    }

    pub(in crate::workspace) fn close_placed_dialog(&mut self, id: window::Id) -> Task<Message> {
        if self.dialog_placement.writer.is_some()
            && let Some(live) = self.dialog_placement.open.get_mut(&id)
            && live.current.is_some()
        {
            if live.closing {
                return Task::none();
            }
            live.closing = true;
            return inspect_normal(id)
                .map(move |placement| Message::DialogPlacementClosing(id, placement));
        }
        self.dialog_placement.closed(id);
        window::close(id)
    }

    pub(super) fn finish_dialog_close(
        &mut self,
        id: window::Id,
        placement: Option<WindowPlacement>,
    ) -> Task<Message> {
        self.dialog_placement.capture(id, placement);
        self.dialog_placement.closed(id);
        window::close(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_on_a_smaller_display_does_not_replace_the_saved_dialog_bounds() {
        let mut state = State::default();
        let saved = WindowPlacement {
            width: 1400.0,
            height: 1000.0,
            position: None,
        };
        let smaller = WindowPlacement {
            width: 900.0,
            height: 620.0,
            position: None,
        };
        state.remembered.insert(Kind::AiManager, Some(saved));
        let id = window::Id::unique();
        state.register(id, Kind::AiManager, saved, None);
        let _ = state.observe(
            id,
            &window::Event::Opened {
                size: Size::new(smaller.width, smaller.height),
                position: None,
            },
        );
        state.capture(id, Some(smaller));
        assert_eq!(state.remembered(Kind::AiManager), Some(saved));
        let moved = WindowPlacement {
            position: SavedPosition::new(40.0, 60.0),
            ..smaller
        };
        state.capture(id, Some(moved));
        let preferred = WindowPlacement {
            position: moved.position,
            ..saved
        };
        assert_eq!(state.remembered(Kind::AiManager), Some(preferred));
        let changed = WindowPlacement {
            width: 825.0,
            ..moved
        };
        state.capture(id, Some(changed));
        state.closed(id);
        assert_eq!(
            state.remembered(Kind::AiManager),
            Some(WindowPlacement {
                width: changed.width,
                ..preferred
            })
        );
        assert_eq!(state.remembered(Kind::GlobalSettings), None);
    }
}
