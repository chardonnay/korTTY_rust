//! Remember the normal bounds of each native dialog independently.

use std::collections::HashMap;
use std::sync::mpsc;
#[cfg(target_os = "macos")]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use iced::{Point, Size, Task, window};

use crate::profile::{SavedPosition, WindowPlacement, dialog_geometry};

use super::{Message, Workspace, window_lifecycle};

#[cfg(any(target_os = "windows", target_os = "linux"))]
mod desktop;
pub(super) mod escape;

const SAVE_DELAY: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(in crate::workspace) enum Kind {
    About,
    AiManager,
    SavedChats,
    AsciiArt,
    GlobalSettings,
    QuickConnect,
    TerminalLink,
    ConnectionManager,
    ConnectionEditor,
    ConnectionOverview,
    ConnectionMapPdf,
    TeamworkSettings,
    TeamworkRestore,
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
    HighlightRules,
    OpenPgpRecipients,
    VideoManager,
    VideoExport,
    VideoReplay,
    JournalManager,
    JournalViewer,
    JournalSummaryProgress,
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
    CodingAgentAlias,
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
    TerminalAgent,
    TerminalAgentFloating,
    TerminalAgentPictureInPicture,
    AgentWorkflow,
    AgentSwarm,
}

impl Kind {
    const fn key(self) -> &'static str {
        match self {
            Self::About => "about",
            Self::AiManager => "ai-manager",
            Self::SavedChats => "saved-chats",
            Self::AsciiArt => "ascii-art",
            Self::GlobalSettings => "global-settings",
            Self::QuickConnect => "quick-connect",
            Self::TerminalLink => "terminal-link",
            Self::ConnectionManager => "connection-manager",
            Self::ConnectionEditor => "connection-editor",
            Self::ConnectionOverview => "connection-overview",
            Self::ConnectionMapPdf => "connection-map-pdf",
            Self::TeamworkSettings => "teamwork-settings",
            Self::TeamworkRestore => "teamwork-restore",
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
            Self::HighlightRules => "terminal-highlighting",
            Self::OpenPgpRecipients => "openpgp-recipients",
            Self::VideoManager => "video-manager",
            Self::VideoExport => "video-export",
            Self::VideoReplay => "video-replay",
            Self::JournalManager => "journal-manager",
            Self::JournalViewer => "journal-viewer",
            Self::JournalSummaryProgress => "journal-summary-progress",
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
            Self::CodingAgentAlias => "coding-agent-alias",
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
            Self::TerminalAgent => "terminal-agent",
            Self::TerminalAgentFloating => "terminal-agent-floating",
            Self::TerminalAgentPictureInPicture => "terminal-agent-picture-in-picture",
            Self::AgentWorkflow => "agent-workflow",
            Self::AgentSwarm => "agent-swarm",
        }
    }
}

#[derive(Default)]
pub(super) struct State {
    writer: Option<Writer>,
    disabled: bool,
    remembered: HashMap<Kind, Option<WindowPlacement>>,
    open: HashMap<window::Id, Live>,
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    focused: Option<window::Id>,
}

#[derive(Clone, Copy)]
struct Owner {
    window: window::Id,
    modal: bool,
}

struct Live {
    kind: Kind,
    current: Option<WindowPlacement>,
    preferred: WindowPlacement,
    restored_position: Option<Point>,
    revision: u64,
    checking: bool,
    closing: bool,
    after_close: Task<Message>,
    rebasing: bool,
    owner: Option<Owner>,
    native_opened: bool,
    #[cfg(target_os = "macos")]
    pending_native_show: Option<Arc<AtomicBool>>,
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
    pub(super) fn initialize(remembering: bool) -> Self {
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
            disabled: !remembering,
            ..Self::default()
        }
    }

    fn remembered(&mut self, kind: Kind) -> Option<WindowPlacement> {
        if self.disabled {
            return None;
        }
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

    pub(super) fn set_remembering(&mut self, remembering: bool) -> Task<Message> {
        let disabled = !remembering;
        if self.disabled == disabled {
            return Task::none();
        }
        self.disabled = disabled;
        if self.disabled || self.writer.is_none() {
            return Task::none();
        }
        // Observe a new baseline for dialogs that were open while saving was off.
        // Bump the revision so a placement already in flight cannot become that baseline.
        Task::batch(self.open.iter_mut().filter_map(|(&id, live)| {
            live.current?;
            live.rebasing = true;
            (!live.closing).then(|| live.request_check(id))
        }))
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
                after_close: Task::none(),
                rebasing: false,
                owner: None,
                native_opened: false,
                #[cfg(target_os = "macos")]
                pending_native_show: None,
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
        if self.disabled || self.writer.is_none() || live.current.is_none() || live.closing {
            return Task::none();
        }
        live.request_check(id)
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
        if self.disabled {
            live.checking = false;
            return Task::none();
        }
        if live.revision != revision {
            return observe_placement(id, live.revision);
        }
        live.checking = false;
        self.capture(id, placement);
        Task::none()
    }

    pub(super) fn capture(&mut self, id: window::Id, placement: Option<WindowPlacement>) {
        if self.disabled {
            return;
        }
        let Some((live, placement)) = self.open.get_mut(&id).zip(placement) else {
            return;
        };
        let Some(current) = live.current else {
            return;
        };
        if std::mem::take(&mut live.rebasing) {
            live.current = Some(placement);
            live.restored_position = None;
            return;
        }
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
        let (kind, preferred) = (live.kind, live.preferred);
        let previous = self.remembered(kind).unwrap_or(preferred);
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
        self.remembered.insert(kind, Some(placement));
        if let Some(writer) = &self.writer {
            let _ = writer.requests.send(Write::Placement(kind, placement));
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
            (!self.disabled && self.writer.is_some() && live.current.is_some()).then_some(id)
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
        #[cfg(target_os = "macos")]
        for live in self.open.values_mut() {
            live.cancel_native_show();
        }
        let Writer { requests, stopped } = self.writer.take()?;
        drop(requests);
        Some(stopped)
    }
}

impl Live {
    fn start_closing(&mut self) {
        self.closing = true;
        #[cfg(target_os = "macos")]
        self.cancel_native_show();
    }

    #[cfg(target_os = "macos")]
    fn cancel_native_show(&mut self) {
        if let Some(show) = self.pending_native_show.take() {
            show.store(false, Ordering::Relaxed);
        }
    }

    fn request_check(&mut self, id: window::Id) -> Task<Message> {
        self.revision = self.revision.wrapping_add(1);
        if self.checking {
            return Task::none();
        }
        self.checking = true;
        observe_placement(id, self.revision)
    }
}

#[cfg(target_os = "macos")]
impl Drop for Live {
    fn drop(&mut self) {
        self.cancel_native_show();
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
        settings: window::Settings,
        fallback: Option<WindowPlacement>,
    ) -> (window::Id, Task<window::Id>) {
        self.open_placed_dialog_from(kind, settings, fallback, None)
    }

    pub(super) fn open_owned_placed_dialog(
        &mut self,
        owner: window::Id,
        kind: Kind,
        settings: window::Settings,
        focus: Option<fn(window::Id) -> Message>,
    ) -> (window::Id, Task<Message>) {
        self.open_associated_dialog(
            Owner {
                window: owner,
                modal: true,
            },
            kind,
            settings,
            focus,
        )
    }

    pub(super) fn open_modeless_placed_dialog(
        &mut self,
        owner: window::Id,
        kind: Kind,
        settings: window::Settings,
    ) -> (window::Id, Task<Message>) {
        self.open_associated_dialog(
            Owner {
                window: owner,
                modal: false,
            },
            kind,
            settings,
            None,
        )
    }

    fn open_associated_dialog(
        &mut self,
        owner: Owner,
        kind: Kind,
        settings: window::Settings,
        focus: Option<fn(window::Id) -> Message>,
    ) -> (window::Id, Task<Message>) {
        #[cfg(target_os = "windows")]
        let settings = window::Settings {
            visible: false,
            minimizable: settings.minimizable && !owner.modal,
            platform_specific: window::settings::PlatformSpecific {
                skip_taskbar: true,
                ..settings.platform_specific
            },
            ..settings
        };
        #[cfg(target_os = "macos")]
        let settings = window::Settings {
            visible: false,
            minimizable: settings.minimizable && !owner.modal,
            ..settings
        };
        let (id, opened) = self.open_placed_dialog_from(kind, settings, None, Some(owner));
        self.dialog_placement
            .open
            .get_mut(&id)
            .expect("placement for a newly opened dialog")
            .owner = Some(owner);
        let opened = opened.map(move |id| {
            Message::OwnedDialogOpened(id, owner.window, focus.map(|focus| Box::new(focus(id))))
        });
        (id, opened)
    }

    pub(super) fn placed_dialog_owner(&self, id: window::Id) -> Option<window::Id> {
        self.dialog_placement
            .open
            .get(&id)?
            .owner
            .map(|owner| owner.window)
    }

    fn native_dialog_owner_is_live(&self, dialog: window::Id, mut owner: window::Id) -> bool {
        while owner != dialog {
            if self.windows.contains_key(&owner) {
                return !self.main_placement_closing.contains(&owner);
            }
            let Some(placement) = self
                .dialog_placement
                .open
                .get(&owner)
                .filter(|placement| !placement.closing)
            else {
                return false;
            };
            let Some(parent) = placement.owner else {
                return true;
            };
            owner = parent.window;
        }
        false
    }

    pub(super) fn finish_owned_dialog_open(
        &mut self,
        id: window::Id,
        owner: window::Id,
        focus: Option<Message>,
    ) -> Task<Message> {
        let owner_is_live = self.native_dialog_owner_is_live(id, owner);
        let Some(placement) = self.dialog_placement.open.get_mut(&id) else {
            return window::close(id);
        };
        let Some(association) = placement
            .owner
            .filter(|association| association.window == owner)
        else {
            return Task::none();
        };
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        let _ = association;
        if placement.closing || placement.native_opened {
            return Task::none();
        }
        if !owner_is_live {
            return self.queue_placed_dialog_close(id, None);
        }
        placement.native_opened = true;
        #[cfg(target_os = "windows")]
        {
            native_dialog_owner(owner)
                .then(move |owner| {
                    let bind = owner.map_or_else(Task::none, |owner| {
                        window::run(id, move |native| {
                            if let Ok(dialog) = native.window_handle() {
                                kortty_windowing::bind_dialog_owner(
                                    dialog,
                                    owner,
                                    association.modal,
                                );
                            }
                        })
                        .discard()
                    });
                    bind.chain(show_native_owned_dialog(id))
                })
                .chain(focus.map_or_else(Task::none, Task::done))
        }
        #[cfg(target_os = "macos")]
        {
            let show = Arc::new(AtomicBool::new(true));
            placement.pending_native_show = Some(Arc::clone(&show));
            native_dialog_owner(owner)
                .then(move |owner| {
                    let show = Arc::clone(&show);
                    window::run(id, move |native| {
                        // Close can be requested while the owner lookup is queued.
                        if show.swap(false, Ordering::Relaxed)
                            && let Ok(dialog) = native.window_handle()
                        {
                            kortty_windowing::show_appkit_owned_dialog(dialog, owner);
                        }
                    })
                    .discard()
                })
                .chain(focus.map_or_else(Task::none, Task::done))
        }
        #[cfg(target_os = "linux")]
        {
            let mut focus = focus;
            desktop::attach_x11_owner(id, association).then(move |x11| {
                let focus = focus.take();
                if x11 {
                    Task::done(Message::X11OwnedDialogReady(id, owner, focus.map(Box::new)))
                } else {
                    focus.map_or_else(Task::none, Task::done)
                }
            })
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        focus.map_or_else(Task::none, Task::done)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn show_x11_owned_dialog(
        &self,
        id: window::Id,
        owner: window::Id,
        focus: Option<Message>,
    ) -> Task<Message> {
        if !self.native_dialog_owner_is_live(id, owner)
            || !self
                .dialog_placement
                .open
                .get(&id)
                .is_some_and(|placement| {
                    placement
                        .owner
                        .is_some_and(|association| association.window == owner)
                        && placement.native_opened
                        && !placement.closing
                })
        {
            return Task::none();
        }
        show_native_owned_dialog(id).chain(focus.map_or_else(Task::none, Task::done))
    }

    fn open_placed_dialog_from(
        &mut self,
        kind: Kind,
        mut settings: window::Settings,
        fallback: Option<WindowPlacement>,
        owner: Option<Owner>,
    ) -> (window::Id, Task<window::Id>) {
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        let _ = owner;
        let remembered = self.dialog_placement.remembered(kind);
        let saved = remembered.or(fallback.filter(|_| !self.dialog_placement.disabled));
        if let Some(saved) = saved {
            settings.size = Size::new(saved.width, saved.height);
        }
        let position = saved.and_then(|saved| saved.position);
        let preferred = WindowPlacement {
            width: settings.size.width,
            height: settings.size.height,
            position,
        };
        #[cfg(target_os = "linux")]
        {
            settings.platform_specific.application_id = kortty_windowing::DESKTOP_APP_ID.into();
        }
        window_lifecycle::fit_dialog_settings(&mut settings, position);
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        let probe_desktop = self.dialog_placement.writer.is_some();
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        let reference = owner.map(|owner| owner.window);
        #[cfg(target_os = "linux")]
        let probe_desktop = probe_desktop || owner.is_some();
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if probe_desktop
            && let Some(reference) = reference.or(self.dialog_placement.focused).or_else(|| {
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
                #[cfg(target_os = "linux")]
                owner,
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
        self.close_placed_dialog_with_focus(id, None)
    }

    pub(super) fn close_placed_dialog_then(
        &mut self,
        id: window::Id,
        after: Task<Message>,
    ) -> Task<Message> {
        self.close_placed_dialog_with_followup(id, None, after)
    }

    pub(super) fn close_placed_dialogs_for_owner_shutdown(
        &mut self,
        owner: window::Id,
        after: Task<Message>,
    ) -> Task<Message> {
        let owned: Vec<_> = self
            .dialog_placement
            .open
            .iter()
            .filter_map(|(&id, live)| {
                live.owner
                    .is_some_and(|association| association.window == owner)
                    .then_some(id)
            })
            .collect();
        owned.into_iter().fold(after, |after, id| {
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            let close = {
                // Detach and close each child through Iced before its native
                // owner, including children awaiting a bounds check.
                let followup = self.finish_placed_dialog_closed(id);
                close_native_owned_dialog(id).chain(followup).chain(after)
            };
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            let close = self.close_placed_dialog_then(id, after);
            self.close_placed_dialogs_for_owner_shutdown(id, close)
        })
    }

    pub(super) fn close_placed_dialog_and_focus(
        &mut self,
        id: window::Id,
        owner: window::Id,
    ) -> Task<Message> {
        self.close_placed_dialog_with_focus(id, Some(owner))
    }

    fn close_placed_dialog_with_focus(
        &mut self,
        id: window::Id,
        focus: Option<window::Id>,
    ) -> Task<Message> {
        self.close_placed_dialog_with_followup(id, focus, Task::none())
    }

    pub(super) fn close_placed_dialog_with_followup(
        &mut self,
        id: window::Id,
        focus: Option<window::Id>,
        after: Task<Message>,
    ) -> Task<Message> {
        let Some(live) = self.dialog_placement.open.get_mut(&id) else {
            return self.queue_placed_dialog_close(id, focus).chain(after);
        };
        live.after_close = std::mem::take(&mut live.after_close).chain(after);
        if live.closing {
            return Task::none();
        }
        if !self.dialog_placement.disabled
            && self.dialog_placement.writer.is_some()
            && live.current.is_some()
        {
            live.start_closing();
            return inspect_normal(id)
                .map(move |placement| Message::DialogPlacementClosing(id, placement, focus));
        }
        self.queue_placed_dialog_close(id, focus)
    }

    pub(super) fn finish_dialog_close(
        &mut self,
        id: window::Id,
        placement: Option<WindowPlacement>,
        focus: Option<window::Id>,
    ) -> Task<Message> {
        if !self.dialog_placement.open.contains_key(&id) {
            return Task::none();
        }
        self.dialog_placement.capture(id, placement);
        self.queue_placed_dialog_close(id, focus)
    }

    fn queue_placed_dialog_close(
        &mut self,
        id: window::Id,
        focus: Option<window::Id>,
    ) -> Task<Message> {
        let focus = focus.filter(|&owner| self.native_dialog_owner_is_live(id, owner));
        let after = self.take_placed_dialog_followup(id);
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        if let Some(live) = self.dialog_placement.open.get_mut(&id)
            && live.owner.is_some()
            && live.native_opened
        {
            // Keep an acknowledged child discoverable by a concurrent owner
            // shutdown until Iced removes the native window and reports Closed.
            live.start_closing();
            return focus_after_close(close_native_owned_dialog(id), focus).chain(after);
        }
        self.dialog_placement.closed(id);
        focus_after_close(window::close(id), focus).chain(after)
    }

    fn take_placed_dialog_followup(&mut self, id: window::Id) -> Task<Message> {
        self.dialog_placement
            .open
            .get_mut(&id)
            .map_or_else(Task::none, |live| std::mem::take(&mut live.after_close))
    }

    pub(super) fn finish_placed_dialog_closed(&mut self, id: window::Id) -> Task<Message> {
        let after = self.take_placed_dialog_followup(id);
        self.dialog_placement.closed(id);
        after
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn native_dialog_owner(owner: window::Id) -> Task<Option<kortty_windowing::NativeWindow>> {
    window::run(owner, |native| {
        kortty_windowing::identify(native.window_handle().ok()?, native.display_handle().ok()?)
    })
    .collect()
    .map(|mut handles| handles.pop().flatten())
}

fn focus_after_close(close: Task<Message>, focus: Option<window::Id>) -> Task<Message> {
    match focus {
        Some(owner) => close.chain(window::gain_focus(owner)),
        None => close,
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
fn show_native_owned_dialog(id: window::Id) -> Task<Message> {
    window::set_mode(id, window::Mode::Windowed).chain(window::gain_focus(id))
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn close_native_owned_dialog(id: window::Id) -> Task<Message> {
    window::run(id, |native| {
        if let Ok(dialog) = native.window_handle() {
            kortty_windowing::release_modal_dialog_owner(dialog);
        }
    })
    .discard()
    .chain(window::close(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placed_dialog_close_follows_bounds_capture_before_continuing() {
        use iced::futures::{StreamExt, executor::block_on};
        use iced_runtime::{Action as RuntimeAction, task, window::Action as WindowAction};

        let mut workspace = Workspace::default();
        let (dialog, _) =
            workspace.open_placed_dialog(Kind::ShellIntegration, window::Settings::default());
        let (requests, _incoming) = mpsc::channel();
        let (_stop, stopped) = async_channel::bounded(1);
        workspace.dialog_placement.writer = Some(Writer { requests, stopped });
        let live = workspace.dialog_placement.open.get_mut(&dialog).unwrap();
        live.current = Some(live.preferred);
        let capture = workspace.close_placed_dialog_then(dialog, Task::done(Message::Noop));
        assert!(task::into_stream(capture).is_some());
        assert!(workspace.dialog_placement.open[&dialog].closing);

        let close = workspace.finish_dialog_close(dialog, None, None);
        let mut actions = task::into_stream(close).unwrap();
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Close(id))) if id == dialog
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Output(Message::Noop))
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn canceled_wayland_open_acknowledgment_drops_orphaned_placement() {
        use iced::futures::{StreamExt, executor::block_on};
        use iced_runtime::{Action as RuntimeAction, task, window::Action as WindowAction};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_window(false);
        let (dialog, _) = workspace.open_owned_placed_dialog(
            owner,
            Kind::GlobalSettings,
            window::Settings::default(),
            None,
        );
        // Model a close racing the queued native creation and completion.
        workspace.windows.remove(&owner);
        let close = workspace.update(Message::OwnedDialogOpened(dialog, owner, None));
        let mut actions = task::into_stream(close).unwrap();
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Close(id))) if id == dialog
        ));
        assert!(!workspace.dialog_placement.open.contains_key(&dialog));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn withdrawn_x11_dialog_maps_only_for_its_live_owner() {
        use iced::futures::{StreamExt, executor::block_on};
        use iced_runtime::{Action as RuntimeAction, task, window::Action as WindowAction};

        use super::super::settings_window;

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        let _ = workspace.update_settings(settings_window::Message::Open(owner));
        let dialog = workspace.settings.for_owner(owner).unwrap();

        assert!(task::into_stream(workspace.show_x11_owned_dialog(dialog, owner, None)).is_none());
        let binding = workspace.finish_owned_dialog_open(dialog, owner, None);
        let mut actions = task::into_stream(binding).unwrap();
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Run(id, _))) if id == dialog
        ));
        assert!(task::into_stream(workspace.show_x11_owned_dialog(dialog, other, None)).is_none());
        let show = workspace.show_x11_owned_dialog(dialog, owner, None);
        let mut actions = task::into_stream(show).unwrap();
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::SetMode(id, window::Mode::Windowed)))
                if id == dialog
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::GainFocus(id))) if id == dialog
        ));
        let _ = workspace.update_settings(settings_window::Message::Cancel(dialog));
        assert!(task::into_stream(workspace.show_x11_owned_dialog(dialog, owner, None)).is_none());
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn owned_dialog_is_closed_before_its_owner_while_bounds_are_pending() {
        use iced::futures::{StreamExt, executor::block_on};
        use iced_runtime::{Action as RuntimeAction, task, window::Action as WindowAction};

        use super::super::settings_window;

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_window(false);
        let _ = workspace.update_settings(settings_window::Message::Open(owner));
        let dialog = workspace.settings.owner_closed(owner).unwrap();
        workspace
            .dialog_placement
            .open
            .get_mut(&dialog)
            .unwrap()
            .closing = true;
        assert!(
            task::into_stream(workspace.finish_owned_dialog_open(dialog, owner, None)).is_none()
        );

        let mut actions = task::into_stream(workspace.close_window(owner)).unwrap();
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Run(id, _))) if id == dialog
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Close(id))) if id == dialog
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Close(id))) if id == owner
        ));
        assert!(
            task::into_stream(workspace.finish_dialog_close(dialog, None, Some(owner))).is_none()
        );

        let late_open = workspace.finish_owned_dialog_open(dialog, owner, None);
        assert!(matches!(
            block_on(task::into_stream(late_open).unwrap().next()),
            Some(RuntimeAction::Window(WindowAction::Close(id))) if id == dialog
        ));
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn owner_shutdown_can_still_find_a_dialog_with_a_queued_close() {
        use super::super::settings_window;

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_window(false);
        let _ = workspace.update_settings(settings_window::Message::Open(owner));
        let dialog = workspace.settings.for_owner(owner).unwrap();
        let _ = workspace.finish_owned_dialog_open(dialog, owner, None);
        #[cfg(target_os = "macos")]
        let show = Arc::clone(
            workspace.dialog_placement.open[&dialog]
                .pending_native_show
                .as_ref()
                .unwrap(),
        );
        let _queued_close = workspace.update_settings(settings_window::Message::Cancel(dialog));

        assert!(workspace.settings.for_owner(owner).is_none());
        assert!(workspace.dialog_placement.open[&dialog].closing);
        #[cfg(target_os = "macos")]
        assert!(!show.load(Ordering::Relaxed));
        let _forced_close = workspace.close_window(owner);
        assert!(!workspace.dialog_placement.open.contains_key(&dialog));
    }

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

    #[test]
    fn dialog_geometry_can_be_disabled_and_resumed_without_recording_disabled_changes() {
        let mut workspace = Workspace::default();
        let (requests, writes) = mpsc::channel();
        let (_stop, stopped) = async_channel::bounded(1);
        let state = &mut workspace.dialog_placement;
        state.writer = Some(Writer { requests, stopped });
        let kind = Kind::AiManager;
        let saved = WindowPlacement {
            width: 1100.0,
            height: 750.0,
            position: SavedPosition::new(70.0, 90.0),
        };
        state.remembered.insert(kind, Some(saved));
        let id = window::Id::unique();
        state.register(id, kind, saved, None);
        let _ = state.observe(
            id,
            &window::Event::Opened {
                size: Size::new(saved.width, saved.height),
                position: Some(Point::new(70.0, 90.0)),
            },
        );
        let _ = state.observe(id, &window::Event::Resized(Size::new(830.0, 500.0)));
        let old_revision = state.open[&id].revision;
        let _ = state.set_remembering(false);
        let changed_while_disabled = WindowPlacement {
            width: 830.0,
            height: 500.0,
            position: SavedPosition::new(100.0, 120.0),
        };
        state.capture(id, Some(changed_while_disabled));
        assert_eq!(state.remembered(kind), None);
        assert_eq!(state.native_windows().next(), None);
        assert!(writes.try_recv().is_err());

        let default = Size::new(720.0, 530.0);
        let (opened_disabled, _) = workspace.open_placed_dialog_with_fallback(
            kind,
            window::Settings {
                size: default,
                ..window::Settings::default()
            },
            Some(saved),
        );
        let state = &mut workspace.dialog_placement;
        assert_eq!(
            state.open[&opened_disabled].preferred,
            WindowPlacement {
                width: default.width,
                height: default.height,
                position: None,
            }
        );

        let _ = state.set_remembering(true);
        let fresh_revision = state.open[&id].revision;
        assert_ne!(old_revision, fresh_revision);
        let _ = state.checked(id, old_revision, Some(changed_while_disabled));
        assert_eq!(state.open[&id].current, Some(saved));
        let _ = state.checked(id, fresh_revision, Some(changed_while_disabled));
        assert_eq!(state.open[&id].current, Some(changed_while_disabled));
        assert_eq!(state.remembered(kind), Some(saved));
        assert!(writes.try_recv().is_err());

        let changed_while_enabled = WindowPlacement {
            width: 850.0,
            ..changed_while_disabled
        };
        state.capture(id, Some(changed_while_enabled));
        let expected = WindowPlacement {
            width: changed_while_enabled.width,
            ..saved
        };
        assert_eq!(state.remembered(kind), Some(expected));
        let Write::Placement(written_kind, placement) = writes.try_recv().unwrap() else {
            panic!("Expected a placement save after remembering was enabled");
        };
        assert_eq!(written_kind, kind);
        assert_eq!(placement, expected);
    }
}
