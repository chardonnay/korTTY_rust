use async_channel::Receiver;
use iced::widget::{button, row, text};
use iced::{Alignment, Element, Subscription, Task, window};
use kortty_terminal::TerminalProfile;

use crate::profile::session::{RecentlyClosed, RestoreMode, Snapshot, Store};
use crate::profile::{WindowLayout, WindowPlacement, WorkspaceLayout};

use super::{Message as WorkspaceMessage, Workspace, window_lifecycle};

mod writer;
use writer::{Event, Writer};

#[derive(Clone, Debug)]
pub enum Message {
    Restore(window::Id),
    Dismiss(window::Id),
    Mode(RestoreMode),
    Worker(Event),
}

#[derive(Default)]
pub(super) struct Restore {
    writer: Option<Writer>,
    mode: RestoreMode,
    previous: Option<Snapshot>,
    latest: Option<Snapshot>,
    offered: bool,
    automatic: bool,
    clear_when_empty: bool,
    write_failed: bool,
    history_writable: bool,
    loaded_history: Option<RecentlyClosed>,
    notice: Option<String>,
}

impl Restore {
    pub(super) fn initialize() -> Self {
        match Self::load() {
            Ok(restore) => restore,
            Err(error) => Self {
                notice: Some(format!("Session recovery unavailable ({error})")),
                ..Self::default()
            },
        }
    }

    fn load() -> std::io::Result<Self> {
        Self::load_from(Store::open()?)
    }

    fn load_from(store: Store) -> std::io::Result<Self> {
        // Do not replace unreadable recovery data during this run.
        let previous = store.previous()?;
        let (mode, mut notice) = match store.restore_mode() {
            Ok(mode) => (mode, None),
            Err(error) => (
                RestoreMode::Ask,
                Some(format!(
                    "Could not load the session restore setting ({error})"
                )),
            ),
        };
        let mut latest = previous.clone();
        let mut automatic = false;
        if let Some(snapshot) = latest.as_mut().filter(|snapshot| snapshot.clean_exit) {
            let auto_allowed = should_restore_automatically(mode, snapshot);
            // An interrupted startup must require confirmation on the next run.
            snapshot.clean_exit = false;
            match store.write(Some(snapshot)) {
                Ok(()) => automatic = auto_allowed,
                Err(error) => {
                    latest = previous.clone();
                    notice = Some(format!(
                        "Could not mark session recovery as started ({error})"
                    ));
                }
            }
        }
        let offered = previous.is_some() && mode != RestoreMode::Off && !automatic;
        let loaded_history = match store.recently_closed() {
            Ok(history) => Some(history),
            Err(error) => {
                notice.get_or_insert_with(|| {
                    format!("Could not load recently closed tabs ({error})")
                });
                None
            }
        };
        Ok(Self {
            writer: Some(Writer::start(store)?),
            mode,
            previous,
            latest,
            offered,
            automatic,
            history_writable: loaded_history.is_some(),
            loaded_history,
            notice,
            ..Self::default()
        })
    }

    pub(super) fn save_recently_closed(&self, history: RecentlyClosed) {
        if self.history_writable
            && let Some(writer) = &self.writer
        {
            writer.history(history);
        }
    }

    pub(super) fn history_load_failed(&mut self, error: std::io::Error) {
        self.history_writable = false;
        self.notice
            .get_or_insert_with(|| format!("Could not load recently closed tabs ({error})"));
    }

    pub(super) fn preference(&self) -> Option<RestoreMode> {
        self.writer.as_ref().map(|_| self.mode)
    }

    pub(super) fn can_restore(&self) -> bool {
        self.previous.is_some()
    }

    pub(super) fn subscription(&self) -> Subscription<WorkspaceMessage> {
        self.writer
            .as_ref()
            .map_or_else(Subscription::none, Writer::subscription)
    }

    pub(super) fn changed(&mut self, user_closed_last_tab: bool) {
        if let Some(writer) = &self.writer {
            self.clear_when_empty |= user_closed_last_tab;
            writer.changed();
        }
    }

    fn save_layout(&mut self, layout: WorkspaceLayout) {
        let Some(writer) = &self.writer else {
            return;
        };
        let clear = std::mem::take(&mut self.clear_when_empty);
        let candidate = if layout.windows.is_empty() {
            if !clear {
                return;
            }
            None
        } else {
            Some(Snapshot {
                layout,
                clean_exit: false,
            })
        };
        if self.latest != candidate || self.write_failed {
            self.latest = candidate.clone();
            self.write_failed = false;
            writer.write(candidate);
        }
    }

    fn final_write(&self, layout: Option<WorkspaceLayout>) -> Option<Option<Snapshot>> {
        let layout = layout?;
        if layout.windows.is_empty() {
            if self.clear_when_empty {
                return Some(None);
            }
            // Closing a window is different from explicitly closing its last tab.
            if self.offered {
                return None;
            }
            return self.latest.clone().map(|mut snapshot| {
                snapshot.clean_exit = true;
                Some(snapshot)
            });
        }
        Some(Some(Snapshot {
            layout,
            clean_exit: true,
        }))
    }

    fn finish(&mut self, layout: Option<WorkspaceLayout>) -> Option<Receiver<()>> {
        let writer = self.writer.take()?;
        Some(writer.finish(self.final_write(layout)))
    }
}

#[cfg(test)]
mod tests;

fn should_restore_automatically(mode: RestoreMode, snapshot: &Snapshot) -> bool {
    mode == RestoreMode::Auto && snapshot.clean_exit
}

pub(super) fn changes_layout(message: &WorkspaceMessage) -> bool {
    use iced::keyboard::{Key, key::Named};
    use iced::widget::pane_grid::DragEvent;

    match message {
        WorkspaceMessage::WindowGeometryChecked(_, _, Some(_), _)
        | WorkspaceMessage::ConnectIbm(_)
        | WorkspaceMessage::Serial(_, crate::serial_dialog::Message::Connect)
        | WorkspaceMessage::Ssh(super::ssh_access::Message::Dialed(..))
        | WorkspaceMessage::SelectTab(_, _)
        | WorkspaceMessage::Split(_, _)
        | WorkspaceMessage::FocusPane(_, _)
        | WorkspaceMessage::ClosePane(_, _)
        | WorkspaceMessage::ConfirmSerialClose(_, _, true)
        | WorkspaceMessage::ZoomPane(_, _)
        | WorkspaceMessage::ResizeSplit(_, _)
        | WorkspaceMessage::DragPane(_, DragEvent::Dropped { .. })
        | WorkspaceMessage::LayoutLoaded(_, Ok(_)) => true,
        WorkspaceMessage::Key(_, Key::Named(key), _, modifiers, _) => {
            modifiers.control()
                && modifiers.alt()
                && matches!(
                    key,
                    Named::ArrowLeft | Named::ArrowRight | Named::ArrowUp | Named::ArrowDown
                )
        }
        _ => false,
    }
}

impl Workspace {
    pub(super) fn boot_session_restore(&mut self) -> (window::Id, Task<WorkspaceMessage>) {
        if self.clipboard.policy.problem.is_some() {
            return self.open_policy_window();
        }
        self.session_restore = Restore::initialize();
        if let Some(history) = self.session_restore.loaded_history.take() {
            self.load_recently_closed(history);
        }
        self.startup_session_window()
    }

    fn startup_session_window(&mut self) -> (window::Id, Task<WorkspaceMessage>) {
        if std::mem::take(&mut self.session_restore.automatic) {
            match self.restore_previous_layout(None) {
                Ok(Some(opened)) => return opened,
                Ok(None) => {}
                Err(error) => {
                    self.session_restore.offered = true;
                    self.session_restore.notice = Some(format!(
                        "Could not restore the session automatically ({error})"
                    ));
                }
            }
        }
        let (id, task) = self.open_recovery_window();
        if let Some(notice) = self.session_restore.notice.take() {
            self.notice(id, notice);
        }
        self.session_restore.changed(false);
        (id, task)
    }

    fn restore_previous_layout(
        &mut self,
        reuse: Option<window::Id>,
    ) -> Result<Option<(window::Id, Task<WorkspaceMessage>)>, String> {
        let Some(previous) = self.session_restore.previous.take() else {
            return Ok(None);
        };
        match self.restore_layout_into(&previous.layout, reuse) {
            Ok(opened) => {
                self.session_restore.offered = false;
                self.session_restore.changed(false);
                Ok(Some(opened))
            }
            Err(error) => {
                self.session_restore.previous = Some(previous);
                Err(error)
            }
        }
    }

    pub(super) fn open_recovery_window(&mut self) -> (window::Id, Task<WorkspaceMessage>) {
        if self.session_restore.offered && self.clipboard.policy.problem.is_none() {
            self.open_window(false)
        } else {
            self.open_policy_window()
        }
    }

    pub(super) fn update_session_restore(&mut self, message: Message) -> Task<WorkspaceMessage> {
        match message {
            Message::Mode(mode) => {
                if let Some(writer) = &self.session_restore.writer {
                    self.session_restore.mode = mode;
                    writer.mode(mode);
                }
            }
            Message::Restore(id) => {
                if self.clipboard.policy.problem.is_some() || !self.windows.contains_key(&id) {
                    return Task::none();
                }
                let window = &self.windows[&id];
                let reuse = (window.tabs.is_empty()
                    && window.pending_ibm.is_none()
                    && window.pending_serial.is_none()
                    && window.pending_ssh.is_none())
                .then_some(id);
                match self.restore_previous_layout(reuse) {
                    Ok(Some((_, task))) => return task,
                    Ok(None) => {}
                    Err(error) => {
                        self.notice(id, format!("Could not restore the session ({error})"))
                    }
                }
            }
            Message::Dismiss(id) => {
                if !self.session_restore.offered || !self.windows.contains_key(&id) {
                    return Task::none();
                }
                self.session_restore.offered = false;
                self.session_restore.clear_when_empty = true;
                let window = &self.windows[&id];
                if window.tabs.is_empty()
                    && window.pending_ibm.is_none()
                    && window.pending_serial.is_none()
                    && window.pending_ssh.is_none()
                {
                    self.new_tab(id, TerminalProfile::Xterm);
                }
                self.session_restore.changed(false);
            }
            Message::Worker(Event::Capture) => {
                if let Some(layout) = self.capture_session_layout() {
                    self.session_restore.save_layout(layout);
                }
            }
            Message::Worker(Event::HistoryWriteFailed(error)) => {
                if let Some(id) = self
                    .windows
                    .iter()
                    .max_by_key(|(_, window)| (window.focused, window.number))
                    .map(|(&id, _)| id)
                {
                    self.notice(id, format!("Could not save recently closed tabs ({error})"));
                }
            }
            Message::Worker(Event::WriteFailed(error)) => {
                self.session_restore.write_failed = true;
                if let Some(id) = self
                    .windows
                    .iter()
                    .max_by_key(|(_, window)| (window.focused, window.number))
                    .map(|(&id, _)| id)
                {
                    self.notice(id, format!("Could not save session recovery ({error})"));
                }
            }
        }
        Task::none()
    }

    pub(super) fn session_restore_offer(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, WorkspaceMessage>> {
        let snapshot = self
            .session_restore
            .previous
            .as_ref()
            .filter(|_| self.session_restore.offered)?;
        let windows = snapshot.layout.windows.len();
        let tabs: usize = snapshot
            .layout
            .windows
            .iter()
            .map(|window| window.tabs.len())
            .sum();
        Some(
            row![
                text(format!(
                    "Restore previous session ({windows} windows, {tabs} tabs)?"
                ))
                .size(13),
                button("Restore").on_press(WorkspaceMessage::SessionRestore(Message::Restore(id))),
                button("Dismiss").on_press(WorkspaceMessage::SessionRestore(Message::Dismiss(id))),
            ]
            .spacing(5)
            .align_y(Alignment::Center)
            .into(),
        )
    }

    fn capture_session_layout(&self) -> Option<WorkspaceLayout> {
        let mut layout = self.capture_layout()?;
        layout.windows.retain(|window| !window.tabs.is_empty());
        Some(layout)
    }

    pub(super) fn preserve_closing_session_window(&mut self, closing: window::Id) {
        if self.session_restore.writer.is_some()
            && self.windows.iter().all(|(&id, window)| {
                id == closing || window.terminal_tabs().all(|tab| tab.incognito)
            })
            && let Some(layout) = self.capture_session_layout()
        {
            self.session_restore.save_layout(layout);
        }
    }

    pub(super) fn finish_session_restore(&mut self) {
        if self.session_restore.writer.is_some() {
            let layout = self.capture_session_layout();
            if let Some(done) = self.session_restore.finish(layout) {
                self.pending_shutdown.push(done);
            }
        }
    }

    pub(super) fn open_restored_window(
        &mut self,
        saved: &WindowLayout,
        reuse: Option<window::Id>,
    ) -> (window::Id, Task<WorkspaceMessage>) {
        let placement = saved.placement();
        let Some(id) = reuse.filter(|id| {
            self.windows
                .get(id)
                .is_some_and(|window| window.tabs.is_empty())
        }) else {
            let (id, opened) = self.open_window_at_placement(placement);
            let task = if saved.maximized {
                opened.chain(window::maximize(id, true))
            } else {
                opened
            };
            return (id, task);
        };
        let window = self.windows.get_mut(&id).expect("checked window");
        window.geometry.cancel_pending_placement();
        if window.fullscreen() {
            return (id, Task::none());
        }
        if window.maximized() {
            // Wayland can discard a resize before the compositor confirms unmaximize.
            return (
                id,
                self.restore_placement_after_unmaximize(id, placement, saved.maximized),
            );
        }
        (
            id,
            self.apply_restored_window_placement(id, placement, saved.maximized),
        )
    }

    pub(super) fn apply_restored_window_placement(
        &mut self,
        id: window::Id,
        placement: WindowPlacement,
        maximize: bool,
    ) -> Task<WorkspaceMessage> {
        let Some(window) = self.windows.get_mut(&id) else {
            return Task::none();
        };
        let settings = window_lifecycle::main_window_settings(placement);
        let size = settings.size;
        window.geometry.remember_placement(size, placement.position);
        let resized = window::resize(id, size);
        let moved = match settings.position {
            window::Position::Specific(point) => window::move_to(id, point),
            _ => window_lifecycle::restore_native_position(id, placement.position),
        };
        let positioned = resized.chain(moved);
        if maximize {
            positioned.chain(window::maximize(id, true))
        } else {
            positioned
        }
    }
}
